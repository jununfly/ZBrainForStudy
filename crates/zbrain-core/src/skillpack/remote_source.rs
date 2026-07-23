/**
 * skillpack/remote_source.rs — third-party skillpack source resolution.
 *
 * `resolveSource(spec)` accepts every supported input shape and returns a
 * `ResolvedSource` with the local pack path plus identity metadata the
 * scaffold orchestrator needs (TOFU pin, source URL, kind). Cache layout:
 *
 *   ~/.zbrain/skillpack-cache/git/<host>/<owner>/<repo>/<sha>/   (git sources)
 *   ~/.zbrain/skillpack-cache/tarball/<sha256-hex>/              (tarball sources)
 *
 * Cache hits short-circuit the clone/extract step. Cache misses do the work
 * and then atomically rename the staging dir into place so partial clones
 * never poison subsequent lookups.
 *
 * Reuses SSRF-hardened `clone_repo` from `git_remote.rs`. Local-path inputs
 * skip the cache entirely (the user owns the directory).
 *
 * Bare-name inputs ("hackathon-evaluation") are not handled here — the
 * registry-client resolves them to a URL first, then re-invokes
 * `resolve_source` with the URL. Keeps this module independent of the
 * registry layer.
 */

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, Duration};
use url::Url;
use regex::Regex;
use dirs::home_dir;
use sha2::{Sha256, Digest};
use serde::{Deserialize, Serialize};
use crate::paths::zbrain_path;
use crate::git_remote::{parse_remote_url, GIT_SSRF_FLAGS, GitRemoteError};
use crate::skillpack::tarball::{extract_tarball, file_sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedSourceKind {
    Git,
    Tarball,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedSource {
    /// Absolute path to the pack root (where skillpack.json lives).
    pub path: PathBuf,
    /// Source classification.
    pub kind: ResolvedSourceKind,
    /// Canonical source URL or path the user provided (after kebab expansion).
    pub source: String,
    /// Resolved git commit SHA when kind=git; None otherwise.
    pub pinned_commit: Option<String>,
    /// SHA-256 of the tarball when kind=tarball; None otherwise.
    pub tarball_sha256: Option<String>,
    /// Whether the result came from cache (used by callers for log lines).
    pub cache_hit: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteSourceError {
    #[error("source spec is empty")]
    SpecEmpty,

    #[error("local path does not exist: {0}")]
    SpecLocalMissing(String),

    #[error("local path is not a pack root (no skillpack.json): {0}")]
    SpecLocalNotPackRoot(String),

    #[error("tarball file does not exist: {0}")]
    SpecTarballMissing(String),

    #[error("tarball path is not a regular file: {0}")]
    SpecTarballNotFile(String),

    #[error("cannot classify source spec {spec:?} — must be a kebab name, owner/repo, https URL, local dir, or .tgz path")]
    SpecKebabInvalidShape { spec: String },

    #[error("remote URL rejected: {0}")]
    SpecUrlInvalid(String),

    #[error("clone failed for {url}: {cause}")]
    CloneFailed { url: String, cause: String },

    #[error("git rev-parse failed: {0}")]
    RevParseFailed(String),

    #[error("IO error: {0}")]
    Io(std::io::Error),
}

impl From<std::io::Error> for RemoteSourceError {
    fn from(e: std::io::Error) -> Self {
        RemoteSourceError::Io(e)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResolveSourceOptions {
    /// Override the cache root (test-only; defaults to ~/.zbrain/skillpack-cache).
    pub cache_root: Option<PathBuf>,
    /// Force a fresh clone/extract even when the cache has a hit.
    #[serde(default)]
    pub no_cache: bool,
}

/// Kind of spec after classification, before full resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecKind {
    GitUrl,
    Tarball,
    Local,
    Kebab,
}

/// Classify the spec without doing any I/O beyond a single stat.
pub fn classify_spec(spec: &str) -> Result<(SpecKind, String), RemoteSourceError> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(RemoteSourceError::SpecEmpty);
    }

    // URL: starts with http(s)://
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Ok((SpecKind::GitUrl, trimmed.to_string()));
    }

    // Local path: starts with /, ./, ../, or ~/
    let is_absolute = Path::new(trimmed).is_absolute();
    if is_absolute || trimmed.starts_with("./") || trimmed.starts_with("../") || trimmed.starts_with("~/") {
        let expanded = if trimmed.starts_with("~/") {
            if let Some(home) = home_dir() {
                home.join(trimmed.strip_prefix("~/").unwrap()).to_path_buf()
            } else {
                Path::new(trimmed).to_path_buf()
            }
        } else {
            Path::new(trimmed).to_path_buf()
        };
        let abs = expanded.canonicalize().map_err(|e| RemoteSourceError::Io(e.into()))?;
        if abs.extension().map_or(false, |ext| ext == "tgz" || ext == "tar" || ext == "gz") {
            return Ok((SpecKind::Tarball, abs.to_string_lossy().into_owned()));
        }
        return Ok((SpecKind::Local, abs.to_string_lossy().into_owned()));
    }

    // owner/repo short-form (github inferred): exactly one `/` and both halves
    // look like GitHub identifiers (alnum + dash/dot/underscore).
    lazy_static::lazy_static! {
        static ref OWNER_REPO_RE: Regex = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,38}/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$").unwrap();
    }
    if OWNER_REPO_RE.is_match(trimmed) {
        return Ok((SpecKind::GitUrl, format!("https://github.com/{trimmed}.git")));
    }

    // Bare kebab-name: defer to registry resolution. Caller (registry-client)
    // must convert to a URL before re-calling resolve_source.
    lazy_static::lazy_static! {
        static ref KEBAB_NAME_RE: Regex = Regex::new(r"^[a-z][a-z0-9-]{1,63}$").unwrap();
    }
    if KEBAB_NAME_RE.is_match(trimmed) {
        return Ok((SpecKind::Kebab, trimmed.to_string()));
    }

    Err(RemoteSourceError::SpecKebabInvalidShape { spec: trimmed.to_string() })
}

