//! 1-5-3: Cycle module — types + phases + runCycle orchestrator.
//!
//! Ports `src/core/cycle.ts`. The cycle composes lint, backlinks, sync,
//! extract, embed, orphans, purge, and LLM-heavy phases into one unit of work.
//!
//! Per grill Q2 decision:
//! - Orchestrator (types + runCycle + lock) → fully implemented
//! - Trivial phases (orphans, purge) → real implementation (one-line engine call)
//! - Medium phases (sync, lint, backlinks, extract, embed, etc.) → skipped stub
//! - LLM phases (synthesize, patterns, propose_takes, etc.) → skipped stub
//!
//! The `autopilot_cycle` handler (1-4-3 smoke) can be upgraded to call
//! `run_cycle()` once this module is complete.

use std::collections::HashSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::engine::BrainEngine;

// ── Types ──────────────────────────────────────────────────────────────

/// All cycle phases in execution order. Mirrors TS `CyclePhase`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CyclePhase {
    Lint,
    Backlinks,
    Sync,
    Synthesize,
    Extract,
    ExtractFacts,
    ExtractAtoms,
    ResolveSymbolEdges,
    Patterns,
    SynthesizeConcepts,
    RecomputeEmotionalWeight,
    Consolidate,
    ProposeTakes,
    GradeTakes,
    CalibrationProfile,
    ConversationFactsBackfill,
    Embed,
    Orphans,
    SchemaSuggest,
    Purge,
}

impl CyclePhase {
    /// All phases in execution order. Mirrors TS `ALL_PHASES`.
    pub const ALL: &[CyclePhase] = &[
        CyclePhase::Lint,
        CyclePhase::Backlinks,
        CyclePhase::Sync,
        CyclePhase::Synthesize,
        CyclePhase::Extract,
        CyclePhase::ExtractFacts,
        CyclePhase::ExtractAtoms,
        CyclePhase::ResolveSymbolEdges,
        CyclePhase::Patterns,
        CyclePhase::SynthesizeConcepts,
        CyclePhase::RecomputeEmotionalWeight,
        CyclePhase::Consolidate,
        CyclePhase::ProposeTakes,
        CyclePhase::GradeTakes,
        CyclePhase::CalibrationProfile,
        CyclePhase::ConversationFactsBackfill,
        CyclePhase::Embed,
        CyclePhase::Orphans,
        CyclePhase::SchemaSuggest,
        CyclePhase::Purge,
    ];

    /// String label for the phase (matches TS kebab-case names).
    pub fn label(&self) -> &'static str {
        match self {
            CyclePhase::Lint => "lint",
            CyclePhase::Backlinks => "backlinks",
            CyclePhase::Sync => "sync",
            CyclePhase::Synthesize => "synthesize",
            CyclePhase::Extract => "extract",
            CyclePhase::ExtractFacts => "extract-facts",
            CyclePhase::ExtractAtoms => "extract-atoms",
            CyclePhase::ResolveSymbolEdges => "resolve-symbol-edges",
            CyclePhase::Patterns => "patterns",
            CyclePhase::SynthesizeConcepts => "synthesize-concepts",
            CyclePhase::RecomputeEmotionalWeight => "recompute-emotional-weight",
            CyclePhase::Consolidate => "consolidate",
            CyclePhase::ProposeTakes => "propose-takes",
            CyclePhase::GradeTakes => "grade-takes",
            CyclePhase::CalibrationProfile => "calibration-profile",
            CyclePhase::ConversationFactsBackfill => "conversation-facts-backfill",
            CyclePhase::Embed => "embed",
            CyclePhase::Orphans => "orphans",
            CyclePhase::SchemaSuggest => "schema-suggest",
            CyclePhase::Purge => "purge",
        }
    }

    /// Whether this phase mutates state and needs the cycle lock.
    /// Mirrors TS `NEEDS_LOCK_PHASES`. Only `orphans` is read-only.
    pub fn needs_lock(&self) -> bool {
        !matches!(self, CyclePhase::Orphans)
    }

    /// Phase scope: per-source, brain-global, or mixed.
    /// Mirrors TS `PHASE_SCOPE`.
    pub fn scope(&self) -> PhaseScope {
        match self {
            CyclePhase::Lint
            | CyclePhase::Backlinks
            | CyclePhase::Sync
            | CyclePhase::Extract
            | CyclePhase::ExtractFacts
            | CyclePhase::ExtractAtoms
            | CyclePhase::RecomputeEmotionalWeight
            | CyclePhase::Consolidate
            | CyclePhase::ProposeTakes
            | CyclePhase::SchemaSuggest
            | CyclePhase::ConversationFactsBackfill => PhaseScope::Source,
            CyclePhase::ResolveSymbolEdges
            | CyclePhase::GradeTakes
            | CyclePhase::CalibrationProfile
            | CyclePhase::Embed
            | CyclePhase::Orphans
            | CyclePhase::Purge
            | CyclePhase::SynthesizeConcepts => PhaseScope::Global,
            CyclePhase::Synthesize | CyclePhase::Patterns => PhaseScope::Mixed,
        }
    }
}

