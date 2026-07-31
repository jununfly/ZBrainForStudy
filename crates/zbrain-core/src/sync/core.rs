//! Sync core — main sync loop orchestration.
//!
//! Combines the low-level modules (anchor, walker, manifest, import,
//! failures, concurrency) into the two main sync entry points:
//!
//! - `perform_full_sync`: walk the entire source directory and import every
//!   syncable file. Used for initial sync or after chunker version changes.
//!
//! - `perform_sync`: incremental sync — compute the git diff since the last
//!   anchor commit, then import only changed/added files and delete removed
//!   files from the knowledge base.

use crate::engine::BrainEngine;
use crate::progress::ProgressReporter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::anchor::{
    get_sync_anchor, is_anchor_current, is_chunker_stale, set_sync_anchor, SyncAnchor,
};
use super::concurrency::{detect_concurrency, SyncConcurrency};
use super::failures::{record_sync_failures, SyncFailure};
use super::import::{import_one_path, ImportOnePathOpts};
use super::manifest::{build_manifest, SyncManifest};
use super::walker::{walk_source, WalkOpts};

/// Options for a full sync operation.
#[derive(Debug, Clone)]
pub struct FullSyncOpts {
    /// Source identifier.
    pub source_id: String,
    /// Absolute path to the cloned repo root.
    pub repo_path: PathBuf,
    /// Current git HEAD commit SHA.
    pub current_commit: String,
    /// Chunker version to stamp on pages.
    pub chunker_version: Option<i32>,
    /// Directory for recording sync failures.
    pub failures_dir: PathBuf,
    /// Maximum file size in bytes (None = no limit).
    pub max_file_size: Option<u64>,
}

/// Result of a sync operation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Number of files imported.
    pub imported: usize,
    /// Number of files deleted from the KB.
    pub deleted: usize,
    /// Number of failures recorded.
    pub failures: usize,
    /// Whether a full sync was performed.
    pub full_sync: bool,
}

/// Determine whether a full sync is needed.
///
/// Returns `true` if:
/// - The source has no anchor (never synced before).
/// - The chunker version has changed since the last sync.
pub async fn needs_full_sync(
    engine: &dyn BrainEngine,
    source_id: &str,
    current_chunker_version: &str,
) -> Result<bool, crate::error::Error> {
    let source = engine.get_source(source_id).await?.ok_or_else(|| {
        crate::error::StructuredError::new(
            "SourceNotFound",
            "source_not_found",
            format!("source not found: {source_id}"),
        )
    })?;

    if source.last_commit.is_none() {
        return Ok(true);
    }

    if is_chunker_stale(&source, current_chunker_version) {
        return Ok(true);
    }

    Ok(false)
}

/// Perform a full sync: walk all files and import them.
///
/// This is a simplified synchronous (single-threaded) full sync that
/// walks the entire repo and imports every syncable markdown file.
/// The concurrency strategy is determined by the engine type.
///
/// If `progress` is `Some`, emits human/JSON progress events via the
/// reporter (`start` with total count, `tick` per path, `finish`).
pub async fn perform_full_sync(
    engine: &dyn BrainEngine,
    opts: &FullSyncOpts,
    mut progress: Option<&mut ProgressReporter>,
) -> Result<SyncResult, crate::error::Error> {
    // Walk the repo
    let walk_opts = WalkOpts {
        root: opts.repo_path.clone(),
        follow_links: false,
        max_file_size: opts.max_file_size,
    };

    let entries = walk_source(&walk_opts, default_is_syncable).map_err(|e| {
        crate::error::StructuredError::new(
            "SyncWalkError",
            "sync_walk_error",
            format!("failed to walk source directory: {e}"),
        )
    })?;

    let mut imported = 0usize;
    let mut failures = Vec::new();

    if let Some(ref mut p) = progress {
        p.start("sync.import", Some(entries.len()));
    }

    // Import each file (serial for now; concurrency will be added later)
    for entry in &entries {
        let import_opts = ImportOnePathOpts {
            source_id: opts.source_id.clone(),
            abs_path: entry.abs_path.clone(),
            rel_path: entry.rel_path.clone(),
            chunker_version: opts.chunker_version,
            path_prefixes: None,
        };

        match import_one_path(engine, &import_opts).await {
            Ok(_) => imported += 1,
            Err(e) => {
                failures.push(SyncFailure {
                    path: entry.rel_path.to_string_lossy().to_string(),
                    error: e.to_string(),
                    recorded_at: crate::time::current_utc_iso8601(),
                    acknowledged: false,
                });
            }
        }

        if let Some(ref mut p) = progress {
            p.tick(1, None);
        }
    }

    if let Some(ref mut p) = progress {
        p.finish(None);
    }

    // Record failures
    if !failures.is_empty() {
        if let Err(e) = record_sync_failures(&opts.source_id, &failures, &opts.failures_dir).await {
            tracing::warn!("failed to record sync failures: {e}");
        }
    }

    // Update the sync anchor
    let anchor = SyncAnchor::now(
        opts.current_commit.clone(),
        opts.chunker_version.map(|v| v.to_string()),
    );
    set_sync_anchor(engine, &opts.source_id, &anchor).await?;

    Ok(SyncResult {
        imported,
        deleted: 0,
        failures: failures.len(),
        full_sync: true,
    })
}