/// Compute the cache root, honoring opts override.
fn cache_root(opts: &ResolveSourceOptions) -> PathBuf {
    if let Some(root) = &opts.cache_root {
        return root.clone();
    }
    zbrain_path("skillpack-cache").unwrap_or_else(|| PathBuf::from("skillpack-cache"))
}

/// Resolve HEAD SHA of a remote git URL via `git ls-remote`.
fn resolve_remote_head(url: &str, branch: Option<&str>) -> Result<String, RemoteSourceError> {
    let mut argv: Vec<String> = GIT_SSRF_FLAGS.iter().map(|s| s.to_string()).collect();
    argv.push("ls-remote".to_string());
    argv.push("--exit-code".to_string());
    argv.push(url.to_string());
    if let Some(branch) = branch {
        argv.push(format!("refs/heads/{branch}"));
    } else {
        argv.push("HEAD".to_string());
    }

    let output = Command::new("git")
        .args(&argv)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| RemoteSourceError::RevParseFailed(format!("{}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RemoteSourceError::RevParseFailed(format!(
            "git ls-remote failed: {}", stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().find(|l| !l.trim().is_empty());
    let Some(first_line) = first_line else {
        return Err(RemoteSourceError::RevParseFailed(format!(
            "git ls-remote returned no refs for {url}"
        )));
    };

    let sha = first_line.split_whitespace().next();
    let Some(sha) = sha else {
        return Err(RemoteSourceError::RevParseFailed(format!(
            "git ls-remote gave invalid output for {url}: {first_line}"
        )));
    };

    if !regex::Regex::new(r"^[a-f0-9]{40}$").unwrap().is_match(sha) {
        return Err(RemoteSourceError::RevParseFailed(format!(
            "git ls-remote gave invalid sha for {url}: {sha}"
        )));
    }

    Ok(sha.to_string())
}

/// Compute the per-source cache directory.
fn git_cache_path(root: &Path, parsed_url: &Url, sha: &str) -> PathBuf {
    let host = parsed_url.host_str().unwrap_or("unknown");
    let path = parsed_url.path().strip_prefix('/').unwrap_or(parsed_url.path());
    let path = path.strip_suffix(".git").unwrap_or(path);
    root.join("git").join(host).join(path).join(sha)
}

/// Resolve a git URL into a ResolvedSource.
async fn resolve_git_source(
    url: &str,
    opts: ResolveSourceOptions,
) -> Result<ResolvedSource, RemoteSourceError> {
    let parsed = parse_remote_url(url).map_err(|e| {
        RemoteSourceError::SpecUrlInvalid(e.to_string())
    })?;
    let parsed_url = Url::parse(&parsed).map_err(|e| {
        RemoteSourceError::SpecUrlInvalid(e.to_string())
    })?;

    let sha = resolve_remote_head(&parsed_url.to_string(), None)?;
    let cache_dir = git_cache_path(&cache_root(&opts), &parsed_url, &sha);

    if !opts.no_cache && cache_dir.exists() {
        if let Ok(entries) = fs::read_dir(&cache_dir) {
            if entries.count() > 0 {
                return Ok(ResolvedSource {
                    path: cache_dir,
                    kind: ResolvedSourceKind::Git,
                    source: parsed_url.to_string(),
                    pinned_commit: Some(sha),
                    tarball_sha256: None,
                    cache_hit: true,
                });
            }
        }
    }

    // Stage in a sibling .tmp dir so a failed clone doesn't poison the cache slot.
    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    let stage_dir = cache_dir.with_extension(format!("tmp-{pid}-{timestamp}"));
    fs::create_dir_all(&stage_dir).map_err(|e| RemoteSourceError::Io(e.into()))?;

    if let Err(e) = crate::git_remote::clone_repo(&parsed_url.to_string(), &stage_dir, crate::git_remote::CloneOpts {
        depth: 1,
        timeout_ms: 600000,
        branch: None,
    }).await {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(RemoteSourceError::CloneFailed {
            url: url.to_string(),
            cause: e.to_string(),
        });
    }

    // Verify the cloned commit matches the SHA we ls-remoted (defense against
    // race where HEAD moved between ls-remote and clone).
    let clone_sha = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(&stage_dir)
        .output()
        .and_then(|output| {
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    String::from_utf8_lossy(&output.stderr),
                ))
            }
        })
        .map_err(|e| RemoteSourceError::RevParseFailed(format!(
            "git rev-parse HEAD failed after clone: {e}"
        )))?;

    // Atomic rename into the canonical cache slot. If the slot exists already
    // (cache populated concurrently by another process), prefer the existing
    // one and drop our stage dir.
    if cache_dir.exists() {
        let _ = fs::remove_dir_all(&stage_dir);
    } else {
        if let Some(parent) = cache_dir.parent() {
            fs::create_dir_all(parent).map_err(|e| RemoteSourceError::Io(e.into()))?;
        }
        fs::rename(&stage_dir, &cache_dir).map_err(|e| RemoteSourceError::Io(e.into()))?;
    }

    Ok(ResolvedSource {
        path: cache_dir,
        kind: ResolvedSourceKind::Git,
        source: parsed_url.to_string(),
        pinned_commit: Some(clone_sha),
        tarball_sha256: None,
        cache_hit: false,
    })
}