/// Phase scope taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseScope {
    Source,
    Global,
    Mixed,
}

/// Status of a single phase execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PhaseStatus {
    Ok,
    Warn,
    Fail,
    Skipped,
}

/// Error details when a phase fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseError {
    pub class: String,
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub docs_url: Option<String>,
}

/// Result of a single phase execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseResult {
    pub phase: String,
    pub status: PhaseStatus,
    pub duration_ms: u64,
    pub summary: String,
    pub details: serde_json::Value,
    pub error: Option<PhaseError>,
}

/// Overall cycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CycleStatus {
    Ok,
    Clean,
    Partial,
    Skipped,
    Failed,
}

/// Cycle totals (additive; new fields can be added).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleTotals {
    pub lint_fixes: u64,
    pub backlinks_added: u64,
    pub pages_synced: u64,
    pub pages_extracted: u64,
    pub pages_embedded: u64,
    pub orphans_found: u64,
    pub transcripts_processed: u64,
    pub synth_pages_written: u64,
    pub patterns_written: u64,
    pub pages_emotional_weight_recomputed: u64,
    pub edges_resolved: u64,
    pub edges_ambiguous: u64,
    pub purged_sources_count: u64,
    pub purged_pages_count: u64,
    pub facts_consolidated: u64,
    pub consolidate_takes_written: u64,
    pub phantoms_redirected: u64,
    pub phantoms_ambiguous: u64,
    pub phantoms_skipped_drift: u64,
}

/// Full cycle report. Mirrors TS `CycleReport`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleReport {
    pub schema_version: &'static str,
    pub timestamp: String,
    pub duration_ms: u64,
    pub status: CycleStatus,
    pub reason: Option<String>,
    pub brain_dir: Option<String>,
    pub phases: Vec<PhaseResult>,
    pub totals: CycleTotals,
}

/// Options for `run_cycle`. Mirrors TS `CycleOpts`.
#[derive(Debug, Clone, Default)]
pub struct CycleOpts {
    /// If true, no writes to filesystem or DB.
    pub dry_run: bool,
    /// Phase subset. Defaults to `CyclePhase::ALL`.
    pub phases: Option<Vec<CyclePhase>>,
    /// Brain directory (git repo). Required for filesystem phases.
    pub brain_dir: String,
    /// Whether sync should run `git pull`.
    pub pull: bool,
    /// Source ID for per-source lock scoping (optional).
    pub source_id: Option<String>,
}

// ── runCycle orchestrator ──────────────────────────────────────────────

/// Execute a maintenance cycle. Composes phases sequentially, returning
/// a `CycleReport`.
///
/// Per grill Q2:
/// - `orphans` phase → real implementation (`engine.find_orphan_pages()`)
/// - `purge` phase → real implementation (`engine.purge_deleted_pages(72)`)
/// - All other phases → `skipped("not_migrated")` stub
///
/// Lock acquisition is simplified: InMemory engine skips the lock entirely
/// (no concurrent cycles in tests). SQL backends can add advisory lock later.
pub async fn run_cycle(
    engine: &dyn BrainEngine,
    opts: &CycleOpts,
) -> CycleReport {
    let start = Instant::now();
    let phases = opts.phases.as_deref().unwrap_or(CyclePhase::ALL);
    let dry_run = opts.dry_run;
    let timestamp = chrono::Utc::now().to_rfc3339();

    let mut phase_results: Vec<PhaseResult> = Vec::new();
    let mut totals = CycleTotals::default();

    for &phase in phases {
        let result = execute_phase(engine, phase, opts, dry_run, &mut totals).await;
        phase_results.push(result);
    }

    // Derive overall status from phase results
    let status = derive_cycle_status(&phase_results);

    CycleReport {
        schema_version: "1",
        timestamp,
        duration_ms: start.elapsed().as_millis() as u64,
        status,
        reason: None,
        brain_dir: Some(opts.brain_dir.clone()),
        phases: phase_results,
        totals,
    }
}