/// Options for an incremental sync operation.
#[derive(Debug, Clone)]
pub struct IncrementalSyncOpts {
    /// Source identifier.
    pub source_id: String,
    /// Absolute path to the cloned repo root.
    pub repo_path: PathBuf,
    /// Current git HEAD commit SHA.
    pub current_commit: String,
    /// Previous commit SHA (the anchor).
    pub previous_commit: Option<String>,
    /// Chunker version to stamp on pages.
    pub chunker_version: Option<i32>,
    /// Directory for recording sync failures.
    pub failures_dir: PathBuf,
    /// Maximum file size in bytes.
    pub max_file_size: Option<u64>,
}

/// Perform an incremental sync: git diff → import changed files → delete removed.
///
/// If `previous_commit` is `None`, this falls back to a full sync.
pub async fn perform_sync(
    engine: &dyn BrainEngine,
    opts: &IncrementalSyncOpts,
    mut progress: Option<&mut ProgressReporter>,
) -> Result<SyncResult, crate::error::Error> {
    let previous_commit = match &opts.previous_commit {
        Some(c) => c.clone(),
        None => {
            // No previous commit → fall back to full sync
            return perform_full_sync(
                engine,
                &FullSyncOpts {
                    source_id: opts.source_id.clone(),
                    repo_path: opts.repo_path.clone(),
                    current_commit: opts.current_commit.clone(),
                    chunker_version: opts.chunker_version,
                    failures_dir: opts.failures_dir.clone(),
                    max_file_size: opts.max_file_size,
                },
                progress,
            )
            .await;
        }
    };

    // Run git diff to get changed files
    let diff_output = run_git_diff(&opts.repo_path, &previous_commit, &opts.current_commit)
        .map_err(|e| {
            crate::error::StructuredError::new(
                "SyncGitError",
                "sync_git_error",
                format!("failed to run git diff: {e}"),
            )
        })?;

    // Build manifest
    let manifest = build_manifest(&diff_output, default_is_syncable);

    let mut imported = 0usize;
    let mut failures = Vec::new();

    if let Some(ref mut p) = progress {
        p.start("sync.import", Some(manifest.to_sync.len()));
    }

    // Import changed files
    for file in &manifest.to_sync {
        let abs_path = opts.repo_path.join(&file.rel_path);
        let import_opts = ImportOnePathOpts {
            source_id: opts.source_id.clone(),
            abs_path,
            rel_path: file.rel_path.clone(),
            chunker_version: opts.chunker_version,
            path_prefixes: None,
        };

        match import_one_path(engine, &import_opts).await {
            Ok(_) => imported += 1,
            Err(e) => {
                failures.push(SyncFailure {
                    path: file.rel_path.to_string_lossy().to_string(),
                    error: e.to_string(),
                    recorded_at: crate::time::current_utc_iso8601(),
                    acknowledged: false,
                });
            }
        }

        if let Some(ref mut p) = progress {
            p.tick(1, None);
        }
    }

    if let Some(ref mut p) = progress {
        p.finish(None);
    }

    // Delete removed files from KB
    let mut deleted = 0usize;

    if let Some(ref mut p) = progress {
        p.start("sync.delete", Some(manifest.to_delete.len()));
    }

    for file in &manifest.to_delete {
        let rel_path_str = file.rel_path.to_string_lossy();
        // Infer the slug from the path (same logic as import)
        let slug = crate::markdown::infer_slug(&serde_json::Value::Null, &rel_path_str);
        // Soft-delete the page
        match engine.delete_page(&slug, Some(&opts.source_id)).await {
            Ok(()) => deleted += 1,
            Err(_e) => {
                // Page might not exist — that's OK
                tracing::debug!("delete_page failed for {rel_path_str}: {_e}");
            }
        }

        if let Some(ref mut p) = progress {
            p.tick(1, None);
        }
    }

    if let Some(ref mut p) = progress {
        p.finish(None);
    }

    // Record failures
    if !failures.is_empty() {
        if let Err(e) = record_sync_failures(&opts.source_id, &failures, &opts.failures_dir).await {
            tracing::warn!("failed to record sync failures: {e}");
        }
    }

    // Update the sync anchor
    let anchor = SyncAnchor::now(
        opts.current_commit.clone(),
        opts.chunker_version.map(|v| v.to_string()),
    );
    set_sync_anchor(engine, &opts.source_id, &anchor).await?;

    Ok(SyncResult {
        imported,
        deleted,
        failures: failures.len(),
        full_sync: false,
    })
}