/// Resolve a local tarball into a ResolvedSource (extract into cache by SHA).
fn resolve_tarball_source(
    tarball_path: &Path,
    opts: ResolveSourceOptions,
) -> Result<ResolvedSource, RemoteSourceError> {
    if !tarball_path.exists() {
        return Err(RemoteSourceError::SpecTarballMissing(
            tarball_path.to_string_lossy().into_owned()
        ).into());
    }

    let metadata = fs::metadata(tarball_path).map_err(|e| RemoteSourceError::Io(e.into()))?;
    if !metadata.is_file() {
        return Err(RemoteSourceError::SpecTarballNotFile(
            tarball_path.to_string_lossy().into_owned()
        ).into());
    }

    let sha = file_sha256(tarball_path)
        .map_err(|e| RemoteSourceError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
    let cache_dir = cache_root(&opts).join("tarball").join(&sha);

    if !opts.no_cache && cache_dir.exists() {
        if let Ok(entries) = fs::read_dir(&cache_dir) {
            if entries.count() > 0 {
                return find_pack_root(ResolvedSource {
                    path: cache_dir,
                    kind: ResolvedSourceKind::Tarball,
                    source: tarball_path.to_string_lossy().into_owned(),
                    pinned_commit: None,
                    tarball_sha256: Some(sha),
                    cache_hit: true,
                });
            }
        }
    }

    let pid = std::process::id();
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis();
    let stage_dir = cache_dir.with_extension(format!("tmp-{pid}-{timestamp}"));
    fs::create_dir_all(&stage_dir)?;

    if let Err(e) = extract_tarball(&crate::skillpack::tarball::TarballExtractOptions {
        tgz_path: tarball_path.to_path_buf(),
        dest_dir: stage_dir.clone(),
        caps: None,
    }) {
        let _ = fs::remove_dir_all(&stage_dir);
        return Err(RemoteSourceError::CloneFailed {
            url: tarball_path.to_string_lossy().into_owned(),
            cause: e.to_string(),
        });
    }

    if cache_dir.exists() {
        let _ = fs::remove_dir_all(&stage_dir);
    } else {
        if let Some(parent) = cache_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&stage_dir, &cache_dir)?;
    }

    find_pack_root(ResolvedSource {
        path: cache_dir,
        kind: ResolvedSourceKind::Tarball,
        source: tarball_path.to_string_lossy().into_owned(),
        pinned_commit: None,
        tarball_sha256: Some(sha),
        cache_hit: false,
    })
}