/// Execute a single phase. Real implementation for orphans + purge;
/// skipped stub for all others.
async fn execute_phase(
    engine: &dyn BrainEngine,
    phase: CyclePhase,
    _opts: &CycleOpts,
    dry_run: bool,
    totals: &mut CycleTotals,
) -> PhaseResult {
    let phase_start = Instant::now();
    let label = phase.label();

    match phase {
        // ── Real implementations ────────────────────────────────────
        CyclePhase::Orphans => {
            match engine.find_orphan_pages().await {
                Ok(orphans) => {
                    let count = orphans.len() as u64;
                    totals.orphans_found = count;
                    PhaseResult {
                        phase: label.into(),
                        status: if count == 0 { PhaseStatus::Ok } else { PhaseStatus::Warn },
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: format!("{} orphan page{} found", count, if count == 1 { "" } else { "s" }),
                        details: serde_json::json!({
                            "count": count,
                            "orphans": orphans.iter().take(50).map(|o| o.slug.clone()).collect::<Vec<_>>(),
                        }),
                        error: None,
                    }
                }
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "orphans phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(PhaseError {
                        class: "DatabaseConnection".into(),
                        code: "UNKNOWN".into(),
                        message: e.to_string(),
                        hint: None,
                        docs_url: None,
                    }),
                }
            }
        }

        CyclePhase::ExtractFacts => {
            use crate::autopilot::phases::extract_facts::{run_extract_facts, ExtractFactsOpts};

            match run_extract_facts(
                engine,
                &ExtractFactsOpts {
                    dry_run,
                    ..Default::default()
                },
            )
            .await
            {
                Ok(r) => {
                    let status = if r.guard_triggered || !r.warnings.is_empty() {
                        PhaseStatus::Warn
                    } else {
                        PhaseStatus::Ok
                    };
                    PhaseResult {
                        phase: label.into(),
                        status,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: format!(
                            "extract-facts: {} pages scanned, {} facts inserted, {} deleted",
                            r.pages_scanned, r.facts_inserted, r.facts_deleted
                        ),
                        details: serde_json::json!({
                            "pages_scanned": r.pages_scanned,
                            "pages_with_facts": r.pages_with_facts,
                            "facts_inserted": r.facts_inserted,
                            "facts_deleted": r.facts_deleted,
                            "legacy_rows_pending": r.legacy_rows_pending,
                            "guard_triggered": r.guard_triggered,
                            "warnings": r.warnings,
                        }),
                        error: None,
                    }
                }
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "extract-facts phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(PhaseError {
                        class: "DatabaseConnection".into(),
                        code: "UNKNOWN".into(),
                        message: e.to_string(),
                        hint: None,
                        docs_url: None,
                    }),
                },
            }
        }

        CyclePhase::Purge => {
            if dry_run {
                PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Ok,
                    duration_ms: 0,
                    summary: "purge skipped (dry-run)".into(),
                    details: serde_json::json!({ "dry_run": true }),
                    error: None,
                }
            } else {
                // TS uses 72h recovery window
                match engine.purge_deleted_pages(72).await {
                    Ok(result) => {
                        let count = result.count;
                        totals.purged_pages_count = count;
                        PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Ok,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: format!(
                                "purged {} page{}",
                                count,
                                if count == 1 { "" } else { "s" },
                            ),
                            details: serde_json::json!({
                                "pages_deleted": count,
                                "slugs": result.slugs,
                            }),
                            error: None,
                        }
                    }
                    Err(e) => PhaseResult {
                        phase: label.into(),
                        status: PhaseStatus::Fail,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: "purge phase failed".into(),
                        details: serde_json::json!({}),
                        error: Some(PhaseError {
                            class: "DatabaseConnection".into(),
                            code: "UNKNOWN".into(),
                            message: e.to_string(),
                            hint: None,
                            docs_url: None,
                        }),
                    }
                }
            }
        }

        // ── Skipped stubs (not yet migrated) ────────────────────────
        _ => {
            let reason = if matches!(
                phase,
                CyclePhase::Sync
                    | CyclePhase::Lint
                    | CyclePhase::Backlinks
                    | CyclePhase::Extract
                    | CyclePhase::ExtractFacts
                    | CyclePhase::Embed
                    | CyclePhase::RecomputeEmotionalWeight
                    | CyclePhase::Consolidate
            ) {
                "not_migrated: needs orchestration function (syncRepo/recomputeBacklinks/embedBackfill etc.)"
            } else {
                "not_migrated: LLM-heavy phase (no Rust chat provider integration yet)"
            };

            PhaseResult {
                phase: label.into(),
                status: PhaseStatus::Skipped,
                duration_ms: 0,
                summary: format!("{} skipped", label),
                details: serde_json::json!({ "reason": reason }),
                error: None,
            }
        }
    }
}