/// Run `git diff --name-status <from> <to>` in the given repo directory.
fn run_git_diff(repo_path: &Path, from: &str, to: &str) -> Result<String, std::io::Error> {
    let output = std::process::Command::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "diff",
            "--name-status",
            from,
            to,
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("git diff failed: {stderr}"),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Default syncability predicate: only sync markdown files.
pub fn default_is_syncable(path: &Path) -> bool {
    crate::file_classify::is_markdown_path(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CreateSourceInput, InMemoryEngine, UpdateSourceInput};

    async fn setup_engine_with_source() -> (Arc<dyn BrainEngine>, String, tempfile::TempDir) {
        let engine = Arc::new(InMemoryEngine::default()) as Arc<dyn BrainEngine>;
        let source_id = "test-source";
        let dir = tempfile::TempDir::new().unwrap();

        engine
            .create_source(&CreateSourceInput {
                id: source_id.to_string(),
                name: "Test".to_string(),
                config: None,
            })
            .await
            .unwrap();

        (engine, source_id.to_string(), dir)
    }

    fn write_md(dir: &Path, rel_path: &str, content: &str) {
        let full = dir.join(rel_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&full, content).unwrap();
    }

    #[tokio::test]
    async fn needs_full_sync_when_no_anchor() {
        let (engine, source_id, _dir) = setup_engine_with_source().await;
        assert!(needs_full_sync(&*engine, &source_id, "v1").await.unwrap());
    }

    #[tokio::test]
    async fn needs_full_sync_when_chunker_stale() {
        let (engine, source_id, _dir) = setup_engine_with_source().await;

        // Set an anchor with chunker v1
        engine
            .update_source(
                &source_id,
                &UpdateSourceInput {
                    name: None,
                    config: None,
                    local_path: None,
                    last_commit: Some("abc123".to_string()),
                    last_sync_at: Some("2026-01-01T00:00:00Z".to_string()),
                    chunker_version: Some("v1".to_string()),
                    contextual_retrieval_mode: None,
                    trust_frontmatter_overrides: None,
                },
            )
            .await
            .unwrap();

        // v2 is different → needs full sync
        assert!(needs_full_sync(&*engine, &source_id, "v2").await.unwrap());
        // v1 is same → no full sync needed
        assert!(!needs_full_sync(&*engine, &source_id, "v1").await.unwrap());
    }

    #[tokio::test]
    async fn full_sync_imports_all_markdown_files() {
        let (engine, source_id, dir) = setup_engine_with_source().await;

        write_md(dir.path(), "readme.md", "# Readme\n\nHello.\n");
        write_md(dir.path(), "docs/guide.md", "# Guide\n\nGuide content.\n");
        write_md(dir.path(), "image.png", "NOT_MARKDOWN");
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();

        let failures_dir = dir.path().join("failures");

        let opts = FullSyncOpts {
            source_id: source_id.clone(),
            repo_path: dir.path().to_path_buf(),
            current_commit: "abc123".to_string(),
            chunker_version: Some(1),
            failures_dir,
            max_file_size: None,
        };

        let result = perform_full_sync(&*engine, &opts, None).await.unwrap();

        assert_eq!(result.imported, 2); // readme.md + docs/guide.md
        assert_eq!(result.deleted, 0);
        assert!(result.full_sync);

        // Verify anchor was set
        let anchor = get_sync_anchor(&*engine, &source_id).await.unwrap();
        assert_eq!(anchor.last_commit.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn full_sync_empty_directory() {
        let (engine, source_id, dir) = setup_engine_with_source().await;
        let failures_dir = dir.path().join("failures");

        let opts = FullSyncOpts {
            source_id,
            repo_path: dir.path().to_path_buf(),
            current_commit: "abc123".to_string(),
            chunker_version: Some(1),
            failures_dir,
            max_file_size: None,
        };

        let result = perform_full_sync(&*engine, &opts, None).await.unwrap();
        assert_eq!(result.imported, 0);
    }
}
