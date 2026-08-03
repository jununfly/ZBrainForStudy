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

use crate::ai::chat::ChatProvider;
use crate::engine::BrainEngine;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Best-effort audit-dir resolution for `BaseCyclePhase` consumers (1-6-3-3).
/// Mirrors `skillpack::audit::resolve_audit_dir` but without the feature-gated
/// `skillpack` dependency: `ZBRAIN_AUDIT_DIR` env, else `~/.zbrain/audit`,
/// else `./audit`. The ledger is best-effort, so a fallback is acceptable.
fn audit_default_dir() -> std::path::PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".zbrain").join("audit")
    } else {
        std::path::PathBuf::from("./audit")
    }
}

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
    ExtractTakes,
    ResolveSymbolEdges,
    Patterns,
    AutoThink,
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
    Drift,
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
        CyclePhase::ExtractTakes,
        CyclePhase::ResolveSymbolEdges,
        CyclePhase::Patterns,
        CyclePhase::AutoThink,
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
        CyclePhase::Drift,
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
            CyclePhase::ExtractTakes => "extract-takes",
            CyclePhase::ResolveSymbolEdges => "resolve-symbol-edges",
            CyclePhase::Patterns => "patterns",
            CyclePhase::AutoThink => "auto-think",
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
        CyclePhase::Drift => "drift",
    }
    }

    /// Whether this phase mutates state and needs the cycle lock.
    /// Mirrors TS `NEEDS_LOCK_PHASES`. Read-only phases: `orphans`
    /// (read-only scan), `drift` (default-disabled, surfacing only), and
    /// `backlinks` (audit-only — writes nothing; the link graph is the
    /// canonical backlink store).
    pub fn needs_lock(&self) -> bool {
        !matches!(self, CyclePhase::Orphans | CyclePhase::Drift | CyclePhase::Backlinks)
    }

    /// Whether this phase requires a persistent database backend. The only
    /// engine-free phase is `Lint` (purely skipped — not ported to Rust);
    /// every other phase calls into the engine and is therefore DB-dependent
    /// (1-6-1-4).
    pub fn requires_database(&self) -> bool {
        !matches!(self, CyclePhase::Lint)
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
            | CyclePhase::ExtractTakes
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
            | CyclePhase::SynthesizeConcepts
            | CyclePhase::Drift => PhaseScope::Global,
            CyclePhase::Synthesize | CyclePhase::Patterns | CyclePhase::AutoThink => PhaseScope::Mixed,
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

/// Build a [`PhaseError`] from an exception, replacing the ~20 ad-hoc
/// `PhaseError { .. }` literals scattered through [`execute_phase`]. Mirrors
/// the TS `makeErrorFromException` error envelope
/// (class / code / message / hint / docsUrl).
///
/// `code` defaults to `"UNKNOWN"` when empty; `hint` / `docs_url` are `None`
/// unless supplied (1-6-1-3). Call sites pass the concrete `class` / `code`
/// so each phase keeps its TS-parity error identity.
fn make_error_from_exception<E: std::fmt::Display>(
    e: E,
    class: &str,
    code: &str,
    hint: Option<&str>,
    docs_url: Option<&str>,
) -> PhaseError {
    PhaseError {
        class: class.to_string(),
        code: if code.is_empty() { "UNKNOWN" } else { code }.to_string(),
        message: e.to_string(),
        hint: hint.map(|s| s.to_string()),
        docs_url: docs_url.map(|s| s.to_string()),
    }
}

/// Resolve a `source_id` from a brain directory when `CycleOpts.source_id`
/// is `None` (mirrors TS `resolveSourceForDir`). Lists registered sources and
/// returns the id whose tracked `local_path` matches `brain_dir`; falls back
/// to the first non-archived source (maintenance default when a brain has a
/// single source). Returns `None` if no source can be resolved — source-scoped
/// phases then skip as they do today (1-6-1-2).
async fn resolve_source_for_dir(engine: &dyn BrainEngine, brain_dir: &str) -> Option<String> {
    let dir = std::path::Path::new(brain_dir);
    let sources = engine.list_sources(false).await.ok()?;
    // Prefer an exact `local_path` match.
    for s in &sources {
        if let Some(lp) = s.local_path.as_deref() {
            if std::path::Path::new(lp) == dir {
                return Some(s.id.clone());
            }
        }
    }
    // Fallback: first non-archived source.
    sources.first().map(|s| s.id.clone())
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
#[derive(Clone)]
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
    /// LLM chat provider for LLM-heavy phases (extract-atoms, future
    /// propose-takes/grade-takes). `None` → those phases return `Skipped`
    /// (no chat provider wired). Production wiring lives in the runner
    /// (1-6 orchestration) — see KNOWN-GAPS.
    pub chat: Option<Arc<dyn ChatProvider>>,
    // 1-6-1: orchestrator enhancements (mirror TS `CycleOpts` v0.23+ fields)
    /// Called between phases (TS `yieldBetweenPhases`).
    pub yield_between_phases: Option<Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>>,
    /// Called during long-running phases (TS `yieldDuringPhase`).
    pub yield_during_phase: Option<Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>>,
    /// Synthesize ad-hoc transcript path (TS `synthInputFile`).
    pub synth_input_file: Option<String>,
    /// Synthesize single-date filter.
    pub synth_date: Option<String>,
    /// Synthesize inclusive-from date filter.
    pub synth_from: Option<String>,
    /// Synthesize inclusive-to date filter.
    pub synth_to: Option<String>,
    /// Disable the synthesize self-consumption guard.
    pub synth_bypass_dream_guard: bool,
    // 1-6-1-1: abort signal (TS `AbortSignal` parity). When the flag is set,
    // remaining phases are skipped with `reason = "aborted"`. `None` = no
    /// cancellation (the default).
    pub signal: Option<Arc<AtomicBool>>,
}

impl Default for CycleOpts {
    fn default() -> Self {
        Self {
            dry_run: false,
            phases: None,
            brain_dir: String::new(),
            pull: false,
            source_id: None,
            chat: None,
            yield_between_phases: None,
            yield_during_phase: None,
            synth_input_file: None,
            synth_date: None,
            synth_from: None,
            synth_to: None,
            synth_bypass_dream_guard: false,
            signal: None,
        }
    }
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
/// Lock acquisition (1-6-2): a per-source advisory file lock
/// (`autopilot::cycle_lock`) guards cycles against concurrent runs. The
/// lock is only consulted when at least one state-mutating phase is in
/// scope (mirrors TS `needsLock = phases.some(NEEDS_LOCK_PHASES.has)`).
/// `Orphans` is the only read-only phase and skips the lock entirely.
pub async fn run_cycle(
    engine: &dyn BrainEngine,
    opts: &CycleOpts,
) -> CycleReport {
    let start = Instant::now();
    let phases = opts.phases.as_deref().unwrap_or(CyclePhase::ALL);
    let dry_run = opts.dry_run;
    let timestamp = chrono::Utc::now().to_rfc3339();

    // 1-6-2: acquire the cycle advisory lock if any mutating phase is in
    // scope. Busy → `Skipped/cycle_already_running`; I/O failure →
    // `Failed/lock_acquisition_error` (TS parity). A `None` brain_dir
    // (legacy callers) skips the lock — same as TS `engine === null` path
    // which used the no-DB file lock only.
    if phases.iter().any(|p| p.needs_lock()) && !opts.brain_dir.is_empty() {
        match crate::autopilot::cycle_lock::acquire_cycle_lock(
            std::path::Path::new(&opts.brain_dir),
            opts.source_id.as_deref(),
        ) {
            Ok(_lock) => {
                // Held for the duration of the cycle (dropped at function
                // exit). No refresh API is needed because the 30-minute TTL
                // is well above any single phase's runtime.
            }
            Err(crate::autopilot::cycle_lock::AcquireCycleLockError::Busy(holder)) => {
                let _ = holder; // holder is for the consumer's error message; we just early-return.
                return CycleReport {
                    schema_version: "1",
                    timestamp,
                    duration_ms: start.elapsed().as_millis() as u64,
                    status: CycleStatus::Skipped,
                    reason: Some("cycle_already_running".into()),
                    brain_dir: Some(opts.brain_dir.clone()),
                    phases: Vec::new(),
                    totals: CycleTotals::default(),
                };
            }
            Err(crate::autopilot::cycle_lock::AcquireCycleLockError::Io(_e)) => {
                return CycleReport {
                    schema_version: "1",
                    timestamp,
                    duration_ms: start.elapsed().as_millis() as u64,
                    status: CycleStatus::Failed,
                    reason: Some("lock_acquisition_error".into()),
                    brain_dir: Some(opts.brain_dir.clone()),
                    phases: vec![PhaseResult {
                        phase: "sync".into(),
                        status: PhaseStatus::Fail,
                        duration_ms: 0,
                        summary: "could not acquire cycle lock".into(),
                        details: serde_json::json!({}),
                        error: Some(make_error_from_exception(
                            _e,
                            "FilesystemError",
                            "CYCLE_LOCK_IO",
                            None,
                            None,
                        )),
                    }],
                    totals: CycleTotals::default(),
                };
            }
        }
    }

    // 1-6-1-2: resolve the source id from the brain dir when the caller did
    // not pin one. The resolved id is threaded through every phase (and the
    // finalizer) via `effective_opts`, mirroring TS `resolveSourceForDir` at
    // the cycle entry point.
    let mut resolved_source_id = opts.source_id.clone();
    if resolved_source_id.is_none() && !opts.brain_dir.is_empty() {
        resolved_source_id = resolve_source_for_dir(engine, &opts.brain_dir).await;
    }
    let effective_opts = CycleOpts {
        source_id: resolved_source_id,
        ..opts.clone()
    };

    let mut phase_results: Vec<PhaseResult> = Vec::new();
    let mut totals = CycleTotals::default();

    for &phase in phases {
        // 1-6-1-1: abort signal (TS `checkAborted` parity). If the caller's
        // `AbortSignal` equivalent has fired, skip each remaining phase with
        // `reason = "aborted"` rather than executing it.
        if let Some(sig) = &opts.signal {
            if sig.load(Ordering::Relaxed) {
                phase_results.push(PhaseResult {
                    phase: phase.label().into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: 0,
                    summary: "skipped (aborted via signal)".into(),
                    details: serde_json::json!({ "reason": "aborted" }),
                    error: None,
                });
                continue;
            }
        }
        // 1-6-1-4: no-database guard (TS `engine === null` parity). When the
        // backend reports no persistent store, DB-dependent phases skip with
        // `reason = "no_database"` instead of failing at runtime. `Lint` is
        // engine-free and runs regardless.
        if phase.requires_database() && !engine.supports_database() {
            phase_results.push(PhaseResult {
                phase: phase.label().into(),
                status: PhaseStatus::Skipped,
                duration_ms: 0,
                summary: "skipped (no database backend)".into(),
                details: serde_json::json!({ "reason": "no_database" }),
                error: None,
            });
            continue;
        }
        let result = execute_phase(engine, phase, &effective_opts, dry_run, &mut totals).await;
        phase_results.push(result);
    }

    // 1-6-1: pull totals from the per-phase details (TS `extractTotals`).
    extract_totals(&phase_results, &mut totals);

    // Derive overall status from phase results.
    // 1-6-1: empty list → Failed (TS parity).
    let status = derive_cycle_status(&phase_results, &totals);

    // 1-6-1: best-effort write of last_full_cycle_at on success.
    // Mirrors TS v0.38: only when sourceId set + engine present + not dryRun
    // + status in {ok, clean, partial}.
    if !dry_run
        && !opts.brain_dir.is_empty()
        && matches!(status, CycleStatus::Ok | CycleStatus::Clean | CycleStatus::Partial)
    {
        write_last_full_cycle_at(engine, &effective_opts, &status).await;
    }

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
                    error: Some(make_error_from_exception(
                        e,
                        "DatabaseConnection",
                        "UNKNOWN",
                        None,
                        None,
                    )),
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
                    error: Some(make_error_from_exception(
                        e,
                        "DatabaseConnection",
                        "UNKNOWN",
                        None,
                        None,
                    )),
                },
            }
        }

        CyclePhase::ExtractAtoms => {
            use crate::autopilot::phases::extract_atoms::{run_extract_atoms, ExtractAtomsOpts};

            match &_opts.chat {
                None => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "extract-atoms: no chat provider wired (skipped)".into(),
                    details: serde_json::json!({ "reason": "no_chat_provider" }),
                    error: None,
                },
                Some(chat) => {
                    match run_extract_atoms(
                        engine,
                        chat.as_ref(),
                        &ExtractAtomsOpts {
                            dry_run,
                            source_id: _opts.source_id.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        Ok(r) => {
                            let status = if r.pages_total == 0 && r.transcripts_total == 0 {
                                PhaseStatus::Skipped
                            } else if r.failures.is_empty()
                                && r.transcripts_skipped_budget == 0
                                && r.pages_skipped_budget == 0
                            {
                                PhaseStatus::Ok
                            } else {
                                PhaseStatus::Warn
                            };
                            PhaseResult {
                                phase: label.into(),
                                status,
                                duration_ms: phase_start.elapsed().as_millis() as u64,
                                summary: format!(
                                    "extract-atoms: {} atoms from {} pages ({} failed)",
                                    r.atoms_extracted,
                                    r.pages_processed,
                                    r.failures.len()
                                ),
                                details: serde_json::json!({
                                    "atoms_extracted": r.atoms_extracted,
                                    "transcripts_processed": r.transcripts_processed,
                                    "transcripts_total": r.transcripts_total,
                                    "transcripts_skipped_budget": r.transcripts_skipped_budget,
                                    "pages_processed": r.pages_processed,
                                    "pages_total": r.pages_total,
                                    "pages_skipped_budget": r.pages_skipped_budget,
                                    "duplicates_skipped": r.duplicates_skipped,
                                    "failures": r.failures,
                                    "estimated_spend_usd": r.estimated_spend_usd,
                                    "budget_usd": r.budget_usd,
                                    "source_id": r.source_id,
                                    "dry_run": r.dry_run,
                                }),
                                error: None,
                            }
                        }
                        Err(e) => PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Fail,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: "extract-atoms phase failed".into(),
                            details: serde_json::json!({}),
                            error: Some(make_error_from_exception(
                                e,
                                "DatabaseConnection",
                                "UNKNOWN",
                                None,
                                None,
                            )),
                        },
                    }
                }
            }
        }

        CyclePhase::ExtractTakes => {
            use crate::autopilot::phases::extract_takes::{run_extract_takes, ExtractTakesOpts};

            match run_extract_takes(
                engine,
                &ExtractTakesOpts {
                    dry_run,
                    source_id: _opts.source_id.clone(),
                    ..Default::default()
                },
            )
            .await
            {
                Ok(r) => {
                    let status = if r.warnings.is_empty() {
                        PhaseStatus::Ok
                    } else {
                        PhaseStatus::Warn
                    };
                    PhaseResult {
                        phase: label.into(),
                        status,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: format!(
                            "extract-takes: {} pages scanned, {} takes upserted",
                            r.pages_scanned, r.takes_upserted
                        ),
                        details: serde_json::json!({
                            "pages_scanned": r.pages_scanned,
                            "pages_with_takes": r.pages_with_takes,
                            "takes_upserted": r.takes_upserted,
                            "warnings": r.warnings,
                            "failed_files": r.failed_files.iter().map(|f| serde_json::json!({
                                "path": f.path, "error": f.error
                            })).collect::<Vec<_>>(),
                        }),
                        error: None,
                    }
                }
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "extract-takes phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(make_error_from_exception(
                        e,
                        "DatabaseConnection",
                        "UNKNOWN",
                        None,
                        None,
                    )),
                },
            }
        }

        CyclePhase::ProposeTakes => {
            use crate::autopilot::phases::propose_takes::{run_propose_takes, ProposeTakesOpts};

            match &_opts.chat {
                None => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "propose-takes: no chat provider wired (skipped)".into(),
                    details: serde_json::json!({ "reason": "no_chat_provider" }),
                    error: None,
                },
                Some(chat) => {
                    match run_propose_takes(
                        engine,
                        chat.as_ref(),
                        &ProposeTakesOpts {
                            dry_run,
                            source_id: _opts.source_id.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        Ok(r) => {
                            let status = if r.budget_exhausted || !r.warnings.is_empty() {
                                PhaseStatus::Warn
                            } else {
                                PhaseStatus::Ok
                            };
                            PhaseResult {
                                phase: label.into(),
                                status,
                                duration_ms: phase_start.elapsed().as_millis() as u64,
                                summary: format!(
                                    "propose-takes: scanned {} pages, {} proposals inserted ({} cache hits)",
                                    r.pages_scanned, r.proposals_inserted, r.cache_hits
                                ),
                                details: serde_json::json!({
                                    "pages_scanned": r.pages_scanned,
                                    "cache_hits": r.cache_hits,
                                    "cache_misses": r.cache_misses,
                                    "proposals_inserted": r.proposals_inserted,
                                    "budget_exhausted": r.budget_exhausted,
                                    "warnings": r.warnings,
                                }),
                                error: None,
                            }
                        }
                        Err(e) => PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Fail,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: "propose-takes phase failed".into(),
                            details: serde_json::json!({}),
                            error: Some(make_error_from_exception(
                                e,
                                "DatabaseConnection",
                                "UNKNOWN",
                                None,
                                None,
                            )),
                        },
                    }
                }
            }
        }

        CyclePhase::GradeTakes => {
            use crate::autopilot::phases::grade_takes::{run_grade_takes, GradeTakesOpts};

            match &_opts.chat {
                None => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "grade-takes: no chat provider wired (skipped)".into(),
                    details: serde_json::json!({ "reason": "no_chat_provider" }),
                    error: None,
                },
                Some(chat) => {
                    match run_grade_takes(
                        engine,
                        chat.as_ref(),
                        &GradeTakesOpts {
                            ..Default::default()
                        },
                    )
                    .await
                    {
                        Ok(r) => {
                            let status = if r.budget_exhausted || !r.warnings.is_empty() {
                                PhaseStatus::Warn
                            } else {
                                PhaseStatus::Ok
                            };
                            PhaseResult {
                                phase: label.into(),
                                status,
                                duration_ms: phase_start.elapsed().as_millis() as u64,
                                summary: format!(
                                    "grade-takes: scanned {} takes, {} verdicts written ({} cached, {} auto-applied)",
                                    r.takes_scanned, r.verdicts_written, r.cache_hits, r.auto_applied
                                ),
                                details: serde_json::json!({
                                    "takes_scanned": r.takes_scanned,
                                    "cache_hits": r.cache_hits,
                                    "verdicts_written": r.verdicts_written,
                                    "auto_applied": r.auto_applied,
                                    "too_recent": r.too_recent,
                                    "budget_exhausted": r.budget_exhausted,
                                    "ensemble_invoked": r.ensemble_invoked,
                                    "ensemble_unanimous": r.ensemble_unanimous,
                                    "warnings": r.warnings,
                                }),
                                error: None,
                            }
                        }
                        Err(e) => PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Fail,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: "grade-takes phase failed".into(),
                            details: serde_json::json!({}),
                            error: Some(make_error_from_exception(
                                e,
                                "DatabaseConnection",
                                "UNKNOWN",
                                None,
                                None,
                            )),
                        },
                    }
                }
            }
        }

        CyclePhase::ConversationFactsBackfill => {
            use crate::autopilot::phases::conversation_facts_backfill::{
                run_phase_conversation_facts_backfill, ConversationFactsBackfillOpts,
            };

            match &_opts.chat {
                None => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "conversation-facts-backfill: no chat provider wired (skipped)"
                        .into(),
                    details: serde_json::json!({ "reason": "no_chat_provider" }),
                    error: None,
                },
                Some(chat) => {
                    // No global config store in Rust yet: the TS gate
                    // `cycle.conversation_facts_backfill.enabled` defaults to
                    // false, so the cycle arm passes `enabled: false` and the
                    // phase self-reports Skipped (same behavior as TS default).
                    match run_phase_conversation_facts_backfill(
                        engine,
                        chat.as_ref(),
                        &ConversationFactsBackfillOpts::default(),
                    )
                    .await
                    {
                        Ok(r) => {
                            let status = match r.status.as_str() {
                                "skipped" => PhaseStatus::Skipped,
                                "ok" => PhaseStatus::Ok,
                                _ => PhaseStatus::Warn,
                            };
                            PhaseResult {
                                phase: label.into(),
                                status,
                                duration_ms: phase_start.elapsed().as_millis() as u64,
                                summary: format!("conversation-facts-backfill: {}", r.summary),
                                details: serde_json::json!({
                                    "sources_count": r.sources_count,
                                    "sources_processed": r.sources_processed,
                                    "pages_processed": r.pages_processed,
                                    "pages_skipped": r.pages_skipped,
                                    "facts_inserted": r.facts_inserted,
                                    "spent_usd": r.spent_usd,
                                    "skipped_by_brain_wide_cap": r.skipped_by_brain_wide_cap,
                                    "skipped_by_brain_wide_walltime": r.skipped_by_brain_wide_walltime,
                                    "types": r.types,
                                }),
                                error: None,
                            }
                        }
                        Err(e) => PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Fail,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: "conversation-facts-backfill phase failed".into(),
                            details: serde_json::json!({}),
                            error: Some(make_error_from_exception(
                                e,
                                "DatabaseConnection",
                                "UNKNOWN",
                                None,
                                None,
                            )),
                        },
                    }
                }
            }
        }

        CyclePhase::RecomputeEmotionalWeight => {
            use crate::autopilot::phases::recompute_emotional_weight::{
                run_phase_recompute_emotional_weight, RecomputeEmotionalWeightOpts,
            };

            // Deterministic phase, no LLM — runs regardless of chat wiring.
            // Overrides (high_emotion_tags / user_holder) come from opts; the
            // Rust cycle has no global config store, so defaults apply here
            // (the calibration/backfill consumers supply overrides in 1-6).
            match run_phase_recompute_emotional_weight(
                engine,
                &RecomputeEmotionalWeightOpts::default(),
            )
            .await
            {
                Ok(r) => {
                    let status = match r.status.as_str() {
                        "ok" => PhaseStatus::Ok,
                        "fail" => PhaseStatus::Fail,
                        _ => PhaseStatus::Warn,
                    };
                    PhaseResult {
                        phase: label.into(),
                        status,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: r.summary.clone(),
                        details: serde_json::json!({
                            "pages_recomputed": r.pages_recomputed,
                            "mode": r.mode,
                            "dry_run": r.dry_run,
                        }),
                        error: None,
                    }
                }
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "recompute_emotional_weight phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(make_error_from_exception(
                        e,
                        "DatabaseConnection",
                        "UNKNOWN",
                        None,
                        None,
                    )),
                },
            }
        }

        CyclePhase::CalibrationProfile => {
            use crate::calibration::calibration_profile::{
                run_calibration_profile, CalibrationProfileOpts, CalibrationProfileStatus,
            };

            // LLM-heavy phase: needs a chat provider to generate pattern
            // statements / bias tags through the voice gate. No chat → skipped
            // stub (mirrors extract-atoms / propose-takes behaviour). When a
            // provider is wired but the brain is cold (insufficient resolved
            // takes), `run_calibration_profile` short-circuits to Skipped.
            match &_opts.chat {
                None => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: 0,
                    summary: "calibration-profile: no chat provider wired (skipped)".into(),
                    details: serde_json::json!({ "reason": "no_chat_provider" }),
                    error: None,
                },
                Some(chat) => {
                    let cp_opts = CalibrationProfileOpts {
                        chat: Some(chat.clone()),
                        source_id: _opts.source_id.clone(),
                        ..Default::default()
                    };
                    match run_calibration_profile(engine, &cp_opts).await {
                        Ok(r) => {
                            let status = match r.status {
                                CalibrationProfileStatus::Ok => PhaseStatus::Ok,
                                CalibrationProfileStatus::Warn => PhaseStatus::Warn,
                                CalibrationProfileStatus::Skipped => PhaseStatus::Skipped,
                            };
                            let summary = if r.status == CalibrationProfileStatus::Skipped {
                                format!(
                                    "calibration-profile skipped ({})",
                                    r.skipped.as_deref().unwrap_or("unknown")
                                )
                            } else {
                                "calibration-profile".into()
                            };
                            PhaseResult {
                                phase: label.into(),
                                status,
                                duration_ms: phase_start.elapsed().as_millis() as u64,
                                summary,
                                details: serde_json::json!({
                                    "profile_written": r.profile_written,
                                    "voice_gate_passed": r.voice_gate_passed,
                                    "voice_gate_attempts": r.voice_gate_attempts,
                                    "pattern_statements": r.pattern_statements.len(),
                                    "active_bias_tags": r.active_bias_tags.len(),
                                    "total_resolved": r.total_resolved,
                                    "brier": r.brier,
                                    "skipped": r.skipped,
                                    "warnings": r.warnings,
                                }),
                                error: None,
                            }
                        }
                        Err(e) => PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Fail,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: "calibration-profile phase failed".into(),
                            details: serde_json::json!({}),
                                error: Some(make_error_from_exception(
                                    e,
                                    "Calibration",
                                    "UNKNOWN",
                                    None,
                                    None,
                                )),
                        },
                    }
                }
            }
        }

        CyclePhase::SynthesizeConcepts => {
            use crate::autopilot::phases::synthesize_concepts::{
                run_synthesize_concepts, SynthesizeConceptsOpts,
            };

            // LLM phase (T1/T2 narratives). No chat → skipped stub, mirroring
            // extract-atoms / propose-takes. With chat wired, atoms without
            // an execute_raw-capable engine fail-soft to a clean skip.
            match &_opts.chat {
                None => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "synthesize-concepts: no chat provider wired (skipped)".into(),
                    details: serde_json::json!({ "reason": "no_chat_provider" }),
                    error: None,
                },
                Some(chat) => {
                    let sc_opts = SynthesizeConceptsOpts {
                        dry_run,
                        source_id: _opts.source_id.clone(),
                        atoms: None,
                    };
                    match run_synthesize_concepts(engine, chat.as_ref(), &sc_opts).await {
                        Ok(r) => PhaseResult {
                            phase: label.into(),
                            status: match r.status.as_str() {
                                "ok" => PhaseStatus::Ok,
                                "warn" => PhaseStatus::Warn,
                                _ => PhaseStatus::Skipped,
                            },
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: r.summary.clone(),
                            details: serde_json::json!({
                                "reason": r.reason,
                                "concepts_written": r.concepts_written,
                                "tier_counts": {
                                    "T1": r.tier_t1,
                                    "T2": r.tier_t2,
                                    "T3": r.tier_t3,
                                },
                                "groups_found": r.groups_found,
                                "atoms_seen": r.atoms_seen,
                                "failures": r.failures,
                                "estimated_spend_usd": r.estimated_spend_usd,
                                "budget_usd": r.budget_usd,
                                "dry_run": r.dry_run,
                            }),
                            error: None,
                        },
                        Err(e) => PhaseResult {
                            phase: label.into(),
                            status: PhaseStatus::Fail,
                            duration_ms: phase_start.elapsed().as_millis() as u64,
                            summary: "synthesize-concepts phase failed".into(),
                            details: serde_json::json!({}),
                            error: Some(make_error_from_exception(
                                e,
                                "DatabaseConnection",
                                "UNKNOWN",
                                None,
                                None,
                            )),
                        },
                    }
                }
            }
        }

        CyclePhase::AutoThink => {
            use crate::autopilot::phases::auto_think::{run_phase_auto_think, AutoThinkPhaseOpts};
            let a_opts = AutoThinkPhaseOpts {
                brain_dir: Some(_opts.brain_dir.clone()),
                dry_run,
                ..Default::default()
            };
            match run_phase_auto_think(engine, _opts.chat.as_deref(), &a_opts).await {
                Ok(r) => PhaseResult {
                    phase: label.into(),
                    status: match r.status.as_str() {
                        "complete" => PhaseStatus::Ok,
                        "partial" => PhaseStatus::Warn,
                        "failed" => PhaseStatus::Fail,
                        _ => PhaseStatus::Skipped,
                    },
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: r.detail.clone(),
                    details: serde_json::json!({
                        "reason": r.reason,
                        "questions_run": r.questions_run,
                        "synthesized": r.synthesized,
                        "dry_run": r.dry_run,
                        "outcomes": r.outcomes.iter().map(|o| serde_json::json!({
                            "question": o.question,
                            "status": o.status,
                            "slug": o.slug,
                            "warnings": o.warnings,
                        })).collect::<Vec<_>>(),
                    }),
                    error: None,
                },
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "auto-think phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(make_error_from_exception(
                        e,
                        "DatabaseConnection",
                        "AUTO_THINK_PHASE_FAIL",
                        None,
                        None,
                    )),
                },
            }
        }

        CyclePhase::SchemaSuggest => {
            use crate::autopilot::phases::schema_suggest::{
                run_schema_suggest_phase, SchemaSuggestPhaseOpts,
            };

            // No LLM (hermetic heuristic library) — runs without a chat
            // provider, unlike extract-atoms/propose-takes. Best-effort:
            // library errors on the normal path surface as Skipped with a
            // reason (never abort the cycle); dry-run errors → Fail.
            let ss_opts = SchemaSuggestPhaseOpts {
                source_id: _opts.source_id.clone(),
                dry_run,
            };
            match run_schema_suggest_phase(engine, &ss_opts).await {
                Ok(r) => PhaseResult {
                    phase: label.into(),
                    status: if r.skipped { PhaseStatus::Skipped } else { PhaseStatus::Ok },
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: if r.skipped {
                        format!("skipped: {}", r.reason.as_deref().unwrap_or("unknown"))
                    } else {
                        format!("{} suggestions emitted", r.suggestions_emitted)
                    },
                    details: serde_json::to_value(&r).unwrap_or_else(|_| serde_json::json!({})),
                    error: None,
                },
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: format!("error: {e}"),
                    details: serde_json::json!({ "error": e.to_string() }),
                    error: Some(make_error_from_exception(
                        e,
                        "DatabaseConnection",
                        "UNKNOWN",
                        None,
                        None,
                    )),
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
                        error: Some(make_error_from_exception(
                            e,
                            "DatabaseConnection",
                            "UNKNOWN",
                            None,
                            None,
                        )),
                    }
                }
            }
        }

        CyclePhase::Drift => {
            use crate::autopilot::base_phase::{BaseCyclePhase, BasePhaseCtx, BasePhaseOpts};
            use crate::autopilot::phases::drift::DriftPhase;

            // First real `BaseCyclePhase` consumer (1-6-3-3). Default-disabled;
            // `DriftPhase::run` returns Skipped unless `dream.drift.enabled`.
            // Audit dir mirrors `budget_meter::resolve_audit_dir` (avoids the
            // feature-gated `skillpack` path): ZBRAIN_AUDIT_DIR else ~/.zbrain/audit.
            let audit_dir = if let Ok(dir) = std::env::var("ZBRAIN_AUDIT_DIR") {
                if !dir.trim().is_empty() {
                    std::path::PathBuf::from(dir.trim())
                } else {
                    audit_default_dir()
                }
            } else {
                audit_default_dir()
            };
            let ctx = BasePhaseCtx::new(
                _opts.source_id.clone(),
                _opts.chat.clone(),
                dry_run,
                audit_dir,
            );
            DriftPhase.run(engine, &ctx, &BasePhaseOpts::default()).await
        }

        CyclePhase::Patterns => {
            use crate::autopilot::phases::patterns::{run_phase_patterns, PatternsPhaseOpts};

            // Cross-session theme detection: enqueues a single subagent and
            // waits for it (the subagent runs in the minion worker, which is
            // wired with a chat provider — so this phase needs no chat here).
            // Empty-brain / insufficient-reflection runs skip cleanly.
            let p_opts = PatternsPhaseOpts {
                brain_dir: Some(_opts.brain_dir.clone()),
                dry_run,
                wait_timeout_ms: None,
            };
            match run_phase_patterns(engine, &p_opts).await {
                Ok(r) => PhaseResult {
                    phase: label.into(),
                    status: match r.status.as_str() {
                        "ok" => PhaseStatus::Ok,
                        "warn" => PhaseStatus::Warn,
                        _ => PhaseStatus::Skipped,
                    },
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: r.summary.clone(),
                    details: serde_json::json!({
                        "reason": r.reason,
                        "reflections_considered": r.reflections_considered,
                        "patterns_written": r.patterns_written,
                        "reverse_write_count": r.reverse_write_count,
                        "child_outcome": r.child_outcome,
                        "job_id": r.job_id,
                        "dry_run": r.dry_run,
                    }),
                    error: None,
                },
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "patterns phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(make_error_from_exception(
                        e,
                        "InternalError",
                        "PATTERNS_PHASE_FAIL",
                        None,
                        None,
                    )),
                },
            }
        }

        CyclePhase::Synthesize => {
            use crate::autopilot::phases::synthesize::{run_phase_synthesize, SynthesizePhaseOpts};

            // Fan-out orchestration: discovers transcripts, judges significance
            // via the wired chat provider, and enqueues one "subagent" minion per
            // worth-processing transcript/chunk (synthesis runs in the worker).
            // corpus_dir is wired from the engine config store in 1-3-4-6, so until
            // then real runs skip "not_configured".
            // 1-6-1: forward synth opts from CycleOpts (TS `synthInputFile`/`synthDate`/
            // `synthFrom`/`synthTo`/`synthBypassDreamGuard`).
            let s_opts = SynthesizePhaseOpts {
                brain_dir: Some(_opts.brain_dir.clone()),
                dry_run,
                corpus_dir: _opts.synth_input_file.clone(),
                date: _opts.synth_date.clone(),
                from: _opts.synth_from.clone(),
                to: _opts.synth_to.clone(),
                bypass_dream_guard: _opts.synth_bypass_dream_guard,
                ..Default::default()
            };
            match run_phase_synthesize(engine, _opts.chat.as_deref(), &s_opts).await {
                Ok(r) => PhaseResult {
                    phase: label.into(),
                    status: match r.status.as_str() {
                        "ok" => PhaseStatus::Ok,
                        "warn" => PhaseStatus::Warn,
                        _ => PhaseStatus::Skipped,
                    },
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: r.summary.clone(),
                    details: serde_json::json!({
                        "reason": r.reason,
                        "transcripts_discovered": r.transcripts_discovered,
                        "transcripts_processed": r.transcripts_processed,
                        "children_submitted": r.children_submitted,
                        "pages_written": r.pages_written,
                        "disk_files_written": r.disk_files_written,
                        "dry_run": r.dry_run,
                        "verdicts": r.verdicts.iter().map(|v| serde_json::json!({
                            "file_path": v.file_path,
                            "worth": v.worth,
                            "reasons": v.reasons,
                            "cached": v.cached,
                        })).collect::<Vec<_>>(),
                        "child_outcomes": r.child_outcomes.iter().map(|c| serde_json::json!({
                            "job_id": c.job_id,
                            "status": c.status,
                        })).collect::<Vec<_>>(),
                        "skips": r.skips.iter().map(|s| serde_json::json!({
                            "file_path": s.file_path,
                            "reason": s.reason,
                        })).collect::<Vec<_>>(),
                    }),
                    error: None,
                },
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "synthesize phase failed".into(),
                    details: serde_json::json!({}),
                    error: Some(make_error_from_exception(
                        e,
                        "InternalError",
                        "SYNTH_PHASE_FAIL",
                        None,
                        None,
                    )),
                },
            }
        }

        // ── Sync: reuse sync::core::perform_sync ─────────────────────
        CyclePhase::Sync => {
            // Sync needs a real git repo (brain_dir) AND a resolvable source
            // (perform_sync stamps the sync anchor on the source row; TS falls
            // back to global config.sync.* keys, which Rust's perform_sync
            // does not implement). Skeleton/maintenance brains without either
            // simply skip — there is nothing to pull/import.
            if _opts.brain_dir.is_empty() || _opts.source_id.is_none() {
                return PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "sync skipped (no brain_dir or source)".into(),
                    details: serde_json::json!({ "reason": "no_brain_dir_or_source" }),
                    error: None,
                };
            }
            use crate::git::GitClient;
            use crate::sync::core::{perform_sync, IncrementalSyncOpts};
            use std::path::Path;
            let source_id = _opts.source_id.clone().unwrap_or_default();
            let current_commit = GitClient::head_commit(Path::new(&_opts.brain_dir))
                .await
                .unwrap_or_default();
            let previous_commit = match engine.get_source(&source_id).await {
                Ok(Some(s)) => s.last_commit,
                _ => None,
            };
            let opts = IncrementalSyncOpts {
                source_id,
                repo_path: Path::new(&_opts.brain_dir).to_path_buf(),
                current_commit,
                previous_commit,
                chunker_version: None,
                failures_dir: std::env::temp_dir(),
                max_file_size: None,
            };
            match perform_sync(engine, &opts, None).await {
                Ok(r) => {
                    let status = if r.failures > 0 {
                        PhaseStatus::Warn
                    } else {
                        PhaseStatus::Ok
                    };
                    PhaseResult {
                        phase: label.into(),
                        status,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: format!("+{} imported, -{} deleted", r.imported, r.deleted),
                        details: serde_json::json!({
                            "added": r.imported,
                            "modified": 0u64,
                            "deleted": r.deleted,
                            "failures": r.failures,
                            "full_sync": r.full_sync,
                            "dryRun": dry_run,
                        }),
                        error: None,
                    }
                }
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "sync phase failed".into(),
                    details: serde_json::json!({}),
                error: Some(make_error_from_exception(
                    e,
                    "SyncError",
                    "SYNC_FAILED",
                    None,
                    None,
                )),
                },
            }
        }

        // ── Lint: not yet ported to Rust (TS commands/lint.ts runLintCore) ──
        CyclePhase::Lint => {
            // Rust has no port of the page-level markdown lint that TS
            // `runPhaseLint` performs. The only Rust "lint" is
            // schema_pack::lint_rules (manifest validation) — a different
            // concern. Tracked as KNOWN-GAP G65. We report skipped rather
            // than fake a run.
            PhaseResult {
                phase: label.into(),
                status: PhaseStatus::Skipped,
                duration_ms: phase_start.elapsed().as_millis() as u64,
                summary: "lint skipped (not ported to Rust)".into(),
                details: serde_json::json!({
                    "reason": "lint_not_ported_to_rust",
                    "ts_ref": "commands/lint.ts runLintCore",
                }),
                error: None,
            }
        }

        // ── Backlinks: audit-only (derived from page_links, writes nothing) ──
        CyclePhase::Backlinks => {
            // Maintenance backlinks are audit-only: the link graph
            // (page_links) is the canonical backlink store, and maintenance
            // cycles must NOT rewrite tracked pages with generated
            // "Referenced in" bullets. We audit the graph on read and report
            // it intact (gaps=0 by construction). Writes nothing.
            match engine.list_all_page_refs().await {
                Ok(refs) => {
                    let pages_affected = refs.len() as u64;
                    PhaseResult {
                        phase: label.into(),
                        status: PhaseStatus::Ok,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: format!(
                            "{} page(s) audited (backlinks derived from page_links)",
                            pages_affected
                        ),
                        details: serde_json::json!({
                            "gaps": 0u64,
                            "added": 0u64,
                            "pages_affected": pages_affected,
                            "mode": "audit-only",
                            "dryRun": dry_run,
                        }),
                        error: None,
                    }
                }
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "backlinks phase failed".into(),
                    details: serde_json::json!({}),
                error: Some(make_error_from_exception(
                    e,
                    "BacklinksError",
                    "BACKLINKS_FAILED",
                    None,
                    None,
                )),
                },
            }
        }

        // ── Extract: reuse auto_fix::extract_links ──────────────────
        CyclePhase::Extract => {
            use crate::auto_fix::{extract_links, ExtractLinksOpts};
            // TS honors dry-run by skipping (no clean dry-run mode yet).
            // Mirror that honestly.
            if dry_run {
                return PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "dry-run: extract skipped (no dry-run mode yet)".into(),
                    details: serde_json::json!({ "dryRun": true, "reason": "no_dry_run_support" }),
                    error: None,
                };
            }
            match extract_links(engine, &ExtractLinksOpts::default()).await {
                Ok(r) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Ok,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: format!("{} link(s) created", r.links_created),
                    details: serde_json::json!({
                        "linksCreated": r.links_created,
                        "pages_processed": r.pages_processed,
                        "dangling": r.dangling,
                        "incremental": false,
                    }),
                    error: None,
                },
                Err(e) => PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Fail,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "extract phase failed".into(),
                    details: serde_json::json!({}),
                error: Some(make_error_from_exception(
                    e,
                    "ExtractError",
                    "EXTRACT_FAILED",
                    None,
                    None,
                )),
                },
            }
        }

        // ── Embed: reuse auto_fix::embed_stale (needs embedding client) ──
        CyclePhase::Embed => {
            #[cfg(feature = "embedding")]
            {
                use crate::auto_fix::{embed_stale, EmbedStaleOpts};
                use crate::embedding::EmbeddingClient;
                let Some(client) = EmbeddingClient::from_env() else {
                    return PhaseResult {
                        phase: label.into(),
                        status: PhaseStatus::Skipped,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: "embed skipped (no embedding client configured)".into(),
                        details: serde_json::json!({ "reason": "no_embedding_client" }),
                        error: None,
                    };
                };
                let opts = EmbedStaleOpts {
                    dry_run,
                    source_id: _opts.source_id.clone(),
                };
                match embed_stale(engine, &client, &opts).await {
                    Ok(r) => PhaseResult {
                        phase: label.into(),
                        status: PhaseStatus::Ok,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: if dry_run {
                            format!("{} chunk(s) would be embedded (dry-run)", r.would_embed)
                        } else {
                            format!("{} chunk(s) newly embedded", r.embedded)
                        },
                        details: serde_json::json!({
                            "embedded": r.embedded,
                            "would_embed": r.would_embed,
                            "skipped": r.skipped,
                            "total": r.total,
                            "dryRun": dry_run,
                        }),
                        error: None,
                    },
                    Err(e) => PhaseResult {
                        phase: label.into(),
                        status: PhaseStatus::Fail,
                        duration_ms: phase_start.elapsed().as_millis() as u64,
                        summary: "embed phase failed".into(),
                        details: serde_json::json!({}),
                    error: Some(make_error_from_exception(
                        e,
                        "EmbeddingError",
                        "EMBED_FAILED",
                        None,
                        None,
                    )),
                    },
                }
            }
            #[cfg(not(feature = "embedding"))]
            {
                PhaseResult {
                    phase: label.into(),
                    status: PhaseStatus::Skipped,
                    duration_ms: phase_start.elapsed().as_millis() as u64,
                    summary: "embed skipped (embedding feature disabled)".into(),
                    details: serde_json::json!({ "reason": "embedding_feature_disabled" }),
                    error: None,
                }
            }
        }

        // ── Skipped stubs (not yet migrated) ────────────────────────
        _ => {
            let reason = if matches!(phase, CyclePhase::Consolidate) {
                "not_migrated: needs orchestration function (consolidateFacts etc.)"
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
///
/// 1-6-1: empty result list → `Failed` (TS parity: an empty run is not
/// silently `Skipped`). Distinguishes "no phases requested" from "all phases
/// skipped due to a real precondition" (e.g. no database).
fn derive_cycle_status(results: &[PhaseResult], totals: &CycleTotals) -> CycleStatus {
    if results.is_empty() {
        return CycleStatus::Failed;
    }

    let has_fail = results.iter().any(|r| r.status == PhaseStatus::Fail);
    let has_warn = results.iter().any(|r| r.status == PhaseStatus::Warn);
    let all_skipped = results.iter().all(|r| r.status == PhaseStatus::Skipped);
    let all_ok_or_skipped = results
        .iter()
        .all(|r| r.status == PhaseStatus::Ok || r.status == PhaseStatus::Skipped);

    if has_fail {
        // If all attempted (non-skipped) phases failed
        let any_non_skipped = results.iter().any(|r| r.status != PhaseStatus::Skipped);
        if !any_non_skipped {
            CycleStatus::Failed
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
    } else if all_ok_or_skipped {
        // 1-6-1: TS `deriveStatus` parity — any non-zero field in totals
        // means at least one phase did work, so the report is `Ok`; an
        // entirely no-op run is `Clean`.
        let any_work = totals.any_nonzero();
        if any_work {
            CycleStatus::Ok
        } else {
            CycleStatus::Clean
        }
    } else {
        CycleStatus::Ok
    }
}

impl CycleTotals {
    /// True if any counter is non-zero. Used to derive Ok vs Clean.
    fn any_nonzero(&self) -> bool {
        self.lint_fixes > 0
            || self.backlinks_added > 0
            || self.pages_synced > 0
            || self.pages_extracted > 0
            || self.pages_embedded > 0
            || self.orphans_found > 0
            || self.transcripts_processed > 0
            || self.synth_pages_written > 0
            || self.patterns_written > 0
            || self.pages_emotional_weight_recomputed > 0
            || self.edges_resolved > 0
            || self.edges_ambiguous > 0
            || self.purged_sources_count > 0
            || self.purged_pages_count > 0
            || self.facts_consolidated > 0
            || self.consolidate_takes_written > 0
            || self.phantoms_redirected > 0
            || self.phantoms_ambiguous > 0
            || self.phantoms_skipped_drift > 0
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

// ── 1-6-1 helpers ─────────────────────────────────────────────────────

/// Pull totals from per-phase details (TS `extractTotals`).
///
/// Each phase's `details` is a JSON object; the same key names the TS code
/// reads (e.g. `pages_synced` for `sync`, `phantoms_redirected` for
/// `extract_facts`) are looked up here. Phases not yet implemented or
/// returning empty details simply contribute zeros.
fn extract_totals(phases: &[PhaseResult], totals: &mut CycleTotals) {
    for p in phases {
        let Some(d) = p.details.as_object() else { continue };
        let get = |k: &str| -> u64 {
            d.get(k).and_then(|v| v.as_u64()).unwrap_or(0)
        };
        match p.phase.as_str() {
            "lint" => totals.lint_fixes = get("fixed"),
            "backlinks" => totals.backlinks_added = get("added"),
            "sync" => {
                totals.pages_synced = get("added") + get("modified");
            }
            "extract" => totals.pages_extracted = get("linksCreated"),
            "embed" => {
                let dry = matches!(d.get("dryRun").and_then(|v| v.as_bool()), Some(true));
                totals.pages_embedded = if dry { get("would_embed") } else { get("embedded") };
            }
            "synthesize" => {
                totals.transcripts_processed = get("transcripts_processed");
                totals.synth_pages_written = get("pages_written");
            }
            "patterns" => totals.patterns_written = get("patterns_written"),
            "recompute-emotional-weight" => {
                totals.pages_emotional_weight_recomputed = get("pages_recomputed");
            }
            "resolve-symbol-edges" => {
                totals.edges_resolved = get("edges_resolved");
                totals.edges_ambiguous = get("edges_ambiguous");
            }
            "purge" => {
                totals.purged_sources_count = get("purged_sources_count");
                totals.purged_pages_count = get("purged_pages_count");
            }
            "consolidate" => {
                totals.facts_consolidated = get("facts_consolidated");
                totals.consolidate_takes_written = get("takes_written");
            }
            "extract-facts" => {
                totals.phantoms_redirected = get("phantoms_redirected");
                totals.phantoms_ambiguous = get("phantoms_ambiguous");
                totals.phantoms_skipped_drift = get("phantoms_skipped_drift");
            }
            _ => {}
        }
    }
}

/// Best-effort write of `last_full_cycle_at` into the source's `config`
/// JSONB blob. Mirrors TS v0.38 cycle finalizer:
///   - only when `source_id` is set + not dryRun + status in {ok, clean, partial}
///   - failures are logged at warn, not fatal (the cycle already succeeded)
///
/// We read the current source row, merge the new key into `config`, and
/// pass the merged object back through `update_source`. Any failure
/// (source missing, DB down) is logged and swallowed.
async fn write_last_full_cycle_at(
    engine: &dyn BrainEngine,
    opts: &CycleOpts,
    _status: &CycleStatus,
) {
    let Some(source_id) = opts.source_id.as_deref() else { return };
    let now = chrono::Utc::now().to_rfc3339();
    let row = match engine.get_source(source_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            eprintln!(
                "[cycle] source {} not found; skipping last_full_cycle_at",
                source_id
            );
            return;
        }
        Err(e) => {
            eprintln!(
                "[cycle] failed to read source {} for last_full_cycle_at: {}",
                source_id, e
            );
            return;
        }
    };
    let mut config = row.config.clone();
    if let Some(obj) = config.as_object_mut() {
        obj.insert("last_full_cycle_at".to_string(), serde_json::Value::String(now));
    } else {
        config = serde_json::json!({ "last_full_cycle_at": now });
    }
    let input = crate::engine::UpdateSourceInput {
        config: Some(config),
        ..Default::default()
    };
    if let Err(e) = engine.update_source(source_id, &input).await {
        eprintln!(
            "[cycle] failed to write last_full_cycle_at for source {}: {}",
            source_id, e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, InMemoryEngine, PageInput};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique per-test brain_dir so the cycle advisory file lock (1-6-2)
    /// doesn't serialise parallel-running tests against a shared path.
    fn unique_brain_dir(label: &str) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "zbrain-cycle-test-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().into_owned()
    }

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
        // TS has 20 phases (lint through purge). Rust adds `extract-takes` as
        // a 21st dedicated cycle phase (see extract_takes.rs taxonomy note) —
        // TS only consumes it via the v0_28_0 orchestrator, not runCycle.
        // 1-6-3-3 adds `drift` as a 22nd dedicated phase (default-disabled
        // scaffold; the first real `BaseCyclePhase` consumer).
        assert_eq!(CyclePhase::ALL.len(), 23);
    }

    #[test]
    fn read_only_phases_do_not_need_lock() {
        for &phase in CyclePhase::ALL {
            if phase == CyclePhase::Orphans
                || phase == CyclePhase::Drift
                || phase == CyclePhase::Backlinks
            {
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
        // schema-suggest is a real phase now and appends to the schema-events
        // audit JSONL — isolate ~/.zbrain so the test never touches the real
        // home (thread-local override; tokio::test runs current-thread).
        let _home = crate::paths::ScopedTestHome::new();
        let engine = setup().await;
        let report = run_cycle(
            &engine,
            &CycleOpts {
                brain_dir: unique_brain_dir("empty_brain"),
                ..Default::default()
            },
        )
        .await;

        assert_eq!(report.schema_version, "1");
        // 22 = 20 TS phases + extract-takes (elevated to a dedicated Rust
        // cycle phase; see extract_takes.rs taxonomy note) + drift (1-6-3-3).
        assert_eq!(report.phases.len(), 23);
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
        // extract-takes is a real phase now: empty brain → 0 pages scanned, Ok
        // (db path enumerates 0 page refs; no LLM, no warnings).
        let extract_takes = report.phases.iter().find(|p| p.phase == "extract-takes").unwrap();
        assert_eq!(extract_takes.status, PhaseStatus::Ok);
        // extract-atoms is a real phase now, but the empty-brain test wires no
        // chat provider → Skipped (it would call the LLM otherwise). When a
        // provider is wired but there is no work, it still returns Skipped.
        let extract_atoms = report.phases.iter().find(|p| p.phase == "extract-atoms").unwrap();
        assert_eq!(extract_atoms.status, PhaseStatus::Skipped);
        assert_eq!(extract_atoms.summary, "extract-atoms: no chat provider wired (skipped)");
        // propose-takes is a real LLM phase now, but the empty-brain test wires
        // no chat provider → Skipped (it would call the LLM otherwise).
        let propose_takes = report.phases.iter().find(|p| p.phase == "propose-takes").unwrap();
        assert_eq!(propose_takes.status, PhaseStatus::Skipped);
        assert_eq!(propose_takes.summary, "propose-takes: no chat provider wired (skipped)");
        // conversation-facts-backfill is a real phase now, but the empty-brain
        // test wires no chat provider → Skipped. Even with a provider it stays
        // Skipped by default (enabled=false gate, matching the TS default).
        let conv_backfill = report
            .phases
            .iter()
            .find(|p| p.phase == "conversation-facts-backfill")
            .unwrap();
        assert_eq!(conv_backfill.status, PhaseStatus::Skipped);
        assert_eq!(
            conv_backfill.summary,
            "conversation-facts-backfill: no chat provider wired (skipped)"
        );
        // recompute-emotional-weight is a real (deterministic, no-LLM) phase:
        // empty brain → 0 pages recomputed, Ok.
        let recompute = report
            .phases
            .iter()
            .find(|p| p.phase == "recompute-emotional-weight")
            .unwrap();
        assert_eq!(recompute.status, PhaseStatus::Ok);
        assert_eq!(recompute.summary, "recompute_emotional_weight (0 pages)");
        // calibration-profile is a real (LLM-heavy) phase now, but the
        // empty-brain test wires no chat provider → Skipped.
        let calibration = report
            .phases
            .iter()
            .find(|p| p.phase == "calibration-profile")
            .unwrap();
        assert_eq!(calibration.status, PhaseStatus::Skipped);
        assert_eq!(
            calibration.summary,
            "calibration-profile: no chat provider wired (skipped)"
        );
        // synthesize-concepts is a real (LLM) phase now, but the empty-brain
        // test wires no chat provider → Skipped.
        let synth_concepts = report
            .phases
            .iter()
            .find(|p| p.phase == "synthesize-concepts")
            .unwrap();
        assert_eq!(synth_concepts.status, PhaseStatus::Skipped);
        assert_eq!(
            synth_concepts.summary,
            "synthesize-concepts: no chat provider wired (skipped)"
        );
        // schema-suggest is a real phase now with NO LLM dependency
        // (hermetic heuristic library): empty brain → 0 suggestions, Ok.
        let schema_suggest = report
            .phases
            .iter()
            .find(|p| p.phase == "schema-suggest")
            .unwrap();
        assert_eq!(schema_suggest.status, PhaseStatus::Ok);
        assert_eq!(schema_suggest.summary, "0 suggestions emitted");
        // sync is now wired (reuses sync::core::perform_sync) but the empty-
        // brain test has no source_id → Skipped (no anchor target).
        let sync = report.phases.iter().find(|p| p.phase == "sync").unwrap();
        assert_eq!(sync.status, PhaseStatus::Skipped);
        assert_eq!(sync.details["reason"], "no_brain_dir_or_source");
        // extract is now wired (reuses auto_fix::extract_links) and runs on the
        // DB; empty brain → 0 links, Ok.
        let extract = report.phases.iter().find(|p| p.phase == "extract").unwrap();
        assert_eq!(extract.status, PhaseStatus::Ok);
        assert_eq!(extract.details["linksCreated"], 0);
        // backlinks is now wired as audit-only (derived from page_links); empty
        // brain → 0 pages audited, Ok.
        let backlinks = report.phases.iter().find(|p| p.phase == "backlinks").unwrap();
        assert_eq!(backlinks.status, PhaseStatus::Ok);
        assert_eq!(backlinks.details["pages_affected"], 0);
        assert_eq!(backlinks.details["mode"], "audit-only");
        // embed is now wired (reuses auto_fix::embed_stale) but no embedding
        // client/feature in the test → Skipped.
        let embed = report.phases.iter().find(|p| p.phase == "embed").unwrap();
        assert_eq!(embed.status, PhaseStatus::Skipped);
        // lint is not yet ported to Rust → Skipped with a clear reason.
        let lint = report.phases.iter().find(|p| p.phase == "lint").unwrap();
        assert_eq!(lint.status, PhaseStatus::Skipped);
        assert_eq!(lint.details["reason"], "lint_not_ported_to_rust");
        // patterns is a real phase now: empty brain → 0 reflections <
        // min_evidence(3) → Skipped (insufficient_evidence).
        let patterns = report
            .phases
            .iter()
            .find(|p| p.phase == "patterns")
            .unwrap();
        assert_eq!(patterns.status, PhaseStatus::Skipped);
        assert_eq!(patterns.summary, "0 reflections in last 30d (need ≥3)");
        // All other phases should be Skipped. Count is 15: extract-atoms +
        // propose-takes + grade-takes + calibration-profile +
        // conversation-facts-backfill + synthesize-concepts (real LLM phases,
        // no chat here) + auto-think (default-disabled → Skipped) + drift
        // (default-disabled scaffold → Skipped, 1-6-3-3) + embed (no client/
        // feature) + lint (not ported). Sync/extract/backlinks are now wired
        // and run to Ok(0) on an empty brain, dropping the count from 17 to 15.
        let skipped_count = report.phases.iter().filter(|p| p.status == PhaseStatus::Skipped).count();
        assert_eq!(skipped_count, 15);
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
                brain_dir: unique_brain_dir("with_orphan_pages"),
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
                brain_dir: unique_brain_dir("dry_run_skips_purge"),
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
                brain_dir: unique_brain_dir("phase_subset"),
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
                brain_dir: unique_brain_dir("status_partial_when_orphans_found"),
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
                brain_dir: unique_brain_dir("status_ok_when_no_issues"),
                ..Default::default()
            },
        )
        .await;

        // 1-6-1: TS `deriveStatus` parity — no work done in any phase
        // (orphans=0, purge=0) → `Clean`, not `Ok`. `Ok` is reserved for
        // a run where totals show at least one unit of work.
        assert_eq!(report.status, CycleStatus::Clean);
    }

    // 1-6-2: cycle advisory lock — busy → `Skipped/cycle_already_running`.
    // We hold the lock out-of-band then call `run_cycle`; the second call
    // must short-circuit without touching any phase.
    #[tokio::test]
    async fn run_cycle_busy_lock_returns_skipped() {
        let _home = crate::paths::ScopedTestHome::new();
        let engine = setup().await;
        let brain = unique_brain_dir("busy_lock");

        // Hold the lock externally so the cycle cannot acquire it.
        let _held = crate::autopilot::cycle_lock::acquire_cycle_lock(
            std::path::Path::new(&brain),
            None,
        )
        .expect("test setup: lock should be acquirable");

        let report = run_cycle(
            &engine,
            &CycleOpts {
                brain_dir: brain,
                ..Default::default()
            },
        )
        .await;

        assert_eq!(report.status, CycleStatus::Skipped);
        assert_eq!(report.reason.as_deref(), Some("cycle_already_running"));
        assert!(report.phases.is_empty());
    }

    // 1-6-2: per-source lock scope — two sources may run concurrently.
    #[tokio::test]
    async fn run_cycle_per_source_locks_independent() {
        let _home = crate::paths::ScopedTestHome::new();
        let engine = setup().await;
        let brain = unique_brain_dir("per_source");

        // Hold a lock for source "a".
        let _held = crate::autopilot::cycle_lock::acquire_cycle_lock(
            std::path::Path::new(&brain),
            Some("a"),
        )
        .expect("test setup: lock 'a' should be acquirable");

        // A cycle for source "b" must still be able to run.
        let report = run_cycle(
            &engine,
            &CycleOpts {
                brain_dir: brain,
                source_id: Some("b".into()),
                ..Default::default()
            },
        )
        .await;
        assert_ne!(report.status, CycleStatus::Skipped);
    }

    #[tokio::test]
    async fn run_cycle_all_skipped_status() {
        let engine = setup().await;

        let report = run_cycle(
            &engine,
            &CycleOpts {
                phases: Some(vec![CyclePhase::Lint, CyclePhase::Sync]),
                brain_dir: unique_brain_dir("all_skipped_status"),
                ..Default::default()
            },
        )
        .await;

        // Both lint and sync are skipped stubs
        assert_eq!(report.status, CycleStatus::Skipped);
    }

    #[test]
    fn derive_status_empty_is_failed() {
        // 1-6-1: TS parity — empty phase list is `Failed`, not `Skipped`.
        let status = derive_cycle_status(&[], &CycleTotals::default());
        assert_eq!(status, CycleStatus::Failed);
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
        assert_eq!(derive_cycle_status(&results, &CycleTotals::default()), CycleStatus::Clean);
    }

    #[test]
    fn derive_status_all_ok_with_work_is_ok() {
        let results = vec![PhaseResult {
            phase: "orphans".into(),
            status: PhaseStatus::Ok,
            duration_ms: 10,
            summary: "ok".into(),
            details: serde_json::json!({}),
            error: None,
        }];
        let mut totals = CycleTotals::default();
        totals.orphans_found = 3;
        assert_eq!(derive_cycle_status(&results, &totals), CycleStatus::Ok);
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
        assert_eq!(derive_cycle_status(&results, &CycleTotals::default()), CycleStatus::Partial);

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
        assert_eq!(derive_cycle_status(&all_fail, &CycleTotals::default()), CycleStatus::Failed);
    }
}