/// Derive overall cycle status from phase results.
fn derive_cycle_status(results: &[PhaseResult]) -> CycleStatus {
    if results.is_empty() {
        return CycleStatus::Skipped;
    }

    let has_fail = results.iter().any(|r| r.status == PhaseStatus::Fail);
    let has_warn = results.iter().any(|r| r.status == PhaseStatus::Warn);
    let all_skipped = results.iter().all(|r| r.status == PhaseStatus::Skipped);
    let all_ok_or_clean = results
        .iter()
        .all(|r| r.status == PhaseStatus::Ok || r.status == PhaseStatus::Skipped);

    if has_fail {
        // If all attempted (non-skipped) phases failed
        let any_non_skipped = results.iter().any(|r| r.status != PhaseStatus::Skipped);
        if !any_non_skipped {
            CycleStatus::Skipped
        } else {
            let all_failed = results
                .iter()
                .filter(|r| r.status != PhaseStatus::Skipped)
                .all(|r| r.status == PhaseStatus::Fail);
            if all_failed {
                CycleStatus::Failed
            } else {
                CycleStatus::Partial
            }
        }
    } else if all_skipped {
        CycleStatus::Skipped
    } else if has_warn {
        CycleStatus::Partial
    } else if all_ok_or_clean {
        // Check if any work was done (non-skipped, non-clean)
        let any_work = results
            .iter()
            .any(|r| r.status == PhaseStatus::Ok);
        if any_work {
            CycleStatus::Ok
        } else {
            CycleStatus::Clean
        }
    } else {
        CycleStatus::Ok
    }
}

