//! Per-source autopilot fan-out (1-5-1).
//!
//! Mirrors `src/commands/autopilot-fanout.ts`. Pure functions
//! (`read_last_full_cycle_at`, `is_source_stale`, `select_sources_for_dispatch`,
//! `resolve_fanout_max`) are testable without an engine. `dispatch_per_source`
//! wires them into the MinionQueue.

use chrono::{DateTime, Utc};

use crate::engine::{BrainEngine, EngineKind, SourceRow};
use crate::minions::queue::MinionQueue;
use crate::minions::types::MinionJobInput;

/// Minimum minutes between full cycles for a source to be considered fresh.
const FULL_CYCLE_FLOOR_MIN: i64 = 60;

/// Options for [`dispatch_per_source`].
pub struct FanoutOpts {
    pub repo_path: String,
    /// Time slot identifier (e.g. ISO timestamp truncated to minute).
    /// Used in idempotency keys so repeated ticks within the same slot
    /// coalesce.
    pub slot: String,
    pub timeout_ms: i64,
    /// Cap on per-tick job submissions.
    pub fanout_max: usize,
    pub json_mode: bool,
}

/// Result of [`dispatch_per_source`].
#[derive(Debug, PartialEq, Eq)]
pub struct FanoutResult {
    pub dispatched: Vec<String>,
    pub skipped_fresh: Vec<String>,
    pub skipped_cap: Vec<String>,
    pub legacy_fallback: bool,
}

/// Read `last_full_cycle_at` from a source's config JSONB.
/// Returns `None` when missing or unparseable.
pub fn read_last_full_cycle_at(src: &SourceRow) -> Option<DateTime<Utc>> {
    let raw = src
        .config
        .get("last_full_cycle_at")
        .and_then(|v| v.as_str())?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// A source needs work when either:
///   1. It has never had a full cycle complete (`last_full_cycle_at` null), OR
///   2. The last full cycle is older than the freshness floor.
pub fn is_source_stale(src: &SourceRow, now: DateTime<Utc>, floor_min: i64) -> bool {
    match read_last_full_cycle_at(src) {
        None => true,
        Some(last) => {
            let age_min = (now - last).num_minutes();
            age_min >= floor_min
        }
    }
}

/// Selection result from [`select_sources_for_dispatch`].
pub struct DispatchSelection<'a> {
    pub dispatch: Vec<&'a SourceRow>,
    pub skipped_fresh: Vec<&'a SourceRow>,
    pub skipped_cap: Vec<&'a SourceRow>,
}

/// Decide which sources to dispatch this tick. Pure function.
///
/// - Filters to stale sources (per [`is_source_stale`]).
/// - Sorts oldest-first (NULL `last_full_cycle_at` goes first; then oldest
///   by ascending date). Deterministic for tests.
/// - Caps at `fanout_max`. Sources past the cap retry next tick.
pub fn select_sources_for_dispatch<'a>(
    sources: &'a [SourceRow],
    fanout_max: usize,
    now: DateTime<Utc>,
    floor_min: i64,
) -> DispatchSelection<'a> {
    let mut stale: Vec<&SourceRow> = Vec::new();
    let mut fresh: Vec<&SourceRow> = Vec::new();
    for s in sources {
        if is_source_stale(s, now, floor_min) {
            stale.push(s);
        } else {
            fresh.push(s);
        }
    }
    // Oldest-first: NULL last_full_cycle_at sorts before any timestamp.
    stale.sort_by(|a, b| {
        let la = read_last_full_cycle_at(a).map(|d| d.timestamp()).unwrap_or(i64::MIN);
        let lb = read_last_full_cycle_at(b).map(|d| d.timestamp()).unwrap_or(i64::MIN);
        la.cmp(&lb).then_with(|| a.id.cmp(&b.id))
    });
    let dispatch: Vec<&SourceRow> = stale.iter().take(fanout_max).copied().collect();
    let skipped_cap: Vec<&SourceRow> = stale.iter().skip(fanout_max).copied().collect();
    DispatchSelection {
        dispatch,
        skipped_fresh: fresh,
        skipped_cap,
    }
}

