//! Sync anchor management.
//!
//! The sync anchor tracks the last-synced state of a source:
//! - `last_commit`: the git commit SHA that was last synced.
//! - `last_sync_at`: ISO 8601 timestamp of the last sync.
//! - `chunker_version`: version of the chunker used during last sync.
//!
//! These are stored on the `SourceRow` via `BrainEngine::update_source`.

use crate::engine::{BrainEngine, SourceRow, UpdateSourceInput};
use std::sync::Arc;

/// A snapshot of the sync anchor for a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncAnchor {
    pub last_commit: Option<String>,
    pub last_sync_at: Option<String>,
    pub chunker_version: Option<String>,
}

impl SyncAnchor {
    /// Create a new anchor with the given commit and optional chunker version.
    /// `last_sync_at` is set to the current time.
    pub fn now(last_commit: String, chunker_version: Option<String>) -> Self {
        SyncAnchor {
            last_commit: Some(last_commit),
            last_sync_at: Some(crate::time::current_utc_iso8601()),
            chunker_version,
        }
    }
}

/// Read the current sync anchor from the engine for a source.
pub async fn get_sync_anchor(
    engine: &dyn BrainEngine,
    source_id: &str,
) -> Result<SyncAnchor, crate::error::Error> {
    let source = engine.get_source(source_id).await?.ok_or_else(|| {
        crate::error::StructuredError::new(
            "SourceNotFound",
            "source_not_found",
            format!("source not found: {source_id}"),
        )
    })?;
    Ok(SyncAnchor {
        last_commit: source.last_commit,
        last_sync_at: source.last_sync_at,
        chunker_version: source.chunker_version,
    })
}

/// Write the sync anchor to the engine for a source.
pub async fn set_sync_anchor(
    engine: &dyn BrainEngine,
    source_id: &str,
    anchor: &SyncAnchor,
) -> Result<(), crate::error::Error> {
    engine
        .update_source(
            source_id,
            &UpdateSourceInput {
                name: None,
                config: None,
                local_path: None,
                last_commit: anchor.last_commit.clone(),
                last_sync_at: anchor.last_sync_at.clone(),
                chunker_version: anchor.chunker_version.clone(),
                contextual_retrieval_mode: None,
                trust_frontmatter_overrides: None,
            },
        )
        .await?;
    Ok(())
}

/// Check whether the current source commit matches the anchor.
/// Returns `true` if the source is already synced at this commit.
pub fn is_anchor_current(source: &SourceRow, current_commit: &str) -> bool {
    source
        .last_commit
        .as_deref()
        .is_some_and(|c| c == current_commit)
}

/// Check whether the chunker version has changed since the last sync.
/// Returns `true` if a full re-sync is needed due to chunker version mismatch.
pub fn is_chunker_stale(source: &SourceRow, current_chunker_version: &str) -> bool {
    source
        .chunker_version
        .as_deref()
        .is_none_or(|v| v != current_chunker_version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{CreateSourceInput, InMemoryEngine};

    async fn setup_engine() -> (Arc<dyn BrainEngine>, String) {
        let engine = Arc::new(InMemoryEngine::default());
        let source_id = "test-source";

        engine
            .create_source(&CreateSourceInput {
                id: source_id.to_string(),
                name: "Test Source".to_string(),
                config: None,
            })
            .await
            .unwrap();

        (engine, source_id.to_string())
    }

    #[tokio::test]
    async fn get_set_anchor_roundtrip() {
        let (engine, source_id) = setup_engine().await;

        let anchor = SyncAnchor {
            last_commit: Some("abc123".to_string()),
            last_sync_at: Some("2026-07-01T12:00:00Z".to_string()),
            chunker_version: Some("v2".to_string()),
        };

        set_sync_anchor(&*engine, &source_id, &anchor).await.unwrap();

        let read_back = get_sync_anchor(&*engine, &source_id).await.unwrap();
        assert_eq!(read_back, anchor);
    }

    #[tokio::test]
    async fn sync_anchor_now_sets_timestamp() {
        let (engine, source_id) = setup_engine().await;

        let anchor = SyncAnchor::now("def456".to_string(), Some("v3".to_string()));
        assert!(anchor.last_sync_at.is_some());
        assert_eq!(anchor.last_commit.as_deref(), Some("def456"));
        assert_eq!(anchor.chunker_version.as_deref(), Some("v3"));

        set_sync_anchor(&*engine, &source_id, &anchor).await.unwrap();

        let read_back = get_sync_anchor(&*engine, &source_id).await.unwrap();
        assert_eq!(read_back.last_commit.as_deref(), Some("def456"));
        assert_eq!(read_back.chunker_version.as_deref(), Some("v3"));
        assert!(read_back.last_sync_at.is_some());
    }

    #[tokio::test]
    async fn anchor_none_fields() {
        let (engine, source_id) = setup_engine().await;

        // All None anchor — should store correctly
        let anchor = SyncAnchor {
            last_commit: None,
            last_sync_at: None,
            chunker_version: None,
        };

        set_sync_anchor(&*engine, &source_id, &anchor).await.unwrap();

        let read_back = get_sync_anchor(&*engine, &source_id).await.unwrap();
        assert_eq!(read_back.last_commit, None);
        assert_eq!(read_back.last_sync_at, None);
        assert_eq!(read_back.chunker_version, None);
    }

    #[test]
    fn is_anchor_current_matches() {
        let source = SourceRow {
            id: "s1".into(),
            name: "test".into(),
            local_path: None,
            last_commit: Some("abc123".into()),
            last_sync_at: None,
            config: serde_json::json!({}),
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        };

        assert!(is_anchor_current(&source, "abc123"));
        assert!(!is_anchor_current(&source, "def456"));
    }

    #[test]
    fn is_anchor_current_none_never_matches() {
        let source = SourceRow {
            id: "s1".into(),
            name: "test".into(),
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: serde_json::json!({}),
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        };

        // No anchor → never "current"
        assert!(!is_anchor_current(&source, "abc123"));
    }

    #[test]
    fn is_chunker_stale_detects_mismatch() {
        let source = SourceRow {
            id: "s1".into(),
            name: "test".into(),
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: serde_json::json!({}),
            created_at: None,
            chunker_version: Some("v1".into()),
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        };

        assert!(is_chunker_stale(&source, "v2"));
        assert!(!is_chunker_stale(&source, "v1"));
    }

    #[test]
    fn is_chunker_stale_none_is_stale() {
        let source = SourceRow {
            id: "s1".into(),
            name: "test".into(),
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: serde_json::json!({}),
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        };

        // No chunker version → always stale
        assert!(is_chunker_stale(&source, "v1"));
    }
}
