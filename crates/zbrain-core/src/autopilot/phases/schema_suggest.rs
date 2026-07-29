//! Dream-cycle schema-suggest phase (v0.39 T12).
//!
//! Port of `src/core/cycle/schema-suggest.ts`.
//!
//! Thin wrapper around the `schema_pack::discovery::run_suggest` library
//! (D4 from plan-eng-review: single library, multiple thin callers). Runs
//! AFTER `sync` — schema-suggest only needs sync to have completed so
//! source_path is fresh; it doesn't depend on extract/extract-facts/
//! resolve-symbol-edges/patterns.
//!
//! Writes nothing to the user's brain. Suggestion outcomes are logged to
//! `~/.zbrain/audit/schema-events-YYYY-Www.jsonl` (T15 audit, see
//! `crate::schema_events`). Reviewed via `zbrain schema review-candidates`.
//!
//! No LLM: `run_suggest` is hermetic-by-default (heuristic fallback), so
//! unlike extract-atoms/propose-takes this phase runs without a chat
//! provider.
//!
//! Error contract (TS parity):
//! - dry-run: errors PROPAGATE to the caller (TS calls `runSuggest` outside
//!   the try/catch on the dry-run path; the cycle turns them into `fail`).
//! - normal run: errors are swallowed into `skipped` + reason, and an
//!   `error` schema event is logged (best-effort telemetry, never aborts
//!   the cycle).

use serde::Serialize;

use crate::engine::BrainEngine;
use crate::schema_events::{log_schema_event, SchemaEventInput, SchemaEventOutcome};
use crate::schema_pack::discovery::{run_suggest, DetectOpts, DetectResult, Suggestion};

/// Options for [`run_schema_suggest_phase`].
#[derive(Debug, Clone, Default)]
pub struct SchemaSuggestPhaseOpts {
    /// Source to suggest against. Defaults to `default`.
    pub source_id: Option<String>,
    /// Dry-run: still calls `run_suggest` but logs nothing (no audit append).
    pub dry_run: bool,
}

/// Result of [`run_schema_suggest_phase`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SchemaSuggestPhaseResult {
    pub suggestions_emitted: usize,
    pub source_id: String,
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Run the schema-suggest phase. See module docs for the error contract.
pub async fn run_schema_suggest_phase(
    engine: &dyn BrainEngine,
    opts: &SchemaSuggestPhaseOpts,
) -> crate::Result<SchemaSuggestPhaseResult> {
    let source_id = opts.source_id.clone().unwrap_or_else(|| "default".to_string());
    // Hermetic heuristic path — no LLM refiner injected (TS parity: the
    // gateway path also falls back to heuristics in v0.39).
    let no_llm = None::<fn(&DetectResult) -> Vec<Suggestion>>;

    // Dry-run still calls run_suggest but logs only — no audit append.
    if opts.dry_run {
        let result = run_suggest(engine, &source_id, DetectOpts::default(), no_llm).await?;
        return Ok(SchemaSuggestPhaseResult {
            suggestions_emitted: result.suggestions.len(),
            source_id,
            skipped: false,
            reason: Some("dry-run".to_string()),
        });
    }

    match run_suggest(engine, &source_id, DetectOpts::default(), no_llm).await {
        Ok(result) => {
            log_schema_event(&SchemaEventInput {
                verb: "cycle:schema-suggest".to_string(),
                outcome: SchemaEventOutcome::Success,
                flags: Some(vec![
                    format!("source={source_id}"),
                    format!("count={}", result.suggestions.len()),
                ]),
            });
            Ok(SchemaSuggestPhaseResult {
                suggestions_emitted: result.suggestions.len(),
                source_id,
                skipped: false,
                reason: None,
            })
        }
        Err(e) => {
            let msg = e.to_string();
            let short: String = msg.chars().take(80).collect();
            log_schema_event(&SchemaEventInput {
                verb: "cycle:schema-suggest".to_string(),
                outcome: SchemaEventOutcome::Error,
                flags: Some(vec![format!("source={source_id}"), format!("err={short}")]),
            });
            Ok(SchemaSuggestPhaseResult {
                suggestions_emitted: 0,
                source_id,
                skipped: true,
                reason: Some(msg),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{InMemoryEngine, PageInput};
    use crate::schema_events::read_recent_schema_events;

    async fn seeded_engine() -> InMemoryEngine {
        let engine = InMemoryEngine::default();
        // 5 untyped pages under people/ → one prefix cluster at default
        // min_pages_per_prefix=5 → one heuristic add_type suggestion.
        for slug in ["people/alice", "people/bob", "people/carol", "people/dave", "people/erin"] {
            let input = PageInput {
                title: slug.to_string(),
                compiled_truth: format!("# {slug}"),
                ..Default::default()
            };
            engine.put_page(slug, Some("src1"), &input).await.unwrap();
        }
        engine
    }

    #[tokio::test]
    async fn emits_heuristic_suggestion_and_logs_success_event() {
        let _home = crate::paths::ScopedTestHome::new();
        let engine = seeded_engine().await;

        let r = run_schema_suggest_phase(
            &engine,
            &SchemaSuggestPhaseOpts {
                source_id: Some("src1".to_string()),
                dry_run: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(r.suggestions_emitted, 1);
        assert_eq!(r.source_id, "src1");
        assert!(!r.skipped);
        assert_eq!(r.reason, None);

        let events = read_recent_schema_events(1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verb, "cycle:schema-suggest");
        assert_eq!(events[0].outcome, SchemaEventOutcome::Success);
        let flags = events[0].flags.as_ref().unwrap();
        assert!(flags.contains(&"source=src1".to_string()));
        assert!(flags.contains(&"count=1".to_string()));
    }

    #[tokio::test]
    async fn dry_run_reports_without_audit_append() {
        let _home = crate::paths::ScopedTestHome::new();
        let engine = seeded_engine().await;

        let r = run_schema_suggest_phase(
            &engine,
            &SchemaSuggestPhaseOpts {
                source_id: Some("src1".to_string()),
                dry_run: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(r.suggestions_emitted, 1);
        assert!(!r.skipped);
        assert_eq!(r.reason.as_deref(), Some("dry-run"));
        // Dry-run: no schema event written.
        assert!(read_recent_schema_events(1).is_empty());
    }

    #[tokio::test]
    async fn empty_brain_emits_zero_suggestions_ok() {
        let _home = crate::paths::ScopedTestHome::new();
        let engine = InMemoryEngine::default();

        let r = run_schema_suggest_phase(&engine, &SchemaSuggestPhaseOpts::default())
            .await
            .unwrap();

        assert_eq!(r.suggestions_emitted, 0);
        assert_eq!(r.source_id, "default");
        assert!(!r.skipped);

        let events = read_recent_schema_events(1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].outcome, SchemaEventOutcome::Success);
        assert!(events[0].flags.as_ref().unwrap().contains(&"count=0".to_string()));
    }

    #[tokio::test]
    async fn source_id_defaults_to_default() {
        let _home = crate::paths::ScopedTestHome::new();
        let engine = InMemoryEngine::default();
        let r = run_schema_suggest_phase(&engine, &SchemaSuggestPhaseOpts { source_id: None, dry_run: true })
            .await
            .unwrap();
        assert_eq!(r.source_id, "default");
    }
}