/// Resolve `fanout_max` honoring engine kind + operator override.
///
/// Defaults: Postgres = 4, Libsql/InMemory = 1.
/// Override: `autopilot.fanout_max_per_tick` config value (must be >= 1).
pub fn resolve_fanout_max(kind: EngineKind, override_val: Option<&str>) -> usize {
    if let Some(raw) = override_val {
        if let Ok(n) = raw.parse::<usize>() {
            if n >= 1 {
                return n;
            }
        }
        // Invalid override falls through to default — never silently below 1.
    }
    match kind {
        EngineKind::Postgres => 4,
        EngineKind::Libsql | EngineKind::InMemory => 1,
    }
}

/// Per-tick autopilot fan-out.
///
/// Fallback path: if `list_sources` returns 0 rows with `local_path`,
/// submit ONE legacy autopilot-cycle with no source_id.
pub async fn dispatch_per_source(
    engine: &dyn BrainEngine,
    queue: &MinionQueue<'_>,
    opts: &FanoutOpts,
) -> crate::Result<FanoutResult> {
    // List sources, filter to local_path only (P1-4: pure-DB sources
    // don't get dispatched).
    let all_sources = engine.list_sources(false).await.unwrap_or_default();
    let sources: Vec<&SourceRow> = all_sources
        .iter()
        .filter(|s| s.local_path.is_some())
        .collect();

    if sources.is_empty() {
        // Legacy path — single-source brains (default source) and
        // pre-v0.18 brains without sources table.
        let input = MinionJobInput {
            name: "autopilot-cycle".into(),
            data: Some(serde_json::json!({ "repoPath": opts.repo_path })),
            queue: Some("default".into()),
            idempotency_key: Some(format!("autopilot-cycle:{}", opts.slot)),
            max_attempts: Some(2),
            timeout_ms: Some(opts.timeout_ms),
            ..Default::default()
        };
        queue.add(&input).await?;
        return Ok(FanoutResult {
            dispatched: vec![],
            skipped_fresh: vec![],
            skipped_cap: vec![],
            legacy_fallback: true,
        });
    }

    let now = Utc::now();
    let selection = select_sources_for_dispatch(
        &all_sources,
        opts.fanout_max,
        now,
        FULL_CYCLE_FLOOR_MIN,
    );

    let mut dispatched = Vec::new();
    for src in &selection.dispatch {
        let remote_url = src
            .config
            .get("remote_url")
            .and_then(|v| v.as_str());
        let input = MinionJobInput {
            name: "autopilot-cycle".into(),
            data: Some(serde_json::json!({
                "repoPath": opts.repo_path,
                "source_id": src.id,
                "pull": remote_url.is_some(),
            })),
            queue: Some("default".into()),
            idempotency_key: Some(format!("autopilot-cycle:{}:{}", src.id, opts.slot)),
            max_attempts: Some(2),
            timeout_ms: Some(opts.timeout_ms),
            ..Default::default()
        };
        match queue.add(&input).await {
            Ok(_) => dispatched.push(src.id.clone()),
            Err(_) => {
                // Per-source submit failure does NOT abort the tick.
                // This source retries next tick.
            }
        }
    }

    Ok(FanoutResult {
        dispatched,
        skipped_fresh: selection.skipped_fresh.iter().map(|s| s.id.clone()).collect(),
        skipped_cap: selection.skipped_cap.iter().map(|s| s.id.clone()).collect(),
        legacy_fallback: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, EngineKind, SourceRow};
    use crate::minions::queue::MinionQueue;
    use chrono::TimeZone;

    // ── read_last_full_cycle_at ──────────────────────────────────────────

    fn make_source(id: &str, config: serde_json::Value) -> SourceRow {
        SourceRow {
            id: id.into(),
            name: format!("Source {}", id),
            local_path: Some("/tmp/repo".into()),
            last_commit: None,
            last_sync_at: None,
            config,
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        }
    }

    #[test]
    fn read_last_full_cycle_at_returns_none_for_missing() {
        let src = make_source("a", serde_json::json!({}));
        assert!(read_last_full_cycle_at(&src).is_none());
    }

    #[test]
    fn read_last_full_cycle_at_parses_valid_iso() {
        let src = make_source(
            "a",
            serde_json::json!({ "last_full_cycle_at": "2026-07-14T10:00:00Z" }),
        );
        let dt = read_last_full_cycle_at(&src).unwrap();
        assert_eq!(dt.to_rfc3339(), "2026-07-14T10:00:00+00:00");
    }

    #[test]
    fn read_last_full_cycle_at_returns_none_for_invalid() {
        let src = make_source(
            "a",
            serde_json::json!({ "last_full_cycle_at": "not-a-date" }),
        );
        assert!(read_last_full_cycle_at(&src).is_none());
    }

    // ── is_source_stale ──────────────────────────────────────────────────

    #[test]
    fn is_source_stale_true_when_no_last_full_cycle() {
        let src = make_source("a", serde_json::json!({}));
        assert!(is_source_stale(&src, Utc::now(), FULL_CYCLE_FLOOR_MIN));
    }

    #[test]
    fn is_source_stale_false_when_within_floor() {
        let now = Utc::now();
        let recent = now - chrono::Duration::minutes(30);
        let src = make_source(
            "a",
            serde_json::json!({ "last_full_cycle_at": recent.to_rfc3339() }),
        );
        assert!(!is_source_stale(&src, now, FULL_CYCLE_FLOOR_MIN));
    }

    #[test]
    fn is_source_stale_true_when_older_than_floor() {
        let now = Utc::now();
        let old = now - chrono::Duration::minutes(120);
        let src = make_source(
            "a",
            serde_json::json!({ "last_full_cycle_at": old.to_rfc3339() }),
        );
        assert!(is_source_stale(&src, now, FULL_CYCLE_FLOOR_MIN));
    }

    // ── select_sources_for_dispatch ──────────────────────────────────────

    #[test]
    fn select_dispatches_stale_sources_up_to_cap() {
        let now = Utc::now();
        let old = now - chrono::Duration::minutes(120);
        let src = make_source(
            "a",
            serde_json::json!({ "last_full_cycle_at": old.to_rfc3339() }),
        );
        let sources = vec![src];
        let sel = select_sources_for_dispatch(&sources, 4, now, FULL_CYCLE_FLOOR_MIN);
        assert_eq!(sel.dispatch.len(), 1);
        assert_eq!(sel.skipped_fresh.len(), 0);
        assert_eq!(sel.skipped_cap.len(), 0);
    }

    #[test]
    fn select_skips_fresh_sources() {
        let now = Utc::now();
        let recent = now - chrono::Duration::minutes(10);
        let src = make_source(
            "fresh",
            serde_json::json!({ "last_full_cycle_at": recent.to_rfc3339() }),
        );
        let sources = vec![src];
        let sel = select_sources_for_dispatch(&sources, 4, now, FULL_CYCLE_FLOOR_MIN);
        assert_eq!(sel.dispatch.len(), 0);
        assert_eq!(sel.skipped_fresh.len(), 1);
        assert_eq!(sel.skipped_fresh[0].id, "fresh");
    }

    #[test]
    fn select_caps_at_fanout_max() {
        let now = Utc::now();
        let old = now - chrono::Duration::minutes(120);
        let sources: Vec<SourceRow> = (0..5)
            .map(|i| {
                make_source(
                    &format!("s{}", i),
                    serde_json::json!({ "last_full_cycle_at": old.to_rfc3339() }),
                )
            })
            .collect();
        let sel = select_sources_for_dispatch(&sources, 2, now, FULL_CYCLE_FLOOR_MIN);
        assert_eq!(sel.dispatch.len(), 2);
        assert_eq!(sel.skipped_cap.len(), 3);
    }

    #[test]
    fn select_sorts_null_first_then_oldest_first() {
        let now = Utc::now();
        let old1 = now - chrono::Duration::minutes(180);
        let old2 = now - chrono::Duration::minutes(120);
        let sources = vec![
            make_source("c", serde_json::json!({ "last_full_cycle_at": old2.to_rfc3339() })),
            make_source("a", serde_json::json!({})), // null → first
            make_source("b", serde_json::json!({ "last_full_cycle_at": old1.to_rfc3339() })),
        ];
        let sel = select_sources_for_dispatch(&sources, 10, now, FULL_CYCLE_FLOOR_MIN);
        let ids: Vec<&str> = sel.dispatch.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    // ── resolve_fanout_max ───────────────────────────────────────────────

    #[test]
    fn resolve_fanout_max_defaults_by_engine_kind() {
        assert_eq!(resolve_fanout_max(EngineKind::Postgres, None), 4);
        assert_eq!(resolve_fanout_max(EngineKind::Libsql, None), 1);
        assert_eq!(resolve_fanout_max(EngineKind::InMemory, None), 1);
    }

    #[test]
    fn resolve_fanout_max_honors_valid_override() {
        assert_eq!(resolve_fanout_max(EngineKind::Postgres, Some("8")), 8);
        assert_eq!(resolve_fanout_max(EngineKind::Libsql, Some("3")), 3);
    }

    #[test]
    fn resolve_fanout_max_ignores_invalid_override() {
        assert_eq!(resolve_fanout_max(EngineKind::Postgres, Some("0")), 4);
        assert_eq!(resolve_fanout_max(EngineKind::Postgres, Some("abc")), 4);
        assert_eq!(resolve_fanout_max(EngineKind::Postgres, Some("")), 4);
    }

    // ── dispatch_per_source (legacy fallback) ────────────────────────────

    #[tokio::test]
    async fn dispatch_legacy_fallback_when_no_sources() {
        // InMemory engine with no sources → legacy fallback
        let engine = crate::engine::InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        let queue = MinionQueue::new(&engine);
        let opts = FanoutOpts {
            repo_path: "/tmp/repo".into(),
            slot: "2026-07-14T13:00".into(),
            timeout_ms: 30_000,
            fanout_max: 4,
            json_mode: false,
        };
        let result = dispatch_per_source(&engine, &queue, &opts).await.unwrap();
        assert!(result.legacy_fallback);
        assert!(result.dispatched.is_empty());
    }

    #[tokio::test]
    async fn dispatch_per_source_with_stale_source() {
        let engine = crate::engine::InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        // Add a source with local_path but no last_full_cycle_at (stale)
        engine
            .create_source(&crate::engine::CreateSourceInput {
                id: "test-src".into(),
                name: "Test Source".into(),
                config: Some(serde_json::json!({})),
            })
            .await
            .unwrap();
        // Set local_path (create_source doesn't set it, need update)
        engine
            .update_source("test-src", &crate::engine::UpdateSourceInput {
                name: None,
                config: None,
                local_path: Some("/tmp/repo".into()),
                last_commit: None,
                last_sync_at: None,
                chunker_version: None,
                contextual_retrieval_mode: None,
                trust_frontmatter_overrides: None,
            })
            .await
            .unwrap();

        let queue = MinionQueue::new(&engine);
        let opts = FanoutOpts {
            repo_path: "/tmp/repo".into(),
            slot: "2026-07-14T13:00".into(),
            timeout_ms: 30_000,
            fanout_max: 4,
            json_mode: false,
        };
        let result = dispatch_per_source(&engine, &queue, &opts).await.unwrap();
        assert!(!result.legacy_fallback);
        assert_eq!(result.dispatched, vec!["test-src"]);
        assert!(result.skipped_fresh.is_empty());
        assert!(result.skipped_cap.is_empty());
    }

    #[tokio::test]
    async fn dispatch_skips_fresh_source() {
        let engine = crate::engine::InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        let now = Utc::now();
        let recent = now - chrono::Duration::minutes(10);
        engine
            .create_source(&crate::engine::CreateSourceInput {
                id: "fresh-src".into(),
                name: "Fresh".into(),
                config: Some(serde_json::json!({
                    "last_full_cycle_at": recent.to_rfc3339(),
                })),
            })
            .await
            .unwrap();
        engine
            .update_source("fresh-src", &crate::engine::UpdateSourceInput {
                name: None,
                config: None,
                local_path: Some("/tmp/repo".into()),
                last_commit: None,
                last_sync_at: None,
                chunker_version: None,
                contextual_retrieval_mode: None,
                trust_frontmatter_overrides: None,
            })
            .await
            .unwrap();

        let queue = MinionQueue::new(&engine);
        let opts = FanoutOpts {
            repo_path: "/tmp/repo".into(),
            slot: "2026-07-14T13:00".into(),
            timeout_ms: 30_000,
            fanout_max: 4,
            json_mode: false,
        };
        let result = dispatch_per_source(&engine, &queue, &opts).await.unwrap();
        assert!(!result.legacy_fallback);
        assert!(result.dispatched.is_empty());
        assert_eq!(result.skipped_fresh, vec!["fresh-src"]);
    }
}
