//! zbrain remote-source git helpers — ported from TS `src/core/git-remote.ts`.
//!
//! Single source of SSRF-defensive git invocations. `clone_repo` and
//! `pull_repo` both spread `GIT_SSRF_FLAGS` so a future flag added to one
//! path lands on both — single source of truth.
//!
//! Uses `tokio::process::Command` for async git CLI invocation.

use std::path::Path;
use std::time::Duration;
use std::error::Error;
use std::fmt;

/// Error parsing a remote source spec.
#[derive(Debug)]
pub enum GitRemoteError {
    /// Could not parse the spec as a valid remote.
    InvalidSpec(String),
}

impl fmt::Display for GitRemoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GitRemoteError::InvalidSpec(s) => write!(f, "Invalid remote spec: {}", s),
        }
    }
}

impl Error for GitRemoteError {}

/// Parse a remote source spec into a URL. Accepts:
/// - owner/repo (→ GitHub HTTPS)
/// - https://... (already a URL)
/// - git@github.com:owner/repo (→ SSH → still converted to HTTPS for anonymous fetch)
pub fn parse_remote_url(spec: &str) -> Result<String, GitRemoteError> {
    // Already a full URL
    if spec.starts_with("https://") || spec.starts_with("http://") {
        return Ok(spec.to_string());
    }
    // SSH format git@github.com:owner/repo → convert to https://github.com/owner/repo
    if let Some((_, rest)) = spec.split_once("git@") {
        let https = rest.replace(':', "/");
        return Ok(format!("https://{}", https));
    }
    // owner/repo → assume GitHub HTTPS
    if spec.contains('/') && !spec.contains(|c| c == ' ' || c == ':') {
        return Ok(format!("https://github.com/{}", spec));
    }
    Err(GitRemoteError::InvalidSpec(spec.to_string()))
}

/// Global git config flags. Spread BEFORE the subcommand verb.
/// - `http.followRedirects=false`: closes DNS rebinding via redirect chains
/// - `protocol.file.allow=never`: no local-file URLs (defense in depth)
/// - `protocol.ext.allow=never`: no external helpers (`git-remote-foo`)
pub const GIT_SSRF_FLAGS: &[&str] = &[
    "-c",
    "http.followRedirects=false",
    "-c",
    "protocol.file.allow=never",
    "-c",
    "protocol.ext.allow=never",
];

/// Subcommand-level flags. Spread AFTER the subcommand verb (clone/pull).
/// - `--no-recurse-submodules`: .gitmodules cannot become a second fetch surface
pub const GIT_SSRF_SUBCOMMAND_FLAGS: &[&str] = &["--no-recurse-submodules"];

/// Environment variables for git process isolation.
const GIT_ENV: &[(&str, &str)] = &[
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GCM_INTERACTIVE", "never"),
    ("GIT_ASKPASS", "/bin/false"),
    ("SSH_ASKPASS", "/bin/false"),
];

/// Clone options.
#[derive(Debug, Clone)]
pub struct CloneOpts {
    /// Clone depth. Default 1 (shallow). 0 means full clone.
    pub depth: u32,
    /// Branch to clone. `None` means default branch.
    pub branch: Option<String>,
    /// Timeout in milliseconds. Default 600_000 (10 min).
    pub timeout_ms: u64,
}

impl Default for CloneOpts {
    fn default() -> Self {
        Self {
            depth: 1,
            branch: None,
            timeout_ms: 600_000,
        }
    }
}

/// Error type for git operations.
#[derive(Debug, Clone)]
pub struct GitOperationError {
    pub op: GitOp,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitOp {
    Clone,
    Pull,
    RemoteGetUrl,
}

impl GitOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Pull => "pull",
            Self::RemoteGetUrl => "remote_get_url",
        }
    }
}

impl GitOperationError {
    pub fn new(op: GitOp, message: impl Into<String>) -> Self {
        Self {
            op,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for GitOperationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "git {} failed: {}", self.op.as_str(), self.message)
    }
}

impl std::error::Error for GitOperationError {}

/// Repo on-disk state classification. Mirrors TS `RepoState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoState {
    /// `.git` exists + remote origin URL matches
    Healthy,
    /// Path does not exist (ENOENT)
    Missing,
    /// Path exists but is not a directory
    NotADir,
    /// Directory exists but no `.git/`
    NoGit,
    /// `.git` exists but origin URL differs from expected
    UrlDrift,
    /// `.git` exists but `git remote get-url origin` failed
    Corrupted,
}

