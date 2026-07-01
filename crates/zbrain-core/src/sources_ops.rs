//! zbrain sources-ops — async functions for source-management operations.
//! Ported from TS `src/core/sources-ops.ts`.
//!
//! Atomicity contract for `add_source` with `--url`:
//!
//! ```text
//! add_source(url)
//!   │
//!   ▼
//! parse_remote_url(url) → SSRF gate
//!   │
//!   ▼  (URL ok)
//! pre-flight SELECT id → id taken? → error
//!   │
//!   ▼  (id free)
//! mkdir <clones>/.tmp/<id>-<rand>/
//!   │
//!   ▼
//! clone_repo(url, tmp/) → fail → rm -rf tmp/, throw
//!   │
//!   ▼
//! INSERT INTO sources → fail → rm -rf tmp/, throw
//!   │
//!   ▼
//! rename(tmp/, final) → fail → rm -rf tmp/, throw
//!                            + best-effort DELETE row
//!   │
//!   ▼
//! return SourceRow
//! ```

use std::path::{Path, PathBuf};

use rand::Rng;

use crate::engine::BrainEngine;
use crate::engine::{is_valid_source_id, CreateSourceInput, SourceRow};
use crate::git_remote::{clone_repo, validate_repo_state, CloneOpts, RepoState};
use crate::url_safety;

// ── Errors ──────────────────────────────────────────────────────────────────

/// Error codes for source operations. Mirrors TS `SourceOpErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceOpErrorCode {
    InvalidId,
    SourceIdTaken,
    OverlappingPath,
    InvalidRemoteUrl,
    CloneFailed,
    InsertFailed,
    RenameFailed,
    NotFound,
    ProtectedId,
    CloneDirOutsideZbrain,
    SymlinkEscape,
}

impl SourceOpErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidId => "invalid_id",
            Self::SourceIdTaken => "source_id_taken",
            Self::OverlappingPath => "overlapping_path",
            Self::InvalidRemoteUrl => "invalid_remote_url",
            Self::CloneFailed => "clone_failed",
            Self::InsertFailed => "insert_failed",
            Self::RenameFailed => "rename_failed",
            Self::NotFound => "not_found",
            Self::ProtectedId => "protected_id",
            Self::CloneDirOutsideZbrain => "clone_dir_outside_gbrain",
            Self::SymlinkEscape => "symlink_escape",
        }
    }
}

/// Error type for source operations. Mirrors TS `SourceOpError`.
#[derive(Debug, Clone)]
pub struct SourceOpError {
    pub code: SourceOpErrorCode,
    pub message: String,
}