/// Set of phases that need the cycle lock (all except orphans).
pub fn needs_lock_phases() -> HashSet<&'static str> {
    CyclePhase::ALL
        .iter()
        .filter(|p| p.needs_lock())
        .map(|p| p.label())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, InMemoryEngine, PageInput};

    async fn setup() -> InMemoryEngine {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        engine
    }

    async fn put_page(engine: &InMemoryEngine, slug: &str) {
        engine
            .put_page(
                slug,
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: slug.to_string(),
                    compiled_truth: "content".to_string(),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: None,
                },
            )
            .await
            .unwrap();
    }

    // ── Type tests ─────────────────────────────────────────────────────

    #[test]
    fn all_phases_count_matches_ts() {
        // TS has 20 phases (lint through purge); Rust should match
        assert_eq!(CyclePhase::ALL.len(), 20);
    }

    #[test]
    fn only_orphans_does_not_need_lock() {
        for &phase in CyclePhase::ALL {
            if phase == CyclePhase::Orphans {
                assert!(!phase.needs_lock(), "{} should not need lock", phase.label());
            } else {
                assert!(phase.needs_lock(), "{} should need lock", phase.label());
            }
        }
    }

    #[test]
    fn phase_scope_source_vs_global() {
        assert_eq!(CyclePhase::Sync.scope(), PhaseScope::Source);
        assert_eq!(CyclePhase::Embed.scope(), PhaseScope::Global);
        assert_eq!(CyclePhase::Synthesize.scope(), PhaseScope::Mixed);
    }

    // ── run_cycle tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn run_cycle_empty_brain() {
        let engine = setup().await;
        let report = run_cycle(
            &engine,
            &CycleOpts {
                brain_dir: "/tmp/brain".into(),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(report.schema_version, "1");
        assert_eq!(report.phases.len(), 20);
        // orphans should be Ok (0 found)
        let orphans = report.phases.iter().find(|p| p.phase == "orphans").unwrap();
        assert_eq!(orphans.status, PhaseStatus::Ok);
        assert_eq!(orphans.summary, "0 orphan pages found");
        // purge should be Ok
        let purge = report.phases.iter().find(|p| p.phase == "purge").unwrap();
        assert_eq!(purge.status, PhaseStatus::Ok);
        // extract-facts is a real phase now: empty brain → 0 pages scanned, Ok
        let extract_facts = report.phases.iter().find(|p| p.phase == "extract-facts").unwrap();
        assert_eq!(extract_facts.status, PhaseStatus::Ok);
        // All other phases should be Skipped
        let skipped_count = report.phases.iter().filter(|p| p.status == PhaseStatus::Skipped).count();
        assert_eq!(skipped_count, 17);
    }

    #[tokio::test]
    async fn run_cycle_with_orphan_pages() {
        let engine = setup().await;
        // Create pages with no links → all orphaned
        put_page(&engine, "page-a").await;
        put_page(&engine, "page-b").await;

        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Orphans]),
                brain_dir: "/tmp/brain".into(),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(report.phases.len(), 1);
        let orphans = &report.phases[0];
        assert_eq!(orphans.status, PhaseStatus::Warn);
        assert!(orphans.summary.contains("2 orphan"));
        assert_eq!(report.totals.orphans_found, 2);
    }

    #[tokio::test]
    async fn run_cycle_dry_run_skips_purge() {
        let engine = setup().await;
        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Purge]),
                brain_dir: "/tmp/brain".into(),
                dry_run: true,
                ..Default::default()
            },
        )
        .await;

        let purge = &report.phases[0];
        assert_eq!(purge.status, PhaseStatus::Ok);
        assert!(purge.summary.contains("dry-run"));
        assert_eq!(report.totals.purged_pages_count, 0);
    }

    #[tokio::test]
    async fn run_cycle_phase_subset() {
        let engine = setup().await;
        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Orphans, CyclePhase::Purge]),
                brain_dir: "/tmp/brain".into(),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(report.phases.len(), 2);
        assert_eq!(report.phases[0].phase, "orphans");
        assert_eq!(report.phases[1].phase, "purge");
    }

    #[tokio::test]
    async fn run_cycle_status_partial_when_orphans_found() {
        let engine = setup().await;
        put_page(&engine, "orphan").await;

        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Orphans, CyclePhase::Purge]),
                brain_dir: "/tmp/brain".into(),
                ..Default::default()
            },
        )
        .await;

        // orphans → Warn, purge → Ok → overall Partial
        assert_eq!(report.status, CycleStatus::Partial);
    }

    #[tokio::test]
    async fn run_cycle_status_ok_when_no_issues() {
        let engine = setup().await;

        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Orphans, CyclePhase::Purge]),
                brain_dir: "/tmp/brain".into(),
                ..Default::default()
            },
        )
        .await;

        // orphans → Ok (0 found), purge → Ok → overall Ok
        assert_eq!(report.status, CycleStatus::Ok);
    }

    #[tokio::test]
    async fn run_cycle_all_skipped_status() {
        let engine = setup().await;

        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Lint, CyclePhase::Sync]),
                brain_dir: "/tmp/brain".into(),
                ..Default::default()
            },
        )
        .await;

        // Both lint and sync are skipped stubs
        assert_eq!(report.status, CycleStatus::Skipped);
    }

    #[test]
    fn derive_status_empty_is_skipped() {
        let status = derive_cycle_status(&[]);
        assert_eq!(status, CycleStatus::Skipped);
    }

    #[test]
    fn derive_status_all_ok_is_ok() {
        let results = vec![
            PhaseResult {
                phase: "orphans".into(),
                status: PhaseStatus::Ok,
                duration_ms: 10,
                summary: "ok".into(),
                details: serde_json::json!({}),
                error: None,
            },
            PhaseResult {
                phase: "purge".into(),
                status: PhaseStatus::Ok,
                duration_ms: 5,
                summary: "ok".into(),
                details: serde_json::json!({}),
                error: None,
            },
        ];
        assert_eq!(derive_cycle_status(&results), CycleStatus::Ok);
    }

    #[test]
    fn derive_status_has_fail_is_partial_or_failed() {
        let results = vec![
            PhaseResult {
                phase: "orphans".into(),
                status: PhaseStatus::Fail,
                duration_ms: 10,
                summary: "fail".into(),
                details: serde_json::json!({}),
                error: None,
            },
            PhaseResult {
                phase: "purge".into(),
                status: PhaseStatus::Ok,
                duration_ms: 5,
                summary: "ok".into(),
                details: serde_json::json!({}),
                error: None,
            },
        ];
        assert_eq!(derive_cycle_status(&results), CycleStatus::Partial);

        let all_fail = vec![
            PhaseResult {
                phase: "orphans".into(),
                status: PhaseStatus::Fail,
                duration_ms: 10,
                summary: "fail".into(),
                details: serde_json::json!({}),
                error: None,
            },
        ];
        assert_eq!(derive_cycle_status(&all_fail), CycleStatus::Failed);
    }
}