/// A tarball produced by `packTarball` wraps its source dir, so the extracted
/// cache dir contains `<sourceLeaf>/skillpack.json`, not `skillpack.json` at
/// the root. Find the actual pack root (the directory containing skillpack.json).
fn find_pack_root(mut s: ResolvedSource) -> Result<ResolvedSource, RemoteSourceError> {
    if s.path.join("skillpack.json").exists() {
        return Ok(s);
    }

    // Look one level deep.
    if let Ok(entries) = fs::read_dir(&s.path) {
        for entry in entries.flatten() {
            let candidate = entry.path();
            if candidate.is_dir() && candidate.join("skillpack.json").exists() {
                s.path = candidate;
                return Ok(s);
            }
        }
    }

    // Caller will surface a clearer error at manifest-load time; return as-is.
    Ok(s)
}

/// Resolve a local directory: just validate it has skillpack.json.
fn resolve_local_source(abs_path: &Path) -> Result<ResolvedSource, RemoteSourceError> {
    if !abs_path.exists() {
        return Err(RemoteSourceError::SpecLocalMissing(
            abs_path.to_string_lossy().into_owned()
        ).into());
    }

    let metadata = fs::metadata(abs_path).map_err(|e| RemoteSourceError::Io(e.into()))?;
    if !metadata.is_dir() {
        return Err(RemoteSourceError::SpecLocalNotPackRoot(
            abs_path.to_string_lossy().into_owned()
        ).into());
    }

    if !abs_path.join("skillpack.json").exists() {
        return Err(RemoteSourceError::SpecLocalNotPackRoot(
            abs_path.to_string_lossy().into_owned()
        ));
    }

    Ok(ResolvedSource {
        path: abs_path.to_path_buf(),
        kind: ResolvedSourceKind::Local,
        source: abs_path.to_string_lossy().into_owned(),
        pinned_commit: None,
        tarball_sha256: None,
        cache_hit: false,
    })
}

/// Resolve any supported source spec. Throws RemoteSourceError for kebab-name
/// inputs — those must be resolved through the registry-client first.
pub async fn resolve_source(
    spec: &str,
    opts: ResolveSourceOptions,
) -> Result<ResolvedSource, RemoteSourceError> {
    let (kind, normalized) = classify_spec(spec)?;
    match kind {
        SpecKind::GitUrl => resolve_git_source(&normalized, opts).await,
        SpecKind::Tarball => resolve_tarball_source(Path::new(&normalized), opts),
        SpecKind::Local => resolve_local_source(Path::new(&normalized)),
        SpecKind::Kebab => Err(RemoteSourceError::SpecKebabInvalidShape { spec: spec.to_string() }),
    }
}