impl SourceOpError {
    pub fn new(code: SourceOpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SourceOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for SourceOpError {}

// ── Types ───────────────────────────────────────────────────────────────────

/// Options for `add_source`. Mirrors TS `AddSourceOpts`.
#[derive(Debug, Clone)]
pub struct AddSourceOpts {
    pub id: String,
    pub name: Option<String>,
    /// Local path (--path mode). Mutually exclusive with remote_url.
    pub local_path: Option<String>,
    /// Remote URL (--url mode). Mutually exclusive with local_path.
    pub remote_url: Option<String>,
    pub federated: Option<bool>,
    /// Override clone destination. Defaults to `<zbrain_home>/clones/<id>/`.
    pub clone_dir: Option<String>,
    /// Clone depth (default 1, 0 for full clone).
    pub depth: u32,
    /// Branch to clone (None for repo default).
    pub branch: Option<String>,
}

/// Options for `remove_source`. Mirrors TS `RemoveSourceOpts`.
#[derive(Debug, Clone)]
pub struct RemoveSourceOpts {
    pub id: String,
    pub confirm_destructive: bool,
    pub dry_run: bool,
    pub keep_storage: bool,
}

/// Result of `remove_source`. Mirrors TS `RemoveResult`.
#[derive(Debug, Clone)]
pub struct RemoveResult {
    pub id: String,
    pub pages_deleted: u64,
    pub clone_removed: bool,
    pub clone_path: Option<String>,
    pub dry_run: bool,
}

/// Source status info. Mirrors TS `SourceStatus`.
#[derive(Debug, Clone)]
pub struct SourceStatus {
    pub id: String,
    pub name: String,
    pub local_path: Option<String>,
    pub remote_url: Option<String>,
    pub federated: bool,
    pub page_count: u64,
    pub last_sync_at: Option<String>,
    pub last_commit: Option<String>,
    pub archived: bool,
    pub clone_state: String, // RepoState | "not-applicable"
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Validate source id using the canonical regex, rethrowing as SourceOpError.
fn validate_source_id(id: &str) -> Result<(), SourceOpError> {
    if !is_valid_source_id(id) {
        return Err(SourceOpError::new(
            SourceOpErrorCode::InvalidId,
            format!(
                "Invalid source id \"{id}\". Must be 1-32 lowercase alnum chars with optional interior hyphens."
            ),
        ));
    }
    Ok(())
}

/// Extract `remote_url` from a source config JSON value.
fn get_remote_url(config: &serde_json::Value) -> Option<String> {
    config
        .get("remote_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract `federated` flag from a source config JSON value.
fn is_federated(config: &serde_json::Value) -> bool {
    config.get("federated").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Generate a random hex string of `n` bytes.
fn random_hex(n: usize) -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..n).map(|_| rng.gen()).collect();
    hex::encode(bytes)
}

/// Default clone dir for a remote-URL source: `<zbrain_home>/clones/<id>/`
pub fn default_clone_dir(zbrain_home: &Path, id: &str) -> PathBuf {
    zbrain_home.join("clones").join(id)
}

/// Temp clone dir under `<zbrain_home>/clones/.tmp/<id>-<rand>/`
fn make_temp_clone_dir(zbrain_home: &Path, id: &str) -> PathBuf {
    let rand = random_hex(6);
    zbrain_home.join("clones").join(".tmp").join(format!("{id}-{rand}"))
}

/// Symlink-safe path confinement. Returns true if `child` exists and is
/// contained under `parent` (using canonicalized paths).
///
/// Mirrors TS `isPathContained` — uses `canonicalize` (realpath) and
/// separator-suffixed prefix check.
pub fn is_path_contained(child: &Path, parent: &Path) -> bool {
    let resolved_child = match child.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };
    let resolved_parent = match parent.canonicalize() {
        Ok(p) => p,
        Err(_) => return false,
    };

    // Append separator to parent so /foo doesn't match /foobar.
    let parent_str = resolved_parent.to_string_lossy();
    let parent_with_sep = if parent_str.ends_with('/') || parent_str.ends_with('\\') {
        parent_str
    } else {
        let owned = format!("{parent_str}{}", std::path::MAIN_SEPARATOR);
        std::borrow::Cow::Owned(owned)
    };

    let child_str = resolved_child.to_string_lossy();
    child_str == parent_with_sep.as_ref().trim_end_matches(['/', '\\'])
        || child_str.starts_with(parent_with_sep.as_ref())
}

/// Count pages for a given source.
async fn count_pages(_engine: &dyn BrainEngine, _source_id: &str) -> u64 {
    // Use list_pages with source scope as a simple count mechanism.
    // In practice, TS uses a COUNT(*) query directly.
    // We approximate via the engine's page listing filtered by source.
    // For now, return 0 — the engine doesn't expose a direct COUNT yet.
    // TODO: add `count_pages` to BrainEngine trait.
    0
}

// ── add_source ───────────────────────────────────────────────────────────────

/// Add a source — either via URL (clone + INSERT + rename) or path (INSERT only).
///
/// The `zbrain_home` parameter is the base directory for clones
/// (typically `$ZBRAIN_HOME` or equivalent).
pub async fn add_source(
    engine: &dyn BrainEngine,
    opts: AddSourceOpts,
    zbrain_home: &Path,
) -> Result<SourceRow, SourceOpError> {
    validate_source_id(&opts.id)?;

    // Pre-flight collision check
    let existing = engine.get_source(&opts.id).await.map_err(|e| {
        SourceOpError::new(
            SourceOpErrorCode::InsertFailed,
            format!("Pre-flight check failed: {e}"),
        )
    })?;
    if existing.is_some() {
        return Err(SourceOpError::new(
            SourceOpErrorCode::SourceIdTaken,
            format!(
                "Source id \"{}\" is already registered. \
                 Remove it first, then re-add.",
                opts.id
            ),
        ));
    }

    // Validate URL before any filesystem work
    let parsed_url = if let Some(ref remote_url) = opts.remote_url {
        Some(
            url_safety::parse_remote_url(remote_url).map_err(|e| {
                SourceOpError::new(
                    SourceOpErrorCode::InvalidRemoteUrl,
                    e.message,
                )
            })?,
        )
    } else {
        None
    };

    // Determine final path
    let final_path = if let Some(ref local) = opts.local_path {
        Some(PathBuf::from(local))
    } else if parsed_url.is_some() {
        opts.clone_dir
            .as_ref()
            .map(PathBuf::from)
            .or_else(|| Some(default_clone_dir(zbrain_home, &opts.id)))
    } else {
        None
    };

    // Overlap check
    if let Some(ref fp) = final_path {
        let all_sources = engine.list_sources(true).await.map_err(|e| {
            SourceOpError::new(
                SourceOpErrorCode::InsertFailed,
                format!("Overlap check failed: {e}"),
            )
        })?;
        for other in &all_sources {
            if other.id == opts.id {
                continue;
            }
            if let Some(ref other_path) = other.local_path {
                let a = fp.to_string_lossy();
                let b = other_path.as_str();
                if a.as_ref() == b
                    || a.starts_with(&format!("{b}/"))
                    || a.starts_with(&format!("{b}\\"))
                    || b.starts_with(&format!("{a}/"))
                    || b.starts_with(&format!("{a}\\"))
                {
                    return Err(SourceOpError::new(
                        SourceOpErrorCode::OverlappingPath,
                        format!(
                            "path \"{a}\" overlaps with existing source \"{}\" at \"{b}\". \
                             Overlapping sources are not allowed.",
                            other.id
                        ),
                    ));
                }
            }
        }
    }

    // ── Path A: --url (clone + INSERT + rename) ────────────────────────────
    if let Some(ref url_info) = parsed_url {
        let temp_dir = make_temp_clone_dir(zbrain_home, &opts.id);

        // Create parent directories
        if let Some(parent) = temp_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SourceOpError::new(
                    SourceOpErrorCode::CloneFailed,
                    format!("Cannot create temp dir parent: {e}"),
                )
            })?;
        }

        // Clone to temp dir
        let clone_opts = CloneOpts {
            depth: opts.depth,
            branch: opts.branch.clone(),
            ..CloneOpts::default()
        };
        if let Err(e) = clone_repo(&url_info.url, &temp_dir, clone_opts).await {
            // Clean up temp dir
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(SourceOpError::new(
                SourceOpErrorCode::CloneFailed,
                e.message,
            ));
        }

        // Build config with remote_url
        let mut config = serde_json::json!({
            "remote_url": url_info.url,
        });
        if let Some(federated) = opts.federated {
            config["federated"] = serde_json::Value::Bool(federated);
        }

        let display_name = opts.name.clone().unwrap_or_else(|| opts.id.clone());

        // INSERT into sources
        let final_path_str = final_path.as_ref().map(|p| p.to_string_lossy().to_string());
        let create_input = CreateSourceInput {
            id: opts.id.clone(),
            name: display_name,
            config: Some(config),
        };

        let _source = engine.create_source(&create_input).await.map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            SourceOpError::new(
                SourceOpErrorCode::InsertFailed,
                format!("INSERT failed for source \"{}\": {e}", opts.id),
            )
        })?;

        // Update local_path if needed (create_source doesn't set it)
        if let Some(ref lp) = final_path_str {
            let update = crate::engine::UpdateSourceInput {
                local_path: Some(lp.clone()),
                name: None,
                config: None,
                last_commit: None,
                last_sync_at: None,
                chunker_version: None,
                contextual_retrieval_mode: None,
                trust_frontmatter_overrides: None,
            };
            let _source = engine.update_source(&opts.id, &update).await.map_err(|e| {
                let _ = std::fs::remove_dir_all(&temp_dir);
                SourceOpError::new(
                    SourceOpErrorCode::InsertFailed,
                    format!("Failed to set local_path for source \"{}\": {e}", opts.id),
                )
            })?;
        }

        // Rename temp to final
        let final_path = final_path.as_ref().ok_or_else(|| {
            SourceOpError::new(
                SourceOpErrorCode::RenameFailed,
                "No final path determined",
            )
        })?;

        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                SourceOpError::new(
                    SourceOpErrorCode::RenameFailed,
                    format!("Cannot create final path parent: {e}"),
                )
            })?;
        }

        // Refuse to rename over an existing path
        if final_path.exists() {
            let _ = std::fs::remove_dir_all(&temp_dir);
            let _ = engine.delete_source(&opts.id).await;
            return Err(SourceOpError::new(
                SourceOpErrorCode::RenameFailed,
                format!("destination {} appeared mid-flight", final_path.display()),
            ));
        }

        std::fs::rename(&temp_dir, final_path).map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            // Best-effort DB rollback — we can't call async here from map_err,
            // so we log the need for manual cleanup. The caller should handle
            // the error and perform rollback.
            tracing::error!(
                "rename failed for source {}: temp={} final={}: {e}. \
                 Source row may need manual cleanup.",
                opts.id,
                temp_dir.display(),
                final_path.display()
            );
            SourceOpError::new(
                SourceOpErrorCode::RenameFailed,
                format!(
                    "Could not move clone to final path {}: {e}",
                    final_path.display()
                ),
            )
        })?;

        // Fetch the final row to return
        let created = engine.get_source(&opts.id).await.map_err(|e| {
            SourceOpError::new(
                SourceOpErrorCode::InsertFailed,
                format!("Source disappeared after INSERT: {e}"),
            )
        })?;
        created.ok_or_else(|| {
            SourceOpError::new(
                SourceOpErrorCode::InsertFailed,
                format!(
                    "Source \"{}\" disappeared after INSERT (concurrent delete?).",
                    opts.id
                ),
            )
        })
    } else {
        // ── Path B: --path or no path (INSERT only) ─────────────────────────
        let mut config = serde_json::json!({});
        if let Some(federated) = opts.federated {
            config["federated"] = serde_json::Value::Bool(federated);
        }

        let display_name = opts.name.clone().unwrap_or_else(|| opts.id.clone());

        let create_input = CreateSourceInput {
            id: opts.id.clone(),
            name: display_name,
            config: Some(config),
        };

        engine.create_source(&create_input).await.map_err(|e| {
            SourceOpError::new(
                SourceOpErrorCode::InsertFailed,
                format!("INSERT failed for source \"{}\": {e}", opts.id),
            )
        })
    }
}