/// Clone a remote git repo with SSRF-defensive flags.
///
/// - `dest_dir` must NOT exist or must be empty.
/// - Default `--depth=1` (no history); pass `depth: 0` for full clone.
/// - Returns `Err(GitOperationError)` on failure; caller is responsible for cleanup.
pub async fn clone_repo(
    url: &str,
    dest_dir: &Path,
    opts: CloneOpts,
) -> Result<(), GitOperationError> {
    // Pre-condition: dest_dir must not exist or be empty
    if dest_dir.exists() {
        let entries: Vec<_> = match std::fs::read_dir(dest_dir) {
            Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
            Err(e) => {
                return Err(GitOperationError::new(
                    GitOp::Clone,
                    format!("Cannot inspect destination {}: {e}", dest_dir.display()),
                ));
            }
        };
        if !entries.is_empty() {
            return Err(GitOperationError::new(
                GitOp::Clone,
                format!(
                    "Destination {} exists and is not empty; refusing to clone",
                    dest_dir.display()
                ),
            ));
        }
    }

    let mut args: Vec<String> = Vec::new();
    for flag in GIT_SSRF_FLAGS {
        args.push(flag.to_string());
    }
    args.push("clone".to_string());
    for flag in GIT_SSRF_SUBCOMMAND_FLAGS {
        args.push(flag.to_string());
    }
    if opts.depth != 0 {
        args.push(format!("--depth={}", opts.depth));
    }
    if let Some(ref branch) = opts.branch {
        args.push("--branch".to_string());
        args.push(branch.clone());
    }
    args.push(url.to_string());
    args.push(dest_dir.to_string_lossy().to_string());

    run_git(&args, opts.timeout_ms).await.map_err(|e| {
        GitOperationError::new(
            GitOp::Clone,
            format!("git clone failed for {url}: {e}"),
        )
    })
}

/// Pull a repo with `--ff-only` and the same SSRF-defensive flags as `clone_repo`.
pub async fn pull_repo(repo_path: &Path, timeout_ms: u64) -> Result<(), GitOperationError> {
    let mut args: Vec<String> = vec!["-C".to_string(), repo_path.to_string_lossy().to_string()];
    for flag in GIT_SSRF_FLAGS {
        args.push(flag.to_string());
    }
    args.push("pull".to_string());
    for flag in GIT_SSRF_SUBCOMMAND_FLAGS {
        args.push(flag.to_string());
    }
    args.push("--ff-only".to_string());

    run_git(&args, timeout_ms).await.map_err(|e| {
        GitOperationError::new(
            GitOp::Pull,
            format!("git pull failed in {}: {e}", repo_path.display()),
        )
    })
}

/// Classify the on-disk state of a clone. Used by sync to decide whether to
/// run pull (healthy), re-clone (missing/no-git/not-a-dir), refuse with
/// corruption error (corrupted), or refuse with rebase-clone hint (url-drift).
pub fn validate_repo_state(
    repo_path: &Path,
    expected_remote_url: Option<&str>,
) -> RepoState {
    let md = match std::fs::symlink_metadata(repo_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return RepoState::Missing,
        Err(_) => return RepoState::NotADir,
    };

    if !md.is_dir() {
        return RepoState::NotADir;
    }

    if !repo_path.join(".git").exists() {
        return RepoState::NoGit;
    }

    // Get remote origin URL
    let remote_url = get_remote_origin_url(repo_path);
    match remote_url {
        Ok(url) => {
            if let Some(expected) = expected_remote_url {
                if url != expected {
                    return RepoState::UrlDrift;
                }
            }
            RepoState::Healthy
        }
        Err(_) => RepoState::Corrupted,
    }
}

/// Get the origin remote URL for a repo. Synchronous because it's a quick
/// local operation used by `validate_repo_state`.
fn get_remote_origin_url(repo_path: &Path) -> Result<String, GitOperationError> {
    let output = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "remote", "get-url", "origin"])
        .envs(GIT_ENV.iter().copied())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map_err(|e| {
            GitOperationError::new(
                GitOp::RemoteGetUrl,
                format!("Failed to spawn git: {e}"),
            )
        })?;

    if !output.status.success() {
        return Err(GitOperationError::new(
            GitOp::RemoteGetUrl,
            format!(
                "git remote get-url origin failed in {}",
                repo_path.display()
            ),
        ));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(url)
}

/// Run git with the given args and timeout.
async fn run_git(args: &[String], timeout_ms: u64) -> Result<(), String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .envs(GIT_ENV.iter().copied())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();

    let result = tokio::time::timeout(Duration::from_millis(timeout_ms), output).await;

    match result {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(stderr.trim().to_string())
            }
        }
        Ok(Err(e)) => Err(format!("Failed to spawn git: {e}")),
        Err(_) => Err(format!("git timed out after {timeout_ms}ms")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_repo_state_missing() {
        let state = validate_repo_state(Path::new("/nonexistent/path/12345"), None);
        assert_eq!(state, RepoState::Missing);
    }

    #[test]
    fn validate_repo_state_no_git() {
        let tmp = TempDir::new().unwrap();
        let state = validate_repo_state(tmp.path(), None);
        assert_eq!(state, RepoState::NoGit);
    }

    #[test]
    fn git_ssrf_flags_order() {
        // Verify GIT_SSRF_FLAGS come in key-value pairs
        assert_eq!(GIT_SSRF_FLAGS.len(), 6); // 3 pairs
        assert_eq!(GIT_SSRF_FLAGS[0], "-c");
        assert_eq!(GIT_SSRF_FLAGS[1], "http.followRedirects=false");
        assert_eq!(GIT_SSRF_FLAGS[2], "-c");
        assert_eq!(GIT_SSRF_FLAGS[3], "protocol.file.allow=never");
        assert_eq!(GIT_SSRF_FLAGS[4], "-c");
        assert_eq!(GIT_SSRF_FLAGS[5], "protocol.ext.allow=never");
    }

    #[test]
    fn git_ssrf_subcommand_flags() {
        assert_eq!(GIT_SSRF_SUBCOMMAND_FLAGS.len(), 1);
        assert_eq!(GIT_SSRF_SUBCOMMAND_FLAGS[0], "--no-recurse-submodules");
    }
}