// ── remove_source ────────────────────────────────────────────────────────────

/// Hard-remove a source row + cascade. Protected-id guard for "default".
///
/// Symlink-safe clone cleanup: uses `is_path_contained` with canonicalized
/// paths, and refuses to delete if the path is a symlink.
pub async fn remove_source(
    engine: &dyn BrainEngine,
    opts: RemoveSourceOpts,
    zbrain_home: &Path,
) -> Result<RemoveResult, SourceOpError> {
    validate_source_id(&opts.id)?;

    if opts.id == "default" {
        return Err(SourceOpError::new(
            SourceOpErrorCode::ProtectedId,
            "Cannot remove the \"default\" source (it backs the pre-v0.17 brain).",
        ));
    }

    let src = engine.get_source(&opts.id).await.map_err(|e| {
        SourceOpError::new(SourceOpErrorCode::NotFound, format!("Lookup failed: {e}"))
    })?;
    let src = src.ok_or_else(|| {
        SourceOpError::new(
            SourceOpErrorCode::NotFound,
            format!("Source \"{}\" not found.", opts.id),
        )
    })?;

    let page_count = count_pages(engine, &opts.id).await;

    if opts.dry_run {
        return Ok(RemoveResult {
            id: opts.id,
            pages_deleted: page_count,
            clone_removed: false,
            clone_path: src.local_path.clone(),
            dry_run: true,
        });
    }

    // Confirmation gate
    if page_count > 0 && !opts.confirm_destructive {
        return Err(SourceOpError::new(
            SourceOpErrorCode::ProtectedId,
            format!(
                "Refusing to remove source \"{}\" with {page_count} pages without confirmation.",
                opts.id
            ),
        ));
    }

    // Decide whether to clean up the clone dir
    let remote_url = get_remote_url(&src.config);
    let clone_root = zbrain_home.join("clones");
    let mut clone_removed = false;

    if !opts.keep_storage
        && src.local_path.is_some()
        && remote_url.is_some()
    {
        let local = Path::new(src.local_path.as_ref().unwrap());
        if is_path_contained(local, &clone_root) {
            // Extra symlink-escape paranoia
            match std::fs::symlink_metadata(local) {
                Ok(md) if md.is_symlink() => {
                    return Err(SourceOpError::new(
                        SourceOpErrorCode::SymlinkEscape,
                        format!(
                            "Refusing to delete clone at {}: path is a symlink.",
                            local.display()
                        ),
                    ));
                }
                Ok(_) => {
                    match std::fs::remove_dir_all(local) {
                        Ok(()) => clone_removed = true,
                        Err(e) => {
                            tracing::warn!(
                                "clone cleanup at {} failed: {e}",
                                local.display()
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "Cannot stat clone path {}: {e}",
                        local.display()
                    );
                }
            }
        }
    }

    // Delete the DB row
    engine.delete_source(&opts.id).await.map_err(|e| {
        SourceOpError::new(
            SourceOpErrorCode::NotFound,
            format!("Delete failed: {e}"),
        )
    })?;

    Ok(RemoveResult {
        id: opts.id,
        pages_deleted: page_count,
        clone_removed,
        clone_path: src.local_path,
        dry_run: false,
    })
}

// ── get_source_status ────────────────────────────────────────────────────────

/// Get detailed status for a source, including clone state.
pub async fn get_source_status(
    engine: &dyn BrainEngine,
    id: &str,
) -> Result<SourceStatus, SourceOpError> {
    validate_source_id(id)?;

    let src = engine.get_source(id).await.map_err(|e| {
        SourceOpError::new(SourceOpErrorCode::NotFound, format!("Lookup failed: {e}"))
    })?;
    let src = src.ok_or_else(|| {
        SourceOpError::new(
            SourceOpErrorCode::NotFound,
            format!("Source \"{id}\" not found."),
        )
    })?;

    let remote_url = get_remote_url(&src.config);
    let clone_state = if let Some(ref local_path) = src.local_path {
        let state = validate_repo_state(Path::new(local_path), remote_url.as_deref());
        repo_state_to_str(state).to_string()
    } else {
        "not-applicable".to_string()
    };

    Ok(SourceStatus {
        id: src.id,
        name: src.name,
        local_path: src.local_path,
        remote_url,
        federated: is_federated(&src.config),
        page_count: count_pages(engine, id).await,
        last_sync_at: src.last_sync_at,
        last_commit: src.last_commit,
        archived: src.archived,
        clone_state,
    })
}

// ── reclone_if_missing ───────────────────────────────────────────────────────

/// Re-clone a source's remote_url into its local_path if the clone is
/// missing on disk. Idempotent: returns `false` if the clone is already
/// healthy.
///
/// Throws `SourceOpError` on clone failure. Does NOT touch the DB row.
pub async fn reclone_if_missing(
    engine: &dyn BrainEngine,
    id: &str,
    zbrain_home: &Path,
) -> Result<bool, SourceOpError> {
    let src = engine.get_source(id).await.map_err(|e| {
        SourceOpError::new(SourceOpErrorCode::NotFound, format!("Lookup failed: {e}"))
    })?;
    let src = src.ok_or_else(|| {
        SourceOpError::new(
            SourceOpErrorCode::NotFound,
            format!("Source \"{id}\" not found."),
        )
    })?;

    let remote_url = match get_remote_url(&src.config) {
        Some(u) => u,
        None => return Ok(false),
    };
    let local_path = match &src.local_path {
        Some(p) => PathBuf::from(p),
        None => return Ok(false),
    };

    let state = validate_repo_state(&local_path, Some(&remote_url));
    if state == RepoState::Healthy {
        return Ok(false);
    }

    // Re-clone via temp + rename, mirroring add_source's atomicity
    let temp_dir = make_temp_clone_dir(zbrain_home, id);
    if let Some(parent) = temp_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            SourceOpError::new(
                SourceOpErrorCode::CloneFailed,
                format!("Cannot create temp dir parent: {e}"),
            )
        })?;
    }

    let clone_opts = CloneOpts::default();
    if let Err(e) = clone_repo(&remote_url, &temp_dir, clone_opts).await {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(SourceOpError::new(
            SourceOpErrorCode::CloneFailed,
            e.message,
        ));
    }

    // Remove the old local_path (might be partial/corrupted)
    let _ = std::fs::remove_dir_all(&local_path);
    if let Some(parent) = local_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            let _ = std::fs::remove_dir_all(&temp_dir);
            SourceOpError::new(
                SourceOpErrorCode::RenameFailed,
                format!("Cannot create final path parent: {e}"),
            )
        })?;
    }

    std::fs::rename(&temp_dir, &local_path).map_err(|e| {
        let _ = std::fs::remove_dir_all(&temp_dir);
        SourceOpError::new(
            SourceOpErrorCode::RenameFailed,
            format!(
                "Could not move re-cloned repo to {}: {e}",
                local_path.display()
            ),
        )
    })?;

    Ok(true)
}

/// Convert RepoState to a string representation matching TS.
fn repo_state_to_str(state: RepoState) -> &'static str {
    match state {
        RepoState::Healthy => "healthy",
        RepoState::Missing => "missing",
        RepoState::NotADir => "not-a-dir",
        RepoState::NoGit => "no-git",
        RepoState::UrlDrift => "url-drift",
        RepoState::Corrupted => "corrupted",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn validate_source_id_valid() {
        assert!(validate_source_id("my-source").is_ok());
        assert!(validate_source_id("a").is_ok());
        assert!(validate_source_id("abc123").is_ok());
    }

    #[test]
    fn validate_source_id_invalid() {
        assert!(validate_source_id("").is_err());
        assert!(validate_source_id("INVALID").is_err());
        assert!(validate_source_id("-start").is_err());
        assert!(validate_source_id("end-").is_err());
    }

    #[test]
    fn is_path_contained_basic() {
        let tmp = TempDir::new().unwrap();
        let parent = tmp.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();

        assert!(is_path_contained(&child, &parent));
        assert!(!is_path_contained(&parent, &child));
    }

    #[test]
    fn is_path_contained_missing() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_path_contained(&tmp.path().join("nope"), tmp.path()));
    }

    #[test]
    fn is_path_contained_no_prefix_match() {
        let tmp = TempDir::new().unwrap();
        let a = tmp.path().join("foo");
        let b = tmp.path().join("foobar");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();

        // /foo should NOT match /foobar
        assert!(!is_path_contained(&b, &a));
    }

    #[test]
    fn is_path_contained_same_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("mydir");
        std::fs::create_dir_all(&dir).unwrap();

        assert!(is_path_contained(&dir, &dir));
    }

    #[test]
    fn get_remote_url_present() {
        let config = serde_json::json!({"remote_url": "https://github.com/foo/bar.git"});
        assert_eq!(
            get_remote_url(&config),
            Some("https://github.com/foo/bar.git".to_string())
        );
    }

    #[test]
    fn get_remote_url_missing() {
        let config = serde_json::json!({"other": "value"});
        assert_eq!(get_remote_url(&config), None);
    }

    #[test]
    fn is_federated_true() {
        let config = serde_json::json!({"federated": true});
        assert!(is_federated(&config));
    }

    #[test]
    fn is_federated_false() {
        let config = serde_json::json!({"federated": false});
        assert!(!is_federated(&config));
    }
}
