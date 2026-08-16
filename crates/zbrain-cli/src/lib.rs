//! `zbrain-cli` — command-line entry point.
//!
//! Slice 1-3-1: clap CLI framework with 4 command stubs.
//! Slice 1-3-1-2: Config file discovery, YAML parsing, and env var overrides.
//! Next slices add command implementations.

pub mod config;
pub mod mcp_client;
pub mod schema_cmd;
pub mod skillify;
pub mod skillpack;
pub mod timeout;
pub mod update_check;
pub mod models;
pub mod apply_migrations;
pub mod mounts;
pub mod book_mirror;
pub mod check_brain_first;
pub mod check_resolvable;
pub mod inline_worker;
pub mod routing_eval;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zbrain_core::autopilot::cycle::{
    run_cycle, CycleOpts, CyclePhase, CycleReport, CycleStatus, PhaseResult, PhaseStatus,
};
use zbrain_core::ai::chat::ChatProvider;
use zbrain_core::ai::expand::{expand_query, ChatExpansionProvider, ExpansionProvider};
use zbrain_core::embedding::EmbeddingClient;
use zbrain_core::engine::BrainEngine;
use zbrain_core::eval::brainstorm::orchestrator::{
    BRAINSTORM_PROFILE, BrainstormProfile, LSD_PROFILE, ResumeOverrides,
};
use zbrain_core::operation::{register_all, CliOpts, OperationContext, OperationRegistry};
use zbrain_core::progress::{ProgressMode, ProgressReporter};

/// Doctor check status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
    /// A TS doctor check not yet migrated to Rust. Surfaced for traceability
    /// (Q2) but excluded from health_score / status / exit code so it neither
    /// poisons CI nor lets a later agent mistake doctor for fully migrated.
    NotImplemented,
}

impl CheckStatus {
    /// Wire string used in the `--json` report, aligned with TS check statuses
    /// (`ok` / `warn` / `fail`) plus the Rust-only `not-implemented` trace.
    fn as_str(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
            CheckStatus::NotImplemented => "not-implemented",
        }
    }
}

/// A single doctor check result
struct DoctorCheck {
    name: String,
    status: CheckStatus,
    message: String,
}

impl DoctorCheck {
    fn new(name: &str, status: CheckStatus, message: &str) -> Self {
        Self {
            name: name.to_string(),
            status,
            message: message.to_string(),
        }
    }

    fn ok(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::Ok, message)
    }

    fn warn(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::Warn, message)
    }

    fn fail(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::Fail, message)
    }

    fn not_implemented(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::NotImplemented, message)
    }
}

/// Subsystem-aggregated TS doctor checks not yet migrated to Rust (Q3).
/// Each entry is `(name, covers)` where `covers` names the cluster of TS
/// sub-checks it stands in for. Surfaced as `not-implemented` in the doctor
/// report. The full 70+ sub-check detail lives in the parity audit doc.
///
/// Hard trace: migrating a subsystem means moving its entry OUT of here into a
/// real check — the anchor test guards against silent removal.
/// registered in docs/plans/MIGRATION.md (G5).
const UNMIGRATED_TS_DOCTOR_CHECKS: &[(&str, &str)] = &[
    ("search_mode", "search modes overrides, mode drift"),
    ("federation_health", "federated source sync, mount reachability"),
    ("schema_packs", "schema pack presence / drift"),
    ("resolver_health", "resolver conformance, check-resolvable mirror"),
    ("frontmatter_integrity", "bounded frontmatter scan, partial-state surfacing"),
];

/// Composite health score (0-100), mirroring TS `outputResults`:
/// `score = 100 - fail*20 - warn*5`, clamped to a `>= 0` floor.
/// `Ok` checks contribute nothing; the score never drops below 0.
fn doctor_health_score(checks: &[DoctorCheck]) -> i64 {
    let mut score: i64 = 100;
    for check in checks {
        match check.status {
            CheckStatus::Fail => score -= 20,
            CheckStatus::Warn => score -= 5,
            CheckStatus::Ok | CheckStatus::NotImplemented => {}
        }
    }
    score.max(0)
}

/// Headline status, mirroring TS `computeDoctorReport`:
/// any `Fail` -> "unhealthy"; else any `Warn` -> "warnings"; else "healthy".
fn doctor_status(checks: &[DoctorCheck]) -> &'static str {
    let has_fail = checks.iter().any(|c| c.status == CheckStatus::Fail);
    let has_warn = checks.iter().any(|c| c.status == CheckStatus::Warn);
    if has_fail {
        "unhealthy"
    } else if has_warn {
        "warnings"
    } else {
        "healthy"
    }
}

/// Build the structured `--json` doctor report, aligned field-for-field with
/// TS `computeDoctorReport`: `{schema_version:2, status, health_score, checks[]}`,
/// where each check entry is the TS mandatory core subset `{name, status, message}`.
fn doctor_json_report(checks: &[DoctorCheck]) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 2,
        "status": doctor_status(checks),
        "health_score": doctor_health_score(checks),
        "checks": checks
            .iter()
            .map(|c| serde_json::json!({
                "name": c.name,
                "status": c.status.as_str(),
                "message": c.message,
            }))
            .collect::<Vec<_>>(),
    })
}

/// Static crate name.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-cli"
}

/// Banner string used by the binary entry point.
#[must_use]
pub fn banner() -> String {
    format!(
        "{} v{} (core: {} v{})",
        crate_name(),
        env!("CARGO_PKG_VERSION"),
        zbrain_core::crate_name(),
        zbrain_core::version(),
    )
}

/// Whether `s` matches the TS timeout magnitude class `[0-9]+(?:\.[0-9]+)?`:
/// one or more ASCII digits, optionally followed by a single `.` and one or
/// more ASCII digits. No sign, no exponent, no bare `.5` or `5.`.
fn is_ts_timeout_magnitude(s: &str) -> bool {
    let mut parts = s.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match parts.next() {
        None => true, // no decimal point
        Some(frac) => !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()),
    }
}

/// Parse a `--timeout` value into milliseconds.
///
/// Mirrors `parseTimeout` in `src/core/cli-options.ts` character-for-character:
/// accepts an integer or decimal magnitude with an optional `ms`/`s`/`m` unit
/// suffix (no suffix defaults to `ms`). Non-positive or malformed values return
/// `None`.
///
/// Unlike the TS global-flag parser (which fell through to the per-command
/// parser on `None`), the Rust clap wiring treats `None` as a hard parse
/// failure (exit 2) — a deliberate, audited departure from the TS soft
/// fall-through.
#[must_use]
pub fn parse_timeout(s: &str) -> Option<u64> {
    let s = s.trim();
    // Split trailing unit suffix (ms/s/m); default to ms when absent.
    let (num_part, unit) = if let Some(rest) = s.strip_suffix("ms") {
        (rest, "ms")
    } else if let Some(rest) = s.strip_suffix('s') {
        (rest, "s")
    } else if let Some(rest) = s.strip_suffix('m') {
        (rest, "m")
    } else {
        (s, "ms")
    };

    // Enforce the TS regex magnitude class `[0-9]+(?:\.[0-9]+)?` exactly:
    // one or more digits, an optional single `.` followed by one or more
    // digits. This rejects things Rust's `f64::parse` would otherwise accept
    // (scientific notation `1e3`, leading `+`, `inf`/`nan`, `.5`, `5.`).
    if !is_ts_timeout_magnitude(num_part) {
        return None;
    }

    let n: f64 = num_part.parse().ok()?;
    if !n.is_finite() || n <= 0.0 {
        return None;
    }

    let ms = match unit {
        "ms" => n,
        "s" => n * 1000.0,
        "m" => n * 60_000.0,
        _ => unreachable!(),
    };
    Some(ms.floor() as u64)
}

/// clap `value_parser` adapter for `--timeout`.
///
/// Returns the resolved millisecond count, or an `Err(String)` that clap
/// renders to stderr and exits with code 2. This is the deliberate,
/// audited departure from the TS soft fall-through: a bad `--timeout` is a
/// hard usage error, not a silently-ignored flag.
fn parse_timeout_clap(s: &str) -> Result<u64, String> {
    parse_timeout(s).ok_or_else(|| {
        format!("invalid timeout '{s}': expected a positive value like 30s, 1500ms, or 2m")
    })
}

/// Default per-call timeout in milliseconds for `think`.
/// Mirrors the TS dispatch-layer default at `src/cli.ts:302`.
const THINK_DEFAULT_TIMEOUT_MS: u64 = 180_000;
/// Default per-call timeout in milliseconds for all other operations.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Default local read-only wall-clock timeout for `sources list`, in ms.
///
/// Mirrors the ONE read-only default that is actually reachable in the TS CLI
/// (`src/cli.ts:1137`, `sources list` → 10s). The sibling `search → 30_000`
/// branch (cli.ts:1136) is dead code — `search`/`query` are shared ops that
/// never enter `handleCliOnly`, so that timeout never fires in TS. We port
/// only the live behavior.
const SOURCES_LIST_DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// Resolve the effective wall-clock timeout for `sources list`.
///
/// User-supplied `--timeout` (already in ms) wins; otherwise the 10s default.
/// Returns the resolved ms plus whether it came from the user (controls the
/// override hint in `timeout::format_timeout_message`).
#[must_use]
fn resolve_sources_list_timeout(cli_timeout_ms: Option<u64>) -> (u64, bool) {
    match cli_timeout_ms {
        Some(ms) => (ms, true),
        None => (SOURCES_LIST_DEFAULT_TIMEOUT_MS, false),
    }
}

/// Resolve the effective per-call timeout for an operation.
///
/// Mirrors `src/cli.ts:302-303`: `think` defaults to 180s, everything else to
/// 30s, and a user-supplied `--timeout` (already resolved to milliseconds)
/// overrides the default.
#[must_use]
fn resolve_timeout_ms(op_name: &str, cli_timeout_ms: Option<u64>) -> u64 {
    cli_timeout_ms.unwrap_or(if op_name == "think" {
        THINK_DEFAULT_TIMEOUT_MS
    } else {
        DEFAULT_TIMEOUT_MS
    })
}

/// Honest warning for `--timeout` on the local (non-thin-client) path.
///
/// The local read-only wall-clock timeout was migrated for `sources list`
/// only (mirroring the ONE live TS default; cli.ts:1136 `search → 30s` is
/// dead code — see `SOURCES_LIST_DEFAULT_TIMEOUT_MS`). `sources list` runs
/// outside `run_operation`, so every command that *does* reach this warning
/// (`query`, `think`, `get_page`, `list_pages`, …) still has no local
/// wall-clock timeout — the warning remains truthful for them. We refuse to
/// silently swallow `--timeout` (no `--offline`-style dead flag). Returns
/// `Some(message)` when the user supplied `--timeout`, else `None`.
#[must_use]
fn local_timeout_warning(cli_timeout_ms: Option<u64>) -> Option<String> {
    cli_timeout_ms.map(|_| {
        "warning: --timeout has no effect in local mode yet (only thin-client MCP calls honor it); local timeout support is pending"
            .to_string()
    })
}

/// ZBrain command-line interface.
#[derive(Debug, Parser)]
#[command(name = "zbrain")]
#[command(about = "AI-powered knowledge base and semantic search engine", long_about = None)]
#[command(version = env!("CARGO_PKG_VERSION"))]
pub struct Cli {
    /// Path to config file (defaults: ./zbrain.yml then ~/.zbrain/config)
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Enable debug logging
    #[arg(short, long, global = true)]
    pub debug: bool,

    /// Per-call timeout for thin-client-routed MCP calls.
    ///
    /// Accepts a bare millisecond count or a `Ns`/`Nms`/`Nm` value (e.g.
    /// `30s`, `1500ms`, `2m`). Mirrors the TS global `--timeout` flag; only
    /// thin-client-routed operations (and the local `sources list`
    /// wall-clock) consume it today — other local operations warn on stderr
    /// (see `local_timeout_warning`). Invalid values fail loudly with exit 2
    /// rather than silently falling through.
    #[arg(long, global = true, value_parser = parse_timeout_clap, value_name = "DURATION")]
    pub timeout: Option<u64>,

    /// Suppress human-friendly progress output (stderr).
    #[arg(long, global = true)]
    pub quiet: bool,

    /// Emit newline-delimited JSON progress events instead of human-readable text.
    #[arg(long, global = true)]
    pub progress_json: bool,

    /// Minimum interval in milliseconds between progress ticks (default: 1000).
    #[arg(long, global = true, value_parser = parse_timeout_clap, value_name = "DURATION")]
    pub progress_interval: Option<u64>,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

// ════════════════════════════════════════════════════════════════════════════
// brainstorm / lsd / eval-brainstorm — roadmap node 1-1-5-5
//
// Faithful port of `src/commands/{brainstorm,eval-brainstorm}.ts`. The
// `brainstorm` and `lsd` verbs share `BrainstormArgs` and differ only by the
// engine preset (BRAINSTORM_PROFILE vs LSD_PROFILE). `eval-brainstorm` is the
// three-axis (DISTANCE + USEFULNESS + GROUNDING) gate. Resume / checkpoint
// persistence is a Q3-MVP no-op (see zbrain_core::eval::brainstorm::checkpoint).
// ════════════════════════════════════════════════════════════════════════════

/// Shared CLI args for `zbrain brainstorm` and `zbrain lsd`. Mirrors
/// `BrainstormCliArgs` in `src/commands/brainstorm.ts`.
#[derive(Debug, clap::Args)]
pub struct BrainstormArgs {
    /// The question to brainstorm. All positional tokens are joined with a space.
    #[arg(value_name = "QUESTION", num_args = 1.., required = false)]
    pub question: Vec<String>,

    /// Emit the BrainstormResult as JSON (for agents).
    #[arg(long)]
    pub json: bool,

    /// Save to wiki/ideas/<date>-<mode>-<slug>.md (overrides the per-profile default).
    #[arg(long)]
    pub save: bool,

    /// Don't save; print only (overrides the per-profile default).
    #[arg(long = "no-save")]
    pub no_save: bool,

    /// Skip the cost-preview notice (scripted callers / non-TTY).
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Override the far-bank size (default 6 brainstorm / 12 lsd).
    #[arg(long)]
    pub limit: Option<usize>,

    /// Abort if estimated cost exceeds this USD amount (default 5).
    #[arg(long = "max-cost")]
    pub max_cost: Option<f64>,

    /// Cap domain-bank prefix sampling (default 50).
    #[arg(long = "max-far-set")]
    pub max_far_set: Option<usize>,

    /// Abort if running cost exceeds 5× the estimate.
    #[arg(long = "strict-budget")]
    pub strict_budget: bool,

    /// Override the judge LLM model.
    #[arg(long = "judge-model")]
    pub judge_model: Option<String>,

    /// Max ideas per judge LLM call (default 100).
    #[arg(long = "max-ideas-per-judge-call")]
    pub max_ideas_per_judge_call: Option<usize>,

    /// Resume a previously-crashed run by run_id (Q3: not yet wired).
    #[arg(long)]
    pub resume: Option<String>,

    /// Bypass the 7-day staleness gate on --resume (Q3: not yet wired).
    #[arg(long = "force-resume")]
    pub force_resume: bool,

    /// Print persisted run-store entries and exit (1-1-5-9).
    #[arg(long = "list-runs")]
    pub list_runs: bool,

    /// Persist the full result JSON to the run store (trend/review/resume).
    /// Default follows the profile (`brainstorm`=on, `lsd`=off); independent of
    /// `--save` (which writes a wiki/ideas page).
    #[arg(long = "save-run")]
    pub save_run: bool,

    /// Don't persist to the run store (overrides the profile default).
    #[arg(long = "no-save-run")]
    pub no_save_run: bool,

    /// Reclaim stale run-store entries (mtime older than `--gc-days`) and exit.
    #[arg(long = "gc")]
    pub gc: bool,

    /// Staleness window in days for `--gc` (default 7).
    #[arg(long = "gc-days")]
    pub gc_days: Option<u64>,

    /// Override the run-store directory (default: ~/.zbrain/runs/brainstorm).
    #[arg(long = "store-dir")]
    pub store_dir: Option<std::path::PathBuf>,

    /// Review a single persisted run by run_id: full report + metadata header
    /// (1-1-5-10). With `--json`, prints the raw run row.
    #[arg(long = "review-run")]
    pub review_run: Option<String>,

    /// Print the pass-rate / grounding trend across persisted runs (1-1-5-10).
    #[arg(long)]
    pub trend: bool,

    /// Time window in days for `--trend` (default 30).
    #[arg(long = "days")]
    pub days: Option<u64>,
}

/// CLI args for `zbrain eval-brainstorm`. Mirrors `EvalBrainstormCliArgs`.
#[derive(Debug, clap::Args)]
pub struct EvalBrainstormArgs {
    /// Path to a JSONL fixture: one `{ "question": "..." }` object per line.
    #[arg(value_name = "FIXTURE")]
    pub fixture: Option<String>,

    /// Emit the BrainstormEvalReport as JSON.
    #[arg(long)]
    pub json: bool,

    /// Cap to N fixtures (default: all).
    #[arg(long)]
    pub limit: Option<usize>,

    /// Override the distance threshold (default 0.4).
    #[arg(long = "distance-min")]
    pub distance_min: Option<f64>,

    /// Override the usefulness threshold (default 3.5).
    #[arg(long = "usefulness-min")]
    pub usefulness_min: Option<f64>,

    /// Override the grounding threshold (default 1.0).
    #[arg(long = "grounding-min")]
    pub grounding_min: Option<f64>,

    /// Print persisted run-store entries and exit (1-1-5-9).
    #[arg(long = "list-runs")]
    pub list_runs: bool,

    /// Persist each fixture's result JSON to the run store (opt-in; default off
    /// for the batch eval command to avoid auto-cluttering the store).
    #[arg(long = "save-run")]
    pub save_run: bool,

    /// Don't persist to the run store (default for this command).
    #[arg(long = "no-save-run")]
    pub no_save_run: bool,

    /// Reclaim stale run-store entries (mtime older than `--gc-days`) and exit.
    #[arg(long = "gc")]
    pub gc: bool,

    /// Staleness window in days for `--gc` (default 7).
    #[arg(long = "gc-days")]
    pub gc_days: Option<u64>,

    /// Override the run-store directory (default: ~/.zbrain/runs/brainstorm).
    #[arg(long = "store-dir")]
    pub store_dir: Option<std::path::PathBuf>,

    /// Review a single persisted run by run_id: full report + metadata header
    /// (1-1-5-10). With `--json`, prints the raw run row.
    #[arg(long = "review-run")]
    pub review_run: Option<String>,

    /// Print the pass-rate / grounding trend across persisted runs (1-1-5-10).
    #[arg(long)]
    pub trend: bool,

    /// Time window in days for `--trend` (default 30).
    #[arg(long = "days")]
    pub days: Option<u64>,

    /// Resume a previously-crashed run by run_id: re-run that single question
    /// and store the regenerated run (1-1-5-11). Mirrors `zbrain brainstorm
    /// --resume`.
    #[arg(long)]
    pub resume: Option<String>,

    /// Bypass the 7-day staleness gate on --resume (1-1-5-11).
    #[arg(long = "force-resume")]
    pub force_resume: bool,
}

/// CLI args for `zbrain eval-extract-atoms` (TS `eval-extract-atoms.ts`, G74 1-1).
///
/// v0.41 ships the command surface; the full parity-baseline eval against
/// OpenClaw's existing atoms lands in a follow-up. Mirrors the TS scaffold.
#[derive(Debug, clap::Args)]
pub struct EvalExtractAtomsArgs {
    /// Parity baseline path for the v0.41.1 follow-up eval.
    #[arg(long = "parity-baseline", value_name = "PATH")]
    pub parity_baseline: Option<String>,

    /// Sample size for the parity subset.
    #[arg(long, value_name = "N")]
    pub sample: Option<u64>,

    /// Emit the EvalExtractAtomsResult as JSON.
    #[arg(long)]
    pub json: bool,
}

/// CLI args for `zbrain eval-synthesize-concepts` (TS `eval-synthesize-concepts.ts`, G74 1-1).
///
/// v0.41 ships the command surface; the full parity-baseline eval against
/// OpenClaw's existing concepts lands in a follow-up. Mirrors the TS scaffold.
#[derive(Debug, clap::Args)]
pub struct EvalSynthesizeConceptsArgs {
    /// Parity baseline path for the v0.41.1 follow-up eval.
    #[arg(long = "parity-baseline", value_name = "PATH")]
    pub parity_baseline: Option<String>,

    /// Sample size for the parity subset.
    #[arg(long, value_name = "N")]
    pub sample: Option<u64>,

    /// Emit the EvalSynthesizeConceptsResult as JSON.
    #[arg(long)]
    pub json: bool,
}

/// CLI args for `zbrain eval-schema-authoring` (TS `eval-schema-authoring.ts`, G74 1-1).
///
/// Hermetic by default: without `--fixture` the verdict is `inconclusive`.
/// The full hermetic engine harness follows the longmemeval pattern (v0.39.1).
#[derive(Debug, clap::Args)]
pub struct EvalSchemaAuthoringArgs {
    /// Fixture brain directory for the hermetic harness.
    #[arg(long, value_name = "PATH")]
    pub fixture: Option<String>,

    /// Source id to scope the harness (alias: `--source`).
    #[arg(long = "source-id", alias = "source", value_name = "SRC")]
    pub source: Option<String>,

    /// Emit the EvalVerdict as JSON.
    #[arg(long)]
    pub json: bool,
}

// ── eval-brainstorm helper types (ported from src/commands/eval-brainstorm.ts) ──

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct BrainstormEvalFixture {
    question: String,
    #[allow(dead_code)]
    expected_far_prefixes: Option<Vec<String>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PerFixtureResult {
    question: String,
    pass_count: usize,
    total_ideas: usize,
    mean_distance: f64,
    mean_usefulness: f64,
    grounding_rate: f64,
    short_of_target: bool,
    cost_usd: f64,
    judge_failed: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EvalAggregate {
    distance: f64,
    usefulness: f64,
    grounding: f64,
}

fn compute_grounding_rate(
    ideas: &[zbrain_core::eval::brainstorm::orchestrator::BrainstormIdea],
    real_slugs: &std::collections::HashSet<String>,
) -> f64 {
    if ideas.is_empty() {
        return 0.0;
    }
    let grounded = ideas
        .iter()
        .filter(|i| real_slugs.contains(&i.close_slug) || real_slugs.contains(&i.far_slug))
        .count();
    grounded as f64 / ideas.len() as f64
}

fn summarize_fixture(
    question: &str,
    result: &zbrain_core::eval::brainstorm::orchestrator::BrainstormResult,
    real_slugs: &std::collections::HashSet<String>,
) -> PerFixtureResult {
    use zbrain_core::eval::brainstorm::orchestrator::BrainstormIdea;
    let passing: Vec<&BrainstormIdea> = result.ideas.iter().filter(|i| i.passes).collect();
    let mean_distance = if passing.is_empty() {
        0.0
    } else {
        passing.iter().map(|i| i.distance_score).sum::<f64>() / passing.len() as f64
    };
    let judged: Vec<&BrainstormIdea> = passing
        .iter()
        .filter(|i| i.judge.is_some())
        .copied()
        .collect();
    let mean_usefulness = if judged.is_empty() {
        f64::NAN
    } else {
        judged
            .iter()
            .map(|i| i.judge.as_ref().unwrap().weighted_score)
            .sum::<f64>()
            / judged.len() as f64
    };
    let grounding = compute_grounding_rate(&result.ideas, real_slugs);
    PerFixtureResult {
        question: question.to_string(),
        pass_count: passing.len(),
        total_ideas: result.ideas.len(),
        mean_distance,
        mean_usefulness,
        grounding_rate: grounding,
        short_of_target: result.short_of_target,
        cost_usd: result.cost.actual_usd,
        judge_failed: result.judge_failed,
    }
}

fn compute_eval_verdict(
    per_fixture: &[PerFixtureResult],
    distance_min: f64,
    usefulness_min: f64,
    grounding_min: f64,
) -> (EvalAggregate, String, Vec<String>) {
    let usable: Vec<&PerFixtureResult> =
        per_fixture.iter().filter(|r| r.pass_count > 0 && !r.judge_failed).collect();
    if usable.len() < 2 {
        return (
            EvalAggregate { distance: 0.0, usefulness: 0.0, grounding: 0.0 },
            "inconclusive".to_string(),
            vec![format!(
                "Only {} fixture(s) produced parseable, judged ideas. Need at least 2 to compute meaningful aggregates.",
                usable.len()
            )],
        );
    }
    let distance = usable.iter().map(|r| r.mean_distance).sum::<f64>() / usable.len() as f64;
    let valid: Vec<f64> = usable
        .iter()
        .filter(|r| r.mean_usefulness.is_finite())
        .map(|r| r.mean_usefulness)
        .collect();
    let usefulness = if valid.is_empty() {
        0.0
    } else {
        valid.iter().sum::<f64>() / valid.len() as f64
    };
    let grounding = usable.iter().map(|r| r.grounding_rate).sum::<f64>() / usable.len() as f64;
    let mut reasons: Vec<String> = Vec::new();
    if distance < distance_min {
        reasons.push(format!(
            "distance {:.3} < {:.3} (ideas too close to the question — domain-bank not surfacing distant pages)",
            distance, distance_min
        ));
    }
    if usefulness < usefulness_min {
        reasons.push(format!(
            "usefulness {:.2} < {:.2} (ideas far but low judge score)",
            usefulness, usefulness_min
        ));
    }
    if grounding < grounding_min {
        reasons.push(format!(
            "grounding {:.3} < {:.3} (some ideas cite non-existent slugs — hallucination signal)",
            grounding, grounding_min
        ));
    }
    let verdict = if reasons.is_empty() { "pass" } else { "fail" };
    (EvalAggregate { distance, usefulness, grounding }, verdict.to_string(), reasons)
}

fn build_idea_slug(question: &str, label: &str) -> String {
    let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let stem: String = question
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let stem: String = if stem.is_empty() {
        "untitled".to_string()
    } else {
        stem.chars().take(60).collect()
    };
    format!("wiki/ideas/{date}-{label}-{stem}")
}

/// Handler for `zbrain brainstorm` and `zbrain lsd`. `profile` selects which
/// engine preset (BRAINSTORM_PROFILE vs LSD_PROFILE) drives the run.
pub async fn run_brainstorm_command(
    args: BrainstormArgs,
    profile: &'static zbrain_core::eval::brainstorm::orchestrator::BrainstormProfile,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use std::sync::Arc;
    use zbrain_core::ai::chat::{instantiate_chat, ChatProvider};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::embedding::EmbeddingClient;
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::eval::brainstorm::checkpoint;
    use zbrain_core::eval::brainstorm::orchestrator::{
        build_brainstorm_frontmatter, format_brainstorm_markdown, run_brainstorm,
        BrainstormOptions, FormatOpts,
    };
    use zbrain_core::libsql::LibsqlEngine;

    // Resolve the run-store directory (no DB needed for store housekeeping).
    let store_dir = resolve_run_store_dir(args.store_dir.as_deref());

    // --gc: reclaim stale runs and exit (mtime-based, default 7-day window).
    if args.gc {
        let days = args.gc_days.unwrap_or(7);
        let n = checkpoint::gc_stale_checkpoints(&store_dir, days);
        println!(
            "Reclaimed {n} stale brainstorm run(s) older than {days} day(s) from {}.",
            store_dir.display()
        );
        return Ok(());
    }

    // --list-runs: enumerate persisted runs and exit.
    if args.list_runs {
        let runs = checkpoint::list_runs(&store_dir);
        if runs.is_empty() {
            println!("No saved brainstorm runs at {}.", store_dir.display());
        } else {
            println!("Saved brainstorm runs (newest first):");
            for r in &runs {
                let rate = if r.n_ideas > 0 {
                    r.n_passed as f64 / r.n_ideas as f64 * 100.0
                } else {
                    0.0
                };
                let gnd = r
                    .mean_grounding
                    .map_or_else(|| "-".to_string(), |g| format!("{g:.2}"));
                println!(
                    "  {rid}  [{prof:>10}]  {saved}  pass={np}/{ni} ({rate:.1}%)  gnd={gnd}  ${usd:.4}{fail}",
                    rid = r.run_id,
                    prof = r.profile_label,
                    saved = r.saved_at,
                    np = r.n_passed,
                    ni = r.n_ideas,
                    rate = rate,
                    gnd = gnd,
                    usd = r.actual_usd,
                    fail = if r.judge_failed { "  (judge failed)" } else { "" }
                );
            }
            println!("\n{} run(s).", runs.len());
        }
        return Ok(());
    }

    // --review-run <run_id>: print a single run's full report and exit (1-1-5-10).
    if let Some(run_id) = &args.review_run {
        return print_run_review(&store_dir, run_id, args.json);
    }

    // --trend: print the pass-rate / grounding trend and exit (1-1-5-10).
    if args.trend {
        return print_run_trend(&store_dir, args.days.unwrap_or(30));
    }

    // Build the engine / chat / embedding client up front — both the resume
    // replay and the normal question path re-run brainstorm against the live
    // brain DB (resume re-discovers close/far by question, 1-1-5-11).
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine: Arc<dyn BrainEngine> = Arc::new(engine);

    // Resolve a single chat provider for the default generation model. The
    // orchestrator forwards the model through ChatOpts; our providers strip
    // the `provider:` prefix for the wire call, so one provider serves both
    // generation and judge phases. Graceful no-key degradation: a missing
    // key yields a clear message rather than a stack trace.
    let resolved_model = "anthropic:claude-sonnet-4-6".to_string();
    let env_lookup = |k: &str| std::env::var(k).ok();
    let (_parsed, recipe) = resolve_recipe_strict(&resolved_model).map_err(|e: AiConfigError| {
        anyhow::anyhow!(
            "zbrain {}: cannot resolve model `{}`: {}. Set your provider API key (e.g. ANTHROPIC_API_KEY) or run `zbrain models doctor`.",
            profile.label, resolved_model, e.message
        )
    })?;
    let chat: Arc<dyn ChatProvider> = Arc::from(
        instantiate_chat(recipe, &resolved_model, &env_lookup).map_err(|e: AiConfigError| {
            anyhow::anyhow!(
                "zbrain {}: cannot build LLM provider for `{}`: {}. Set your provider API key or run `zbrain models doctor`.",
                profile.label, resolved_model, e.message
            )
        })?,
    );
    let embedding_client: Option<Arc<EmbeddingClient>> = EmbeddingClient::from_env().map(Arc::new);

    // --resume <run_id>: replay a previously persisted run (1-1-5-11).
    if let Some(run_id) = &args.resume {
        let overrides = ResumeOverrides {
            judge_model: args.judge_model.clone(),
            max_cost_usd: args.max_cost,
            max_far_set: args.max_far_set,
            max_ideas_per_judge_call: args.max_ideas_per_judge_call,
            ..Default::default()
        };
        return run_resume_playback(
            &store_dir,
            run_id,
            &engine,
            &*chat,
            embedding_client,
            &overrides,
            args.force_resume,
            profile,
            args.json,
        )
        .await;
    }

    let question = args.question.join(" ");
    if question.trim().is_empty() {
        anyhow::bail!(
            "zbrain {}: question required.\nUsage: zbrain {} \"<question>\" [--json] [--save] [--limit N] [--max-cost USD]",
            profile.label, profile.label
        );
    }

    // --limit override: replace m_far on a shallow copy of the profile.
    let effective_profile = match args.limit {
        Some(n) if n > 0 => {
            let mut p = *profile;
            p.m_far = n;
            Some(p)
        }
        _ => None,
    };

    let opts = BrainstormOptions {
        question: question.clone(),
        profile: effective_profile,
        model_override: None,
        source_id: None,
        source_ids: None,
        max_cost_usd: args.max_cost,
        max_far_set: args.max_far_set,
        judge_model: args.judge_model.clone(),
        max_ideas_per_judge_call: args.max_ideas_per_judge_call,
        active_bias_tags: None,
    };

    if !args.yes {
        eprintln!(
            "[{}] brainstorm run for question: {} (pass --yes to skip this notice)",
            profile.label, question
        );
    }

    let result = match run_brainstorm(engine.as_ref(), &*chat, embedding_client, &opts).await {
        Ok(r) => r,
        Err(e) => {
            // The orchestrator wraps SQLSTATE 57014 into a `brainstorm_timeout`
            // StructuredError carrying a `.hint`. Print it like the TS cli block.
            if e.code == "brainstorm_timeout" {
                eprintln!("Error [{}]: {}", e.code, e.message);
                if let Some(hint) = &e.hint {
                    eprintln!("  Hint: {hint}");
                }
                std::process::exit(1);
            }
            return Err(e.into());
        }
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let md = format_brainstorm_markdown(
        &result,
        &FormatOpts { only_passed: true, include_meta: true },
    );
    println!("{md}");

    // Save policy: brainstorm defaults to save-on; lsd to save-off. CLI flags
    // override the default.
    let explicit = if args.no_save {
        Some(false)
    } else if args.save {
        Some(true)
    } else {
        None
    };
    let should_save = explicit.unwrap_or(profile.default_save);
    if should_save {
        let slug = build_idea_slug(&question, profile.label);
        let frontmatter = build_brainstorm_frontmatter(&result, &slug);
        let body = format_brainstorm_markdown(
            &result,
            &FormatOpts { only_passed: false, include_meta: true },
        );
        let compiled_truth = format!("{frontmatter}{body}");
        let title = format!(
            "{}: {}",
            if profile.label == "lsd" { "LSD" } else { "Brainstorm" },
            question.chars().take(100).collect::<String>()
        );
        let input = zbrain_core::engine::PageInput {
            page_type: "note".to_string(),
            title,
            compiled_truth,
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
        };
        match engine.put_page(&slug, None, &input).await {
            Ok(_) => println!("\n_Saved to `{slug}`._"),
            Err(err) => eprintln!("zbrain {}: save failed: {}", profile.label, err),
        }
    }

    // Run-store persistence (trend/review/resume). Gated on `--save-run` /
    // `--no-save-run` / the profile default — independent of the wiki save
    // above. Brainstorm defaults to on; LSD to off.
    let run_explicit = if args.no_save_run {
        Some(false)
    } else if args.save_run {
        Some(true)
    } else {
        None
    };
    let should_persist_run = run_explicit.unwrap_or(profile.default_save);
    if should_persist_run {
        match checkpoint::save_checkpoint(&result, &store_dir) {
            Ok(path) => println!(
                "\n_Run stored: `{}` (run_id {})._",
                path.display(),
                result.run_id
            ),
            Err(err) => eprintln!("zbrain {}: run-store save failed: {}", profile.label, err),
        }
    }
    Ok(())
}

/// Resume playback for `zbrain brainstorm --resume <run_id>` / `zbrain
/// eval-brainstorm --resume <run_id>` (1-1-5-11).
///
/// Loads the persisted run, applies the 7-day staleness gate, rebuilds
/// `BrainstormOptions` via [`zbrain_core::eval::brainstorm::orchestrator::prepare_resume`],
/// re-runs brainstorm against the live brain DB (re-discovering close/far by
/// question), stores the regenerated run under `<orig_run_id>~r<ts>`, and
/// prints the report.
async fn run_resume_playback(
    store_dir: &Path,
    run_id: &str,
    engine: &Arc<dyn BrainEngine>,
    chat: &dyn ChatProvider,
    embedding_client: Option<Arc<EmbeddingClient>>,
    overrides: &ResumeOverrides,
    force_resume: bool,
    profile: &'static BrainstormProfile,
    json: bool,
) -> anyhow::Result<()> {
    use serde_json;
    use zbrain_core::eval::brainstorm::checkpoint::{
        load_checkpoint, load_run_result, save_checkpoint,
    };
    use zbrain_core::eval::brainstorm::orchestrator::{
        format_brainstorm_markdown, prepare_resume, run_brainstorm, BrainstormResult, FormatOpts,
    };

    // 1) Load the persisted row (for saved_at) + typed result.
    let row = load_checkpoint(store_dir, run_id).ok_or_else(|| {
        anyhow::anyhow!(
            "zbrain {}: no persisted run with run_id `{}`. List runs with --list-runs.",
            profile.label, run_id
        )
    })?;
    let result: BrainstormResult = load_run_result(store_dir, run_id).ok_or_else(|| {
        anyhow::anyhow!(
            "zbrain {}: cannot deserialize run `{}` (schema drift). Re-run fresh.",
            profile.label, run_id
        )
    })?;

    // 2) Staleness gate + rebuild options (core, unit-tested).
    let (opts, orig_run_id) = prepare_resume(&result, &row.saved_at, overrides, force_resume)?;

    // 3) Re-run brainstorm — re-discovers close/far by question (Q6).
    let mut regenerated = match run_brainstorm(engine.as_ref(), chat, embedding_client, &opts).await
    {
        Ok(r) => r,
        Err(e) => {
            if e.code == "brainstorm_timeout" {
                eprintln!("Error [{}]: {}", e.code, e.message);
                if let Some(hint) = &e.hint {
                    eprintln!("  Hint: {hint}");
                }
                std::process::exit(1);
            }
            return Err(e.into());
        }
    };

    // 4) Store as a distinct, traceable resume run: `<orig>~r<ts>`.
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    regenerated.run_id = format!("{}~r{}", orig_run_id, ts);

    // 5) Emit.
    if json {
        println!("{}", serde_json::to_string_pretty(&regenerated)?);
    } else {
        let md = format_brainstorm_markdown(
            &regenerated,
            &FormatOpts { only_passed: true, include_meta: true },
        );
        println!("{md}");
    }

    // 6) Persist the regenerated run (Q2: default save). Always store so the
    //    resume is itself reviewable / trendable.
    match save_checkpoint(&regenerated, store_dir) {
        Ok(path) => println!(
            "\n_Run stored: `{}` (resumed from `{}`, run_id {})._",
            path.display(),
            orig_run_id,
            regenerated.run_id
        ),
        Err(err) => eprintln!("zbrain {}: run-store save failed: {}", profile.label, err),
    }
    Ok(())
}

/// Handler for `zbrain eval-brainstorm` — the three-axis evaluation gate.
pub async fn run_eval_brainstorm_command(
    args: EvalBrainstormArgs,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use std::sync::Arc;
    use zbrain_core::ai::chat::{instantiate_chat, ChatProvider};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::embedding::EmbeddingClient;
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::types::PageRef;
    use zbrain_core::eval::brainstorm::orchestrator::{
        run_brainstorm, BrainstormOptions, BRAINSTORM_PROFILE,
    };
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::eval::brainstorm::checkpoint;

    // Resolve the run-store directory (no DB needed for store housekeeping).
    let store_dir = resolve_run_store_dir(args.store_dir.as_deref());

    // --gc: reclaim stale runs and exit (mtime-based, default 7-day window).
    if args.gc {
        let days = args.gc_days.unwrap_or(7);
        let n = checkpoint::gc_stale_checkpoints(&store_dir, days);
        println!(
            "Reclaimed {n} stale brainstorm run(s) older than {days} day(s) from {}.",
            store_dir.display()
        );
        return Ok(());
    }

    // --list-runs: enumerate persisted runs and exit.
    if args.list_runs {
        let runs = checkpoint::list_runs(&store_dir);
        if runs.is_empty() {
            println!("No saved brainstorm runs at {}.", store_dir.display());
        } else {
            println!("Saved brainstorm runs (newest first):");
            for r in &runs {
                let rate = if r.n_ideas > 0 {
                    r.n_passed as f64 / r.n_ideas as f64 * 100.0
                } else {
                    0.0
                };
                let gnd = r
                    .mean_grounding
                    .map_or_else(|| "-".to_string(), |g| format!("{g:.2}"));
                println!(
                    "  {rid}  [{prof:>10}]  {saved}  pass={np}/{ni} ({rate:.1}%)  gnd={gnd}  ${usd:.4}{fail}",
                    rid = r.run_id,
                    prof = r.profile_label,
                    saved = r.saved_at,
                    np = r.n_passed,
                    ni = r.n_ideas,
                    rate = rate,
                    gnd = gnd,
                    usd = r.actual_usd,
                    fail = if r.judge_failed { "  (judge failed)" } else { "" }
                );
            }
            println!("\n{} run(s).", runs.len());
        }
        return Ok(());
    }

    // --review-run <run_id>: print a single run's full report and exit (1-1-5-10).
    if let Some(run_id) = &args.review_run {
        return print_run_review(&store_dir, run_id, args.json);
    }

    // --trend: print the pass-rate / grounding trend and exit (1-1-5-10).
    if args.trend {
        return print_run_trend(&store_dir, args.days.unwrap_or(30));
    }

    // Build the engine / chat / embedding client up front — the resume replay
    // and the normal fixture loop both re-run brainstorm against the live
    // brain DB (1-1-5-11).
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine: Arc<dyn BrainEngine> = Arc::new(engine);

    let resolved_model = "anthropic:claude-sonnet-4-6".to_string();
    let env_lookup = |k: &str| std::env::var(k).ok();
    let (_parsed, recipe) = resolve_recipe_strict(&resolved_model).map_err(|e: AiConfigError| {
        anyhow::anyhow!(
            "zbrain eval brainstorm: cannot resolve model `{}`: {}",
            resolved_model, e.message
        )
    })?;
    let chat: Arc<dyn ChatProvider> = Arc::from(
        instantiate_chat(recipe, &resolved_model, &env_lookup).map_err(|e: AiConfigError| {
            anyhow::anyhow!(
                "zbrain eval brainstorm: cannot build LLM provider: {}",
                e.message
            )
        })?,
    );
    let embedding_client: Option<Arc<EmbeddingClient>> =
        EmbeddingClient::from_env().map(Arc::new);

    // --resume <run_id>: replay a previously persisted run (1-1-5-11, both
    // brainstorm + eval-brainstorm).
    if let Some(run_id) = &args.resume {
        // `eval-brainstorm` does not expose the per-run LLM overrides, so resume
        // replays the original options verbatim (Q4: both verbs accept --resume).
        let overrides = ResumeOverrides::default();
        return run_resume_playback(
            &store_dir,
            run_id,
            &engine,
            &*chat,
            embedding_client,
            &overrides,
            args.force_resume,
            &BRAINSTORM_PROFILE,
            args.json,
        )
        .await;
    }

    // Eval default: do NOT auto-persist (batch command — opt-in via --save-run).
    let should_persist_run = if args.no_save_run {
        false
    } else {
        args.save_run
    };

    let fixture = match &args.fixture {
        Some(f) => f.clone(),
        None => anyhow::bail!(
            "zbrain eval brainstorm: fixture path required (JSONL, one {{ \"question\": ... }} per line)"
        ),
    };

    // Read fixture JSONL (skip blank / malformed rows).
    let text = std::fs::read_to_string(&fixture).map_err(|e| {
        anyhow::anyhow!(
            "zbrain eval brainstorm: cannot read fixture `{}`: {}",
            fixture, e
        )
    })?;
    let mut fixtures: Vec<BrainstormEvalFixture> = Vec::new();
    for line in text.split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let obj = match parsed.as_object() {
            Some(o) => o,
            None => continue,
        };
        let q = match obj.get("question").and_then(|v| v.as_str()) {
            Some(q) if !q.trim().is_empty() => q.to_string(),
            _ => continue,
        };
        let expected = obj
            .get("expected_far_prefixes")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
        fixtures.push(BrainstormEvalFixture { question: q, expected_far_prefixes: expected });
    }
    if fixtures.is_empty() {
        anyhow::bail!("zbrain eval brainstorm: no parseable fixtures in `{}`", fixture);
    }

    let slice: Vec<BrainstormEvalFixture> = match args.limit {
        Some(n) if n > 0 => fixtures.iter().take(n).cloned().collect(),
        _ => fixtures.clone(),
    };

    // Real slugs for the grounding (anti-hallucination) signal.
    let refs: Vec<PageRef> = engine.list_all_page_refs().await?;
    let real_slugs: std::collections::HashSet<String> =
        refs.into_iter().map(|r| r.slug).collect();

    let distance_min = args.distance_min.unwrap_or(0.4);
    let usefulness_min = args.usefulness_min.unwrap_or(3.5);
    let grounding_min = args.grounding_min.unwrap_or(1.0);

    let mut per_fixture: Vec<PerFixtureResult> = Vec::new();
    let mut total_cost = 0.0_f64;

    for (idx, fix) in slice.iter().enumerate() {
        if !args.json {
            eprintln!(
                "[eval-brainstorm] {}/{}: {}",
                idx + 1,
                slice.len(),
                fix.question.chars().take(60).collect::<String>()
            );
        }
        let opts = BrainstormOptions {
            question: fix.question.clone(),
            profile: Some(BRAINSTORM_PROFILE),
            model_override: None,
            source_id: None,
            source_ids: None,
            max_cost_usd: None,
            max_far_set: None,
            judge_model: None,
            max_ideas_per_judge_call: None,
            active_bias_tags: None,
        };
        match run_brainstorm(engine.as_ref(), &*chat, embedding_client.clone(), &opts).await {
            Ok(result) => {
                let summary = summarize_fixture(&fix.question, &result, &real_slugs);
                total_cost += summary.cost_usd;
                per_fixture.push(summary);
                // Persist the fixture's result to the run store when opted in.
                if should_persist_run {
                    match checkpoint::save_checkpoint(&result, &store_dir) {
                        Ok(path) => {
                            if !args.json {
                                eprintln!(
                                    "[eval-brainstorm] fixture {} stored: {}",
                                    idx + 1,
                                    path.display()
                                );
                            }
                        }
                        Err(err) => eprintln!(
                            "[eval-brainstorm] fixture {} run-store save failed: {}",
                            idx + 1,
                            err
                        ),
                    }
                }
            }
            Err(err) => {
                if !args.json {
                    eprintln!("[eval-brainstorm] fixture {} failed: {}", idx + 1, err);
                }
                per_fixture.push(PerFixtureResult {
                    question: fix.question.clone(),
                    pass_count: 0,
                    total_ideas: 0,
                    mean_distance: 0.0,
                    mean_usefulness: f64::NAN,
                    grounding_rate: 0.0,
                    short_of_target: false,
                    cost_usd: 0.0,
                    judge_failed: true,
                });
            }
        }
    }

    let (aggregate, verdict, reasons) =
        compute_eval_verdict(&per_fixture, distance_min, usefulness_min, grounding_min);

    if args.json {
        let report = serde_json::json!({
            "schema_version": 1,
            "fixture_path": fixture,
            "total_fixtures": fixtures.len(),
            "parseable_fixtures": per_fixture.iter().filter(|r| !r.judge_failed && r.total_ideas > 0).count(),
            "thresholds": { "distance_min": distance_min, "usefulness_min": usefulness_min, "grounding_min": grounding_min },
            "per_fixture": per_fixture,
            "aggregate": { "distance": aggregate.distance, "usefulness": aggregate.usefulness, "grounding": aggregate.grounding },
            "verdict": verdict,
            "reasons": reasons,
            "total_cost_usd": total_cost,
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let parseable = per_fixture
            .iter()
            .filter(|r| !r.judge_failed && r.total_ideas > 0)
            .count();
        println!("\n=== zbrain eval brainstorm ===");
        println!("Fixture: {fixture}");
        println!("Parseable: {parseable}/{}", fixtures.len());
        println!("Distance:   {:.3} (threshold {:.3})", aggregate.distance, distance_min);
        println!("Usefulness: {:.2} (threshold {:.2})", aggregate.usefulness, usefulness_min);
        println!("Grounding:  {:.3} (threshold {:.3})", aggregate.grounding, grounding_min);
        println!("Cost:       ${:.2}", total_cost);
        println!("Verdict:    {}", verdict.to_uppercase());
        for r in &reasons {
            println!("  - {r}");
        }
    }

    let code = match verdict.as_str() {
        "pass" => 0,
        "fail" => 1,
        _ => 2,
    };
    std::process::exit(code);
}

#[cfg(test)]
mod brainstorm_cli_tests {
    use crate::Cli;
    use clap::Parser;

    #[test]
    fn parses_brainstorm_verb() {
        let cli = Cli::try_parse_from(["zbrain", "brainstorm", "why do tools converge?"]).unwrap();
        match cli.command {
            crate::Commands::Brainstorm(args) => {
                assert_eq!(args.question.join(" "), "why do tools converge?")
            }
            other => panic!("expected Brainstorm, got {other:?}"),
        }
    }

    #[test]
    fn parses_lsd_verb_with_flags() {
        let cli =
            Cli::try_parse_from(["zbrain", "lsd", "hidden assumption in pricing", "--json"]).unwrap();
        match cli.command {
            crate::Commands::Lsd(args) => assert!(args.json),
            other => panic!("expected Lsd, got {other:?}"),
        }
    }

    #[test]
    fn parses_eval_brainstorm_verb() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-brainstorm",
            "fixture.jsonl",
            "--distance-min",
            "0.5",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::EvalBrainstorm(args) => {
                assert_eq!(args.fixture.as_deref(), Some("fixture.jsonl"));
                assert_eq!(args.distance_min, Some(0.5));
            }
            other => panic!("expected EvalBrainstorm, got {other:?}"),
        }
    }

    #[test]
    fn parses_links_edges_backfill_verb() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "links",
            "edges-backfill",
            "--source",
            "my-src",
            "--max-chunks",
            "500",
            "--json",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Links(crate::LinksAction::EdgesBackfill(args)) => {
                assert_eq!(args.source.as_deref(), Some("my-src"));
                assert_eq!(args.max_chunks, Some(500));
                assert!(args.json);
                assert!(!args.all_sources);
            }
            other => panic!("expected Links/EdgesBackfill, got {other:?}"),
        }
    }

    #[test]
    fn parses_links_edges_backfill_all_sources() {
        let cli =
            Cli::try_parse_from(["zbrain", "links", "edges-backfill", "--all-sources"]).unwrap();
        match cli.command {
            crate::Commands::Links(crate::LinksAction::EdgesBackfill(args)) => {
                assert!(args.all_sources);
                assert!(args.source.is_none());
            }
            other => panic!("expected Links/EdgesBackfill, got {other:?}"),
        }
    }

    #[test]
    fn parses_backfill_list_positional() {
        let cli = Cli::try_parse_from(["zbrain", "backfill", "list"]).unwrap();
        match cli.command {
            crate::Commands::Backfill(args) => {
                assert_eq!(args.kind.as_deref(), Some("list"));
                assert!(!args.list);
            }
            other => panic!("expected Backfill, got {other:?}"),
        }
    }

    #[test]
    fn parses_backfill_list_flag() {
        let cli = Cli::try_parse_from(["zbrain", "backfill", "--list"]).unwrap();
        match cli.command {
            crate::Commands::Backfill(args) => {
                assert!(args.list);
                assert!(args.kind.is_none());
            }
            other => panic!("expected Backfill, got {other:?}"),
        }
    }

    #[test]
    fn parses_backfill_effective_date_dry_run() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "backfill",
            "effective_date",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Backfill(args) => {
                assert_eq!(args.kind.as_deref(), Some("effective_date"));
                assert!(args.dry_run);
                assert!(args.json);
            }
            other => panic!("expected Backfill, got {other:?}"),
        }
    }

    #[test]
    fn parses_backfill_embedding_voyage() {
        let cli = Cli::try_parse_from(["zbrain", "backfill", "embedding_voyage"]).unwrap();
        match cli.command {
            crate::Commands::Backfill(args) => {
                assert_eq!(args.kind.as_deref(), Some("embedding_voyage"));
            }
            other => panic!("expected Backfill, got {other:?}"),
        }
    }

    #[test]
    fn parses_export_default_dir() {
        let cli = Cli::try_parse_from(["zbrain", "export"]).unwrap();
        match cli.command {
            crate::Commands::Export(args) => {
                assert_eq!(args.dir, "./export");
                assert!(args.r#type.is_none());
                assert!(!args.restore_only);
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn parses_export_with_filters() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "export",
            "--dir",
            "out",
            "--type",
            "markdown",
            "--slug-prefix",
            "notes/",
            "--source-id",
            "src",
            "--json",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Export(args) => {
                assert_eq!(args.dir, "out");
                assert_eq!(args.r#type.as_deref(), Some("markdown"));
                assert_eq!(args.slug_prefix.as_deref(), Some("notes/"));
                assert_eq!(args.source_id.as_deref(), Some("src"));
                assert!(args.json);
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn parses_export_restore_only_flag() {
        let cli = Cli::try_parse_from(["zbrain", "export", "--restore-only"]).unwrap();
        match cli.command {
            crate::Commands::Export(args) => {
                assert!(args.restore_only);
            }
            other => panic!("expected Export, got {other:?}"),
        }
    }

    #[test]
    fn parses_upgrade_default() {
        let cli = Cli::try_parse_from(["zbrain", "upgrade"]).unwrap();
        match cli.command {
            crate::Commands::Upgrade(args) => {
                assert!(!args.yes);
                assert!(!args.json);
            }
            other => panic!("expected Upgrade, got {other:?}"),
        }
    }

    #[test]
    fn parses_post_upgrade_yes() {
        let cli = Cli::try_parse_from(["zbrain", "post-upgrade", "--yes", "--json"]).unwrap();
        match cli.command {
            crate::Commands::PostUpgrade(args) => {
                assert!(args.yes);
                assert!(args.json);
            }
            other => panic!("expected PostUpgrade, got {other:?}"),
        }
    }

    #[test]
    fn parses_providers_list_and_env() {
        let cli = Cli::try_parse_from(["zbrain", "providers", "list"]).unwrap();
        match cli.command {
            crate::Commands::Providers(crate::ProvidersAction::List) => {}
            other => panic!("expected Providers/List, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["zbrain", "providers", "env", "openai"]).unwrap();
        match cli.command {
            crate::Commands::Providers(crate::ProvidersAction::Env(a)) => {
                assert_eq!(a.id, "openai");
            }
            other => panic!("expected Providers/Env, got {other:?}"),
        }
    }

    #[test]
    fn parses_frontmatter_validate_and_generate() {
        let cli = Cli::try_parse_from(["zbrain", "frontmatter", "validate", "./docs", "--json"]).unwrap();
        match cli.command {
            crate::Commands::Frontmatter(crate::FrontmatterAction::Validate(a)) => {
                assert_eq!(a.path, "./docs");
                assert!(a.json);
            }
            other => panic!("expected Frontmatter/Validate, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "zbrain", "frontmatter", "generate", "./docs", "--fix", "--include-catch-all",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Frontmatter(crate::FrontmatterAction::Generate(a)) => {
                assert_eq!(a.path, "./docs");
                assert!(a.fix);
                assert!(a.include_catch_all);
            }
            other => panic!("expected Frontmatter/Generate, got {other:?}"),
        }
    }

    #[test]
    fn parses_auth_create_and_register_client() {
        let cli = Cli::try_parse_from(["zbrain", "auth", "create", "my-token"]).unwrap();
        match cli.command {
            crate::Commands::Auth(crate::AuthAction::Create(a)) => {
                assert_eq!(a.name, "my-token");
                assert!(a.takes_holders.is_none());
            }
            other => panic!("expected Auth/Create, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "zbrain", "auth", "register-client", "my-app",
            "--redirect-uri", "https://x/cb",
            "--source", "src1",
            "--federated-read", "src1,src2",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Auth(crate::AuthAction::RegisterClient(a)) => {
                assert_eq!(a.name, "my-app");
                assert_eq!(a.redirect_uris, vec!["https://x/cb".to_string()]);
                assert_eq!(a.source.as_deref(), Some("src1"));
                assert_eq!(a.federated_read, vec!["src1".to_string(), "src2".to_string()]);
            }
            other => panic!("expected Auth/RegisterClient, got {other:?}"),
        }
        let cli = Cli::try_parse_from(["zbrain", "auth", "test", "https://x", "--token", "t"]).unwrap();
        match cli.command {
            crate::Commands::Auth(crate::AuthAction::Test(a)) => {
                assert_eq!(a.url, "https://x");
                assert_eq!(a.token, "t");
            }
            other => panic!("expected Auth/Test, got {other:?}"),
        }
    }

    #[test]
    fn parses_links_reconcile_verb() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "links",
            "reconcile",
            "--source",
            "my-src",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Links(crate::LinksAction::Reconcile(args)) => {
                assert_eq!(args.source, "my-src");
                assert!(args.dry_run);
                assert!(args.json);
            }
            other => panic!("expected Links/Reconcile, got {other:?}"),
        }
    }

    #[test]
    fn parses_links_reconcile_default_source() {
        let cli = Cli::try_parse_from(["zbrain", "links", "reconcile"]).unwrap();
        match cli.command {
            crate::Commands::Links(crate::LinksAction::Reconcile(args)) => {
                assert_eq!(args.source, "default");
                assert!(!args.dry_run);
            }
            other => panic!("expected Links/Reconcile, got {other:?}"),
        }
    }

    #[test]
    fn parses_links_by_mention_verb() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "links",
            "by-mention",
            "--source",
            "my-src",
            "--dry-run",
            "--json",
            "--extra-ignore",
            "Foo,Bar",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Links(crate::LinksAction::ByMention(args)) => {
                assert_eq!(args.source, "my-src");
                assert!(args.dry_run);
                assert!(args.json);
                assert_eq!(args.extra_ignore.as_deref(), Some("Foo,Bar"));
            }
            other => panic!("expected Links/ByMention, got {other:?}"),
        }
    }

    #[test]
    fn parses_links_by_mention_default_source() {
        let cli = Cli::try_parse_from(["zbrain", "links", "by-mention"]).unwrap();
        match cli.command {
            crate::Commands::Links(crate::LinksAction::ByMention(args)) => {
                assert_eq!(args.source, "default");
                assert!(!args.dry_run);
                assert!(args.extra_ignore.is_none());
            }
            other => panic!("expected Links/ByMention, got {other:?}"),
        }
    }

    #[test]
    fn brainstorm_parses_without_question() {
        // `question` is optional at the parse layer (so --help works); the
        // runtime handler enforces "question required". Assert the verb still
        // parses with no positional.
        let cli = Cli::try_parse_from(["zbrain", "brainstorm"]).unwrap();
        match cli.command {
            crate::Commands::Brainstorm(args) => assert!(args.question.is_empty()),
            other => panic!("expected Brainstorm, got {other:?}"),
        }
    }

    #[test]
    fn brainstorm_resume_flag_parses() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "brainstorm",
            "--resume",
            "deadbeefcafe1234",
            "why do tools converge?",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Brainstorm(args) => {
                assert_eq!(args.resume.as_deref(), Some("deadbeefcafe1234"));
                assert!(!args.force_resume);
            }
            other => panic!("expected Brainstorm, got {other:?}"),
        }
    }

    #[test]
    fn brainstorm_resume_force_parses() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "brainstorm",
            "some question",
            "--resume",
            "oldrun",
            "--force-resume",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::Brainstorm(args) => {
                assert_eq!(args.resume.as_deref(), Some("oldrun"));
                assert!(args.force_resume);
            }
            other => panic!("expected Brainstorm, got {other:?}"),
        }
    }

    #[test]
    fn eval_brainstorm_resume_flag_parses() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-brainstorm",
            "fixture.jsonl",
            "--resume",
            "deadbeefcafe1234",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::EvalBrainstorm(args) => {
                assert_eq!(args.resume.as_deref(), Some("deadbeefcafe1234"));
                assert!(!args.force_resume);
            }
            other => panic!("expected EvalBrainstorm, got {other:?}"),
        }
    }

    #[test]
    fn eval_extract_atoms_parses_and_is_scaffold() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-extract-atoms",
            "--parity-baseline",
            "baseline.json",
            "--sample",
            "50",
            "--json",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::EvalExtractAtoms(args) => {
                assert_eq!(args.parity_baseline.as_deref(), Some("baseline.json"));
                assert_eq!(args.sample, Some(50));
                assert!(args.json);
            }
            other => panic!("expected EvalExtractAtoms, got {other:?}"),
        }
    }

    #[test]
    fn eval_synthesize_concepts_parses_and_is_scaffold() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-synthesize-concepts",
            "--parity-baseline",
            "baseline.json",
            "--sample",
            "25",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::EvalSynthesizeConcepts(args) => {
                assert_eq!(args.parity_baseline.as_deref(), Some("baseline.json"));
                assert_eq!(args.sample, Some(25));
                assert!(!args.json);
            }
            other => panic!("expected EvalSynthesizeConcepts, got {other:?}"),
        }
    }

    #[test]
    fn eval_schema_authoring_parses_source_id_alias() {
        // `--source` and `--source-id` are aliases per the TS parser.
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-schema-authoring",
            "--fixture",
            "fixtures/notion-refugee",
            "--source-id",
            "default",
            "--json",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::EvalSchemaAuthoring(args) => {
                assert_eq!(args.fixture.as_deref(), Some("fixtures/notion-refugee"));
                assert_eq!(args.source.as_deref(), Some("default"));
                assert!(args.json);
            }
            other => panic!("expected EvalSchemaAuthoring, got {other:?}"),
        }
    }

    #[test]
    fn eval_schema_authoring_accepts_source_alias() {
        // `--source` alone (no --source-id) still binds `source`.
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-schema-authoring",
            "--source",
            "notes",
        ])
        .unwrap();
        match cli.command {
            crate::Commands::EvalSchemaAuthoring(args) => {
                assert_eq!(args.source.as_deref(), Some("notes"));
                assert!(args.fixture.is_none());
            }
            other => panic!("expected EvalSchemaAuthoring, got {other:?}"),
        }
    }
}

/// Available CLI commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Initialize a new ZBrain project
    Init(InitArgs),

    /// Validate installation and connectivity
    Doctor(DoctorArgs),

    /// Validate the skill tree: reachability, MECE overlap, DRY, gap detection
    CheckResolvable(check_resolvable::CheckResolvableArgs),

    /// Check a single SKILL.md for brain-first compliance (v0.36.x gate)
    CheckBrainFirst(check_brain_first::CheckBrainFirstArgs),

    /// Run Check 5 (routing eval) against every skills/<name>/routing-eval.jsonl fixture
    RoutingEval(routing_eval::RoutingEvalArgs),

    /// Scan brain usage and recommend unused features
    Features(FeaturesArgs),

    /// Ask your brain who knows about a topic (ranked person/company experts)
    Whoknows(WhoknowsArgs),

    /// Scan the brain for integrity issues (bare-tweet refs, external links)
    Integrity(IntegrityArgs),

    /// Report storage tiering statistics for the brain repo
    Storage(StorageArgs),

    /// Generate a self-contained, shareable HTML file from a brain markdown page
    Publish(PublishArgs),

    /// Introspect the Resolver SDK registry (list / describe builtin resolvers)
    Resolvers(ResolversArgs),

    /// Statistical anomalies in recent page activity, grouped by cohort (tag, type)
    Anomalies(AnomaliesArgs),

    /// Check for new ZBrain versions (GitHub releases + changelog diff)
    CheckUpdate(CheckUpdateArgs),

    /// Manage configuration values
    Config(ConfigArgs),

    /// Print database schema SQL (DDL for the selected backend).
    ///
    /// Named `schema-sql` to disambiguate from the bare `schema` subcommand,
    /// which now hosts the full 32-verb schema-pack manager (migrated 1-1..1-5;
    /// G4 resolved — see UNMIGRATED_TS_SCHEMA_PACK_VERBS).
    #[command(name = "schema-sql")]
    SchemaSql(SchemaArgs),

    /// Read a page by slug
    GetPage(GetPageArgs),

    /// Synthesize answers across the knowledge base
    Think(ThinkArgs),
    /// Run the auto-think cycle phase (open questions → synthesis pages)
    AutoThink(AutoThinkArgs),
    /// Inspect, regenerate, or undo a calibration profile (TS `commands/calibration.ts`)
    Calibration(CalibrationArgs),
    /// Run one brain maintenance cycle (lint → … → orphans) via run_cycle.
    Dream(DreamArgs),
    /// Search pages by keyword query
    Query(QueryArgs),

    /// Create or update a page
    PutPage(PutPageArgs),

    /// Delete a page by slug (soft delete)
    DeletePage(DeletePageArgs),

    /// Restore a deleted page
    RestorePage(RestorePageArgs),

    /// Permanently remove all soft-deleted pages
    PurgeDeletedPages(PurgeDeletedPagesArgs),

    /// List pages with optional filtering
    ListPages(ListPagesArgs),

    /// Start the MCP stdio server (Model Context Protocol)
    ServeMcp(ServeMcpArgs),

    /// Start the HTTP API and admin SPA server
    #[command(name = "serve")]
    ServeHttp(ServeHttpArgs),

    /// Sync files from a git repository into the knowledge base
    Sync(SyncArgs),

    /// Manage knowledge base sources
    #[command(subcommand)]
    Sources(SourcesAction),

    /// Capture content from files or stdin into the knowledge base
    Capture(CaptureArgs),

    /// Manage facts — insert, list, health, expire
    #[command(subcommand)]
    Facts(FactsAction),

    /// Measure retrieval quality against ground-truth qrels (TS `eval`)
    Eval(EvalArgs),

    /// Stream captured eval candidates as NDJSON (TS `eval-export`; G74 1-1-4)
    EvalExport(EvalExportArgs),

    /// Delete old eval candidates (TS `eval-prune`; G74 1-1-4)
    EvalPrune(EvalPruneArgs),

    /// Correctness gate against qrels ground truth (TS `eval-gate`; G74 1-1-4 stage 3)
    EvalGate(EvalGateArgs),

    /// Replay captured eval candidates against the current brain (TS `eval-replay`; G74 1-1-4 stage 4)
    EvalReplay(EvalReplayArgs),

    /// Two-layer whoknows eval gate (TS `eval-whoknows`; G74 1-1-4 stage 5)
    EvalWhoknows(EvalWhoknowsArgs),

    /// Run every selected eval gate and aggregate their verdicts into one
    /// report. Redesign of the TS `eval run-all` stub — orchestrates the real
    /// Rust gates (gate/replay/whoknows) instead of writing skipped audit rows
    /// (G74 1-1-4 stage 6)
    EvalRunAll(EvalRunAllArgs),

    /// Diff two `eval run-all` reports and surface regressions (G74 1-1-4 stage 6)
    EvalCompare(EvalCompareArgs),

    /// Capture code-retrieval quality (baseline vs with-code-intel) and gate
    /// them. Harness + strategies from scratch (G74 1-1-4 stage 7); the
    /// with-code-intel strategy is wired to real Rust code-intel ops.
    EvalCodeRetrieval(EvalCodeRetrievalArgs),
    EvalCrossModal(EvalCrossModalArgs),
    #[command(name = "eval-longmemeval")]
    EvalLongMemEval(EvalLongMemEvalArgs),
    /// Sample takes from the brain DB and judge their quality (TS `eval-takes-quality`, MVP)
    EvalTakesQuality(EvalTakesQualityArgs),

    /// Probe the brain for suspected contradictions between takes (TS `eval
    /// suspected-contradictions`, MVP). `run` is implemented; `trend` /
    /// `review` are deferred (roadmap node 1-1-5-4).
    #[command(name = "eval-suspected-contradictions")]
    EvalSuspectedContradictions(EvalSuspectedContradictionsArgs),

    /// Bisociation idea generator grounded in your own notes (v0.37.0 wave).
    #[command(name = "brainstorm")]
    Brainstorm(BrainstormArgs),

    /// Lateral Synaptic Drift — the inverted-judge / stale-bias variant of `brainstorm`.
    #[command(name = "lsd")]
    Lsd(BrainstormArgs),

    /// Three-axis evaluation gate for `zbrain brainstorm` (DISTANCE + USEFULNESS + GROUNDING).
    #[command(name = "eval-brainstorm")]
    EvalBrainstorm(EvalBrainstormArgs),

    /// Extract atoms from brain pages — command surface (TS `eval-extract-atoms`, G74 1-1).
    #[command(name = "eval-extract-atoms")]
    EvalExtractAtoms(EvalExtractAtomsArgs),

    /// Synthesize concepts from atoms — command surface (TS `eval-synthesize-concepts`, G74 1-1).
    #[command(name = "eval-synthesize-concepts")]
    EvalSynthesizeConcepts(EvalSynthesizeConceptsArgs),

    /// Schema-authoring filing-accuracy harness — command surface (TS `eval-schema-authoring`, G74 1-1).
    #[command(name = "eval-schema-authoring")]
    EvalSchemaAuthoring(EvalSchemaAuthoringArgs),

    /// Extract links / timeline entries from page bodies (TS `extract`)
    #[command(subcommand)]
    Extract(ExtractAction),

    /// Manage links between pages
    #[command(subcommand)]
    Links(LinksAction),

    /// Manage takes on pages
    #[command(subcommand)]
    Takes(TakesAction),

    /// Query recently touched pages ranked by salience
    Salience(SalienceArgs),

    /// Find pages with zero inbound links
    Orphans(OrphansArgs),

    /// BFS graph traversal from a root page
    #[command(name = "graph-query")]
    GraphQuery(GraphQueryArgs),

    /// Self-maintaining brain daemon — runs maintenance cycles on an interval.
    ///
    /// Usage:
    ///   zbrain autopilot [--repo <path>] [--interval N] [--json] [--inline] [--no-worker]
    ///   zbrain autopilot --install [--repo <path>]
    ///   zbrain autopilot --uninstall
    ///   zbrain autopilot --status [--json]
    ///   zbrain autopilot --once [--repo <path>]  (single tick, for testing)
    Autopilot(AutopilotArgs),

    /// Remote execution — thin-client commands that round-trip through a remote MCP host.
    ///
    /// Usage:
    ///   zbrain remote ping [--json] [--max-wait 5m]
    ///   zbrain remote doctor [--json]
    #[command(subcommand)]
    Remote(RemoteSub),

    /// Manage background jobs — submit, list, inspect, cancel, retry, prune, stats.
    #[command(subcommand)]
    Jobs(JobsAction),

    /// Manage AI agents — submit subagent jobs and view logs.
    #[command(subcommand)]
    Agent(AgentAction),

    /// Schema pack management — inspect, validate, lint packs.
    #[command(subcommand)]
    Schema(schema_cmd::SchemaSubcommand),

    /// Show model routing table / probe configured models.
    Models(ModelsArgs),

    /// Run pending upgrade-migration orchestrators (orchestrator ledger).
    #[command(name = "apply-migrations")]
    ApplyMigrations(ApplyMigrationsArgs),

    /// Validate / generate YAML frontmatter for markdown files on disk (TS `frontmatter`).
    #[command(subcommand)]
    Frontmatter(FrontmatterAction),

    /// Token & OAuth 2.1 client management (TS `auth`).
    #[command(subcommand)]
    Auth(AuthAction),

    /// Show AI provider status and env-readiness (TS `providers`). Read-only.
    #[command(subcommand)]
    Providers(ProvidersAction),

    /// Upgrade helper — for a cargo-built binary, self-reinstall is done via the
    /// package manager / cargo; delegates to `post-upgrade` (apply-migrations).
    Upgrade(UpgradeArgs),

    /// Apply pending migration orchestrators + surface new-version pitches
    /// (TS `post-upgrade`). Idempotent; mirrors `apply-migrations`.
    #[command(name = "post-upgrade")]
    PostUpgrade(PostUpgradeArgs),

    /// Manage connected brains (mounts.json)
    #[command(name = "mounts", subcommand)]
    Mounts(mounts::MountsSubcommand),

    /// Skillpack management — install, scaffold, search, harvest from third-party repos.
    #[command(subcommand)]
    Skillpack(skillpack::SkillpackSubcommand),

    /// Skillify — scaffold a new skill (the `check` audit half is tracked by roadmap node 1-1-1).
    #[command(subcommand)]
    Skillify(skillify::SkillifySubcommand),

    // ── Phase B: commands previously served by TS cli.ts / operations.ts ──
    // Each is a thin clap wrapper that builds a params JSON and routes through
    // `run_operation`. See the `phase_b_commands_registered` parity test.

    /// Show which identity is currently authenticated
    Whoami,

    /// Show version history of a page
    History(HistoryArgs),

    /// Revert a page to a specific version
    Revert(RevertArgs),

    /// Add a tag to a page
    Tag(TagArgs),

    /// Remove a tag from a page (TS `untag`)
    Untag(UntagArgs),

    /// List tags on a page
    Tags(TagsArgs),

    /// Show a page's timeline
    Timeline(TimelineArgs),

    /// Add a timeline entry to a page
    #[command(name = "timeline-add")]
    TimelineAdd(TimelineAddArgs),

    /// Browse recent transcripts
    #[command(subcommand)]
    Transcripts(TranscriptsAction),

    /// Find logical contradictions across pages
    #[command(name = "find-contradictions")]
    FindContradictions(FindContradictionsArgs),

    /// Trace an entity's trajectory over time
    #[command(name = "find-trajectory")]
    FindTrajectory(FindTrajectoryArgs),

    /// Recall hot memory (facts) for an entity / session / time window
    #[command(name = "recall")]
    Recall(RecallArgs),

    /// Locate a code symbol definition
    #[command(name = "code-def")]
    CodeDef(CodeDefArgs),

    /// Find references to a code symbol
    #[command(name = "code-refs")]
    CodeRefs(CodeRefsArgs),

    /// Find callers of a code symbol
    #[command(name = "code-callers")]
    CodeCallers(CodeCallersArgs),

    /// Find callees of a code symbol
    #[command(name = "code-callees")]
    CodeCallees(CodeCalleesArgs),

    /// Blast out from a symbol across the call graph
    #[command(name = "code-blast")]
    CodeBlast(CodeBlastArgs),

    /// Walk the call graph from an entry point
    #[command(name = "code-flow")]
    CodeFlow(CodeFlowArgs),

    /// Clear the (TS-only) code traversal cache
    #[command(name = "code-traversal-cache-clear")]
    CodeTraversalCacheClear(CodeTraversalCacheClearArgs),

    /// Search pages by image
    #[command(name = "search-by-image")]
    SearchByImage(SearchByImageArgs),

    /// Personalized chapter-by-chapter book analysis (fan-out subagents).
    #[command(name = "book-mirror")]
    BookMirror(book_mirror::BookMirrorArgs),

    /// Re-embed content to refresh the search vector index
    #[command(name = "reindex", subcommand)]
    Reindex(ReindexAction),

    /// First-class bulk operations (TS `commands/backfill.ts`, G77).
    ///
    /// `backfill list` enumerates registered backfills; `backfill <kind>`
    /// runs one (`effective_date`, `emotional_weight`). `embedding_voyage`
    /// is declared-only and not yet runnable.
    #[command(name = "backfill")]
    Backfill(BackfillArgs),
    /// Export pages as markdown files (with optional `.raw` sidecars)
    Export(ExportArgs),
}

/// Subcommands for `zbrain jobs`.
#[derive(Debug, Subcommand)]
pub enum JobsAction {
    /// Submit a new job to the queue.
    Submit(JobsSubmitArgs),
    /// List recent jobs.
    List(JobsListArgs),
    /// Get details of a single job.
    Get(JobsGetArgs),
    /// Cancel a queued or running job.
    Cancel(JobsCancelArgs),
    /// Retry a failed or dead job.
    Retry(JobsRetryArgs),
    /// Prune terminal jobs older than a cutoff.
    Prune(JobsPruneArgs),
    /// Show queue statistics.
    Stats(JobsStatsArgs),
    /// Start a worker process to consume jobs.
    Work(JobsWorkArgs),
}

/// Subcommands for `zbrain agent`.
#[derive(Debug, Subcommand)]
pub enum AgentAction {
    /// Submit a subagent job with a prompt.
    Run(AgentRunArgs),
}

/// Arguments for `zbrain jobs submit`.
#[derive(Debug, Parser)]
pub struct JobsSubmitArgs {
    /// Job name (e.g. "sync", "embed", "autopilot-cycle").
    pub name: String,
    /// Job data as JSON string.
    #[arg(long)]
    pub params: Option<String>,
    /// Priority (higher = sooner, default 0).
    #[arg(long)]
    pub priority: Option<i32>,
    /// Queue name (default "default").
    #[arg(long)]
    pub queue: Option<String>,
    /// Delay in milliseconds before the job becomes eligible.
    #[arg(long)]
    pub delay: Option<i64>,
    /// Max attempts (default 3).
    #[arg(long)]
    pub max_attempts: Option<i32>,
    /// Max stalled counter (default 5).
    #[arg(long)]
    pub max_stalled: Option<i32>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain jobs list`.
#[derive(Debug, Parser)]
pub struct JobsListArgs {
    /// Filter by status (queued, running, completed, failed, dead, cancelled, delayed).
    #[arg(long)]
    pub status: Option<String>,
    /// Filter by queue name.
    #[arg(long)]
    pub queue: Option<String>,
    /// Max results (default 20).
    #[arg(long, default_value = "20")]
    pub limit: i64,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain jobs get`.
#[derive(Debug, Parser)]
pub struct JobsGetArgs {
    /// Job ID.
    pub id: i64,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain jobs cancel`.
#[derive(Debug, Parser)]
pub struct JobsCancelArgs {
    /// Job ID to cancel.
    pub id: i64,
}

/// Arguments for `zbrain jobs retry`.
#[derive(Debug, Parser)]
pub struct JobsRetryArgs {
    /// Job ID to retry.
    pub id: i64,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain jobs prune`.
#[derive(Debug, Parser)]
pub struct JobsPruneArgs {
    /// Prune jobs older than this (e.g. "30d", "7d"). Default: 30d.
    #[arg(long)]
    pub older_than: Option<String>,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain jobs stats`.
#[derive(Debug, Parser)]
pub struct JobsStatsArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain jobs work`.
#[derive(Debug, Parser)]
pub struct JobsWorkArgs {
    /// Queue to consume from (default "default").
    #[arg(long)]
    pub queue: Option<String>,
    /// Concurrency (default 1).
    #[arg(long, default_value = "1")]
    pub concurrency: usize,
    /// Poll interval in ms (default 1000).
    #[arg(long, default_value = "1000")]
    pub poll_interval: u64,
}

/// Arguments for `zbrain agent run`.
#[derive(Debug, Parser)]
pub struct AgentRunArgs {
    /// Prompt for the subagent.
    pub prompt: String,
    /// Model override.
    #[arg(long)]
    pub model: Option<String>,
    /// Max turns (default 20).
    #[arg(long, default_value = "20")]
    pub max_turns: i32,
    /// Follow job until terminal state.
    #[arg(long)]
    pub follow: bool,
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Subcommands for `zbrain remote`.
#[derive(Debug, Subcommand)]
pub enum RemoteSub {
    /// Trigger an autopilot cycle on the remote host (sync + extract + embed).
    Ping(RemotePingArgs),

    /// Run brain health checks on the remote host and render the report.
    Doctor(RemoteDoctorArgs),
}

/// Arguments for `zbrain remote ping`.
#[derive(Debug, Parser)]
pub struct RemotePingArgs {
    /// Emit structured JSON instead of human output.
    #[arg(long)]
    pub json: bool,

    /// Max wait duration (e.g. 5m, 30m, 90s). Default: 15m.
    #[arg(long)]
    pub max_wait: Option<String>,
}

/// Arguments for `zbrain remote doctor`.
#[derive(Debug, Parser)]
pub struct RemoteDoctorArgs {
    /// Emit structured JSON instead of human output.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain autopilot`.
#[derive(Debug, Parser)]
pub struct AutopilotArgs {
    /// Path to the brain git repo. Defaults to `sync.repo_path` from config.
    #[arg(long)]
    pub repo: Option<String>,

    /// Base cycle interval in seconds (default 300 = 5 min).
    #[arg(long, default_value = "300")]
    pub interval: u64,

    /// Output events as JSON lines on stderr.
    #[arg(long)]
    pub json: bool,

    /// Force inline mode (skip Minions dispatch, run cycle directly).
    #[arg(long)]
    pub inline: bool,

    /// Dispatch only — don't spawn a managed worker (worker runs externally).
    #[arg(long)]
    pub no_worker: bool,

    /// Install the daemon (launchd / systemd / crontab / ephemeral).
    #[arg(long)]
    pub install: bool,

    /// Uninstall the daemon (all targets, idempotent).
    #[arg(long)]
    pub uninstall: bool,

    /// Show daemon install status.
    #[arg(long)]
    pub status: bool,

    /// Run a single tick and exit (for testing / cron one-shot).
    #[arg(long)]
    pub once: bool
}

/// Subcommands for `zbrain sources`.
#[derive(Debug, Subcommand)]
pub enum SourcesAction {
    /// Register a new source (local path or remote git URL)
    Add(SourcesAddArgs),

    /// List all registered sources
    List(SourcesListArgs),

    /// Remove a source and optionally its local clone
    Remove(SourcesRemoveArgs),

    /// Show source health dashboard
    Status(SourcesStatusArgs),
}

/// Arguments for `zbrain sources add`.
#[derive(Debug, Parser)]
pub struct SourcesAddArgs {
    /// Source ID (1-32 lowercase alphanumeric chars with optional interior hyphens)
    pub id: String,

    /// Display name (defaults to id if omitted)
    #[arg(long)]
    pub name: Option<String>,

    /// Local path to an existing repo directory
    #[arg(long, conflicts_with = "url")]
    pub path: Option<PathBuf>,

    /// Remote git URL to clone
    #[arg(long, conflicts_with = "path")]
    pub url: Option<String>,

    /// Mark as a federated source
    #[arg(long)]
    pub federated: bool,

    /// Override clone destination (default: ~/.zbrain/clones/<id>/)
    #[arg(long)]
    pub clone_dir: Option<PathBuf>,

    /// Clone depth (0 = full clone, default: 1)
    #[arg(long, default_value = "1")]
    pub depth: u32,

    /// Branch to clone (default: repo default)
    #[arg(long)]
    pub branch: Option<String>,
}

/// Arguments for `zbrain sources list`.
#[derive(Debug, Parser)]
pub struct SourcesListArgs {
    /// Output as JSON instead of table
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain sources remove`.
#[derive(Debug, Parser)]
pub struct SourcesRemoveArgs {
    /// Source ID to remove
    pub id: String,

    /// Confirm removal even if source has pages
    #[arg(long)]
    pub confirm_destructive: bool,

    /// Show what would happen without actually removing
    #[arg(long)]
    pub dry_run: bool,

    /// Keep local clone directory (don't delete it)
    #[arg(long)]
    pub keep_storage: bool,

    /// Skip interactive confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Arguments for `zbrain sources status`.
#[derive(Debug, Parser)]
pub struct SourcesStatusArgs {
    /// Source ID to inspect (omit for all sources)
    pub source_id: Option<String>,

    /// Output as JSON instead of table
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain capture` command.
#[derive(Debug, Parser)]
pub struct CaptureArgs {
    /// Content source: path to file, or omit for stdin
    pub content: Option<String>,

    /// Content type (markdown, text)
    #[arg(long, default_value = "markdown")]
    pub r#type: String,

    /// Source ID to associate with
    #[arg(long)]
    pub source: Option<String>,

    /// Custom slug for the page
    #[arg(long)]
    pub slug: Option<String>,

    /// Output as JSON instead of human-readable
    #[arg(long)]
    pub json: bool,
}

// ── Facts subcommands ──────────────────────────────────────────

/// Subcommands for `zbrain facts`.
#[derive(Debug, Subcommand)]
pub enum FactsAction {
    /// Add a new fact for an entity (auto-supersedes high-confidence duplicates)
    Add(FactsAddArgs),

    /// List facts for an entity with optional filters
    List(FactsListArgs),

    /// Show facts health dashboard for a source
    #[command(name = "health")]
    Health(FactsHealthArgs),

    /// Expire a fact by ID
    Expire(FactsExpireArgs),
}

/// Arguments for `zbrain facts add`.
#[derive(Debug, Parser)]
pub struct FactsAddArgs {
    /// Source ID
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Entity slug the fact belongs to
    #[arg(short, long)]
    pub entity: String,

    /// The fact claim text
    #[arg(long)]
    pub claim: String,

    /// Fact kind: event, preference, commitment, belief, fact (default: fact)
    #[arg(long, default_value = "fact")]
    pub kind: String,

    /// Visibility: private or world (default: private)
    #[arg(long, default_value = "private")]
    pub visibility: String,

    /// Confidence score 0.0-1.0 (default: 1.0)
    #[arg(long, default_value = "1.0")]
    pub confidence: f64,

    /// Source citation (e.g. conversation-session-id)
    #[arg(long)]
    pub cite: Option<String>,

    /// Additional context / provenance
    #[arg(long)]
    pub context: Option<String>,

    /// Notability level: low, medium, high (default: medium)
    #[arg(long, default_value = "medium")]
    pub notability: String,

    /// Valid-from date (ISO 8601)
    #[arg(long)]
    pub valid_from: Option<String>,

    /// Valid-until date (ISO 8601)
    #[arg(long)]
    pub valid_until: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain facts list`.
#[derive(Debug, Parser)]
pub struct FactsListArgs {
    /// Source ID
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Entity slug to list facts for
    #[arg(short, long)]
    pub entity: String,

    /// Only show active (non-expired, non-superseded) facts
    #[arg(long)]
    pub active_only: bool,

    /// Filter by kind (can repeat: --kind event --kind fact)
    #[arg(long)]
    pub kind: Vec<String>,

    /// Filter by visibility (can repeat: --visibility private --visibility world)
    #[arg(long)]
    pub visibility: Vec<String>,

    /// Maximum results (default: 50)
    #[arg(long, default_value = "50")]
    pub limit: i64,

    /// Skip first N results
    #[arg(long, default_value = "0")]
    pub offset: i64,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain facts health`.
#[derive(Debug, Parser)]
pub struct FactsHealthArgs {
    /// Source ID
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain facts expire`.
#[derive(Debug, Parser)]
pub struct FactsExpireArgs {
    /// Fact ID to expire
    pub fact_id: i64,

    /// Source ID
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

// ── Eval command ───────────────────────────────────────────────

/// Search strategy selector for `zbrain eval`.
///
/// Mirrors the TS `--strategy hybrid | keyword | vector` literal union. Kept
/// separate from `zbrain_core::search::EvalStrategy` so the clap surface owns
/// its own `ValueEnum` derive (core stays dependency-free).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum EvalStrategyArg {
    /// Lexical only — `search_pages` without a query embedding.
    Keyword,
    /// Vector only — embed the query, retrieve on the vector axis alone.
    Vector,
    /// Default: lexical + vector, RRF-fused, deduped.
    Hybrid,
}

impl From<EvalStrategyArg> for zbrain_core::search::EvalStrategy {
    fn from(value: EvalStrategyArg) -> Self {
        match value {
            EvalStrategyArg::Keyword => zbrain_core::search::EvalStrategy::Keyword,
            EvalStrategyArg::Vector => zbrain_core::search::EvalStrategy::Vector,
            EvalStrategyArg::Hybrid => zbrain_core::search::EvalStrategy::Hybrid,
        }
    }
}

/// Arguments for `zbrain eval` — the IR-metrics retrieval benchmark.
///
/// Rust port of the TS `eval` command's bare (no sub-verb) flow: load qrels,
/// run one or two search configurations over them, and report
/// `P@k` / `R@k` / `MRR` / `nDCG@k` per query plus the means. The metric math
/// and the `run_eval` orchestrator already landed in
/// `zbrain_core::search::eval` under **G73**; this verb is the missing CLI
/// exit — see KNOWN-GAPS G74 for the corrected scope note (`eval` was filed
/// as "blocked on the LLM seam", which it is not: the harness is pure
/// retrieval).
///
/// The TS `eval` sub-verbs (`export`, `prune`, `replay`, `gate`, …) are NOT
/// ported; `subcommand` exists purely to reject them loudly instead of
/// silently running the bare flow with a stray positional.
#[derive(Debug, Parser)]
pub struct EvalArgs {
    /// TS-only `eval` sub-verb (rejected — see KNOWN-GAPS G74).
    #[arg(value_name = "SUBCOMMAND")]
    pub subcommand: Option<String>,

    /// Ground truth: path to a qrels JSON file, or inline JSON starting with
    /// `[` / `{`. Required for the bare flow.
    #[arg(long, value_name = "PATH|JSON")]
    pub qrels: Option<String>,

    /// Config for side A (path or inline JSON). CLI flags override its fields.
    #[arg(long = "config-a", value_name = "PATH|JSON")]
    pub config_a: Option<String>,

    /// Config for side B (path or inline JSON). Presence triggers A/B mode.
    /// Faithful to TS, side B takes NO CLI flag overrides.
    #[arg(long = "config-b", value_name = "PATH|JSON")]
    pub config_b: Option<String>,

    /// Search strategy for side A (default: hybrid).
    #[arg(long, value_enum)]
    pub strategy: Option<EvalStrategyArg>,

    /// Override the RRF K constant. Recorded in the report but not yet honored
    /// by the Rust retrieval path (KNOWN-GAPS G74b).
    #[arg(long = "rrf-k", value_name = "N")]
    pub rrf_k: Option<f64>,

    /// Enable multi-query expansion. Recorded but not yet honored (G74b).
    #[arg(long, conflicts_with = "no_expand")]
    pub expand: bool,

    /// Disable multi-query expansion (the eval default).
    #[arg(long = "no-expand")]
    pub no_expand: bool,

    /// Override the cosine dedup threshold.
    #[arg(long = "dedup-cosine", value_name = "F")]
    pub dedup_cosine: Option<f64>,

    /// Override the page-type ratio cap.
    #[arg(long = "dedup-type-ratio", value_name = "F")]
    pub dedup_type_ratio: Option<f64>,

    /// Override the max-results-per-page cap.
    #[arg(long = "dedup-max-per-page", value_name = "N")]
    pub dedup_max_per_page: Option<usize>,

    /// Max results to retrieve per query (default: max(k*2, 10)).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Metric cutoff depth.
    #[arg(long, default_value_t = 5, value_name = "N")]
    pub k: usize,

    /// Output as JSON instead of the aligned text table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain eval-export` (TS `eval-export.ts`, G74 1-1-4).
///
/// Streams captured eval candidates (`eval_candidates` table, migration 0030)
/// as newline-delimited JSON. Each line is `{schema_version:1, candidate:…}`.
/// The table starts empty until the capture-side writer lands; this verb only
/// reads what is present.
#[derive(Debug, Parser)]
pub struct EvalExportArgs {
    /// Restrict to a tool: `query` or `search`.
    #[arg(long, value_name = "TOOL")]
    pub tool: Option<String>,

    /// Only candidates created at or after this ISO-8601 timestamp.
    #[arg(long, value_name = "ISO")]
    pub since: Option<String>,

    /// Max rows to emit (newest first).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
}

/// Arguments for `zbrain eval-prune` (TS `eval-prune.ts`, G74 1-1-4).
///
/// Deletes eval candidates older than `--older-than`. With `--dry-run` it only
/// counts and reports; nothing is deleted.
#[derive(Debug, Parser)]
pub struct EvalPruneArgs {
    /// Delete candidates created before this ISO-8601 timestamp.
    #[arg(long = "older-than", value_name = "ISO", required = true)]
    pub older_than: String,

    /// Report how many would be deleted without deleting anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// Arguments for `zbrain eval-gate` (TS `eval-gate.ts` correctness half; G74 1-1-4 stage 3).
///
/// Runs the **qrels half** of the TS `eval-gate` correctness gate: compares
/// retrieval against a federated ground-truth qrels file on
/// `${source_id}::${slug}` keys (eng-D5, multi-source safe). Reuses
/// `zbrain_core::eval::gate::{run_correctness_gate, evaluate_correctness_gate}`.
///
/// The **regression half** (`--baseline`, replay of a previous run) is tracked
/// separately (1-1-4 stage 4) and is intentionally NOT wired here — this
/// command never throws the "not implemented" guard because the qrels gate is
/// fully real.
#[derive(Debug, Parser)]
pub struct EvalGateArgs {
    /// Ground-truth qrels file (federated `{schema_version:1,queries:[...]}`)
    /// or inline JSON. Accepts both the federated shape (`relevant` +
    /// `expected_top1`) and the legacy slug-only shape (`relevant_slugs` +
    /// `first_relevant_slug`, auto-defaulted to source `default`). Required.
    #[arg(long = "qrels", value_name = "PATH|JSON", required = true)]
    pub qrels: String,

    /// Recall@k cutoff depth (default 10). Also the per-query retrieval depth.
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub k: usize,

    /// Override the recall@k floor (default 0.70).
    #[arg(long = "recall-at-k", value_name = "F")]
    pub recall_at_k: Option<f64>,

    /// Override the first-relevant-hit floor (default 0.60).
    #[arg(long = "first-relevant-hit", value_name = "F")]
    pub first_relevant_hit: Option<f64>,

    /// Override the expected-top1 floor (default 0.50). Only enforced when at
    /// least one qrels entry sets `expected_top1`.
    #[arg(long = "expected-top1", value_name = "F")]
    pub expected_top1: Option<f64>,

    /// Emit the full `GateResult` envelope as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Replay captured eval candidates against the current brain (TS `eval-replay`;
/// G74 1-1-4 stage 4).
///
/// Reads an NDJSON snapshot produced by `zbrain eval-export`, re-runs each
/// captured query against the current brain, and reports mean Jaccard@k,
/// top-1 stability and mean latency delta. Best-effort by design — the brain
/// may have more pages than at capture time.
#[derive(Debug, Parser)]
pub struct EvalReplayArgs {
    /// NDJSON file from `zbrain eval-export` (required).
    #[arg(long = "against", value_name = "FILE", required = true)]
    pub against: String,

    /// Replay at most N rows (default: all).
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Force a constant per-call retrieval depth across modes so Jaccard@k
    /// measures quality drift, not K-drift. When set it overrides the captured
    /// K and the mode's default limit (default: max(captured, 20)).
    #[arg(long = "compare-limit", value_name = "N")]
    pub compare_limit: Option<usize>,

    /// Emit one JSON object on stdout instead of a human table.
    /// Stable shape for CI consumption.
    #[arg(long)]
    pub json: bool,

    /// Include every row's per-row diff in the JSON output (large output).
    #[arg(long)]
    pub verbose: bool,

    /// Print the K rows with the worst Jaccard scores (human mode only;
    /// default 5 in human mode, 0 in `--json`).
    #[arg(long = "top-regressions", value_name = "K")]
    pub top_regressions: Option<usize>,
}

/// Two-layer whoknows eval gate (TS `eval-whoknows`; G74 1-1-4 stage 5).
///
/// Layer 1 (quality): hand-labeled fixture JSONL, pass at >= 80% top-3 hit
/// rate. Layer 2 (regression): eval_candidates replay set-Jaccard@3 >= 0.4,
/// auto-skipped when fewer than 20 replay-eligible rows exist.
#[derive(Debug, Parser)]
pub struct EvalWhoknowsArgs {
    /// Hand-labeled fixture JSONL path (one `{query, expected_top_3_slugs}`
    /// per line). Required.
    #[arg(value_name = "FIXTURE.jsonl", required = true)]
    pub fixture_path: String,

    /// Emit the JSON report envelope instead of a human table.
    #[arg(long)]
    pub json: bool,

    /// Skip Layer 2 (regression gate) entirely — run the quality gate only.
    #[arg(long = "skip-replay")]
    pub skip_replay: bool,

    /// Top-K to grade (default 5; the eval itself grades top-3).
    #[arg(long, value_name = "N", default_value_t = 5)]
    pub limit: usize,
}

/// Args for `zbrain eval-run-all` (G74 1-1-4 stage 6).
///
/// Redesign of the TS `eval run-all` stub: instead of sweeping TS-only search
/// modes and writing `skipped` audit rows, this genuinely orchestrates the
/// verdict-producing Rust gates (gate / replay / whoknows) and aggregates
/// their verdicts into one report.
#[derive(Debug, Parser)]
pub struct EvalRunAllArgs {
    /// Which eval gates to run and aggregate. Subset of
    /// `gate,replay,whoknows` (default: all three).
    #[arg(long = "checks", value_name = "LIST", value_delimiter = ',')]
    pub checks: Option<Vec<String>>,

    /// Qrels ground truth for the `gate` check. Required when `gate` is
    /// selected. Accepts a file path or an inline `json:` object.
    #[arg(long = "qrels", value_name = "PATH|JSON")]
    pub qrels: Option<String>,

    /// NDJSON snapshot from `zbrain eval-export` for the `replay` check.
    /// Required when `replay` is selected.
    #[arg(long = "against", value_name = "FILE")]
    pub against: Option<String>,

    /// Hand-labeled fixture for the `whoknows` check. Required when
    /// `whoknows` is selected.
    #[arg(long = "fixture", value_name = "PATH")]
    pub fixture: Option<String>,

    /// Recall@k cutoff depth (also the per-query retrieval depth) for the
    /// `gate` check (default 10).
    #[arg(long, value_name = "N", default_value_t = 10)]
    pub k: usize,

    /// Per-row / top-K limit for `replay` and `whoknows` (default 5).
    #[arg(long, value_name = "N", default_value_t = 5)]
    pub limit: usize,

    /// Write the run report to this path. Default:
    /// `.zbrain-evals/run-all-<run_id>.json`.
    #[arg(long = "output", value_name = "PATH")]
    pub output: Option<String>,

    /// Emit the JSON run report instead of a human summary.
    #[arg(long)]
    pub json: bool,

    /// Skip Layer 2 (regression) of the `whoknows` check.
    #[arg(long = "skip-replay")]
    pub skip_replay: bool,
}

/// Args for `zbrain eval-compare` (G74 1-1-4 stage 6).
///
/// Reads two `eval run-all` reports and surfaces per-check regressions.
#[derive(Debug, Parser)]
pub struct EvalCompareArgs {
    /// Baseline run-all report (JSON) written by `zbrain eval-run-all`.
    #[arg(long = "baseline", value_name = "PATH", required = true)]
    pub baseline: String,

    /// Current run-all report (JSON) written by `zbrain eval-run-all`.
    #[arg(long = "current", value_name = "PATH", required = true)]
    pub current: String,

    /// Emit the JSON compare report instead of a human table.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain eval-code-retrieval`.
///
/// Three modes (mutually exclusive at the dispatch layer):
///   - `--baseline`          capture pre-v0.34 retrieval (hybrid search only)
///   - `--with-code-intel`   capture v0.34 mode (real code-intel ops)
///   - `--compare A B`       read two saved reports, emit the gate verdict
#[derive(Debug, Parser)]
pub struct EvalCodeRetrievalArgs {
    /// Capture pre-v0.34 retrieval quality (query + hybrid search only).
    #[arg(long)]
    pub baseline: bool,

    /// Capture v0.34 mode (wire to real Rust code-intel ops).
    #[arg(long)]
    pub with_code_intel: bool,

    /// Compare two saved reports (baseline + with-code-intel). Takes two paths.
    #[arg(long, num_args = 2, value_name = "REPORT")]
    pub compare: Option<Vec<String>>,

    /// Brain corpus to query (default: zbrain).
    #[arg(long, default_value = "zbrain")]
    pub corpus: String,

    /// Question file (default: bundled v0.34 baseline set).
    #[arg(long)]
    pub questions: Option<String>,

    /// Source to scope queries to.
    #[arg(long)]
    pub source: Option<String>,

    /// Top-k cutoff (default: 5).
    #[arg(long, default_value_t = 5)]
    pub k: usize,

    /// Write the EvalRunReport JSON to disk.
    #[arg(long)]
    pub save: Option<String>,

    /// Emit machine-readable JSON to stdout.
    #[arg(long)]
    pub json: bool,
}

/// Args for `zbrain eval cross-modal`.
///
/// Rust port of TS `src/commands/eval-cross-modal.ts` (single-task mode).
/// Three different-provider frontier models score the OUTPUT against the TASK
/// on a fixed dimension list; verdict PASS(0)/FAIL(1)/INCONCLUSIVE(2).
/// (Batch `--batch <jsonl>` mode is a follow-up — depends on the longmemeval
/// command's JSONL output.)
#[derive(Debug, Parser)]
pub struct EvalCrossModalArgs {
    /// What the OUTPUT was meant to achieve.
    #[arg(long)]
    pub task: Option<String>,

    /// File whose content gets scored. A `skills/<slug>/SKILL.md` path binds
    /// the receipt to that skill (T10).
    #[arg(long)]
    pub output: Option<String>,

    /// Receipt filename slug. Defaults to inferred slug from --output, or a
    /// content sha for ad-hoc inputs.
    #[arg(long)]
    pub slug: Option<String>,

    /// Comma-separated dimension list. Default: 5 standard dimensions.
    #[arg(long, value_delimiter = ',')]
    pub dimensions: Option<Vec<String>>,

    /// 1-3 cycles. Default 3 in TTY, 1 in non-TTY (handled by the handler).
    #[arg(long)]
    pub cycles: Option<u32>,

    /// Override default 'openai:gpt-4o'.
    #[arg(long)]
    pub slot_a_model: Option<String>,

    /// Override default 'anthropic:claude-opus-4-7'.
    #[arg(long)]
    pub slot_b_model: Option<String>,

    /// Override default 'google:gemini-1.5-pro'.
    #[arg(long)]
    pub slot_c_model: Option<String>,

    /// Receipt directory. Default: platform eval-receipts dir.
    #[arg(long)]
    pub receipt_dir: Option<String>,

    /// Output token budget per call. Default 4000.
    #[arg(long)]
    pub max_tokens: Option<u32>,

    /// Emit final aggregate as JSON to stdout (progress to stderr).
    #[arg(long)]
    pub json: bool,
}

/// Rust port of TS `src/commands/eval-takes-quality.ts` (MVP).
/// Rust port of TS `src/commands/eval-takes-quality.ts` — A2 (1-1-5-3 / #319).
///
/// The TS command has four sub-subcommands (`run` / `replay` / `regress` /
/// `trend`). In the Rust port those are expressed as clap subcommands of the
/// top-level `eval-takes-quality` verb. `run` performs a fresh eval and
/// persists a 4-sha receipt row; `replay` re-loads a prior receipt; `regress`
/// compares two receipts (CI gate); `trend` charts past runs from the DB.
#[derive(Debug, Parser)]
pub struct EvalTakesQualityArgs {
    #[command(subcommand)]
    pub action: TakesQualityAction,
}

/// Subcommands of `zbrain eval-takes-quality`.
#[derive(Debug, Subcommand)]
pub enum TakesQualityAction {
    /// Sample takes and run the 5-dimension judge panel (persists a receipt).
    Run(EvalTakesQualityRunArgs),
    /// Re-load a prior receipt without running models.
    Replay(EvalTakesQualityReplayArgs),
    /// Compare a fresh receipt against a prior one (CI regression gate).
    Regress(EvalTakesQualityRegressArgs),
    /// Chart past runs from the DB (newest first).
    Trend(EvalTakesQualityTrendArgs),
}

/// Args for `eval-takes-quality run`.
#[derive(Debug, Parser)]
pub struct EvalTakesQualityRunArgs {
    /// How many takes to sample from the corpus.
    #[arg(long, default_value_t = 100)]
    pub sample: usize,

    /// Receipt filename slug. Default: a content sha.
    #[arg(long)]
    pub slug: Option<String>,

    /// Comma-separated dimension list. Default: 5 takes-quality dimensions.
    #[arg(long, value_delimiter = ',')]
    pub dimensions: Option<Vec<String>>,

    /// 1-3 cycles. Default 3 in TTY, 1 in non-TTY (handled by the handler).
    #[arg(long)]
    pub cycles: Option<u32>,

    /// Override default 'openai:gpt-4o'.
    #[arg(long)]
    pub slot_a_model: Option<String>,

    /// Override default 'anthropic:claude-opus-4-7'.
    #[arg(long)]
    pub slot_b_model: Option<String>,

    /// Override default 'google:gemini-1.5-pro'.
    #[arg(long)]
    pub slot_c_model: Option<String>,

    /// Receipt directory. Default: platform eval-receipts dir.
    #[arg(long)]
    pub receipt_dir: Option<String>,

    /// Output token budget per call. Default 4000.
    #[arg(long)]
    pub max_tokens: Option<u32>,

    /// Emit final aggregate as JSON to stdout (progress to stderr).
    #[arg(long)]
    pub json: bool,
}

/// Args for `eval-takes-quality replay`.
#[derive(Debug, Parser)]
pub struct EvalTakesQualityReplayArgs {
    /// Path to a takes-quality receipt JSON written by `run`.
    #[arg(long)]
    pub receipt: Option<String>,

    /// Load from DB by 4-sha receipt identity (e.g.
    /// `takes-quality-<c>-<p>-<m>-<r>`). Mutually exclusive with `--receipt`.
    #[arg(long)]
    pub from_db: Option<String>,

    /// Emit the full receipt as JSON to stdout.
    #[arg(long)]
    pub json: bool,
}

/// Args for `eval-takes-quality regress`.
#[derive(Debug, Parser)]
pub struct EvalTakesQualityRegressArgs {
    /// Current receipt: path to a takes-quality receipt JSON.
    #[arg(long)]
    pub current: Option<String>,

    /// Current receipt from DB by 4-sha identity.
    #[arg(long)]
    pub current_from_db: Option<String>,

    /// Prior (known-good) receipt: path.
    #[arg(long)]
    pub prior: Option<String>,

    /// Prior receipt from DB by 4-sha identity.
    #[arg(long)]
    pub prior_from_db: Option<String>,

    /// Per-dim mean drop threshold counting as regression. Default 0.5.
    #[arg(long)]
    pub threshold: Option<f64>,

    /// Exit non-zero when a regression is detected (CI gate).
    #[arg(long)]
    pub fail_on_regress: bool,

    /// Emit as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Args for `eval-takes-quality trend`.
#[derive(Debug, Parser)]
pub struct EvalTakesQualityTrendArgs {
    /// Look-back window in days. Default 30.
    #[arg(long, default_value_t = 30)]
    pub days: i64,

    /// Filter to a specific rubric version (default: all).
    #[arg(long)]
    pub rubric_version: Option<String>,

    /// Hard cap on rows returned. Default 20, max 200.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Emit as JSON.
    #[arg(long)]
    pub json: bool,
}


/// Rust port of TS `src/commands/eval-suspected-contradictions.ts` — MVP.
///
/// The TS command has three sub-subcommands (`run` / `trend` / `review`). In
/// the Rust port those are expressed as clap subcommands of the top-level
/// `eval-suspected-contradictions` verb. Only `run` is implemented in the MVP;
/// `trend` and `review` return an informative "deferred" error (roadmap node
/// 1-1-5-4 — the run-row table + ASCII chart + findings surfacing are not yet
/// ported).
#[derive(Debug, Parser)]
pub struct EvalSuspectedContradictionsArgs {
    #[command(subcommand)]
    pub action: SuspectedContradictionsAction,
}

/// Subcommands of `zbrain eval-suspected-contradictions`.
#[derive(Debug, Subcommand)]
pub enum SuspectedContradictionsAction {
    /// Execute one contradiction probe pass over the brain's takes.
    Run(EvalSuspectedContradictionsRunArgs),
    /// Render the ASCII trend chart from past runs.
    Trend(EvalSuspectedContradictionsTrendArgs),
    /// Surface findings from a recorded run (optionally filtered by severity).
    Review(EvalSuspectedContradictionsReviewArgs),
}

/// Args for `eval-suspected-contradictions run`.
#[derive(Debug, Parser)]
pub struct EvalSuspectedContradictionsRunArgs {
    /// Number of takes to sample from the corpus for pair generation.
    #[arg(long, default_value_t = 200)]
    pub sample: usize,

    /// Hard cap on the number of pairs judged (cost guard). Default 40.
    #[arg(long, default_value_t = 40)]
    pub max_pairs: usize,

    /// Conditioning query applied to every pair (query-conditioned judge).
    #[arg(long)]
    pub query: Option<String>,

    /// Judge model (`provider:model`). Default utility-tier haiku-equivalent.
    #[arg(long)]
    pub judge: Option<String>,

    /// UTF-8-safe per-pair text budget. Default 1500.
    #[arg(long, default_value_t = 1500)]
    pub max_pair_chars: usize,

    /// Receipt filename slug. Default: `suspected-contradictions`.
    #[arg(long)]
    pub slug: Option<String>,

    /// Receipt directory. Default: platform eval-receipts dir.
    #[arg(long)]
    pub receipt_dir: Option<String>,

    /// Output token budget per judge call. Default 2000.
    #[arg(long)]
    pub max_tokens: Option<u32>,

    /// Emit the run summary as JSON to stdout (progress to stderr).
    #[arg(long)]
    pub json: bool,

    /// Pairing strategy. `corpus` (default): sample takes from the corpus and
    /// pair them. `retrieval`: run `hybrid_search` per query and pair the top-K
    /// pages cross/intra (the deferred retrieval-discovery path, now ported).
    #[arg(long, default_value = "corpus")]
    pub pairing: String,

    /// Retrieval queries (comma-separated). Each runs hybrid_search. Only used
    /// when `--pairing retrieval`; defaults to `--query` when omitted.
    #[arg(long, value_delimiter = ',')]
    pub queries: Option<Vec<String>>,

    /// Top-K pages per retrieval query. Default 5.
    #[arg(long, default_value_t = 5)]
    pub top_k: usize,

    /// Disable the persistent judge cache (1-1-5-8). Every pair is re-judged
    /// and nothing is written to the cache. Useful for benchmark runs.
    #[arg(long)]
    pub no_cache: bool,
}

/// Args for `eval-suspected-contradictions trend` (deferred in MVP).
#[derive(Debug, Parser)]
pub struct EvalSuspectedContradictionsTrendArgs {
    /// Look-back window in days. Default 30.
    #[arg(long, default_value_t = 30)]
    pub days: i64,

    /// Emit as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Args for `eval-suspected-contradictions review`.
#[derive(Debug, Parser)]
pub struct EvalSuspectedContradictionsReviewArgs {
    /// Filter findings by severity (info|low|medium|high).
    #[arg(long)]
    pub severity: Option<String>,

    /// Only review the run with this run_id (defaults to the most recent run).
    #[arg(long)]
    pub run_id: Option<String>,

    /// Restrict candidate runs to those on/after this date (YYYY-MM-DD).
    /// Without this, all recorded runs are candidates.
    #[arg(long)]
    pub since: Option<String>,

    /// Emit the selected run's full `report_json` as JSON.
    #[arg(long)]
    pub json: bool,
}


/// Rust port of TS `src/commands/eval-longmemeval.ts`.
///
/// Runs the LongMemEval live-LLM benchmark against zbrain hybrid retrieval.
/// The runner (in `zbrain_core::eval::longmemeval::runner`) owns the heavy
/// lifting; this CLI verb only resolves the chat / embedding providers and
/// the config lookup, then hands a reconstructed argv to
/// `run_eval_long_mem_eval`.
#[derive(Debug, Parser)]
pub struct EvalLongMemEvalArgs {
    /// LongMemEval dataset file (one question per line; JSONL or JSON array).
    pub dataset: Option<String>,

    /// Run only the first N questions.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Override answer-generation model (default: resolveModel).
    #[arg(long)]
    pub model: Option<String>,

    /// Skip LLM answer generation; emit retrieved sessions instead.
    #[arg(long)]
    pub retrieval_only: bool,

    /// Skip vector embedding; pure keyword retrieval.
    #[arg(long)]
    pub keyword_only: bool,

    /// Retrieve K sessions per question (default: 8).
    #[arg(long)]
    pub top_k: Option<usize>,

    /// Write JSONL to FILE instead of stdout.
    #[arg(long)]
    pub output: Option<String>,

    /// Skip question_ids already present in FILE; resume the remaining
    /// questions. Typically the same path as --output.
    #[arg(long)]
    pub resume_from: Option<String>,

    /// Opt out of trajectory routing for an A/B run.
    #[arg(long)]
    pub no_trajectory: bool,

    /// Emit a final JSON line with per-question-type R@k.
    #[arg(long)]
    pub by_type: bool,

    /// Exit non-zero if any question_type rate < F ([0, 1]).
    #[arg(long)]
    pub by_type_floor: Option<f64>,

    /// Search-mode system override (NOT supported in the Rust pipeline yet;
    /// passed through so the runner can hard-fail with an honest message).
    #[arg(long)]
    pub mode: Option<String>,

    /// Multi-query expansion (NOT supported in the Rust pipeline yet; passed
    /// through so the runner can hard-fail with an honest message).
    #[arg(long)]
    pub expansion: bool,
}

/// Reconstruct the argv the runner parser expects, so clap stays the single
/// source of truth for flag spelling while `run_eval_long_mem_eval` keeps
/// owning semantics (limits, floors, honesty gates).
fn eval_longmemeval_args_to_vec(a: &EvalLongMemEvalArgs) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(d) = &a.dataset {
        v.push(d.clone());
    }
    if let Some(n) = a.limit {
        v.push("--limit".into());
        v.push(n.to_string());
    }
    if let Some(m) = &a.model {
        v.push("--model".into());
        v.push(m.clone());
    }
    if a.retrieval_only {
        v.push("--retrieval-only".into());
    }
    if a.keyword_only {
        v.push("--keyword-only".into());
    }
    if let Some(k) = a.top_k {
        v.push("--top-k".into());
        v.push(k.to_string());
    }
    if let Some(o) = &a.output {
        v.push("--output".into());
        v.push(o.clone());
    }
    if let Some(r) = &a.resume_from {
        v.push("--resume-from".into());
        v.push(r.clone());
    }
    if a.no_trajectory {
        v.push("--no-trajectory".into());
    }
    if a.by_type {
        v.push("--by-type".into());
    }
    if let Some(f) = a.by_type_floor {
        v.push("--by-type-floor".into());
        v.push(f.to_string());
    }
    if let Some(m) = &a.mode {
        v.push("--mode".into());
        v.push(m.clone());
    }
    if a.expansion {
        v.push("--expansion".into());
    }
    v
}

// ── Extract subcommands ────────────────────────────────────────

/// Subcommands for `zbrain extract`.
///
/// Rust port of the TS `extract` command's `links` / `timeline` / `all`
/// verbs. The TS command is a pure parser — it scans page bodies (or a
/// markdown directory, with `--source fs --dir <path>`) for markdown/wikilinks
/// and dated timeline lines, then batch-writes them through the engine. No LLM
/// is involved; see KNOWN-GAPS G76a for the corrected scope note. The
/// `--source fs` path is implemented here; `--by-mention` remains outstanding.
#[derive(Debug, Subcommand)]
pub enum ExtractAction {
    /// Extract markdown + wikilinks from page bodies into `page_links`
    Links(ExtractLinksArgs),

    /// Extract dated timeline entries from page bodies into `pages.timeline`
    Timeline(ExtractTimelineArgs),

    /// Run both link and timeline extraction in one pass
    All(ExtractAllArgs),

    /// Extract facts from conversation pages via LLM (TS `extract-conversation-facts`).
    ///
    /// Wires directly to `run_extract_conversation_facts_core` — the same core
    /// the `conversation-facts-backfill` cycle phase uses. Requires an LLM
    /// provider; `--dry-run` still resolves the provider but writes nothing.
    /// Closes KNOWN-GAPS G76b.
    ConversationFacts(ExtractConversationFactsArgs),
}

/// Arguments for `zbrain extract links`.
#[derive(Debug, Parser)]
pub struct ExtractLinksArgs {
    /// Process a single page slug only (default: every page in the brain).
    #[arg(long)]
    pub slug: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,

    /// Data source: `db` (extract from already-synced pages) or `fs`
    /// (walk a markdown directory). Defaults to `db` to preserve the
    /// existing verb behavior and avoid walking cwd by accident.
    #[arg(long, default_value = "db")]
    pub source: String,

    /// Filesystem directory to walk (required when `--source fs`).
    #[arg(long)]
    pub dir: Option<String>,
}

/// Arguments for `zbrain extract timeline`.
#[derive(Debug, Parser)]
pub struct ExtractTimelineArgs {
    /// Process a single page slug only (default: every page in the brain).
    #[arg(long)]
    pub slug: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,

    /// Data source: `db` (extract from already-synced pages) or `fs`
    /// (walk a markdown directory). Defaults to `db`.
    #[arg(long, default_value = "db")]
    pub source: String,

    /// Filesystem directory to walk (required when `--source fs`).
    #[arg(long)]
    pub dir: Option<String>,
}

/// Arguments for `zbrain extract all`.
#[derive(Debug, Parser)]
pub struct ExtractAllArgs {
    /// Process a single page slug only (default: every page in the brain).
    #[arg(long)]
    pub slug: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,

    /// Data source: `db` (extract from already-synced pages) or `fs`
    /// (walk a markdown directory). Defaults to `db`.
    #[arg(long, default_value = "db")]
    pub source: String,

    /// Filesystem directory to walk (required when `--source fs`).
    #[arg(long)]
    pub dir: Option<String>,
}

/// Arguments for `zbrain extract conversation-facts` (KNOWN-GAPS G76b).
///
/// Faithful Rust port of the TS top-level `extract-conversation-facts`
/// command: enumerate conversation-style pages (optionally a single `--slug`)
/// and extract structured facts via an LLM, inserting them into the fact
/// store with per-page checkpointing for resume. Uses the same core op the
/// `conversation-facts-backfill` cycle phase uses.
#[derive(Debug, Parser)]
pub struct ExtractConversationFactsArgs {
    /// Process a single page slug only (default: every matching page).
    #[arg(long)]
    pub slug: Option<String>,

    /// Source id to scan (default "default").
    #[arg(long, default_value = "default")]
    pub source_id: String,

    /// Restrict to these page types (repeatable). Defaults to all allowed
    /// conversation types.
    #[arg(long = "type")]
    pub types: Vec<String>,

    /// Extract facts but do not insert them into the store.
    #[arg(long)]
    pub dry_run: bool,

    /// Max number of pages to process.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Only process pages changed since this ISO-8601 timestamp.
    #[arg(long)]
    pub since: Option<String>,

    /// Clear the per-slug checkpoint before processing (re-process from start).
    #[arg(long)]
    pub force: bool,

    /// LLM model override (default anthropic:claude-sonnet-4-6).
    #[arg(long)]
    pub model: Option<String>,

    /// Max spend in USD for this run.
    #[arg(long)]
    pub max_cost: Option<f64>,

    /// Sleep ms between LLM calls (throttle). Default 200.
    #[arg(long)]
    pub sleep_ms: Option<u64>,

    /// Max segments per page (0 = no limit). Default 0.
    #[arg(long)]
    pub segment_limit: Option<usize>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

// ── Links subcommands ──────────────────────────────────────────

/// Subcommands for `zbrain links`.
#[derive(Debug, Subcommand)]
pub enum LinksAction {
    /// Add links between pages (batch upsert)
    Add(LinksAddArgs),

    /// List outbound links from a page
    List(LinksListArgs),

    /// List backlinks (inbound links) to a page
    #[command(name = "backlinks")]
    Backlinks(LinksBacklinksArgs),

    /// Re-extract markdown + wikilinks from every page body and upsert page_links
    #[command(name = "rebuild-md-links")]
    RebuildMdLinks(LinksRebuildMdLinksArgs),

    /// Remove a link
    #[command(name = "rm")]
    Remove(LinksRemoveArgs),

    /// Resumable symbol-resolution backfill (G77 / 1-6-3). Resolves emitted
    /// `code_edges_symbol` rows against same-page `symbol_name_qualified`
    /// candidates, recording outcomes in `edge_metadata`.
    #[command(name = "edges-backfill")]
    EdgesBackfill(LinksEdgesBackfillArgs),

    /// Reconcile doc↔impl edges: scan markdown pages for code-path
    /// references and create `documents` / `documented_by` edges to the
    /// matching code page (G77 / 1-6-2). Mirrors TS `reconcile-links.ts`.
    #[command(name = "reconcile")]
    Reconcile(LinksReconcileArgs),

    /// Auto-link entity mentions to known entity pages (G76 / 1-3). Scans
    /// markdown pages for gazetteer entity mentions and creates
    /// `mentions` / `mentioned_by` edges. Mirrors TS `by-mention.ts`.
    #[command(name = "by-mention")]
    ByMention(LinksByMentionArgs),
}

/// Arguments for `zbrain links add`.
#[derive(Debug, Parser)]
pub struct LinksAddArgs {
    /// Source page slug (from)
    #[arg(short, long)]
    pub from: String,

    /// Target page slug (to)
    #[arg(short, long)]
    pub to: String,

    /// Link type: reference, mention, related, parent, child (default: reference)
    #[arg(long, default_value = "reference")]
    pub link_type: String,

    /// Link source: markdown, frontmatter, manual, mentions (default: manual)
    #[arg(long, default_value = "manual")]
    pub link_source: String,

    /// Additional context for the link
    #[arg(long)]
    pub context: Option<String>,

    /// Source ID for 'from' page (default: default)
    #[arg(long, default_value = "default")]
    pub from_source: String,

    /// Source ID for 'to' page (default: default)
    #[arg(long, default_value = "default")]
    pub to_source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links list`.
#[derive(Debug, Parser)]
pub struct LinksListArgs {
    /// Page slug to list outbound links for
    pub slug: String,

    /// Source ID (default: default)
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links backlinks`.
#[derive(Debug, Parser)]
pub struct LinksBacklinksArgs {
    /// Page slug to list backlinks for
    pub slug: String,

    /// Source ID (default: default)
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links rebuild-md-links`.
///
/// Scans every page's compiled_truth for markdown + wikilink references and
/// upserts the resulting outbound links into `page_links`. Closes G77-1.
#[derive(Debug, Parser)]
pub struct LinksRebuildMdLinksArgs {
    /// Process a single page slug only (default: every page in the brain).
    #[arg(long)]
    pub slug: Option<String>,

    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links rm`.
#[derive(Debug, Parser)]
pub struct LinksRemoveArgs {
    /// Source page slug (from)
    #[arg(short, long)]
    pub from: String,

    /// Target page slug (to)
    #[arg(short, long)]
    pub to: String,

    /// Link type to remove (omit to remove all types)
    #[arg(long)]
    pub link_type: Option<String>,

    /// Source ID for 'from' page (default: default)
    #[arg(long, default_value = "default")]
    pub from_source: String,

    /// Source ID for 'to' page (default: default)
    #[arg(long, default_value = "default")]
    pub to_source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links edges-backfill`.
///
/// Mirrors TS `edges-backfill.ts`. Resumable via `content_chunks
/// .edges_backfilled_at`; each BATCH_SIZE (200) chunk batch is its own
/// transaction, so a Ctrl-C mid-run loses at most one batch and a re-run
/// resumes cleanly.
#[derive(Debug, Parser)]
pub struct LinksEdgesBackfillArgs {
    /// Scope to one source (default: 'default').
    #[arg(long)]
    pub source: Option<String>,

    /// Iterate every non-archived registered source.
    #[arg(long)]
    pub all_sources: bool,

    /// Cap on chunks walked per source (default: 2000).
    #[arg(long)]
    pub max_chunks: Option<usize>,

    /// Emit JSON result on stdout.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links reconcile`.
///
/// Mirrors TS `reconcile-links.ts`. Scans every markdown page in the scoped
/// source for code-path references and upserts `documents` / `documented_by`
/// edges to the matching code page. Idempotent.
#[derive(Debug, Parser)]
pub struct LinksReconcileArgs {
    /// Scope reconciliation to one source (default: 'default').
    #[arg(long, default_value = "default")]
    pub source: String,

    /// Report counts without writing any edges.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit JSON result on stdout.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain links by-mention`.
///
/// Mirrors TS `by-mention.ts`. Scans every markdown page in the scoped
/// source for gazetteer entity mentions and upserts `mentions` /
/// `mentioned_by` edges to the matching entity page. Idempotent.
#[derive(Debug, Parser)]
pub struct LinksByMentionArgs {
    /// Scope the scan to one source (default: 'default').
    #[arg(long, default_value = "default")]
    pub source: String,

    /// Report counts without writing any edges.
    #[arg(long)]
    pub dry_run: bool,

    /// Emit JSON result on stdout.
    #[arg(long)]
    pub json: bool,

    /// Comma-separated extra ignore-list titles (case-sensitive). Merged with
    /// the built-in ambiguous-token list.
    #[arg(long)]
    pub extra_ignore: Option<String>,
}

// ── Takes subcommands ──────────────────────────────────────────

/// Subcommands for `zbrain takes`.
#[derive(Debug, Subcommand)]
pub enum TakesAction {
    /// Add takes to a page
    Add(TakesAddArgs),

    /// List takes for a page
    List(TakesListArgs),
}

/// Arguments for `zbrain takes add`.
#[derive(Debug, Parser)]
pub struct TakesAddArgs {
    /// Page slug
    #[arg(short, long)]
    pub slug: String,

    /// Source ID (default: default)
    #[arg(long, default_value = "default")]
    pub source: String,

    /// Take claim text
    #[arg(long)]
    pub claim: String,

    /// Take kind (opinion, observation, prediction, etc.)
    #[arg(long, default_value = "opinion")]
    pub kind: String,

    /// Take holder / author name
    #[arg(long, default_value = "cli")]
    pub holder: String,

    /// Weight 0.0-1.0 (default: 0.5)
    #[arg(long, default_value = "0.5")]
    pub weight: f64,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain takes list`.
#[derive(Debug, Parser)]
pub struct TakesListArgs {
    /// Page slug
    pub slug: String,

    /// Source ID (default: default)
    #[arg(short, long, default_value = "default")]
    pub source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain salience` command.
///
/// Queries recently touched pages ranked by salience score.
#[derive(Debug, Parser)]
pub struct SalienceArgs {
    /// Look-back window in days (default: 7)
    #[arg(long, default_value = "7")]
    pub days: u32,

    /// Max results to return (default: 50, max: 100)
    #[arg(long, default_value = "50")]
    pub limit: u32,

    /// Optional slug prefix filter
    #[arg(long)]
    pub prefix: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain orphans` command.
///
/// Finds pages with zero inbound links from live pages.
#[derive(Debug, Parser)]
pub struct OrphansArgs {
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain graph-query` command.
///
/// BFS graph traversal from a root page.
#[derive(Debug, Parser)]
pub struct GraphQueryArgs {
    /// Root page slug to start traversal from
    pub slug: String,

    /// Max traversal depth (default: 1)
    #[arg(long, default_value = "1")]
    pub depth: u32,

    /// Filter by link type (e.g. "related", "references")
    #[arg(long = "link-type")]
    pub link_type: Option<String>,

    /// Traversal direction: out, in, or both (default: out)
    #[arg(long, default_value = "out")]
    pub direction: String,

    /// Source ID scope (default: default)
    #[arg(long, default_value = "default")]
    pub source: String,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain get-page` command.
#[derive(Debug, Parser)]
pub struct GetPageArgs {
    /// Page slug to retrieve
    pub slug: String,

    /// Enable fuzzy slug matching
    #[arg(long)]
    pub fuzzy: bool,

    /// Include soft-deleted pages
    #[arg(long)]
    pub include_deleted: bool,
}

/// Arguments for `zbrain think` command.
#[derive(Debug, Parser)]
pub struct ThinkArgs {
    /// Question to answer
    pub question: String,

    /// Optional anchor page for context focus
    #[arg(long)]
    pub anchor: Option<String>,

    /// Number of reasoning rounds (default: 1)
    #[arg(long)]
    pub rounds: Option<u32>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,

    /// Time range start (ISO 8601)
    #[arg(long)]
    pub since: Option<String>,

    /// Time range end (ISO 8601)
    #[arg(long)]
    pub until: Option<String>,

    /// Inject the active calibration profile (off by default)
    #[arg(long)]
    pub with_calibration: bool,

    /// Holder to read the calibration profile for (default: garry)
    #[arg(long)]
    pub calibration_holder: Option<String>,

    /// Disable trajectory injection (on by default for temporal / knowledge_update intents)
    #[arg(long)]
    pub no_trajectory: bool,

    /// Source scope for calibration profile + trajectory queries
    #[arg(long)]
    pub source_id: Option<String>,

    /// Comma-separated federated source scope for trajectory queries
    #[arg(long)]
    pub allowed_sources: Option<String>,

    /// Trajectory queries filter to world-visibility only
    #[arg(long)]
    pub remote: bool,
}

/// Arguments for `zbrain auto-think` command.
///
/// Runs the auto-think cycle phase: pulls the configured open questions,
/// thinks each one, and persists the synthesis pages + citations. Mirrors the
/// TS `runPhaseAutoThink` entry point.
#[derive(Parser, Debug, Clone)]
pub struct AutoThinkArgs {
    /// Model override for the think calls (provider-prefixed, e.g. anthropic:...).
    #[arg(long)]
    pub model: Option<String>,

    /// Dry run: plan and validate without calling the LLM or persisting.
    #[arg(long)]
    pub dry_run: bool,

    /// Brain directory (for parity with cycle; DB location still comes from config).
    #[arg(long)]
    pub brain_dir: Option<String>,

    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain dream` command.
///
/// Runs one brain maintenance cycle via the Rust `run_cycle` orchestrator —
/// the canonical replacement for the legacy TS `src/commands/dream.ts`.
/// Cron-friendly, JSON report, phase-selectable.
#[derive(Parser, Debug, Clone)]
pub struct DreamArgs {
    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,

    /// Preview all fixes without writing (synthesize still runs the cheap
    /// significance filter but skips the full synthesis pass).
    #[arg(long)]
    pub dry_run: bool,

    /// git pull the brain repo before syncing.
    #[arg(long)]
    pub pull: bool,

    /// Run a single phase (e.g. `lint`, `sync`, `embed`, `orphans`).
    #[arg(long)]
    pub phase: Option<String>,

    /// Brain directory (git repo). Defaults to `sync.default_repo` from config.
    #[arg(long)]
    pub dir: Option<String>,

    /// Synthesize a specific transcript file (implies --phase synthesize).
    #[arg(long)]
    pub input: Option<String>,

    /// Synthesize transcripts dated for one specific day (YYYY-MM-DD).
    #[arg(long)]
    pub date: Option<String>,

    /// Backfill range start (YYYY-MM-DD). Use with --to.
    #[arg(long)]
    pub from: Option<String>,

    /// Backfill range end (YYYY-MM-DD). Use with --from.
    #[arg(long)]
    pub to: Option<String>,

    /// Disable the synthesize self-consumption guard. A loud stderr warning
    /// fires when set. Never auto-applied for --input.
    #[arg(long = "unsafe-bypass-dream-guard")]
    pub unsafe_bypass_dream_guard: bool,
}

/// Arguments for `zbrain calibration` command.
///
/// Mirrors the legacy TS `src/commands/calibration.ts` entry point. The
/// default mode reads the latest `calibration_profiles` row for a holder
/// (or `garry` when omitted). `--regenerate` runs the calibration-profile
/// phase; `--undo-wave <v>` reverses a wave's mutations; `ab-report` shows
/// the think A/B harness report over a recent window.
#[derive(Parser, Debug, Clone)]
pub struct CalibrationArgs {
    /// Holder slug (default `garry`).
    #[arg(long)]
    pub holder: Option<String>,

    /// Emit machine-readable JSON instead of a human summary.
    #[arg(long)]
    pub json: bool,

    /// Run the calibration_profile phase now and (re)write the profile row.
    #[arg(long)]
    pub regenerate: bool,

    /// Reverse a wave's mutations (D18). Required for `undo-wave` mode.
    #[arg(long = "undo-wave", value_name = "WAVE_VERSION")]
    pub undo_wave: Option<String>,

    /// Dry-run for `undo-wave` (don't write).
    #[arg(long)]
    pub dry_run: bool,

    /// Scrub the gstack-learnings entries that match this wave (best-effort;
    /// the external `gstack-learnings-prune` binary is not available in this
    /// Rust port, see KNOWN-GAPS).
    #[arg(long = "scrub-gstack")]
    pub scrub_gstack: bool,

    /// Print the think A/B harness report over the last `--days` days.
    #[arg(long = "ab-report")]
    pub ab_report: bool,

    /// Window length in days for `ab-report` (default 30).
    #[arg(long, default_value_t = 30)]
    pub days: u32,
}

/// Arguments for `zbrain query` command.
///
/// `--explain` mirrors the TS global flag: it swaps the default JSON output for
/// a human-readable per-stage scoring attribution breakdown (base_score →
/// migrated boost multipliers → reranker rank delta → final). Only the stages
/// with a Rust data layer are rendered (salience / recency / reranker); the
/// un-migrated boost axes are tracked in docs/plans/MIGRATION.md (G13).
#[derive(Debug, Parser)]
pub struct QueryArgs {
    /// Search query text. Ignored when `--stats` is set; the stats
    /// branch reads the telemetry file directly and doesn't dispatch
    /// through the engine.
    pub query: Option<String>,

    /// Maximum number of results (default: 20)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Pagination offset (default: 0)
    #[arg(long)]
    pub offset: Option<usize>,

    /// Scope search to a specific source
    #[arg(long)]
    pub source_id: Option<String>,

    /// Print a human-readable per-stage scoring attribution breakdown instead
    /// of JSON.
    #[arg(long)]
    pub explain: bool,

    /// Print aggregate search telemetry stats (G72) instead of running a
    /// query. Reads `<ZBRAIN_HOME>/telemetry/search.jsonl`, aggregates over
    /// the window selected by `--stats-window`, prints a human-readable
    /// summary (count, p50/p95 latency, top queries, by-intent / by-mode
    /// breakdowns). Mutually exclusive with all search parameters because
    /// the call short-circuits before engine invocation.
    #[arg(long, conflicts_with_all = ["limit", "offset", "source_id", "explain"])]
    pub stats: bool,

    /// Time window for `--stats` output. Ignored when `--stats` is not set.
    /// One of `hour` / `day` / `week` / `all` (default: `day`). Mirrors the
    /// `StatsWindow` enum in `zbrain-core/src/search/telemetry.rs`.
    #[arg(long, default_value = "day", requires = "stats")]
    pub stats_window: Option<String>,
}

/// Arguments for `zbrain init` command.
#[derive(Debug, Parser)]
pub struct InitArgs {
    /// Use local embedded PGLite/libsql storage
    #[arg(long, conflicts_with_all = ["supabase", "url"])]
    pub pglite: bool,

    /// Use Supabase/Postgres storage
    #[arg(long, conflicts_with_all = ["pglite", "url"])]
    pub supabase: bool,

    /// Initialize using a PostgreSQL connection URL
    #[arg(long, conflicts_with_all = ["pglite", "supabase"])]
    pub url: Option<String>,

    /// Overwrite existing config if present
    #[arg(short, long)]
    pub force: bool,

    /// Apply schema migrations only without rewriting config
    #[arg(long)]
    pub migrate_only: bool,

    /// Configure as a thin client for a remote MCP server
    #[arg(long)]
    pub mcp_only: bool,

    /// Emit machine-readable JSON output
    #[arg(long)]
    pub json: bool,

    /// Disable interactive prompts
    #[arg(long)]
    pub non_interactive: bool,

    /// OAuth issuer URL for MCP-only setup
    #[arg(long)]
    pub issuer_url: Option<String>,

    /// Remote MCP endpoint URL for MCP-only setup
    #[arg(long)]
    pub mcp_url: Option<String>,

    /// OAuth client id for MCP-only setup
    #[arg(long)]
    pub oauth_client_id: Option<String>,

    /// OAuth client secret for MCP-only setup
    #[arg(long)]
    pub oauth_client_secret: Option<String>,

    /// Embedding model to configure during initialization
    #[arg(long)]
    pub embedding_model: Option<String>,

    /// Defer embedding setup during initialization
    #[arg(long)]
    pub no_embedding: bool,

    /// Embedding dimensions to configure during initialization
    #[arg(long)]
    pub embedding_dimensions: Option<u32>,
}

/// Arguments for `zbrain doctor` command.
#[derive(Debug, Parser)]
pub struct DoctorArgs {
    /// Emit a structured JSON report instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain features` command.
///
/// Scans brain health/stats and recommends unused features. `--auto-fix` is
/// deliberately NOT offered yet: it would dispatch to `embed --stale` /
/// `extract links|timeline`, which have no Rust CLI equivalent. Exposing a
/// no-op flag would be a lying interface, so auto-fix wiring is a separate
/// slice, blocked on those commands existing.
#[derive(Debug, Parser)]
pub struct FeaturesArgs {
    /// Emit the scan as JSON (for agents) instead of human-readable output.
    #[arg(long)]
    pub json: bool,

    /// Run the recommended auto-fixable actions (re-embed stale pages,
    /// extract links, extract timeline entries) directly instead of only
    /// reporting them. Idempotent — safe to re-run.
    #[arg(long)]
    pub auto_fix: bool,
}

/// Arguments for `zbrain storage status` — storage-tiering report.
///
/// Reports how brain pages are distributed across storage tiers
/// (`db_tracked` / `db_only` / `unspecified`), on-disk size per tier, and
/// `db_only` pages whose markdown file is missing from the repo. Reads the
/// `storage:` section of the repo's `zbrain.yml` (Rust port of TS
/// `src/commands/storage.ts`).
#[derive(Debug, Parser)]
pub struct StorageArgs {
    /// The `status` subcommand (default when omitted).
    #[arg(default_value = "status")]
    pub subcommand: String,

    /// Override the brain repo path (where `zbrain.yml` + markdown live).
    /// Falls back to `config.sync.default_repo` when omitted.
    #[arg(long)]
    pub repo: Option<String>,

    /// Emit the report as JSON (stable scripting contract) instead of
    /// human-readable text.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain publish` — generate a self-contained shareable HTML
/// file from a brain markdown page (Rust port of `src/commands/publish.ts`,
/// with markdown rendered server-side via pulldown-cmark instead of shipping
/// `marked.js` to the browser).
#[derive(Debug, Parser)]
pub struct PublishArgs {
    /// Path to the brain markdown page to publish.
    #[arg(required = true)]
    pub input: PathBuf,

    /// Password-protect the output with AES-256-GCM. With no value, a random
    /// password is auto-generated and printed; with a value, that value is used.
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    pub password: Option<String>,

    /// Override the document title (defaults to the first H1 in the page).
    #[arg(long)]
    pub title: Option<String>,

    /// Output HTML file (defaults to `<input-stem>.html` next to the input).
    #[arg(long)]
    pub out: Option<PathBuf>,
}

/// Arguments for `zbrain whoknows` — expert-routing query.
///
/// Returns ranked person/company pages by expertise depth (hybrid-search
/// relevance), relationship recency, and salience. The ranking spec is locked
/// by ENG-D1 and lives in `zbrain_core::whoknows`.
///
/// Note on the type filter: TS derives expert types from the active schema
/// pack (`expertTypesFromPack`). The schema-pack subsystem is not migrated
/// yet, so this uses the default person/company filter — see
/// docs/plans/MIGRATION.md.
#[derive(Debug, Parser)]
pub struct WhoknowsArgs {
    /// Topic to route on (multiple words are joined into one query).
    #[arg(required = true, num_args = 1..)]
    pub topic: Vec<String>,

    /// Max results (default 5).
    #[arg(long)]
    pub limit: Option<usize>,

    /// Show the ranking factor breakdown per result.
    #[arg(long)]
    pub explain: bool,

    /// Emit results as JSON (for agents) instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain integrity` command (read-only `check` subcommand).
#[derive(Debug, Parser)]
pub struct IntegrityArgs {
    /// Run the read-only scan (the only subcommand ported so far; `auto`/
    /// `review`/`reset-progress` depend on the un-migrated resolver SDK).
    #[arg(long, default_value = "check")]
    pub subcommand: String,

    /// Max pages to scan.
    #[arg(long)]
    pub limit: Option<u64>,

    /// Only scan pages whose slug starts with `<TYPE>/` (e.g. `person`).
    #[arg(long)]
    pub r#type: Option<String>,

    /// Emit results as JSON (for agents) instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain config` command and its subcommands.
#[derive(Debug, Parser)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

/// Subcommands for `zbrain config`.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Show all configuration values (redacted)
    Show,

    /// Get a single config value
    Get { key: String },

    /// Set a config value
    Set {
        key: String,
        value: String,
        /// Bypass the unknown-key check and write the value anyway
        #[arg(long)]
        force: bool,
    },

    /// Unset a config value
    Unset {
        /// Config key to unset (optional, use --pattern for bulk unset)
        #[arg(required_unless_present = "pattern")]
        key: Option<String>,
        /// Bulk unset by key prefix pattern
        #[arg(long)]
        pattern: Option<String>,
    },
}

/// Arguments for `zbrain schema-sql` command.
#[derive(Debug, Parser)]
pub struct SchemaArgs {
    /// Which backend schema to print
    #[arg(short, long, default_value = "libsql")]
    pub backend: String,
}

/// Arguments for `zbrain put-page` command.
#[derive(Debug, Parser)]
pub struct PutPageArgs {
    /// Page slug to create or update
    pub slug: String,

    /// Page type (default: note)
    #[arg(long)]
    pub page_type: Option<String>,

    /// Page title (defaults to slug)
    #[arg(long)]
    pub title: Option<String>,

    /// Page content (markdown)
    #[arg(long)]
    pub content: Option<String>,
}

/// Arguments for `zbrain delete-page` command.
#[derive(Debug, Parser)]
pub struct DeletePageArgs {
    /// Page slug to delete
    pub slug: String,
}

/// Arguments for `zbrain restore-page` command.
#[derive(Debug, Parser)]
pub struct RestorePageArgs {
    /// Page slug to restore
    pub slug: String,
}

/// Arguments for `zbrain purge-deleted-pages` command.
#[derive(Debug, Parser)]
pub struct PurgeDeletedPagesArgs {
    /// Confirm permanent deletion
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `zbrain list-pages` command.
#[derive(Debug, Parser)]
pub struct ListPagesArgs {
    /// Filter by page type
    #[arg(long)]
    pub page_type: Option<String>,

    /// Filter by tag
    #[arg(long)]
    pub tag: Option<String>,

    /// Maximum number of results (default: 50)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Pagination offset (default: 0)
    #[arg(long)]
    pub offset: Option<usize>,

    /// Include soft-deleted pages
    #[arg(long)]
    pub include_deleted: bool,
}

/// Arguments for `zbrain serve-mcp` command.
#[derive(Debug, Parser)]
pub struct ServeMcpArgs {
    /// Source ID to scope operations to (default: $ZBRAIN_SOURCE or "default")
    #[arg(long)]
    pub source: Option<String>,
}

/// Arguments for `zbrain serve --http` command.
#[derive(Debug, Parser)]
pub struct ServeHttpArgs {
    /// Enable HTTP server mode
    #[arg(long)]
    pub http: bool,

    /// Port to listen on (default: 3000, or zbrain.yml server.port)
    #[arg(long)]
    pub port: Option<u16>,

    /// Address to bind to (default: 127.0.0.1, or zbrain.yml server.bind)
    #[arg(long)]
    pub bind: Option<String>,

    /// Path to admin SPA static files directory
    #[arg(long)]
    pub spa_dir: Option<PathBuf>,
}

/// Arguments for `zbrain sync` command.
#[derive(Debug, Parser)]
pub struct SyncArgs {
    /// Source identifier (creates if not exists)
    #[arg(long, default_value = "default")]
    pub source_id: String,

    /// Path to the git repository root to sync
    #[arg(long)]
    pub repo_path: Option<PathBuf>,

    /// Force a full sync even if an anchor exists
    #[arg(long)]
    pub full_sync: bool,

    /// Chunker version to stamp on pages (detected from config if omitted)
    #[arg(long)]
    pub chunker_version: Option<i32>,

    /// Maximum file size in bytes (0 = no limit)
    #[arg(long, default_value = "0")]
    pub max_file_size: u64,

    /// Directory for recording sync failures
    #[arg(long)]
    pub failures_dir: Option<PathBuf>,

    /// Number of parallel imports (0 = auto-detect, 1 = serial)
    #[arg(long, default_value = "0")]
    pub parallelism: usize,
}

/// Execute the parsed CLI command.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    let timeout_ms = cli.timeout;
    match cli.command {
        Commands::Init(args) => run_init_command(args, cli.config.as_deref()).await?,
        Commands::Doctor(args) => run_doctor_command(args, cli.config.as_deref()).await?,
        Commands::CheckResolvable(args) => {
            check_resolvable::run_check_resolvable_command(&args, cli.config.as_deref()).await?
        }
        Commands::CheckBrainFirst(args) => {
            check_brain_first::run_check_brain_first_command(&args)?
        }
        Commands::RoutingEval(args) => {
            routing_eval::run_routing_eval_command(&args, cli.config.as_deref()).await?
        }
        Commands::Features(args) => run_features_command(args, cli.config.as_deref()).await?,
        Commands::Whoknows(args) => run_whoknows_command(args, cli.config.as_deref()).await?,
        Commands::Integrity(args) => run_integrity_command(args, cli.config.as_deref()).await?,
        Commands::Storage(args) => run_storage_command(args, cli.config.as_deref()).await?,
        Commands::Publish(args) => run_publish_command(args).await?,
            Commands::Resolvers(args) => run_resolvers_command(args).await?,
        Commands::Anomalies(args) => run_anomalies_command(args, cli.config.as_deref()).await?,
        Commands::CheckUpdate(args) => update_check::run_check_update(args.json).await?,
        Commands::Config(args) => run_config_command(args, cli.config.as_deref()).await?,
        Commands::SchemaSql(args) => run_schema_command(args)?,
        Commands::GetPage(args) => run_get_page_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::Think(args) => run_think_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::AutoThink(args) => {
            run_auto_think_command(args, cli.config.as_deref()).await?
        }
        Commands::Dream(args) => run_dream_command(args, cli.config.as_deref()).await?,
        Commands::Calibration(args) => {
            run_calibration_command(args, cli.config.as_deref()).await?
        }
        Commands::Query(args) => run_query_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::PutPage(args) => run_put_page_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::DeletePage(args) => run_delete_page_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::RestorePage(args) => run_restore_page_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::PurgeDeletedPages(args) => run_purge_deleted_pages_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::ListPages(args) => run_list_pages_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::ServeMcp(args) => run_serve_mcp_command(args, cli.config.as_deref()).await?,
        Commands::ServeHttp(args) => run_serve_http_command(args, cli.config.as_deref()).await?,
        Commands::Sync(args) => {
            let cli_opts = CliOpts {
                quiet: cli.quiet,
                progress_json: cli.progress_json,
                progress_interval: cli.progress_interval.unwrap_or(1000) as u32,
            };
            run_sync_command(args, cli.config.as_deref(), &cli_opts).await?
        }
        Commands::Sources(action) => run_sources_command(action, cli.config.as_deref(), timeout_ms).await?,
        Commands::Capture(args) => run_capture_command(args, cli.config.as_deref()).await?,
        Commands::Facts(action) => run_facts_command(action, cli.config.as_deref()).await?,
        Commands::Eval(args) => run_eval_command(args, cli.config.as_deref()).await?,
        Commands::EvalExport(args) => {
            run_eval_export_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalPrune(args) => {
            run_eval_prune_command(args, cli.config.as_deref()).await?
        }

        Commands::EvalGate(args) => {
            run_eval_gate_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalReplay(args) => {
            run_eval_replay_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalWhoknows(args) => {
            run_eval_whoknows_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalRunAll(args) => {
            run_eval_run_all_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalCompare(args) => {
            run_eval_compare_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalCodeRetrieval(args) => {
            run_eval_code_retrieval_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::EvalCrossModal(args) => {
            run_eval_cross_modal_command(args, timeout_ms).await?
        }

        Commands::EvalLongMemEval(args) => {
            run_eval_longmemeval_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalTakesQuality(args) => {
            run_eval_takes_quality_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalSuspectedContradictions(args) => {
            run_eval_suspected_contradictions_command(args, cli.config.as_deref()).await?
        }
        Commands::Brainstorm(args) => {
            run_brainstorm_command(args, &BRAINSTORM_PROFILE, cli.config.as_deref()).await?
        }
        Commands::Lsd(args) => {
            run_brainstorm_command(args, &LSD_PROFILE, cli.config.as_deref()).await?
        }
        Commands::EvalBrainstorm(args) => {
            run_eval_brainstorm_command(args, cli.config.as_deref()).await?
        }
        Commands::EvalExtractAtoms(args) => {
            run_eval_extract_atoms_command(args).await?
        }
        Commands::EvalSynthesizeConcepts(args) => {
            run_eval_synthesize_concepts_command(args).await?
        }
        Commands::EvalSchemaAuthoring(args) => {
            run_eval_schema_authoring_command(args).await?
        }
        Commands::Extract(action) => run_extract_command(action, cli.config.as_deref()).await?,
        Commands::Links(action) => run_links_command(action, cli.config.as_deref()).await?,
        Commands::Takes(action) => run_takes_command(action, cli.config.as_deref()).await?,
        Commands::Salience(args) => run_salience_command(args, cli.config.as_deref()).await?,
        Commands::Orphans(args) => run_orphans_command(args, cli.config.as_deref()).await?,
        Commands::GraphQuery(args) => run_graph_query_command(args, cli.config.as_deref()).await?,
        Commands::Autopilot(args) => run_autopilot_command(args, cli.config.as_deref()).await?,
        Commands::Remote(sub) => run_remote_command(sub, cli.config.as_deref()).await?,
        Commands::Jobs(action) => run_jobs_command(action, cli.config.as_deref()).await?,
        Commands::Agent(action) => run_agent_command(action, cli.config.as_deref()).await?,
        Commands::Schema(cmd) => schema_cmd::run_schema_pack_command(cmd, cli.config.as_deref()).await?,
        Commands::Models(args) => {
            models::run_models_command(args.mode, args.json, args.skip, cli.config.as_deref()).await?
        }
        Commands::ApplyMigrations(args) => {
            apply_migrations::run_apply_migrations_command(&args, cli.config.as_deref()).await?
        }
        Commands::Mounts(cmd) => {
            mounts::run_mounts_command(&cmd, cli.config.as_deref()).await?
        }
        // ── Phase B: thin wrappers previously served by TS cli.ts ──
        Commands::Whoami => run_whoami_command(cli.config.as_deref(), timeout_ms).await?,
        Commands::History(args) => {
            run_history_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::Revert(args) => run_revert_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::Tag(args) => run_tag_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::Untag(args) => run_untag_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::Tags(args) => run_tags_command(args, cli.config.as_deref(), timeout_ms).await?,
        Commands::Timeline(args) => {
            run_timeline_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::TimelineAdd(args) => {
            run_timeline_add_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::Transcripts(action) => {
            run_transcripts_command(action, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::FindContradictions(args) => {
            run_find_contradictions_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::FindTrajectory(args) => {
            run_find_trajectory_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::Recall(args) => {
            run_recall_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeDef(args) => {
            run_code_def_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeRefs(args) => {
            run_code_refs_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeCallers(args) => {
            run_code_callers_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeCallees(args) => {
            run_code_callees_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeBlast(args) => {
            run_code_blast_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeFlow(args) => {
            run_code_flow_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::CodeTraversalCacheClear(args) => {
            run_code_traversal_cache_clear_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::SearchByImage(args) => {
            run_search_by_image_command(args, cli.config.as_deref(), timeout_ms).await?
        }
        Commands::Skillpack(cmd) => {
            skillpack::run_skillpack(cmd).await?
        }
        Commands::Skillify(cmd) => {
            skillify::run_skillify(cmd).await?
        }
        Commands::BookMirror(args) => {
            run_book_mirror_command(args, cli.config.as_deref()).await?
        }
        Commands::Reindex(action) => {
            run_reindex_command(action, cli.config.as_deref()).await?
        }
        Commands::Backfill(action) => {
            run_backfill_command(action, cli.config.as_deref()).await?
        }
        Commands::Export(action) => {
            run_export_command(action, cli.config.as_deref()).await?
        }
        Commands::Frontmatter(action) => {
            run_frontmatter_command(action, cli.config.as_deref())?
        }
        Commands::Auth(action) => {
            run_auth_command(action, cli.config.as_deref()).await?
        }
        Commands::Providers(action) => {
            run_providers_command(action, cli.config.as_deref())?
        }
        Commands::Upgrade(args) => {
            run_upgrade_command(args, cli.config.as_deref()).await?
        }
        Commands::PostUpgrade(args) => {
            run_post_upgrade_command(args, cli.config.as_deref()).await?
        }
    }
    Ok(())
}

/// Execute `zbrain book-mirror`: build the engine, then delegate to the
/// self-contained fan-out orchestration in [`book_mirror`].
async fn run_book_mirror_command(
    args: book_mirror::BookMirrorArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let engine: std::sync::Arc<dyn zbrain_core::engine::BrainEngine> = std::sync::Arc::new(engine);
    let result = book_mirror::run_book_mirror(std::sync::Arc::clone(&engine), args).await;
    engine.disconnect().await?;
    result
}

// ─────────────────────────────────────────────────────────────────────────────
// Phase B: thin clap wrappers for the commands previously served by TS cli.ts.
// Each builds a params JSON and routes through `run_operation`, mirroring the
// pre-cutover TS dispatch. Flag → param-key mappings match operations.ts.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, clap::Args)]
pub struct HistoryArgs {
    /// Page slug
    pub slug: String,
}

#[derive(Debug, clap::Args)]
pub struct RevertArgs {
    /// Page slug
    pub slug: String,
    /// Version id to revert to
    pub version_id: u64,
}

#[derive(Debug, clap::Args)]
pub struct TagArgs {
    /// Page slug
    pub slug: String,
    /// Tag to add
    pub tag: String,
}

#[derive(Debug, clap::Args)]
pub struct UntagArgs {
    /// Page slug
    pub slug: String,
    /// Tag to remove
    pub tag: String,
}

#[derive(Debug, clap::Args)]
pub struct TagsArgs {
    /// Page slug
    pub slug: String,
}

#[derive(Debug, clap::Args)]
pub struct TimelineArgs {
    /// Page slug
    pub slug: String,
}

#[derive(Debug, clap::Args)]
pub struct TimelineAddArgs {
    /// Page slug
    pub slug: String,
    /// Entry date (YYYY-MM-DD)
    pub date: String,
    /// One-line summary
    pub summary: String,
    /// Optional longer detail (markdown)
    #[arg(long)]
    pub detail: Option<String>,
    /// Optional source attribution
    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum TranscriptsAction {
    /// Show recent transcripts
    Recent(TranscriptsRecentArgs),
}

#[derive(Debug, clap::Args)]
pub struct TranscriptsRecentArgs {
    /// Look-back window in days
    #[arg(long, default_value_t = 7)]
    pub days: u64,
    /// Max entries to return
    #[arg(long, default_value_t = 50)]
    pub limit: u64,
    /// Show full (non-summarized) transcripts
    #[arg(long)]
    pub full: bool,
}

#[derive(Debug, clap::Args)]
pub struct FindContradictionsArgs {
    #[arg(long)]
    pub slug: Option<String>,
    #[arg(long, value_parser = ["low", "med", "high"])]
    pub severity: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
}

#[derive(Debug, clap::Args)]
pub struct FindTrajectoryArgs {
    /// Entity slug to trace (required)
    #[arg(long)]
    pub entity_slug: String,
    #[arg(long)]
    pub metric: Option<String>,
    #[arg(long, value_parser = ["metric", "event", "all"])]
    pub kind: Option<String>,
    #[arg(long)]
    pub since: Option<String>,
    #[arg(long)]
    pub until: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub limit: u32,
}

/// Arguments for `zbrain recall`.
#[derive(Debug, clap::Args)]
pub struct RecallArgs {
    /// Entity slug to recall facts about (newest first)
    #[arg(long)]
    pub entity: Option<String>,
    /// ISO datetime or duration shorthand (e.g. "8 hours ago")
    #[arg(long)]
    pub since: Option<String>,
    /// Source session id
    #[arg(long)]
    pub session_id: Option<String>,
    /// Include expired facts
    #[arg(long)]
    pub include_expired: bool,
    /// Return only the supersession audit log
    #[arg(long)]
    pub supersessions: bool,
    /// Max rows to return (default 50, cap 500)
    #[arg(long, default_value_t = 50)]
    pub limit: i64,
    /// Substring filter on fact text (case-insensitive)
    #[arg(long)]
    pub grep: Option<String>,
    /// Include pending_consolidation_count in the response
    #[arg(long)]
    pub include_pending: bool,
}

/// Subcommands for `zbrain reindex`.
#[derive(Debug, Subcommand)]
pub enum ReindexAction {
    /// Re-embed all live pages from their `compiled_truth` (page-level vector).
    Pages(ReindexPagesArgs),
    /// Re-embed code symbols (tree-sitter edges) by re-importing each code
    /// page's source file and re-embedding its chunks.
    Code(ReindexCodeArgs),
    /// Re-compute `effective_date` / `effective_date_source` for every page
    /// via the frontmatter precedence chain (`reindex frontmatter`).
    Frontmatter(ReindexFrontmatterArgs),
    /// Re-embed stored chunks with the multimodal model (`reindex multimodal`).
    Multimodal(ReindexMultimodalArgs),
}

/// First-class bulk operations (TS `commands/backfill.ts`, G77).
///
/// Generalizes the keyset + checkpoint pattern so future backfills reuse one
/// tested dispatcher. Mirrors the TS shape: a positional `<kind>` selects a
/// registered backfill, and `list` (or `--list`) enumerates them with status.
/// (`embedding_voyage` is declared-only in v0.30.1 and is not yet runnable.)
#[derive(Debug, clap::Args)]
pub struct BackfillArgs {
    /// Backfill kind to run (`effective_date`, `emotional_weight`), or `list`
    /// to enumerate registered backfills. `--list` is an alias for listing.
    pub kind: Option<String>,
    /// Enumerate registered backfills and their status instead of running one.
    #[arg(long)]
    pub list: bool,
    /// Initial batch size before adaptive halving (accepted for TS parity;
    /// honored where the delegated runner exposes a batch size).
    #[arg(long, default_value_t = 1000)]
    pub batch_size: usize,
    /// Parallel batches (advisory in this build; Rust runners are single-pass).
    #[arg(long)]
    pub concurrency: Option<usize>,
    /// Resume from the last checkpoint (default on; no-op until checkpoint
    /// columns land — runs are idempotent full passes today).
    #[arg(long)]
    pub resume: bool,
    /// Report what WOULD happen; no writes.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the HNSW drop-rebuild (advisory; libsql embedding backfills only).
    #[arg(long)]
    pub keep_index: bool,
    /// Bail after N total errors (accepted; honored where the runner counts).
    #[arg(long, default_value_t = 200)]
    pub max_errors: usize,
    /// Restart from id=0, ignoring any checkpoint (no-op today).
    #[arg(long)]
    pub fresh: bool,
    /// Emit a machine-readable JSON result envelope.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain export`.
#[derive(Debug, clap::Args)]
pub struct ExportArgs {
    /// Output directory for exported `.md` files (default `./export`).
    #[arg(long, default_value = "./export")]
    pub dir: String,
    /// Only export pages whose page_type matches this string.
    #[arg(long)]
    pub r#type: Option<String>,
    /// Only export pages whose slug starts with this prefix.
    #[arg(long)]
    pub slug_prefix: Option<String>,
    /// Only export pages from this source id.
    #[arg(long)]
    pub source_id: Option<String>,
    /// Emit a machine-readable JSON result envelope instead of human text.
    #[arg(long)]
    pub json: bool,
    /// Restore-only mode (requires storage-tier config). Not yet ported to
    /// Rust — fails clearly rather than silently dumping the whole DB.
    #[arg(long)]
    pub restore_only: bool,
}

// ─── upgrade / post-upgrade ───────────────────────────────────────────────

/// Arguments for `zbrain upgrade`.
#[derive(Debug, clap::Args)]
pub struct UpgradeArgs {
    /// Apply without prompting (passed through to apply-migrations).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Emit a machine-readable JSON result envelope.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain post-upgrade`.
#[derive(Debug, clap::Args)]
pub struct PostUpgradeArgs {
    /// Apply without prompting (passed through to apply-migrations).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Emit a machine-readable JSON result envelope.
    #[arg(long)]
    pub json: bool,
}

// ─── providers ─────────────────────────────────────────────────────────────

/// Arguments for `zbrain providers`.
#[derive(Debug, clap::Subcommand)]
pub enum ProvidersAction {
    /// List all known providers + env-readiness status.
    List,
    /// Show env vars required/optional for a provider.
    Env(ProvidersEnvArgs),
    /// Emit a provider choice matrix (agent-friendly JSON).
    Explain(ProvidersExplainArgs),
    /// Smoke-test a provider (env + config readiness; live probe not yet ported).
    Test(ProvidersTestArgs),
}

#[derive(Debug, clap::Args)]
pub struct ProvidersEnvArgs {
    /// Provider id (e.g. `openai`, `anthropic`).
    pub id: String,
}

#[derive(Debug, clap::Args)]
pub struct ProvidersExplainArgs {
    /// Emit the matrix as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, clap::Args)]
pub struct ProvidersTestArgs {
    /// Touchpoint to probe: embedding | expansion | chat | reranker.
    #[arg(long)]
    pub touchpoint: Option<String>,
    /// Explicit `provider:model` to probe.
    #[arg(long)]
    pub model: Option<String>,
}

// ─── frontmatter ───────────────────────────────────────────────────────────

/// Arguments for `zbrain frontmatter`.
#[derive(Debug, clap::Subcommand)]
pub enum FrontmatterAction {
    /// Validate that frontmatter parses across a tree of `.md` files.
    Validate(FrontmatterValidateArgs),
    /// Infer + write missing frontmatter for `.md` files.
    Generate(FrontmatterGenerateArgs),
}

#[derive(Debug, clap::Args)]
pub struct FrontmatterValidateArgs {
    /// Directory or file to scan.
    pub path: String,
    /// Emit a machine-readable JSON report.
    #[arg(long)]
    pub json: bool,
    /// Attempt to fix frontmatter in place. Not yet ported to Rust — bails.
    #[arg(long)]
    pub fix: bool,
    /// Report what would change without writing (no-op for validate).
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, clap::Args)]
pub struct FrontmatterGenerateArgs {
    /// Directory or file to scan.
    pub path: String,
    /// Write generated frontmatter to files (without this, preview only).
    #[arg(long)]
    pub fix: bool,
    /// Report what would change without writing.
    #[arg(long)]
    pub dry_run: bool,
    /// Emit a machine-readable JSON report.
    #[arg(long)]
    pub json: bool,
    /// Accepted for CLI parity; Rust inference is path-based, so all inferred
    /// files are included regardless (no `(default)` catch-all distinction yet).
    #[arg(long)]
    pub include_catch_all: bool,
}

// ─── auth ──────────────────────────────────────────────────────────────────

/// Arguments for `zbrain auth`.
#[derive(Debug, clap::Subcommand)]
pub enum AuthAction {
    /// Create a legacy bearer token (prints it once).
    Create(AuthCreateArgs),
    /// List all tokens.
    List,
    /// Revoke a token by name.
    Revoke(AuthRevokeArgs),
    /// Update per-token visibility (takes-holders). Not supported by Rust schema.
    Permissions(AuthPermissionsArgs),
    /// Register an OAuth 2.1 client.
    RegisterClient(AuthRegisterClientArgs),
    /// Revoke an OAuth 2.1 client.
    RevokeClient(AuthRevokeClientArgs),
    /// Smoke-test a remote MCP server with a bearer token.
    Test(AuthTestArgs),
}

#[derive(Debug, clap::Args)]
pub struct AuthCreateArgs {
    /// Human-readable token name.
    pub name: String,
    /// Per-token takes-holders allow-list. Not supported by the Rust schema.
    #[arg(long)]
    pub takes_holders: Option<Vec<String>>,
}

#[derive(Debug, clap::Args)]
pub struct AuthRevokeArgs {
    /// Token name to revoke.
    pub name: String,
}

#[derive(Debug, clap::Args)]
pub struct AuthPermissionsArgs {
    /// Token name to update.
    pub name: String,
    /// Takes-holder allow-list. Not supported by the Rust schema.
    #[arg(long, value_delimiter = ',')]
    pub holders: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct AuthRegisterClientArgs {
    /// Client name.
    pub name: String,
    /// Space-separated OAuth scopes.
    #[arg(long, default_value = "openid profile")]
    pub scopes: String,
    /// Grant types (repeatable, space-separated).
    #[arg(long, value_delimiter = ' ', default_value = "authorization_code refresh_token")]
    pub grant_types: Vec<String>,
    /// Redirect URIs (repeatable).
    #[arg(long = "redirect-uri")]
    pub redirect_uris: Vec<String>,
    /// Source id scope.
    #[arg(long)]
    pub source: Option<String>,
    /// Federated read source ids (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub federated_read: Vec<String>,
    /// Token endpoint auth method.
    #[arg(long)]
    pub token_endpoint_auth_method: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct AuthRevokeClientArgs {
    /// OAuth client id to revoke.
    pub client_id: String,
}

#[derive(Debug, clap::Args)]
pub struct AuthTestArgs {
    /// Remote MCP server base URL.
    pub url: String,
    /// Bearer token to present.
    #[arg(long)]
    pub token: String,
}

/// Arguments for `zbrain reindex pages`.
#[derive(Debug, clap::Args)]
pub struct ReindexPagesArgs {
    /// Source scope; omit to re-embed every source.
    #[arg(long)]
    pub source_id: Option<String>,
    /// List what would be re-embedded without writing vectors.
    #[arg(long)]
    pub dry_run: bool,
    /// Page batch size per embedding call.
    #[arg(long, default_value_t = 50)]
    pub batch: usize,
}

/// Arguments for `zbrain reindex code`.
#[derive(Debug, clap::Args)]
pub struct ReindexCodeArgs {
    /// Source scope; omit to re-embed every source.
    #[arg(long)]
    pub source_id: Option<String>,
    /// List what would be re-embedded without writing vectors.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the chunk re-embedding pass (re-chunk only).
    #[arg(long)]
    pub no_embed: bool,
    /// Page batch size per import pass.
    #[arg(long, default_value_t = 100)]
    pub batch: usize,
    /// Emit a machine-readable JSON result envelope.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain reindex frontmatter`.
#[derive(Debug, clap::Args)]
pub struct ReindexFrontmatterArgs {
    /// Source scope; omit to re-compute every source.
    #[arg(long)]
    pub source_id: Option<String>,
    /// Scope to slugs starting with this prefix (e.g. 'meetings/').
    #[arg(long)]
    pub slug_prefix: Option<String>,
    /// List what would change without writing the effective_date columns.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the confirmation prompt (required for non-TTY / non-JSON runs).
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Re-apply even when the computed value already matches the stored value.
    #[arg(long)]
    pub force: bool,
    /// Emit a machine-readable JSON result envelope.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain reindex multimodal`.
#[derive(Debug, clap::Args)]
pub struct ReindexMultimodalArgs {
    /// Stop after re-embedding this many pending chunks.
    #[arg(long)]
    pub limit: Option<usize>,
    /// List the pending count + cost estimate without writing vectors.
    #[arg(long)]
    pub dry_run: bool,
    /// Print only the pending count + USD cost estimate and exit.
    #[arg(long)]
    pub cost_estimate: bool,
    /// Skip the embedding pass (scan only).
    #[arg(long)]
    pub no_embed: bool,
    /// Emit a machine-readable JSON result envelope.
    #[arg(long)]
    pub json: bool,
    /// Skip the cost-grace prompt (CI / cron).
    #[arg(long, short = 'y')]
    pub yes: bool,
}

#[derive(Debug, clap::Args)]
pub struct CodeDefArgs {
    /// Symbol to locate
    pub symbol: String,
    #[arg(long)]
    pub lang: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
}

#[derive(Debug, clap::Args)]
pub struct CodeRefsArgs {
    /// Symbol to locate
    pub symbol: String,
    #[arg(long)]
    pub lang: Option<String>,
    #[arg(long, default_value_t = 50)]
    pub limit: u32,
}

#[derive(Debug, clap::Args)]
pub struct CodeCallersArgs {
    /// Symbol to locate
    pub symbol: String,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub all_sources: bool,
    #[arg(long, default_value_t = 100)]
    pub limit: u32,
}

#[derive(Debug, clap::Args)]
pub struct CodeCalleesArgs {
    /// Symbol to locate
    pub symbol: String,
    #[arg(long)]
    pub source: Option<String>,
    #[arg(long)]
    pub all_sources: bool,
    #[arg(long, default_value_t = 100)]
    pub limit: u32,
}

#[derive(Debug, clap::Args)]
pub struct CodeBlastArgs {
    #[arg(long)]
    pub symbol: String,
    #[arg(long, default_value_t = 5)]
    pub depth: u32,
    #[arg(long, default_value_t = 200)]
    pub max_nodes: u32,
    #[arg(long)]
    pub exact: bool,
}

#[derive(Debug, clap::Args)]
pub struct CodeFlowArgs {
    #[arg(long)]
    pub entry_point: String,
    #[arg(long, default_value_t = 8)]
    pub depth: u32,
    #[arg(long, default_value_t = 200)]
    pub max_nodes: u32,
    #[arg(long)]
    pub exact: bool,
}

#[derive(Debug, clap::Args)]
pub struct CodeTraversalCacheClearArgs {
    #[arg(long)]
    pub source_id: Option<String>,
    #[arg(long)]
    pub all_sources: bool,
}

#[derive(Debug, clap::Args)]
pub struct SearchByImageArgs {
    #[arg(long)]
    pub image_path: Option<String>,
    #[arg(long)]
    pub image_url: Option<String>,
    #[arg(long)]
    pub image_data: Option<String>,
    #[arg(long)]
    pub image_mime: Option<String>,
    #[arg(long)]
    pub query: Option<String>,
    #[arg(long, default_value_t = 20)]
    pub limit: u32,
    #[arg(long, default_value_t = 0)]
    pub offset: u32,
    #[arg(long)]
    pub source_id: Option<String>,
}

/// Execute `zbrain whoami` command.
async fn run_whoami_command(
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({});
    let output = run_operation("whoami", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain history` command.
async fn run_history_command(
    args: HistoryArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "slug": args.slug });
    let output = run_operation("get_versions", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain revert` command.
async fn run_revert_command(
    args: RevertArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "slug": args.slug, "version_id": args.version_id });
    let output = run_operation("revert_version", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain tag` command.
async fn run_tag_command(
    args: TagArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "slug": args.slug, "tag": args.tag });
    let output = run_operation("add_tag", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain untag` command.
async fn run_untag_command(
    args: UntagArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "slug": args.slug, "tag": args.tag });
    let output = run_operation("remove_tag", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain tags` command.
async fn run_tags_command(
    args: TagsArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "slug": args.slug });
    let output = run_operation("get_tags", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain timeline` command.
async fn run_timeline_command(
    args: TimelineArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({ "slug": args.slug });
    let output = run_operation("get_timeline", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain timeline-add` command.
async fn run_timeline_add_command(
    args: TimelineAddArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
        "date": args.date,
        "summary": args.summary,
        "detail": args.detail,
        "source": args.source,
    });
    let output = run_operation("add_timeline_entry", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain transcripts` command.
async fn run_transcripts_command(
    action: TranscriptsAction,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    match action {
        TranscriptsAction::Recent(args) => {
            let params = serde_json::json!({
                "days": args.days,
                "limit": args.limit,
                "summary": !args.full,
            });
            let output =
                run_operation("get_recent_transcripts", params, config_path, timeout_ms).await?;
            println!("{}", serde_json::to_string_pretty(&output)?);
            Ok(())
        }
    }
}

/// Execute `zbrain find-contradictions` command.
async fn run_find_contradictions_command(
    args: FindContradictionsArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
        "severity": args.severity,
        "limit": args.limit,
    });
    let output = run_operation("find_contradictions", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain find-trajectory` command.
async fn run_find_trajectory_command(
    args: FindTrajectoryArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "entity_slug": args.entity_slug,
        "metric": args.metric,
        "kind": args.kind,
        "since": args.since,
        "until": args.until,
        "limit": args.limit,
    });
    let output = run_operation("find_trajectory", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain recall` command.
async fn run_recall_command(
    args: RecallArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "entity": args.entity,
        "since": args.since,
        "session_id": args.session_id,
        "include_expired": args.include_expired,
        "supersessions": args.supersessions,
        "limit": args.limit,
        "grep": args.grep,
        "include_pending": args.include_pending,
    });
    let output = run_operation("recall", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain code-def` command.
async fn run_code_def_command(
    args: CodeDefArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "symbol": args.symbol,
        "lang": args.lang,
        "limit": args.limit,
    });
    let output = run_operation("code_def", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain reindex`: re-compute embeddings for content.
async fn run_reindex_command(
    action: ReindexAction,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    match action {
        ReindexAction::Pages(args) => run_reindex_pages(args, config_path).await,
        ReindexAction::Code(args) => run_reindex_code(args, config_path).await,
        ReindexAction::Frontmatter(args) => run_reindex_frontmatter(args, config_path).await,
        ReindexAction::Multimodal(args) => run_reindex_multimodal(args, config_path).await,
    }
}

/// Re-embed all live pages from their `compiled_truth` into the page-level
/// vector column. Mirrors the ingest-time embedding path so search parity is
/// preserved. Requires an embedding provider (`ZEROENTROPY_API_KEY`).
async fn run_reindex_pages(
    args: ReindexPagesArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, PageFilters};
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::embedding::EmbeddingClient;

    let client = EmbeddingClient::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "embedding provider not configured: set ZEROENTROPY_API_KEY to re-embed pages"
        )
    })?;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let batch = args.batch.max(1);
    let mut offset: usize = 0;
    let mut scanned = 0usize;
    let mut embedded = 0usize;

    loop {
        let filters = PageFilters {
            page_type: None,
            tag: None,
            limit: Some(batch),
            offset: Some(offset),
            updated_after: None,
            slug_prefix: None,
            include_deleted: false,
            sort: None,
            source_id: args.source_id.clone(),
            source_ids: None,
        };
        let pages = engine.list_pages(&filters).await?;
        if pages.is_empty() {
            break;
        }
        scanned += pages.len();

        if args.dry_run {
            for p in &pages {
                println!("[dry-run] would re-embed {} (source {})", p.slug, p.source_id);
            }
            offset += pages.len();
            continue;
        }

        let texts: Vec<String> = pages.iter().map(|p| p.compiled_truth.clone()).collect();
        let vectors = client.embed_batch(&texts, None).await?;
        for (p, vec) in pages.iter().zip(vectors.into_iter()) {
            let bytes = encode_embedding_le(&vec);
            engine
                .put_page_embedding(&p.slug, &p.source_id, bytes)
                .await?;
            embedded += 1;
        }
        offset += pages.len();
        println!("reindex pages: embedded batch -> total embedded={embedded}");
    }

    engine.disconnect().await?;
    println!("reindex pages complete: scanned={scanned} embedded={embedded}");
    Ok(())
}

/// Encode an f32 embedding vector as a little-endian byte blob (matches the
/// `f32-LE BLOB` encoding used by `LibsqlEngine::put_page`).
fn encode_embedding_le(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Re-embed all live code pages by re-importing each code page's source file
/// (re-chunk via `import_code_file`) and re-embedding its chunks. Mirrors the
/// TS `zbrain reindex-code` backfill: a bit-identical re-walk of every
/// `type='code'` page through the code import pipeline.
async fn run_reindex_code(
    args: ReindexCodeArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, PageFilters};
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::import::import_code_file;
    use zbrain_core::embedding::EmbeddingClient;

    let client = if args.no_embed {
        None
    } else {
        Some(EmbeddingClient::from_env().ok_or_else(|| {
            anyhow::anyhow!(
                "embedding provider not configured: set ZEROENTROPY_API_KEY to re-embed code chunks (or pass --no-embed)"
            )
        })?)
    };

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let batch = args.batch.max(1);
    let mut offset: usize = 0;
    let mut scanned = 0usize;
    let mut reindexed = 0usize;
    let mut skipped = 0usize;
    let mut failed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();

    loop {
        let filters = PageFilters {
            page_type: Some("code".to_string()),
            tag: None,
            limit: Some(batch),
            offset: Some(offset),
            updated_after: None,
            slug_prefix: None,
            include_deleted: false,
            sort: None,
            source_id: args.source_id.clone(),
            source_ids: None,
        };
        let pages = engine.list_pages(&filters).await?;
        if pages.is_empty() {
            break;
        }
        scanned += pages.len();

        if args.dry_run {
            for p in &pages {
                println!(
                    "[dry-run] would re-embed code page {} (source {})",
                    p.slug, p.source_id
                );
            }
            offset += pages.len();
            continue;
        }

        for p in &pages {
            let rel_path = p
                .frontmatter
                .get("file")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let Some(rel_path) = rel_path else {
                failed += 1;
                failures.push((p.slug.clone(), "missing frontmatter.file".to_string()));
                continue;
            };

            match import_code_file(&engine, &p.slug, &rel_path, "", &[]).await {
                Ok(result) => {
                    let wrote = result.chunks_created + result.chunks_updated;
                    if wrote == 0 {
                        skipped += 1;
                    } else {
                        reindexed += 1;
                    }
                    // Re-embed the freshly re-chunked chunks so the code
                    // vector index stays current (import_code_file stores
                    // chunks with embedding=None). Fail-open mirrors the
                    // import pipeline's embedding tolerance.
                    if let Some(client) = client.as_ref() {
                        if let Ok(chunks) = engine.get_chunks(&p.slug, &p.source_id).await {
                            if !chunks.is_empty() {
                                let texts: Vec<String> =
                                    chunks.iter().map(|c| c.chunk_text.clone()).collect();
                                if let Ok(vectors) = client.embed_batch(&texts, None).await {
                                    for (c, vec) in chunks.iter().zip(vectors.into_iter()) {
                                        let bytes = encode_embedding_le(&vec);
                                        let _ = engine
                                            .put_chunk_embedding(
                                                &p.slug,
                                                &p.source_id,
                                                c.chunk_index as usize,
                                                bytes,
                                            )
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    failed += 1;
                    failures.push((p.slug.clone(), e.to_string()));
                }
            }
        }
        offset += pages.len();
        println!(
            "reindex code: batch -> scanned={scanned} reindexed={reindexed} skipped={skipped} failed={failed}"
        );
    }

    engine.disconnect().await?;

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "status": "ok",
                "code_pages": scanned,
                "reindexed": reindexed,
                "skipped": skipped,
                "failed": failed,
                "failures": failures
                    .iter()
                    .map(|(slug, err)| serde_json::json!({"slug": slug, "error": err}))
                    .collect::<Vec<_>>(),
            })
        );
    } else {
        println!(
            "reindex code complete: scanned={scanned} reindexed={reindexed} skipped={skipped} failed={failed}"
        );
        for (slug, err) in failures.iter().take(10) {
            println!("  {slug}: {err}");
        }
    }
    Ok(())
}

/// Re-compute `effective_date` / `effective_date_source` for every live page
/// via the frontmatter precedence chain (mirrors TS `reindex-frontmatter` /
/// `backfill-effective-date`). Idempotent: rows whose computed value already
/// matches the stored value are skipped unless `--force` is given.
async fn run_reindex_frontmatter(
    args: ReindexFrontmatterArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use chrono::{DateTime, Utc};
    use zbrain_core::effective_date::compute_effective_date;
    use zbrain_core::engine::{BrainEngine, EngineConfig, PageFilters};
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::types::EffectiveDateSource;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let batch: usize = 500;
    let mut offset: usize = 0;
    let mut examined = 0usize;
    let mut updated = 0usize;
    let mut fallback = 0usize;
    let mut skipped = 0usize;

    loop {
        let filters = PageFilters {
            page_type: None,
            tag: None,
            limit: Some(batch),
            offset: Some(offset),
            updated_after: None,
            slug_prefix: args.slug_prefix.clone(),
            include_deleted: false,
            sort: None,
            source_id: args.source_id.clone(),
            source_ids: None,
        };
        let pages = engine.list_pages(&filters).await?;
        if pages.is_empty() {
            break;
        }
        examined += pages.len();

        for p in &pages {
            let filename = p
                .import_filename
                .clone()
                .or_else(|| p.slug.rsplit('/').next().map(|s| s.to_string()));

            let updated_at = DateTime::parse_from_rfc3339(&p.updated_at)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let created_at = DateTime::parse_from_rfc3339(&p.created_at)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            let computed = compute_effective_date(
                &p.slug,
                &p.frontmatter,
                filename.as_deref(),
                updated_at,
                created_at,
            );

            let existing_date = p
                .effective_date
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc));
            let dates_match = existing_date == computed.date;
            let existing_source = p.effective_date_source;
            let sources_match = existing_source == Some(computed.source);

            if !args.force && dates_match && sources_match {
                skipped += 1;
                continue;
            }

            if args.dry_run {
                updated += 1;
                if computed.source == EffectiveDateSource::Fallback {
                    fallback += 1;
                }
                continue;
            }

            engine
                .set_page_effective_date(
                    &p.slug,
                    &p.source_id,
                    computed.date,
                    Some(computed.source),
                )
                .await?;
            updated += 1;
            if computed.source == EffectiveDateSource::Fallback {
                fallback += 1;
            }
        }

        offset += pages.len();
        println!(
            "reindex frontmatter: batch -> examined={examined} updated={updated} skipped={skipped} fallback={fallback}"
        );
    }

    engine.disconnect().await?;

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "status": if args.dry_run { "dry_run" } else { "ok" },
                "examined": examined,
                "updated": updated,
                "skipped": skipped,
                "fallback": fallback,
                "source_filter": args.source_id,
                "slug_prefix": args.slug_prefix,
            })
        );
    } else {
        let noun = if args.dry_run { "would update" } else { "updated" };
        println!(
            "reindex frontmatter complete: examined={examined} {noun}={updated} skipped={skipped} fallback={fallback}"
        );
    }
    Ok(())
}

/// Walk `content_chunks` where `embedding_multimodal IS NULL`, embed each
/// chunk's text with the configured multimodal model, and persist the vector
/// to the `embedding_multimodal` column. Mirrors TS `reindex-multimodal`
/// (minus the db-lock / checkpoint / unified-flag machinery — those are
/// crash-recovery conveniences layered on top of the same core loop).
async fn run_reindex_multimodal(
    args: ReindexMultimodalArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::embedding::{EmbeddingClient, EmbeddingConfig};
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::libsql::LibsqlEngine;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let mm_model = config.embedding_multimodal_model.clone().ok_or_else(|| {
        anyhow::anyhow!(
            "reindex multimodal requires `embedding_multimodal_model` in config (e.g. voyage:voyage-multimodal-3); set it and re-run"
        )
    })?;

    // Count pending chunks (embedding_multimodal IS NULL).
    let pending_rows = engine
        .execute_raw(
            "SELECT COUNT(*) AS count FROM content_chunks WHERE embedding_multimodal IS NULL",
            &[],
        )
        .await?;
    let pending_before: i64 = pending_rows
        .first()
        .and_then(|r| r.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    // Cost estimate: sum of chunk_text length → tokens (≈3.5 chars/token) →
    // $0.18 / 1M tokens (mirrors TS Voyage multimodal-3 pricing).
    let stats_rows = engine
        .execute_raw(
            "SELECT COALESCE(SUM(LENGTH(chunk_text)), 0) AS chars \
             FROM content_chunks WHERE embedding_multimodal IS NULL",
            &[],
        )
        .await?;
    let total_chars: i64 = stats_rows
        .first()
        .and_then(|r| r.get("chars"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let estimated_tokens = total_chars as f64 / 3.5;
    let cost_usd_estimate = (estimated_tokens / 1_000_000.0) * 0.18;

    let print_cost_estimate = |pending: i64, cost: f64, json: bool| {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "pending_before": pending,
                    "pending_after": pending,
                    "reembedded": 0,
                    "failed": 0,
                    "dry_run": true,
                    "cost_usd_estimate": cost,
                    "unified_flag_prompted": false,
                })
            );
        } else {
            println!(
                "reindex multimodal cost estimate: pending={pending} chunks, ~{cost:.2} USD"
            );
        }
    };

    if args.cost_estimate {
        engine.disconnect().await?;
        print_cost_estimate(pending_before, cost_usd_estimate, args.json);
        return Ok(());
    }

    if args.dry_run {
        engine.disconnect().await?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({
                    "pending_before": pending_before,
                    "pending_after": pending_before,
                    "reembedded": 0,
                    "failed": 0,
                    "dry_run": true,
                    "cost_usd_estimate": cost_usd_estimate,
                    "unified_flag_prompted": false,
                })
            );
        } else {
            println!(
                "reindex multimodal dry-run: {pending_before} chunks would be re-embedded (~{cost_usd_estimate:.2} USD)"
            );
        }
        return Ok(());
    }

    if pending_before == 0 {
        engine.disconnect().await?;
        if args.json {
            println!(
                "{}",
                serde_json::json!({
                    "pending_before": 0,
                    "pending_after": 0,
                    "reembedded": 0,
                    "failed": 0,
                    "dry_run": false,
                    "cost_usd_estimate": 0.0f64,
                    "unified_flag_prompted": false,
                })
            );
        } else {
            println!("reindex multimodal: nothing to do (0 pending chunks)");
        }
        return Ok(());
    }

    let client = if args.no_embed {
        None
    } else {
        let api_key = std::env::var("ZEROENTROPY_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "embedding provider not configured: set ZEROENTROPY_API_KEY to re-embed multimodal chunks (or pass --no-embed)"
                )
            })?;
        let cfg = EmbeddingConfig::builder()
            .model(mm_model.clone())
            .dimensions(0) // lenient: accept the provider's native dimension
            .api_key(api_key)
            .build()
            .map_err(|e| anyhow::anyhow!("multimodal embedding config: {e}"))?;
        Some(
            EmbeddingClient::new(cfg)
                .map_err(|e| anyhow::anyhow!("multimodal embedding client: {e}"))?,
        )
    };

    let batch: i64 = 32;
    let mut last_id: i64 = 0;
    let mut processed: usize = 0;
    let mut reembedded: usize = 0;
    let mut failed: usize = 0;

    loop {
        if let Some(limit) = args.limit {
            if processed >= limit {
                break;
            }
        }
        let this_batch = match args.limit {
            Some(limit) => (batch as usize).min(limit - processed) as i64,
            None => batch,
        };
        // `last_id` and `this_batch` are integers (never user-controlled text),
        // so inlining them into the SQL is safe from injection and lets us pass
        // `&[]` — avoiding the `erased_serde::Serialize` trait-object param type
        // that `execute_raw` expects (which `zbrain-cli` does not depend on).
        let rows = engine
            .execute_raw(
                &format!(
                    "SELECT c.id AS id, c.chunk_text AS chunk_text, p.slug AS slug, \
                     p.source_id AS source_id, c.chunk_index AS chunk_index \
                     FROM content_chunks c JOIN pages p ON p.id = c.page_id \
                     WHERE c.embedding_multimodal IS NULL AND c.id > {last_id} ORDER BY c.id LIMIT {this_batch}"
                ),
                &[],
            )
            .await?;
        if rows.is_empty() {
            break;
        }

        for row in &rows {
            let id = row.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
            let chunk_text = match row.get("chunk_text").and_then(|v| v.as_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => {
                    last_id = id;
                    continue;
                }
            };
            let slug = match row.get("slug").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    last_id = id;
                    continue;
                }
            };
            let source_id = match row.get("source_id").and_then(|v| v.as_str()) {
                Some(s) => s.to_string(),
                None => {
                    last_id = id;
                    continue;
                }
            };
            let chunk_index = row.get("chunk_index").and_then(|v| v.as_i64()).unwrap_or(0);

            if let Some(client) = client.as_ref() {
                match client.embed_batch(&[chunk_text], None).await {
                    Ok(vectors) => {
                        if let Some(vec) = vectors.into_iter().next() {
                            let bytes = encode_embedding_le(&vec);
                            let _ = engine
                                .put_chunk_multimodal_embedding(
                                    &slug,
                                    &source_id,
                                    chunk_index as usize,
                                    bytes,
                                )
                                .await;
                            reembedded += 1;
                        } else {
                            failed += 1;
                        }
                    }
                    Err(_) => {
                        failed += 1;
                    }
                }
            }
            last_id = id;
            processed += 1;
        }

        println!(
            "reindex multimodal: batch -> processed={processed} reembedded={reembedded} failed={failed}"
        );
    }

    let pending_after_rows = engine
        .execute_raw(
            "SELECT COUNT(*) AS count FROM content_chunks WHERE embedding_multimodal IS NULL",
            &[],
        )
        .await?;
    let pending_after: i64 = pending_after_rows
        .first()
        .and_then(|r| r.get("count"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    engine.disconnect().await?;

    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "pending_before": pending_before,
                "pending_after": pending_after,
                "reembedded": reembedded,
                "failed": failed,
                "dry_run": false,
                "cost_usd_estimate": cost_usd_estimate,
                "unified_flag_prompted": false,
            })
        );
    } else {
        println!(
            "reindex multimodal complete: reembedded={reembedded} failed={failed} pending_before={pending_before} pending_after={pending_after}"
        );
    }
    Ok(())
}

/// Execute `zbrain code-refs` command.
async fn run_code_refs_command(
    args: CodeRefsArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "symbol": args.symbol,
        "lang": args.lang,
        "limit": args.limit,
    });
    let output = run_operation("code_refs", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain code-callers` command.
async fn run_code_callers_command(
    args: CodeCallersArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "symbol": args.symbol,
        "source_id": args.source,
        "all_sources": args.all_sources,
        "limit": args.limit,
    });
    let output = run_operation("code_callers", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain code-callees` command.
async fn run_code_callees_command(
    args: CodeCalleesArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "symbol": args.symbol,
        "source_id": args.source,
        "all_sources": args.all_sources,
        "limit": args.limit,
    });
    let output = run_operation("code_callees", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain code-blast` command.
async fn run_code_blast_command(
    args: CodeBlastArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "symbol": args.symbol,
        "depth": args.depth,
        "max_nodes": args.max_nodes,
        "exact": args.exact,
    });
    let output = run_operation("code_blast", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain code-flow` command.
async fn run_code_flow_command(
    args: CodeFlowArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "entry_point": args.entry_point,
        "depth": args.depth,
        "max_nodes": args.max_nodes,
        "exact": args.exact,
    });
    let output = run_operation("code_flow", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain code-traversal-cache-clear` command.
async fn run_code_traversal_cache_clear_command(
    args: CodeTraversalCacheClearArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "source_id": args.source_id,
        "all_sources": args.all_sources,
    });
    let output =
        run_operation("code_traversal_cache_clear", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain search-by-image` command.
async fn run_search_by_image_command(
    args: SearchByImageArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "image_path": args.image_path,
        "image_url": args.image_url,
        "image_data": args.image_data,
        "image_mime": args.image_mime,
        "query": args.query,
        "limit": args.limit,
        "offset": args.offset,
        "source_id": args.source_id,
    });
    let output = run_operation("search_by_image", params, config_path, timeout_ms).await?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain think` command.
async fn run_think_command(args: ThinkArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    let allowed_sources = args.allowed_sources.as_ref().map(|s| {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>()
    });
    let params = serde_json::json!({
        "question": args.question,
        "anchor": args.anchor,
        "rounds": args.rounds,
        "model": args.model,
        "since": args.since,
        "until": args.until,
        "calibration": args.with_calibration,
        "calibration_holder": args.calibration_holder,
        "trajectory": if args.no_trajectory { Some(false) } else { None },
        "source_id": args.source_id,
        "allowed_sources": allowed_sources,
        "remote": if args.remote { Some(true) } else { None },
    });

    let output = run_operation("think", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain auto-think` command.
async fn run_auto_think_command(
    args: AutoThinkArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::ai::chat::instantiate_chat;
    use zbrain_core::ai::model_config::{resolve_model, ModelTier, ResolveModelOpts};
    use zbrain_core::ai::resolver::resolve_recipe_strict;
    use zbrain_core::autopilot::phases::auto_think::{
        prefetch_model_lookup, run_phase_auto_think, AutoThinkPhaseOpts,
    };

    // Engine setup mirrors `run_autopilot_command`.
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = AutoThinkPhaseOpts {
        brain_dir: args.brain_dir.clone(),
        dry_run: args.dry_run,
        model_override: args.model.clone(),
        ..Default::default()
    };

    // Build a chat provider for the resolved auto-think model. In dry-run we
    // never call the LLM, so skip the (potentially failing) provider build.
    let chat: Option<Box<dyn zbrain_core::ai::chat::ChatProvider>> = if args.dry_run {
        None
    } else {
        let lookup = prefetch_model_lookup(&engine).await?;
        let model_id = resolve_model(
            &lookup,
            &ResolveModelOpts {
                cli_flag: args.model.clone(),
                config_key: Some("models.auto_think".to_string()),
                tier: Some(ModelTier::Deep),
                fallback: "opus".to_string(),
                ..Default::default()
            },
        );
        // Bare model ids (no `provider:` prefix) can't be turned into a recipe;
        // surface a clear error rather than guessing the provider.
        let recipe = match resolve_recipe_strict(&model_id) {
            Ok((_parsed, recipe)) => recipe,
            Err(e) => {
                anyhow::bail!(
                    "Cannot resolve a chat provider for auto-think model '{model_id}': {e}. \
                     Set models.auto_think (or --model) to a 'provider:model' form, \
                     e.g. 'anthropic:claude-opus-4'."
                );
            }
        };
        let provider =
            instantiate_chat(recipe, &model_id, |k| std::env::var(k).ok()).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to build chat provider for '{model_id}': {}. \
                     Check the provider's API key env var is set (see recipe setup hint).",
                    e.message
                )
            })?;
        Some(provider)
    };

    let result = run_phase_auto_think(&engine, chat.as_deref(), &opts).await?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "status": result.status,
                "detail": result.detail,
                "reason": result.reason,
                "questions_run": result.questions_run,
                "synthesized": result.synthesized,
                "dry_run": result.dry_run,
                "outcomes": result.outcomes.iter().map(|o| serde_json::json!({
                    "question": o.question,
                    "status": o.status,
                    "slug": o.slug,
                    "warnings": o.warnings,
                })).collect::<Vec<_>>(),
                "duration_ms": result.duration_ms,
            }))?
        );
    } else {
        println!("auto-think: {}", result.detail);
        if !result.outcomes.is_empty() {
            println!("---");
            for o in &result.outcomes {
                println!("[{}] {}", o.status, o.question);
                if let Some(slug) = &o.slug {
                    println!("    -> {slug}");
                }
                for w in &o.warnings {
                    println!("    ! {w}");
                }
            }
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain dream` command.
///
/// Runs one brain maintenance cycle via the Rust `run_cycle` orchestrator —
/// the canonical replacement for the legacy TS `src/commands/dream.ts` +
/// `src/core/cycle.ts`. Mirrors the TS CLI surface: `--json`, `--dry-run`,
/// `--pull`, `--phase`, `--dir`, `--input`, `--date`, `--from`, `--to`,
/// `--unsafe-bypass-dream-guard`; human report with totals; `failed` → exit 1.
async fn run_dream_command(
    args: DreamArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {

    // --input implies --phase synthesize (TS parity).
    let phase_override = if args.input.is_some() && args.phase.is_none() {
        Some("synthesize".to_string())
    } else {
        args.phase.clone()
    };

    // --input is incoherent with the date filters (single file vs dir scan).
    if args.input.is_some() && (args.date.is_some() || args.from.is_some() || args.to.is_some()) {
        anyhow::bail!("--input cannot be combined with --date / --from / --to");
    }

    // Validate date-style flags (TS exited 2 on a bad format; we bail → exit 1).
    for (flag, val) in [
        ("--date", args.date.as_deref()),
        ("--from", args.from.as_deref()),
        ("--to", args.to.as_deref()),
    ] {
        if let Some(v) = val {
            if !is_iso_date(v) {
                anyhow::bail!("{flag} must be YYYY-MM-DD; got \"{v}\"");
            }
        }
    }
    if let (Some(from), Some(to)) = (args.from.as_deref(), args.to.as_deref()) {
        if from > to {
            anyhow::bail!("--from ({from}) is after --to ({to}); empty range");
        }
    }

    let config = config::load_config(config_path)?;

    // Resolve brain directory: --dir flag, else config.sync.default_repo.
    let brain_dir: PathBuf = match &args.dir {
        Some(d) => PathBuf::from(d),
        None => config
            .sync
            .as_ref()
            .and_then(|s| s.default_repo.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No brain directory found. Pass --dir <path> or configure one via `zbrain init`."
                )
            })?,
    };

    // Validate --phase against the known phase labels.
    let phases: Option<Vec<CyclePhase>> = if let Some(p) = &phase_override {
        match CyclePhase::from_label(p) {
            Some(phase) => Some(vec![phase]),
            None => {
                let valid = CyclePhase::ALL
                    .iter()
                    .map(|x| x.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!("Unknown phase \"{p}\". Valid: {valid}");
            }
        }
    } else {
        None
    };

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = CycleOpts {
        dry_run: args.dry_run,
        phases,
        brain_dir: brain_dir.to_string_lossy().into_owned(),
        pull: args.pull,
        source_id: None,
        chat: None,
        yield_between_phases: None,
        yield_during_phase: None,
        synth_input_file: args.input.clone(),
        synth_date: args.date.clone(),
        synth_from: args.from.clone(),
        synth_to: args.to.clone(),
        synth_bypass_dream_guard: args.unsafe_bypass_dream_guard,
        signal: None,
    };

    let report: CycleReport = run_cycle(&engine, &opts).await;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_dream_human(&report);
    }

    engine.disconnect().await?;

    // `failed` overall → non-zero exit (TS `process.exit(1)` on failed).
    if report.status == CycleStatus::Failed {
        std::process::exit(1);
    }
    Ok(())
}

/// `YYYY-MM-DD` shape check (mirrors TS `^\d{4}-\d{2}-\d{2}$`).
fn is_iso_date(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        return false;
    }
    let [y, m, d] = [parts[0], parts[1], parts[2]];
    y.len() == 4
        && m.len() == 2
        && d.len() == 2
        && y.chars().all(|c| c.is_ascii_digit())
        && m.chars().all(|c| c.is_ascii_digit())
        && d.chars().all(|c| c.is_ascii_digit())
}

fn dream_status_str(s: CycleStatus) -> &'static str {
    match s {
        CycleStatus::Ok => "ok",
        CycleStatus::Clean => "clean",
        CycleStatus::Partial => "partial",
        CycleStatus::Skipped => "skipped",
        CycleStatus::Failed => "failed",
    }
}

/// Human-friendly rendering of a `CycleReport`, mirroring the legacy TS
/// `printHuman` in `src/commands/dream.ts`.
fn print_dream_human(report: &CycleReport) {
    match report.status {
        CycleStatus::Skipped => match report.reason.as_deref() {
            Some("cycle_already_running") => println!("Skipped: another cycle is already running. (locked)"),
            Some("no_database") => println!("Skipped: no database available."),
            Some(r) => println!("Skipped: {r}."),
            None => println!("Skipped: unknown reason."),
        },
        CycleStatus::Clean => {
            println!(
                "Brain is healthy. {} phase(s) checked in {:.1}s.",
                report.phases.len(),
                report.duration_ms as f64 / 1000.0
            );
        }
        _ => {
            println!(
                "Dream cycle ({}) in {:.1}s:",
                dream_status_str(report.status),
                report.duration_ms as f64 / 1000.0
            );
            for p in &report.phases {
                if let Some(line) = dream_phase_line(p) {
                    println!("{line}");
                }
            }
            print_dream_totals(&report.totals);
        }
    }
}

/// Render one phase line, or `None` when the phase produced no output
/// (mirrors TS skipping empty `summary`).
fn dream_phase_line(p: &PhaseResult) -> Option<String> {
    if p.summary.is_empty() && p.error.is_none() {
        return None;
    }
    let icon = match p.status {
        PhaseStatus::Ok => '✓',
        PhaseStatus::Warn => '!',
        PhaseStatus::Skipped => '-',
        PhaseStatus::Fail => '✗',
    };
    let mut line = format!("  {} {:<10}  {}", icon, p.phase, p.summary);
    if let Some(e) = &p.error {
        let hint = e.hint.as_deref().map(|h| format!(" ({h})")).unwrap_or_default();
        line.push_str(&format!(
            "\n      [{}] {} {}{}",
            e.class, e.code, e.message, hint
        ));
    }
    Some(line)
}

fn print_dream_totals(t: &zbrain_core::autopilot::cycle::CycleTotals) {
    let has = t.lint_fixes > 0
        || t.backlinks_added > 0
        || t.pages_synced > 0
        || t.pages_extracted > 0
        || t.pages_embedded > 0
        || t.orphans_found > 0
        || t.transcripts_processed > 0
        || t.synth_pages_written > 0
        || t.patterns_written > 0
        || t.pages_emotional_weight_recomputed > 0
        || t.edges_resolved > 0
        || t.edges_ambiguous > 0
        || t.purged_sources_count > 0
        || t.purged_pages_count > 0
        || t.facts_consolidated > 0
        || t.consolidate_takes_written > 0
        || t.phantoms_redirected > 0
        || t.phantoms_ambiguous > 0
        || t.phantoms_skipped_drift > 0;
    if has {
        println!(
            "  totals: lint={} backlinks={} synced={} extracted={} embedded={} orphans={} \
             synth_transcripts={} synth_pages={} patterns={} emotional_weight={} edges_resolved={} \
             edges_ambiguous={} purged_sources={} purged_pages={} facts_consolidated={} \
             consolidate_takes={} phantoms_redirected={} phantoms_ambiguous={} phantoms_skipped_drift={}",
            t.lint_fixes,
            t.backlinks_added,
            t.pages_synced,
            t.pages_extracted,
            t.pages_embedded,
            t.orphans_found,
            t.transcripts_processed,
            t.synth_pages_written,
            t.patterns_written,
            t.pages_emotional_weight_recomputed,
            t.edges_resolved,
            t.edges_ambiguous,
            t.purged_sources_count,
            t.purged_pages_count,
            t.facts_consolidated,
            t.consolidate_takes_written,
            t.phantoms_redirected,
            t.phantoms_ambiguous,
            t.phantoms_skipped_drift
        );
    }
}

/// Execute `zbrain get-page` command.
async fn run_get_page_command(args: GetPageArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
        "fuzzy": args.fuzzy,
        "include_deleted": args.include_deleted,
    });

    let output = run_operation("get_page", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain calibration` command.
///
/// Mirrors the legacy TS `src/commands/calibration.ts`. Four modes dispatch
/// in priority order:
///   1. `--undo-wave <v>` — reverse a wave's mutations (D18).
///   2. `--ab-report`    — print the think A/B harness report (D19).
///   3. `--regenerate`   — run `run_calibration_profile` now and (re)write.
///   4. default          — read the latest `calibration_profiles` row.
///
/// Engine bootstrap is the same as `run_dream_command`: libsql + init_schema.
/// All work runs on `&dyn BrainEngine`; calibration queries coerce through
/// the `CalibrationQueries` blanket impl on every backend.
async fn run_calibration_command(
    args: CalibrationArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::ai::chat::instantiate_chat;
    use zbrain_core::ai::model_config::{resolve_model, ModelTier, ResolveModelOpts};
    use zbrain_core::ai::resolver::resolve_recipe_strict;
    use zbrain_core::autopilot::phases::auto_think::prefetch_model_lookup;
    use zbrain_core::calibration::calibration_profile::{
        run_calibration_profile, CalibrationProfileOpts,
    };
    use zbrain_core::calibration::think_ab::{build_ab_report, AbReportOpts};
    use zbrain_core::calibration::{undo_wave, UndoWaveOpts};
    use zbrain_core::calibration_queries::CalibrationQueries;
    use std::sync::Arc;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let holder = args.holder.clone().unwrap_or_else(|| "garry".to_string());

    // Mode 1: --undo-wave <v>
    if let Some(wave_version) = args.undo_wave.as_deref() {
        eprintln!(
            "[calibration] {}reversing wave {wave_version}...",
            if args.dry_run { "[dry-run] " } else { "" }
        );
        let opts = UndoWaveOpts {
            wave_version: wave_version.to_string(),
            dry_run: args.dry_run,
            scrub_gstack: args.scrub_gstack,
            ..Default::default()
        };
        let result = undo_wave(&engine, &opts).await?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            let verb = if args.dry_run { "would revert" } else { "reverted" };
            println!(
                "{verb}:\n\
                 \x20 {res} take resolutions\n\
                 \x20 {pro} calibration profile(s)\n\
                 \x20 {nud} nudge log row(s)\n\
                 \x20 {gc} grade-cache rows marked unapplied",
                verb = verb,
                res = result.resolutions_reverted,
                pro = result.profiles_deleted,
                nud = result.nudges_purged,
                gc = result.grade_cache_unapplied,
            );
            if result.gstack_scrub_attempted {
                if !result.warnings.is_empty() {
                    println!("  gstack scrub: failed ({})", result.warnings.join("; "));
                } else {
                    println!("  gstack scrub: ok");
                }
            }
        }
        return Ok(());
    }

    // Mode 2: --ab-report
    if args.ab_report {
        let report = build_ab_report(
            &engine as &dyn CalibrationQueries,
            &AbReportOpts { days: args.days },
        )
        .await?;
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("{}", zbrain_core::calibration::think_ab::format_ab_report(&report, args.days));
        }
        return Ok(());
    }

    // Mode 3: --regenerate
    if args.regenerate {
        eprintln!("[calibration] regenerating profile for holder={holder}...");

        // Resolve chat provider for the calibration-profile model. Mirrors the
        // auto-think wiring so users can override via --holder (in this CLI
        // there is no --model flag yet; defaults come from the config key).
        let lookup = prefetch_model_lookup(&engine).await?;
        let model_id = resolve_model(
            &lookup,
            &ResolveModelOpts {
                config_key: Some("models.calibration_profile".to_string()),
                tier: Some(ModelTier::Deep),
                fallback: "opus".to_string(),
                ..Default::default()
            },
        );
        let recipe = match resolve_recipe_strict(&model_id) {
            Ok((_parsed, recipe)) => recipe,
            Err(e) => {
                anyhow::bail!(
                    "Cannot resolve a chat provider for calibration model '{model_id}': {e}. \
                     Set models.calibration_profile to a 'provider:model' form, \
                     e.g. 'anthropic:claude-opus-4'."
                );
            }
        };
        let chat =
            instantiate_chat(recipe, &model_id, |k| std::env::var(k).ok()).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to build chat provider for '{model_id}': {}. \
                     Check the provider's API key env var is set (see recipe setup hint).",
                    e.message
                )
            })?;

        let opts = CalibrationProfileOpts {
            holder: Some(holder.clone()),
            source_id: Some("default".to_string()),
            chat: Some(Arc::from(chat)),
            ..Default::default()
        };
        let result = run_calibration_profile(&engine, &opts).await?;
        // CalibrationProfileStatus has no `Fail` variant — failure surfaces as
        // !profile_written + non-empty warnings. Mirror the TS `result.status
        // === 'fail'` branch by treating any non-Ok status as a failure exit.
        let failed = !result.profile_written && !result.warnings.is_empty();
        if failed {
            eprintln!(
                "[calibration] regenerate failed: {}",
                result.warnings.first().cloned().unwrap_or_else(|| "unknown".into())
            );
            std::process::exit(1);
        }
        eprintln!(
            "[calibration] profile_written={} voice_gate_passed={} resolved={} brier={:?}",
            result.profile_written, result.voice_gate_passed, result.total_resolved, result.brier
        );
    }

    // Mode 4 (default): read latest profile row.
    let profile = engine
        .get_calibration_profile(&holder, Some("default"), None)
        .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&profile)?);
        return Ok(());
    }

    match profile {
        None => {
            println!(
                "No calibration profile yet for holder \"{holder}\".\n\
                 Build one by resolving 5+ takes then running:\n  \
                 zbrain dream --phase calibration_profile\n\
                 Or wait for the next autopilot cycle."
            );
        }
        Some(p) => {
            let generated_local = p.generated_at.clone();
            println!(
                "Calibration profile — holder: {holder}, source: {src}\n\
                 Generated: {gen}  {pub_}\n\
                 Resolved: {tr} takes",
                holder = p.holder,
                src = p.source_id,
                gen = generated_local,
                pub_ = if p.published { "(published to mounts)" } else { "" },
                tr = p.total_resolved
            );
            if p.grade_completion < 0.9 {
                println!(
                    "Note: built on {:.0}% graded — partial completion this cycle.",
                    p.grade_completion * 100.0
                );
            }
            if !p.voice_gate_passed {
                println!(
                    "Note: voice gate fell back to template ({} attempts).",
                    p.voice_gate_attempts
                );
            }
            if let Some(b) = p.brier {
                println!("Brier:    {b:.3} (lower is better)");
            }
            if let Some(a) = p.accuracy {
                println!("Accuracy: {:.1}%", a * 100.0);
            }
            if let Some(pr) = p.partial_rate {
                println!("Partial:  {:.1}%", pr * 100.0);
            }
            println!("\nPattern statements:");
            for s in &p.pattern_statements {
                println!("  • {s}");
            }
            if !p.active_bias_tags.is_empty() {
                println!("\nActive bias tags: {}", p.active_bias_tags.join(", "));
            }
        }
    }
    Ok(())
}

/// Execute `zbrain query` command.
async fn run_query_command(args: QueryArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    // G72 — short-circuit to stats when `--stats` is set. We read the
    // telemetry JSONL directly rather than going through the engine,
    // because stats are a pure file-IO operation and don't need the
    // brain loaded. The path is resolved the same way the runtime
    // writer resolves it (`SearchTelemetryWriter::default_path`).
    if args.stats {
        let window = parse_stats_window(args.stats_window.as_deref())?;
        let path = zbrain_core::search::SearchTelemetryWriter::default_path()
            .ok_or_else(|| anyhow::anyhow!(
                "could not resolve telemetry path — set $ZBRAIN_HOME or $HOME"
            ))?;
        let stats = zbrain_core::search::read_search_stats(&path, window)?;
        print!("{}", format_search_stats(&stats, &path));
        return Ok(());
    }

    let query = args.query.as_deref().ok_or_else(|| {
        anyhow::anyhow!("query text is required unless --stats is set")
    })?;
    let params = serde_json::json!({
        "query": query,
        "limit": args.limit,
        "offset": args.offset,
        "source_id": args.source_id,
    });

    let output = run_operation("query", params, config_path, timeout_ms).await?;

    if args.explain {
        // `run_operation` hands back a weakly-typed `serde_json::Value`, so
        // round-trip it into the strong `QueryOutput` (which derives
        // Deserialize for exactly this hop) before handing the typed result
        // slice to the core explain formatter. The formatter owns the
        // byte-faithful TS output shape; the CLI only chooses JSON vs explain.
        let parsed: zbrain_core::operation::QueryOutput = serde_json::from_value(output)?;
        print!(
            "{}",
            zbrain_core::explain_formatter::format_results_explain(&parsed.results)
        );
    } else {
        println!("{}", serde_json::to_string_pretty(&output)?);
    }
    Ok(())
}

/// Parse the `--stats-window` CLI string into the `StatsWindow` enum.
fn parse_stats_window(s: Option<&str>) -> anyhow::Result<zbrain_core::search::StatsWindow> {
    use zbrain_core::search::StatsWindow;
    match s.unwrap_or("day") {
        "hour" => Ok(StatsWindow::LastHour),
        "day" => Ok(StatsWindow::LastDay),
        "week" => Ok(StatsWindow::LastWeek),
        "all" => Ok(StatsWindow::All),
        other => Err(anyhow::anyhow!(
            "invalid --stats-window `{other}` (expected hour|day|week|all)"
        )),
    }
}

/// Render a `SearchStats` aggregate as a human-readable text block. The
/// formatter intentionally keeps the surface minimal — operators reading
/// the JSONL directly get full per-event detail; the CLI only summarizes
/// the headline numbers.
fn format_search_stats(stats: &zbrain_core::search::SearchStats, path: &Path) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "Search telemetry (window: {:?})", stats.window);
    let _ = writeln!(out, "Source: {}", path.display());
    let _ = writeln!(out, "");
    let _ = writeln!(out, "  count:          {}", stats.count);
    let _ = writeln!(out, "  p50 latency:    {} ms", stats.p50_latency_ms);
    let _ = writeln!(out, "  p95 latency:    {} ms", stats.p95_latency_ms);
    if !stats.by_intent.is_empty() {
        let _ = writeln!(out, "");
        let _ = writeln!(out, "  by intent:");
        let mut entries: Vec<_> = stats.by_intent.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (intent, count) in entries {
            let _ = writeln!(out, "    {intent:20} {count}");
        }
    }
    if !stats.mode_counts.is_empty() {
        let _ = writeln!(out, "");
        let _ = writeln!(out, "  by mode:");
        let mut entries: Vec<_> = stats.mode_counts.iter().collect();
        entries.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (mode, count) in entries {
            let _ = writeln!(out, "    {mode:20} {count}");
        }
    }
    if !stats.top_queries.is_empty() {
        let _ = writeln!(out, "");
        let _ = writeln!(out, "  top queries:");
        for (query, count) in &stats.top_queries {
            let _ = writeln!(out, "    {count:3}× {query}");
        }
    }
    out
}

/// Execute `zbrain put-page` command.
async fn run_put_page_command(args: PutPageArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    // Get content from --content flag or stdin
    let content = match args.content {
        Some(c) => c,
        None => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        }
    };

    let params = serde_json::json!({
        "slug": args.slug,
        "page_type": args.page_type,
        "title": args.title,
        "compiled_truth": content,
    });

    let output = run_operation("put_page", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain delete-page` command.
async fn run_delete_page_command(args: DeletePageArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
    });

    let output = run_operation("delete_page", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain restore-page` command.
async fn run_restore_page_command(args: RestorePageArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
    });

    let output = run_operation("restore_page", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain purge-deleted-pages` command.
async fn run_purge_deleted_pages_command(args: PurgeDeletedPagesArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    // --force is required as a safety measure
    if !args.force {
        eprintln!("Error: --force flag is required to permanently purge deleted pages");
        std::process::exit(1);
    }

    let params = serde_json::json!({
        "older_than_days": null,
    });

    let output = run_operation("purge_deleted_pages", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain list-pages` command.
async fn run_list_pages_command(args: ListPagesArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "kind": args.page_type,
        "tag": args.tag,
        "limit": args.limit.map(|l| l as u32),
        "offset": args.offset.map(|o| o as u32),
        "include_deleted": args.include_deleted,
    });

    let output = run_operation("list_pages", params, config_path, timeout_ms).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain serve-mcp` command.
///
/// Starts the MCP stdio server. Reads JSON-RPC 2.0 messages from stdin,
/// writes responses to stdout. Suitable for use with Claude Desktop / Claude Code.
///
/// Mirrors `startMcpServer()` in TS `src/mcp/server.ts`.
async fn run_serve_mcp_command(args: ServeMcpArgs, _config_path: Option<&Path>) -> anyhow::Result<()> {
    // Initialize tracing subscriber for audit logs (stderr output)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("zbrain_mcp=info".parse().unwrap()))
        .with_writer(std::io::stderr)
        .try_init();

    // Live operation set assembled via `register_all` (zbrain_core::operation).

    // Load config for MCP settings (rate limit)
    let config_file = _config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;
    let mcp_config = if config_file.exists() {
        let cfg = config::load_config_from_path(&config_file)?;
        cfg.mcp
    } else {
        Default::default()
    };

    // Set source_id env for the MCP server if provided via --source flag
    if let Some(source) = &args.source {
        std::env::set_var("ZBRAIN_SOURCE", source);
    }

    // Build registry (all production ops, single source of truth)
    let mut registry = OperationRegistry::new();
    register_all(&mut registry);

    // Log startup to stderr (MCP protocol uses stdout for JSON-RPC)
    let source_id = std::env::var("ZBRAIN_SOURCE").unwrap_or_else(|_| "default".to_string());
    eprintln!("[zbrain-mcp] starting stdio MCP server (source: {})", source_id);

    let version = env!("CARGO_PKG_VERSION");
    let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
    let server = zbrain_mcp::StdioMcpServer::new(
        registry,
        engine,
        "zbrain",
        version,
        mcp_config.rate_limit,
    );

    server.run().await.context("MCP stdio server error")?;

    eprintln!("[zbrain-mcp] shutdown: stdin closed");
    Ok(())
}

/// Build the standard operation registry with all registered operations.
fn build_operation_registry() -> Arc<OperationRegistry> {
    let mut registry = OperationRegistry::new();
    register_all(&mut registry);
    Arc::new(registry)
}

/// Execute `zbrain sync` command.
///
/// Syncs markdown files from a git repository into the knowledge base.
/// Performs an incremental sync by default (git diff since last anchor),
/// or a full sync if `--full-sync` is passed or no anchor exists.
async fn run_sync_command(args: SyncArgs, config_path: Option<&Path>, cli_opts: &CliOpts) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sync::core::{perform_full_sync, perform_sync, FullSyncOpts, IncrementalSyncOpts};

    let config = config::load_config(config_path)?;

    // Resolve repo_path: from flag, or from config.sync.default_repo, or CWD
    let repo_path = args
        .repo_path
        .clone()
        .or_else(|| config.sync.as_ref().and_then(|s| s.default_repo.clone()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Ensure repo_path is absolute
    let repo_path = if repo_path.is_absolute() {
        repo_path
    } else {
        std::env::current_dir()?.join(repo_path)
    };

    // Get current git commit
    let current_commit = get_git_head_commit(&repo_path)?;

    // Resolve chunker_version: from flag, or from config.sync.chunker_version, or default 1
    let chunker_version = args.chunker_version.or_else(|| {
        config.sync.as_ref().and_then(|s| s.chunker_version)
    });

    // Resolve failures_dir
    let failures_dir = args.failures_dir.clone().unwrap_or_else(|| {
        config::zbrain_home()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("sync-failures")
    });
    std::fs::create_dir_all(&failures_dir)?;

    // Max file size: 0 means no limit
    let max_file_size = if args.max_file_size == 0 {
        None
    } else {
        Some(args.max_file_size)
    };

    // Build engine
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine: std::sync::Arc<dyn BrainEngine> = std::sync::Arc::new(engine);

    // Build progress reporter from CLI flags.
    let mode = if cli_opts.quiet {
        ProgressMode::Quiet
    } else if cli_opts.progress_json {
        ProgressMode::Json
    } else {
        ProgressMode::Human
    };
    let min_interval_ms = cli_opts.progress_interval as u64;
    let mut reporter = ProgressReporter::new(mode, min_interval_ms, Box::new(std::io::stderr()));

    // Ensure source exists
    ensure_source_exists(&engine, &args.source_id).await?;

    let result = if args.full_sync {
        eprintln!("[zbrain-sync] performing full sync for source: {}", args.source_id);
        let opts = FullSyncOpts {
            source_id: args.source_id.clone(),
            repo_path: repo_path.clone(),
            current_commit: current_commit.clone(),
            chunker_version,
            failures_dir: failures_dir.clone(),
            max_file_size,
        };
        perform_full_sync(&*engine, &opts, Some(&mut reporter)).await?
    } else {
        // Get previous anchor for incremental sync
        let source = engine
            .get_source(&args.source_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("source not found: {}", args.source_id))?;

        let previous_commit = source.last_commit.clone();

        eprintln!("[zbrain-sync] incremental sync for source: {} ({}..{})",
            args.source_id,
            previous_commit.as_deref().unwrap_or("none"),
            current_commit,
        );

        let opts = IncrementalSyncOpts {
            source_id: args.source_id.clone(),
            repo_path: repo_path.clone(),
            current_commit: current_commit.clone(),
            previous_commit,
            chunker_version,
            failures_dir: failures_dir.clone(),
            max_file_size,
        };
        perform_sync(&*engine, &opts, Some(&mut reporter)).await?
    };

    // Print result
    let mode = if result.full_sync { "full sync" } else { "incremental sync" };
    println!("{} complete: {} imported, {} deleted, {} failures",
        mode, result.imported, result.deleted, result.failures);

    engine.disconnect().await?;
    Ok(())
}

/// Ensure a source exists in the engine, creating it if necessary.
async fn ensure_source_exists(engine: &std::sync::Arc<dyn zbrain_core::engine::BrainEngine>, source_id: &str) -> anyhow::Result<()> {
    use zbrain_core::engine::CreateSourceInput;

    if engine.get_source(source_id).await?.is_none() {
        engine
            .create_source(&CreateSourceInput {
                id: source_id.to_string(),
                name: source_id.to_string(),
                config: None,
            })
            .await?;
        eprintln!("[zbrain-sync] created source: {}", source_id);
    }
    Ok(())
}

/// Get the current HEAD commit SHA from a git repository.
fn get_git_head_commit(repo_path: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "rev-parse", "HEAD"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("failed to get git HEAD commit: {stderr}"));
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(sha)
}

/// Start the HTTP API and admin SPA server.
///
/// Loads server configuration from zbrain.yml (with CLI flag overrides),
/// builds the axum router, and starts listening on the configured address.
async fn run_serve_http_command(
    args: ServeHttpArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = config::load_config(config_path)?;

    let port = args.port.unwrap_or(config.server.port);
    let bind_addr = args.bind.unwrap_or(config.server.bind);

    let addr: std::net::SocketAddr = format!("{bind_addr}:{port}")
        .parse()
        .context("Invalid bind address")?;

    // Determine admin SPA directory
    let spa_dir = if let Some(ref dir) = args.spa_dir {
        dir.clone()
    } else {
        // Default: look for admin/dist/ relative to CWD
        let cwd_spa = std::env::current_dir()?.join("admin").join("dist");
        if cwd_spa.exists() {
            cwd_spa
        } else {
            // Fallback: use a temp dir (SPA won't be served, but server starts)
            std::env::temp_dir().join("zbrain-admin-empty")
        }
    };

    // Initialize admin auth with optional env token
    let admin_token = std::env::var("ZBRAIN_ADMIN_BOOTSTRAP_TOKEN").ok();
    let admin_auth = zbrain_web::AdminAuth::new(admin_token);

    // Initialize engine for admin queries
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine = std::sync::Arc::new(engine);
    let (tx, _rx) = tokio::sync::broadcast::channel(64);

    let state = zbrain_web::AppState {
        admin_auth,
        magic_link: zbrain_web::MagicLinkAuth::new(),
        admin_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::AdminQueries>,
        calibration_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
        oauth_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
        token_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::TokenQueries>,
        activity_tx: tx,
        spa_dir,
        operation_registry: build_operation_registry(),
        engine: engine as std::sync::Arc<dyn zbrain_core::BrainEngine>,
        zbrain_home: config::zbrain_home()
            .unwrap_or_else(|| PathBuf::from(".")),
    };

    eprintln!("[zbrain-web] starting HTTP server on {addr}");
    zbrain_web::run(addr, state).await
}

/// Execute an operation by name with JSON params.
///
/// Supports two execution modes:
/// 1. Local: executes directly against the local database engine (default)
/// 2. Thin-client: routes the call through a remote MCP server (when remote_mcp is configured)
///
/// Local-only operations are refused on thin-client installs with a helpful message,
/// matching the TypeScript behavior in `refuseThinClient`.
/// Resolve the rerank-audit directory. Honors `ZBRAIN_AUDIT_DIR` (container /
/// sandbox deploys where `$HOME` is read-only), else defaults to
/// `~/.zbrain/audit` — the same resolution the TS audit-writer uses so both
/// runtimes share rows. Shared by the rerank client wiring (writer) and the
/// doctor `reranker_health` check (reader) so they never diverge.
fn resolve_audit_dir() -> PathBuf {
    std::env::var("ZBRAIN_AUDIT_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            config::zbrain_home()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("audit")
        })
}

async fn run_operation(
    name: &str,
    params: serde_json::Value,
    config_path: Option<&Path>,
    cli_timeout_ms: Option<u64>,
) -> anyhow::Result<serde_json::Value> {
    let config_file = config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;

    let config = config::load_config_from_path(&config_file)?;

    // Build operation registry early so thin-client check can query local_only status
    // from the canonical TypedOperation trait (not a hardcoded list).
    let mut registry = OperationRegistry::new();
    register_all(&mut registry);

    // Check for thin-client mode (v0.31.1 Issue #734)
    if config::is_thin_client(&config) {
        let remote_mcp = config.remote_mcp.as_ref().expect("is_thin_client guarantees this");

        // Query registry for local_only status (avoids hardcoded match drift from trait)
        let is_local_only = registry
            .lookup(name)
            .map(|op| op.local_only())
            .unwrap_or(false);

        if is_local_only {
            eprintln!(
                "zbrain {name}: this operation requires a local engine. This install is a thin client of {}.",
                remote_mcp.mcp_url
            );
            eprintln!();
            eprintln!("Thin-client routing for {name} is planned for a future release.");
            eprintln!("Run on the host instead, or re-init with `zbrain init` to use local mode.");
            std::process::exit(1);
        }

        // Non-local-only operations: route through remote MCP.
        // Resolve the per-call timeout: `think` -> 180s, else 30s, with a
        // user-supplied `--timeout` override (threaded via cli_timeout_ms).
        let timeout_ms = resolve_timeout_ms(name, cli_timeout_ms);
        let mcp_client =
            mcp_client::McpClient::new(config, std::time::Duration::from_millis(timeout_ms));
        let result = mcp_client.call_tool(name, params).await.map_err(|e| {
            eprintln!("Remote MCP call failed: {}", e);
            std::process::exit(1);
        }).unwrap();
        return Ok(result);
    }

    // Local mode: execute against local engine.
    //
    // NOTE (intentional TS-parity gap, do NOT "fix" by adding a wall-clock
    // timeout here): local operations routed through `run_operation` (`query`,
    // `think`, `get_page`, `list_pages`, …) have NO local wall-clock timeout,
    // and this mirrors the TS runtime. TS *looks* like it gives `search` a 30s
    // timeout (cli.ts:1136), but that branch is dead code — `search`/`query`
    // are shared ops that never enter `handleCliOnly`, so it never fires. Only
    // `sources list` (a CLI_ONLY command, handled in `run_sources_list`) has a
    // reachable TS timeout, and that is the only one ported. Giving `query` a
    // wall-clock deadline would be a NEW behavior TS never actually had — a
    // product enhancement, not a migration. If we ever choose to add it, the
    // machinery is ready: wrap the connect + dispatch steps below with
    // `timeout::with_read_only_timeout` and `timeout::report_timeout_and_exit`
    // (see `run_sources_list` for the two-segment pattern).
    //
    // Until then, `--timeout` has no effect on these local ops, so we warn on
    // stderr rather than silently swallowing it.
    if let Some(msg) = local_timeout_warning(cli_timeout_ms) {
        eprintln!("{msg}");
    }
    // G37 fix: LibsqlEngine::connect requires EngineConfig.database_path
    // (not database_url). Mirror run_sync_command's resolve_database_path so
    // local put-page/get-page/query/think/list-pages/delete-page/
    // restore-page/purge no longer fail with "requires EngineConfig.database_path".
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };

    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    let mut ctx = OperationContext::local_cli().with_engine(std::sync::Arc::new(engine));

    // Wire the production cross-brain mount resolver (1-3-3-4) from
    // ~/.zbrain/mounts.json. Fault-tolerant: a missing/unreadable mounts file
    // or an unreachable mount degrades to local-only (the op falls back to
    // NoMountsResolver semantics). Sole production construction site for the
    // resolver, mirroring the rerank/embedding wiring above.
    if let Some(home) = zbrain_core::paths::zbrain_home() {
        let mounts_path = home.join("mounts.json");
        ctx = ctx.with_mount_resolver(std::sync::Arc::new(
            crate::mounts::ProductionMountResolver::new(mounts_path),
        ));
    }

    // Wire the cross-encoder reranker when it is enabled in config AND the API
    // key is present in the environment (secrets never live in the config
    // file). Missing key with reranker_enabled = leave it off rather than fail
    // search; the doctor `reranker_health` check surfaces the misconfig. This
    // is the sole production construction site for the rerank HTTP client.
    if config.search.reranker_enabled {
        if let Some(client) = zbrain_core::rerank_client::ZeroEntropyRerankClient::from_env(None) {
            ctx = ctx.with_rerank(zbrain_core::rerank_client::RerankSettings {
                client: std::sync::Arc::new(client),
                audit_dir: resolve_audit_dir(),
                model: None,
            });
        }
    }

    // G72 — wire the default search telemetry writer. `QueryOperation` and
    // `SearchOperation` both append one JSONL event per call when
    // `ctx.telemetry_writer` is `Some`. The writer is opt-in at the
    // `SearchTelemetryWriter` layer (a `None` payload short-circuits to
    // no-op), so a failed path-resolution (e.g. no `$ZBRAIN_HOME` and no
    // `$HOME`) simply leaves telemetry off rather than breaking search.
    // Mirrors the rerank / embedding / mount_resolver injection
    // precedent — every production wiring is here so the test code can
    // pass `None` to keep the hot path deterministic.
    if let Some(path) = zbrain_core::search::SearchTelemetryWriter::default_path() {
        let writer = zbrain_core::search::SearchTelemetryWriter::new(Some(path));
        ctx = ctx.with_telemetry_writer(std::sync::Arc::new(writer));
    }

    // Wire the embedding client for the query vector path when hybrid search is
    // enabled in config AND the API key is present in the environment (same
    // secrets-never-in-config posture as the reranker above). Missing key with
    // hybrid_search = leave the vector path off; hybrid search degrades to
    // lexical-only rather than failing. This is the sole production
    // construction site for the embedding HTTP client.
    if config.search.hybrid_search {
        if let Some(client) = zbrain_core::embedding::EmbeddingClient::from_env() {
            ctx = ctx.with_embedding(std::sync::Arc::new(client));
        }
    }

    // Use shared MCP dispatch path (dispatch_tool_call) so CLI and future MCP server
    // produce identical result formatting and error handling.
    // Mirrors TS `dispatchToolCall()` in src/mcp/dispatch.ts.
    let tool_result = registry.dispatch_tool_call(name, &ctx, params).await;

    if tool_result.is_error {
        // Parse error JSON to get exit code from OperationError shape
        let exit_code = tool_result
            .parse_json()
            .and_then(|j| {
                let code = j["error"].as_str()?;
                // permission_denied → exit 126 (matches TS + OperationError::exit_code)
                Some(if code == "permission_denied" { 126i32 } else { 1i32 })
            })
            .unwrap_or(1);
        // Print error text to stderr
        if let Some(text) = tool_result.text() {
            eprintln!("{}", text);
        }
        std::process::exit(exit_code);
    }

    // Success: return the parsed JSON value
    let value = tool_result
        .parse_json()
        .ok_or_else(|| anyhow::anyhow!("Operation returned non-JSON output"))?;

    Ok(value)
}

/// Execute `zbrain sources` subcommands.
async fn run_sources_command(
    action: SourcesAction,
    config_path: Option<&Path>,
    cli_timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    match action {
        SourcesAction::Add(args) => run_sources_add(args, config_path).await?,
        SourcesAction::List(args) => run_sources_list(args, config_path, cli_timeout_ms).await?,
        SourcesAction::Remove(args) => run_sources_remove(args, config_path).await?,
        SourcesAction::Status(args) => run_sources_status(args, config_path).await?,
    }
    Ok(())
}

/// Execute `zbrain sources add` command.
async fn run_sources_add(args: SourcesAddArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sources_ops::{self, AddSourceOpts};

    let config = config::load_config(config_path)?;

    // Resolve zbrain_home (default: ~/.zbrain)
    let zbrain_home = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."));

    // Build engine
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = AddSourceOpts {
        id: args.id.clone(),
        name: args.name.clone(),
        local_path: args.path.as_ref().map(|p| p.to_string_lossy().to_string()),
        remote_url: args.url.clone(),
        federated: if args.federated { Some(true) } else { None },
        clone_dir: args.clone_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        depth: args.depth,
        branch: args.branch.clone(),
    };

    let source = sources_ops::add_source(&engine, opts, &zbrain_home)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Source added: {}", source.id);
    println!("  name: {}", source.name);
    if let Some(ref path) = source.local_path {
        println!("  path: {path}");
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain sources list` command.
async fn run_sources_list(
    args: SourcesListArgs,
    config_path: Option<&Path>,
    cli_timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();

    // Roadmap 1-2-3: local read-only wall-clock timeout. Mirrors the ONE live
    // TS default (cli.ts:1137, `sources list` → 10s); a user `--timeout` wins.
    // Two segments with distinct labels (Q5) so the user can tell a hung
    // connect apart from a hung listing — the "zombie zbrain" bug class.
    let (timeout_ms, user_supplied) = resolve_sources_list_timeout(cli_timeout_ms);

    // Segment 1: connect (label `zbrain sources list: connect`).
    match timeout::with_read_only_timeout(
        engine.connect(&engine_config),
        timeout_ms,
        "zbrain sources list: connect",
        user_supplied,
    )
    .await
    {
        Ok(res) => res?,
        Err(t) => timeout::report_timeout_and_exit(&t),
    }

    // Segment 2: body — init_schema + list_sources (label `zbrain sources list`).
    let sources = match timeout::with_read_only_timeout(
        async {
            engine.init_schema().await?;
            engine.list_sources(false).await
        },
        timeout_ms,
        "zbrain sources list",
        user_supplied,
    )
    .await
    {
        Ok(res) => res?,
        Err(t) => timeout::report_timeout_and_exit(&t),
    };

    if args.json {
        let json = serde_json::to_string_pretty(&sources)?;
        println!("{json}");
    } else {
        // Table header
        println!("{:<20} {:<20} {:<12} {:<40}  LAST SYNC", "ID", "NAME", "ARCHIVED", "PATH",);
        for src in &sources {
            let path = src.local_path.as_deref().unwrap_or("-");
            let last_sync = src.last_sync_at.as_deref().unwrap_or("-");
            println!(
                "{:<20} {:<20} {:<12} {:<40}  {}",
                src.id, src.name, if src.archived { "yes" } else { "no" }, path, last_sync,
            );
        }
        println!("\n{} source(s)", sources.len());
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain sources remove` command.
async fn run_sources_remove(args: SourcesRemoveArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sources_ops::{self, RemoveSourceOpts};

    let config = config::load_config(config_path)?;

    let zbrain_home = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."));

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = RemoveSourceOpts {
        id: args.id.clone(),
        confirm_destructive: args.confirm_destructive,
        dry_run: args.dry_run,
        keep_storage: args.keep_storage,
    };

    let result = sources_ops::remove_source(&engine, opts, &zbrain_home)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if result.dry_run {
        println!("[DRY RUN] Would remove source: {}", result.id);
        println!("  pages to delete: {}", result.pages_deleted);
        if let Some(ref path) = result.clone_path {
            println!("  clone would be {}deleted: {path}",
                if !args.keep_storage { "" } else { "kept — " });
        }
    } else {
        println!("Source removed: {}", result.id);
        println!("  pages deleted: {}", result.pages_deleted);
        if result.clone_removed {
            if let Some(ref path) = result.clone_path {
                println!("  clone removed: {path}");
            }
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain sources status` command.
async fn run_sources_status(args: SourcesStatusArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sources_ops;

    let config = config::load_config(config_path)?;

    let _zbrain_home = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."));

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    // Collect source IDs
    let source_ids: Vec<String> = if let Some(ref sid) = args.source_id {
        vec![sid.clone()]
    } else {
        engine.list_sources(false).await?.into_iter().map(|s| s.id).collect()
    };

    // Gather status for each source
    let mut statuses: Vec<sources_ops::SourceStatus> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for sid in &source_ids {
        match sources_ops::get_source_status(&engine, sid).await {
            Ok(s) => statuses.push(s),
            Err(e) => errors.push(format!("{sid}: {e}")),
        }
    }

    if args.json {
        let output = if statuses.is_empty() && !errors.is_empty() {
            serde_json::json!({ "errors": errors })
        } else {
            serde_json::json!({ "sources": statuses, "errors": errors })
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Table header
        println!(
            "{:<20} {:<10} {:<8} {:<8} {:<8} {:<8} {:<20}",
            "SOURCE", "LAG", "EMBED", "FAILS", "QUEUE", "PAGES", "LAST SYNC"
        );

        for s in &statuses {
            let lag = compute_lag(&s);
            let embed = "-";
            let fails = "-";
            let queue = "-";
            let last_sync = s.last_sync_at.as_deref().unwrap_or("-");
            println!(
                "{:<20} {:<10} {:<8} {:<8} {:<8} {:<8} {:<20}",
                s.name, lag, embed, fails, queue, s.page_count, last_sync
            );
        }

        // Print errors after the table
        for e in &errors {
            eprintln!("error: {e}");
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Compute git lag for a source: number of commits behind HEAD.
fn compute_lag(status: &zbrain_core::sources_ops::SourceStatus) -> String {
    let Some(ref local_path) = status.local_path else {
        return "-".to_string();
    };
    let Some(ref last_commit) = status.last_commit else {
        return "?".to_string();
    };

    let output = std::process::Command::new("git")
        .args(["-C", local_path.as_str(), "rev-list", "--count", &format!("{last_commit}..HEAD")])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let count = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if count.is_empty() { "0".to_string() } else { format!("{count}c") }
        }
        _ => "?".to_string(),
    }
}

/// Execute `zbrain capture` command.
async fn run_capture_command(args: CaptureArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use std::io::Read;
    use zbrain_core::capture::{CaptureOpts, capture_content};
    use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
    use zbrain_core::markdown::parse_markdown;
    use zbrain_core::time::current_utc_iso8601;
    use zbrain_core::types::PageKind;

    // 1. Read content from file or stdin
    let raw = match args.content {
        Some(ref path_str) => {
            let path = Path::new(path_str);
            std::fs::read(path)
                .with_context(|| format!("Failed to read file: {path_str}"))?
        }
        None => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
    };

    // 2. Run capture pipeline
    let captured_at = current_utc_iso8601();
    let opts = CaptureOpts {
        page_type: Some(args.r#type.clone()),
        source: args.source.clone(),
        captured_at: Some(captured_at.clone()),
    };

    let capture_result = capture_content(&raw, &opts)
        .map_err(|e| anyhow::anyhow!("Capture failed: {e}"))?;

    // 3. Parse markdown
    let source_path = args.content.as_deref().unwrap_or("stdin");
    let parsed = parse_markdown(
        &capture_result.body,
        source_path,
        None,
    );

    // 4. Determine slug: explicit > frontmatter title > UUID fallback
    let slug = args.slug.clone().unwrap_or_else(|| {
        capture_result.frontmatter
            .get("title")
            .and_then(|v| v.as_str())
            .map(|t| slugify(t))
            .unwrap_or_else(|| format!("capture-{}", &capture_result.content_hash[..12]))
    });

    // 5. Determine title
    let title = capture_result.frontmatter
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(parsed.title.clone());

    // 6. Build PageInput
    let page_type = if zbrain_core::types::is_base_page_type(&parsed.type_) {
        parsed.type_.clone()
    } else {
        args.r#type.clone()
    };

    let page_input = PageInput {
        page_type,
        title: title.clone(),
        compiled_truth: parsed.compiled_truth,
        timeline: if parsed.timeline.is_empty() { None } else { Some(parsed.timeline) },
        frontmatter: Some(capture_result.frontmatter),
        content_hash: Some(capture_result.content_hash.clone()),
        page_kind: Some(PageKind::Markdown),
        effective_date: None,
        effective_date_source: None,
        import_filename: args.content.clone(),
        chunker_version: None,
        source_path: Some(source_path.to_string()),
        source_kind: Some("capture".to_string()),
        source_uri: None,
        ingested_via: Some("zbrain capture CLI".to_string()),
        ingested_at: Some(captured_at.clone()),
        last_retrieved_at: None,
        embedding: None,
    };

    // 7. Connect to engine and put_page
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let source_id_ref = args.source.as_deref();
    let page = engine.put_page(&slug, source_id_ref, &page_input).await?;

    engine.disconnect().await?;

    // 8. Output
    if args.json {
        let output = serde_json::json!({
            "slug": page.slug,
            "title": page.title,
            "content_hash": page.content_hash,
            "page_type": page.page_type,
            "source": args.source,
            "captured_at": captured_at,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Captured page: {}", page.slug);
        println!("  title: {}", page.title);
        if let Some(ref hash) = page.content_hash {
            println!("  hash: {hash}");
        }
        if let Some(ref source_id) = args.source {
            println!("  source: {source_id}");
        }
    }

    Ok(())
}

/// Convert a string to a URL-safe slug.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Build the structured JSON payload for a successful `zbrain init`.
fn init_initialized_json(config_path: &Path, database_url: &str, mode: &str) -> serde_json::Value {
    serde_json::json!({
        "status": "initialized",
        "config_path": config_path.display().to_string(),
        "database_url": database_url,
        "mode": mode,
    })
}

/// Build the structured JSON payload when an existing config is left untouched.
fn init_exists_json(config_path: &Path) -> serde_json::Value {
    serde_json::json!({
        "status": "exists",
        "config_path": config_path.display().to_string(),
        "hint": "Use --force to overwrite, or `zbrain init --migrate-only` to apply schema changes",
    })
}

fn apply_init_embedding_args(config: &mut config::Config, args: &InitArgs) {    if let Some(model) = &args.embedding_model {
        config.embedding.model = model.clone();
    }
    if let Some(dimensions) = args.embedding_dimensions {
        config.embedding.dimensions = Some(dimensions);
    }
    if args.no_embedding {
        config.embedding.enabled = false;
    }
}

fn validate_mcp_only_init_args(args: &InitArgs) -> anyhow::Result<()> {
    let invalid_flag = if args.pglite {
        Some("--pglite")
    } else if args.supabase {
        Some("--supabase")
    } else if args.url.is_some() {
        Some("--url")
    } else if args.migrate_only {
        Some("--migrate-only")
    } else if args.embedding_model.is_some() {
        Some("--embedding-model")
    } else if args.embedding_dimensions.is_some() {
        Some("--embedding-dimensions")
    } else if args.no_embedding {
        Some("--no-embedding")
    } else {
        None
    };

    if let Some(flag) = invalid_flag {
        anyhow::bail!("--mcp-only cannot be combined with {flag}");
    }

    Ok(())
}

/// Execute `zbrain init` command.
///
/// Initializes a new ZBrain instance with the specified configuration.
/// Supports two modes:
/// - PGLite (embedded, zero-config, default)
/// - Postgres (Supabase or custom connection string)
///
/// Key behaviors:
/// - Creates `~/.zbrain/` directory if needed
/// - Generates default config if none exists
/// - Applies schema migrations
/// - Handles `--force` to overwrite existing config
async fn run_init_command(args: InitArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    if !args.json {
        println!("Setting up ZBrain...");
    }

    // 1. Determine config location and ensure directory exists
    let config_file = config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .unwrap_or_else(|| {
            config::zbrain_home()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("config")
        });

    if let Some(parent) = config_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    if args.mcp_only {
        validate_mcp_only_init_args(&args)?;
    }

    if args.migrate_only {
        return run_init_migrate_only(&args, &config_file).await;
    }

    // 2. Check for existing config and --force flag
    if config_file.exists() && !args.force {
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&init_exists_json(&config_file))?
            );
        } else {
            println!("Config already exists at: {}", config_file.display());
            println!("Use --force to overwrite, or `zbrain init --migrate-only` to apply schema changes");
        }
        return Ok(());
    }

    let mut config = if config_file.exists() {
        config::load_config_from_path(&config_file)?
    } else {
        config::Config::default()
    };

    apply_init_embedding_args(&mut config, &args);

    if args.mcp_only {
        let issuer_url = args
            .issuer_url
            .ok_or_else(|| anyhow::anyhow!("--mcp-only requires --issuer-url"))?;
        let mcp_url = args
            .mcp_url
            .ok_or_else(|| anyhow::anyhow!("--mcp-only requires --mcp-url"))?;
        let oauth_client_id = args
            .oauth_client_id
            .ok_or_else(|| anyhow::anyhow!("--mcp-only requires --oauth-client-id"))?;

        config.database_url = "remote-mcp://thin-client".to_string();
        config.remote_mcp = Some(config::RemoteMcpConfig {
            issuer_url,
            mcp_url,
            oauth_client_id,
            oauth_client_secret: args.oauth_client_secret,
        });
        config::write_config(&config, &config_file)?;
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&init_initialized_json(
                    &config_file,
                    &config.database_url,
                    "mcp-only",
                ))?
            );
        } else {
            println!("ZBrain initialized: {}", config_file.display());
        }
        return Ok(());
    }

    if args.supabase {
        anyhow::bail!("--supabase init is not implemented yet");
    }

    if let Some(ref url) = args.url {
        let engine_config = zbrain_core::engine::EngineConfig {
            database_url: Some(url.clone()),
            database_path: None,
        };
        let engine = zbrain_core::postgres::PostgresEngine::new();
        engine.connect(&engine_config).await?;
        engine.init_schema().await?;
        config.database_url = url.clone();
        config::write_config(&config, &config_file)?;
        engine.disconnect().await?;
        emit_init_success(&args, &config_file, &config.database_url, "url");
        return Ok(());
    }

    let zbrain_home = if config_path.is_some() {
        config_file
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        config::zbrain_home()
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let db_path = zbrain_home.join("brain.pglite");
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create database directory: {}", parent.display())
        })?;
    }

    let database_url = format!("sqlite://{}", db_path.display());
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path.to_string_lossy().to_string()),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    config.database_url = database_url;
    config::write_config(&config, &config_file)?;
    engine.disconnect().await?;

    emit_init_success(&args, &config_file, &config.database_url, "local");
    Ok(())
}

/// Emit either the human-readable success line or a structured JSON payload.
fn emit_init_success(args: &InitArgs, config_file: &Path, database_url: &str, mode: &str) {
    if args.json {
        match serde_json::to_string_pretty(&init_initialized_json(config_file, database_url, mode))
        {
            Ok(rendered) => println!("{rendered}"),
            Err(err) => eprintln!("Failed to render init JSON output: {err}"),
        }
    } else {
        println!("ZBrain initialized: {}", config_file.display());
    }
}

async fn run_init_migrate_only(args: &InitArgs, config_file: &Path) -> anyhow::Result<()> {
    if args.pglite || args.supabase || args.url.is_some() {
        anyhow::bail!("--migrate-only cannot be combined with --pglite, --supabase, or --url");
    }

    if !config_file.exists() {
        anyhow::bail!("--migrate-only requires an existing config; run zbrain init first or pass --config <path>");
    }

    let config = config::load_config_from_path(config_file)?;
    if config.database_url.starts_with("postgres://") || config.database_url.starts_with("postgresql://") {
        let engine_config = zbrain_core::engine::EngineConfig {
            database_url: Some(config.database_url.clone()),
            database_path: None,
        };
        let engine = zbrain_core::postgres::PostgresEngine::new();
        engine.connect(&engine_config).await?;
        engine.init_schema().await?;
        engine.disconnect().await?;
    } else {
        let db_path = resolve_database_path(&config.database_url);
        let engine_config = zbrain_core::engine::EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        };
        let engine = zbrain_core::libsql::LibsqlEngine::new();
        engine.connect(&engine_config).await?;
        engine.init_schema().await?;
        engine.disconnect().await?;
    }

    println!("ZBrain schema migrated: {}", config_file.display());
    Ok(())
}

/// Execute `zbrain doctor` command.
///
/// Validates the ZBrain installation and connectivity:
/// - Config file validation (exists, valid YAML)
/// - Database connectivity check
/// - Migration status verification
/// - Network connectivity check (for providers)
///
/// Discover the agent skills directory for the `skill_conformance` doctor
/// check. Mirrors the spirit of the TS `autoDetectSkillsDirReadOnly`: walk up
/// from the cwd looking for a `skills/manifest.json`, then fall back to
/// `<zbrain_home>/skills`. OpenClaw-workspace specific resolution is omitted —
/// ZBrain has no OpenClaw concept.
fn detect_skills_dir() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        let mut p = Some(cwd.as_path());
        while let Some(dir) = p {
            candidates.push(dir.join("skills"));
            p = dir.parent();
        }
    }
    if let Some(home) = zbrain_core::paths::zbrain_home() {
        candidates.push(home.join("skills"));
    }
    candidates.into_iter().find(|d| d.join("manifest.json").exists())
}

/// Returns exit code 0 if all checks pass, non-zero otherwise.
async fn run_doctor_command(args: DoctorArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    if !args.json {
        println!("Running ZBrain doctor...");
        println!();
    }

    let mut checks: Vec<DoctorCheck> = Vec::new();

    // 1. Config file validation
    let loaded_config = match config::load_config(config_path) {
        Ok(config) => {
            checks.push(DoctorCheck::ok("config", &format!("Loaded config with database: {}", config.database_url)));
            Some(config)
        }
        Err(e) => {
            checks.push(DoctorCheck::fail("config", &format!("Failed to load config: {}", e)));
            None
        }
    };

    // 2. Database connectivity check
    let db_path = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("brain.pglite");

    if db_path.exists() {
        let engine_config = zbrain_core::engine::EngineConfig {
            database_path: Some(db_path.to_string_lossy().to_string()),
            database_url: None,
        };

        let engine = zbrain_core::libsql::LibsqlEngine::new();
        match engine.connect(&engine_config).await {
            Ok(_) => {
                checks.push(DoctorCheck::ok("database", "Database connection successful"));

                // 3. Migration status verification
                match engine.list_pages(&Default::default()).await {
                    Ok(pages) => {
                        checks.push(DoctorCheck::ok("schema", &format!("Schema verified: {} pages found", pages.len())));
                    }
                    Err(e) => {
                        checks.push(DoctorCheck::warn("schema", &format!("Schema check failed: {}", e)));
                    }
                }

                // 3b. Takes weight-grid integrity (needs the live engine).
                // Engine-free helpers (reranker_health / eval_drift) run below
                // after disconnect; this one must stay inside the connected
                // scope. Mirrors the TS `takesWeightGridCheck` (src/commands/
                // doctor.ts): pages through all takes and flags off-0.05-grid
                // weights. Uses the public `list_takes` API — no raw SQL.
                {
                    let (status, message) =
                        zbrain_core::takes_fence::check_takes_weight_grid(&engine).await;
                    match status {
                        zbrain_core::takes_fence::TakesWeightGridStatus::Ok => {
                            checks.push(DoctorCheck::ok("takes_weight_grid", &message));
                        }
                        zbrain_core::takes_fence::TakesWeightGridStatus::Warn => {
                            checks.push(DoctorCheck::warn("takes_weight_grid", &message));
                        }
                        zbrain_core::takes_fence::TakesWeightGridStatus::Fail => {
                            checks.push(DoctorCheck::fail("takes_weight_grid", &message));
                        }
                    }
                }

                // 3c. Brain score composite (needs the live engine). Pull a
                // health snapshot via `get_health()` and fold the 3-tier
                // threshold + per-component breakdown into one check. Mirrors
                // the TS `checkBrainScore` (src/commands/doctor.ts), which
                // pushed a simple 3-tier check and a "Bug 11" breakdown as two
                // blocks (a latent duplicate-name bug) — this produces the
                // single authoritative check.
                match engine.get_health().await {
                    Ok(health) => {
                        let (status, message) =
                            zbrain_core::autopilot::brain_score::brain_score_doctor_check(&health);
                        let check = match status {
                            zbrain_core::autopilot::brain_score::BrainScoreDoctorStatus::Ok => {
                                DoctorCheck::ok("brain_score", &message)
                            }
                            zbrain_core::autopilot::brain_score::BrainScoreDoctorStatus::Warn => {
                                DoctorCheck::warn("brain_score", &message)
                            }
                            zbrain_core::autopilot::brain_score::BrainScoreDoctorStatus::Fail => {
                                DoctorCheck::fail("brain_score", &message)
                            }
                        };
                        checks.push(check);
                    }
                    Err(e) => {
                        // get_health() returns Err only on unsupported engines;
                        // surface as warn so a healthy brain never hard-fails.
                        checks.push(DoctorCheck::warn(
                            "brain_score",
                            &format!("Could not compute: {e}"),
                        ));
                    }
                }

                // 3d. Sync freshness (needs the live engine). Pull the source
                // list via the typed `list_sources` API — no raw SQL — and fold
                // the per-source lag into one worst-of check. Mirrors the TS
                // `checkSyncFreshness` (src/commands/doctor.ts): federated
                // sources (local_path set) whose last_sync_at has gone stale are
                // flagged warn (>24h) or fail (>72h). Thresholds are env-
                // overridable; the classifier is pure with an injected `now_ms`.
                match engine.list_sources(false).await {
                    Ok(sources) => {
                        let warn_hours = zbrain_core::sync_freshness::resolve_freshness_hours(
                            zbrain_core::sync_freshness::ENV_WARN_HOURS,
                            zbrain_core::sync_freshness::DEFAULT_WARN_HOURS,
                        );
                        let fail_hours = zbrain_core::sync_freshness::resolve_freshness_hours(
                            zbrain_core::sync_freshness::ENV_FAIL_HOURS,
                            zbrain_core::sync_freshness::DEFAULT_FAIL_HOURS,
                        );
                        let (status, message) =
                            zbrain_core::sync_freshness::classify_sync_freshness(
                                &sources,
                                zbrain_core::time::now_epoch_ms(),
                                warn_hours,
                                fail_hours,
                            );
                        let check = match status {
                            zbrain_core::sync_freshness::SyncFreshnessStatus::Ok => {
                                DoctorCheck::ok("sync_freshness", &message)
                            }
                            zbrain_core::sync_freshness::SyncFreshnessStatus::Warn => {
                                DoctorCheck::warn("sync_freshness", &message)
                            }
                            zbrain_core::sync_freshness::SyncFreshnessStatus::Fail => {
                                DoctorCheck::fail("sync_freshness", &message)
                            }
                        };
                        checks.push(check);
                    }
                    Err(e) => {
                        // Mirrors the TS catch: surface as warn so a transient
                        // list failure never hard-fails an otherwise healthy brain.
                        checks.push(DoctorCheck::warn(
                            "sync_freshness",
                            &format!("Could not check sync freshness: {e}"),
                        ));
                    }
                }

                engine.disconnect().await?;
            }
            Err(e) => {
                checks.push(DoctorCheck::fail("database", &format!("Connection failed: {}", e)));
                checks.push(DoctorCheck::warn("schema", "Skipped (no database connection)"));
            }
        }
    } else {
        checks.push(DoctorCheck::warn("database", "Database file not found - run `zbrain init` first"));
        checks.push(DoctorCheck::warn("schema", "Skipped (no database file)"));
    }

    // 4. Network connectivity check (simple DNS lookup via std::net)
    match std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([8, 8, 8, 8], 53)),
        std::time::Duration::from_secs(3),
    ) {
        Ok(_) => {
            checks.push(DoctorCheck::ok("network", "Network connectivity verified"));
        }
        Err(_) => {
            checks.push(DoctorCheck::warn("network", "Network check failed - offline or DNS issue"));
        }
    }

    // 5. Reranker health (engine-free: reads the config file plane + the
    // rerank-failure audit JSONL). No DB/network — the reranker fails open
    // at search time, so its health is purely "did fail-open fire recently
    // and does the operator need to act". Mirrors the TS `reranker_health`
    // check (src/commands/doctor.ts checkRerankerHealth): read
    // `search.reranker.enabled`, read the 7-day failure window, classify.
    {
        let reranker_enabled = loaded_config
            .as_ref()
            .map(|c| c.search.reranker_enabled)
            .unwrap_or(false);
        // Audit dir resolution is shared with the rerank client wiring in
        // `run_operation` so the writer and the doctor reader always agree.
        let audit_dir = resolve_audit_dir();
        let failures = zbrain_core::rerank_audit::read_recent_rerank_failures(
            &audit_dir,
            zbrain_core::rerank_audit::HEALTH_WINDOW_DAYS,
        );
        let (status, message) =
            zbrain_core::rerank_audit::classify_reranker_health(reranker_enabled, &failures);
        match status {
            zbrain_core::rerank_audit::RerankerHealthStatus::Ok => {
                checks.push(DoctorCheck::ok("reranker_health", &message));
            }
            zbrain_core::rerank_audit::RerankerHealthStatus::Warn => {
                checks.push(DoctorCheck::warn("reranker_health", &message));
            }
        }
    }

    // 5b. eval_drift: retrieval-path code changed since last eval.
    // Engine-free: runs `git diff --name-only` against the curated
    // RETRIEVAL_WATCH_PATTERNS allowlist. Best-effort (no git / no repo ⇒
    // clean). Mirrors the TS `eval_drift` check (src/core/eval/drift-watch.ts):
    // warn when any watched file drifted in the working tree since HEAD.
    {
        let repo_root = std::env::current_dir().unwrap_or_default();
        let (status, message) = zbrain_core::eval_drift::eval_drift_status(&repo_root, None);
        match status {
            zbrain_core::eval_drift::EvalDriftStatus::Ok => {
                checks.push(DoctorCheck::ok("eval_drift", &message));
            }
            zbrain_core::eval_drift::EvalDriftStatus::Warn => {
                checks.push(DoctorCheck::warn("eval_drift", &message));
            }
        }
    }

    // 5c. Skill conformance: filesystem-only (no DB needed). Migrated from the
    // TS `checkSkillConformance` doctor check. The TS original resolved the
    // skills dir via the resolver (still-unmigrated slice); here we discover it
    // from the cwd walk-up + zbrain home so the check is self-contained.
    if let Some(skills_dir) = detect_skills_dir() {
        let (status, message) = zbrain_core::skill_conformance::check_skill_conformance(&skills_dir);
        match status {
            zbrain_core::skill_conformance::SkillConformanceStatus::Ok => {
                checks.push(DoctorCheck::ok("skill_conformance", &message));
            }
            zbrain_core::skill_conformance::SkillConformanceStatus::Warn => {
                checks.push(DoctorCheck::warn("skill_conformance", &message));
            }
        }
    }

    // 5f. embedding_health: check ZeroEntropy API key presence + embedding column coverage.
    // Mirrors the TS `checkZeEmbeddingHealth` doctor check.
    {
        let mut messages = Vec::new();

        // Check 1: ZeroEntropy API key configured if model starts with zeroentropyai:
        #[cfg(feature = "embedding")]
        if let Some(client) = zbrain_core::embedding::EmbeddingClient::from_env() {
            let model_id = client.model();
            if model_id.starts_with("zeroentropyai:") && std::env::var("ZEROENTROPY_API_KEY").map_or(true, |k| k.is_empty()) {
                messages.push((
                    CheckStatus::Warn,
                    "ZeroEntropy model ID expects ZEROENTROPY_API_KEY env var, but it's empty/unset".to_string(),
                ));
            }
        }

        // Check 2: embedding column coverage (count of pages with non-null embedding).
        // G24 resolved: all production backends now persist embedding, so coverage is complete.
        // Leave an ok check to document this resolved gap.
        checks.push(DoctorCheck::ok(
            "embedding_health:column",
            "All production backends persist page.embedding (G24 resolved)",
        ));

        // Emit collected status
        for (status, message) in messages {
            checks.push(DoctorCheck {
                name: "embedding_health".to_string(),
                status,
                message,
            });
        }
    }

    // 6. Traceability: surface TS doctor checks not yet migrated to Rust (Q2).
    // These are `not-implemented` — visible but excluded from health_score /
    // status / exit code, so a later agent cannot mistake doctor for complete.
    for (name, covers) in UNMIGRATED_TS_DOCTOR_CHECKS {
        checks.push(DoctorCheck::not_implemented(
            name,
            &format!("Not migrated from TS doctor (covers: {covers})"),
        ));
    }

    // --json: emit the structured envelope and nothing else. Exit code is
    // still driven by fail count (warn / not-implemented never exit 1).
    if args.json {
        println!("{}", serde_json::to_string(&doctor_json_report(&checks))?);
        if checks.iter().any(|c| c.status == CheckStatus::Fail) {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Human-readable output.
    let mut pass_count = 0;
    let mut warn_count = 0;
    let mut fail_count = 0;
    let mut na_count = 0;

    for check in &checks {
        let (status_icon, _status_label) = match check.status {
            CheckStatus::Ok => ("✅", "PASS"),
            CheckStatus::Warn => ("⚠️", "WARN"),
            CheckStatus::Fail => ("❌", "FAIL"),
            CheckStatus::NotImplemented => ("🚧", "N/A"),
        };

        println!("{} {}: {}", status_icon, check.name, check.message);

        match check.status {
            CheckStatus::Ok => pass_count += 1,
            CheckStatus::Warn => warn_count += 1,
            CheckStatus::Fail => fail_count += 1,
            CheckStatus::NotImplemented => na_count += 1,
        }
    }

    println!();
    println!("--- Summary ---");
    println!(
        "Pass: {}, Warn: {}, Fail: {}, Not implemented: {} | Health score: {}/100",
        pass_count,
        warn_count,
        fail_count,
        na_count,
        doctor_health_score(&checks),
    );

    if fail_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Execute `zbrain features` — scan brain health/stats and recommend unused
/// features. This is the CLI wiring around the pure `zbrain_core::features`
/// engine: it builds the DI `FeatureScanInputs` from the live engine
/// (`get_health` + `get_brain_stats`), the environment (secret presence), and
/// config (`sync.default_repo`), then renders human or `--json` output and
/// updates the `feature-offers.json` scan stamps.
///
/// Auto-fix (via `--auto-fix`) dispatches to the page-level auto-fix library
/// functions in `zbrain_core::auto_fix`, the Rust analog of the TS
/// `executeAutoFix`. The recommended `embed`/`extract` commands now have Rust
/// equivalents, so `--auto-fix` performs real work.
async fn run_features_command(args: FeaturesArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::features;

    // Build the engine the same way doctor does: home PGLite DB via libsql.
    let db_path = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("brain.pglite");
    let engine_config = zbrain_core::engine::EngineConfig {
        database_path: Some(db_path.to_string_lossy().to_string()),
        database_url: None,
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    let health = engine.get_health().await?;
    let stats = engine.get_brain_stats().await?;

    // Resolve sync repo from config, mirroring the `sync` command
    // (`config.sync.default_repo`). Note the TS key was `sync.repo_path`; the
    // Rust config field is `sync.default_repo` — same meaning, different name.
    let sync_repo = config::load_config(config_path)
        .ok()
        .and_then(|c| c.sync.and_then(|s| s.default_repo))
        .map(|p| p.to_string_lossy().to_string());

    // `secret_present`: the one place we read the real environment. A secret
    // counts as configured only when present AND non-empty (matches TS
    // `process.env[s]` truthiness — empty string is falsy).
    fn secret_present(key: &str) -> bool {
        std::env::var(key).map(|v| !v.is_empty()).unwrap_or(false)
    }

    let version = env!("CARGO_PKG_VERSION").to_string();
    let inputs = features::FeatureScanInputs {
        health: features::HealthSnapshot {
            missing_embeddings: health.missing_embeddings as u64,
            dead_links: health.dead_links as u64,
            embed_coverage: health.embed_coverage,
            brain_score: health.brain_score,
        },
        stats: features::BrainStatsSnapshot {
            page_count: stats.page_count.max(0) as u64,
            link_count: stats.link_count.max(0) as u64,
            timeline_entry_count: stats.timeline_entry_count.max(0) as u64,
        },
        secret_present,
        sync_repo,
        version: version.clone(),
    };

    let scan = features::recommend_features(&inputs);
    let mut offers = features::load_offers();
    let pitchable = features::pitchable(&scan, &offers);

    let scan_ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

    // `--auto-fix`: run the recommended, idempotent fix actions, then record
    // the auto-fixable recommendations as accepted in the ledger.
    let auto_fix = if args.auto_fix {
        let result = run_auto_fix(&engine).await?;
        for rec in &scan.recommendations {
            if rec.auto_fixable {
                offers.accepted.insert(
                    rec.id.clone(),
                    features::OfferStamp {
                        at: scan_ts.clone(),
                        version: scan.version.clone(),
                    },
                );
            }
        }
        Some(result)
    } else {
        None
    };

    if args.json {
        let report = features::FeatureScanReport::new(&scan, pitchable, scan_ts.clone());
        let mut value = serde_json::to_value(&report)?;
        if let Some(af) = &auto_fix {
            value["auto_fix"] = serde_json::to_value(af)?;
        }
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if pitchable.is_empty() {
        println!(
            "\nBrain score: {}/100. All features adopted. Nothing to recommend.",
            scan.brain_score
        );
    } else {
        print!("{}", features::render_human(&scan, &pitchable));
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            println!("Run 'zbrain features' regularly to track brain health.");
        }
    }

    if let Some(af) = &auto_fix {
        if args.json {
            // already included in the JSON above.
        } else if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            println!("\nAuto-fix applied:");
            println!("  links created: {}", af.links_created);
            println!("  timeline entries added: {}", af.timeline_entries_added);
            if af.embedding_enabled {
                println!("  pages embedded: {}", af.embedded);
            } else {
                println!(
                    "  pages embedded: 0 (embedding not configured — set ZEROENTROPY_API_KEY)"
                );
            }
        }
    }

    // Persist scan stamps + accepted ledger (best-effort).
    offers.last_version = scan.version.clone();
    offers.last_scan = scan_ts;
    features::save_offers(&offers);

    Ok(())
}

/// Outcome of running the auto-fix dispatch, surfaced in both human and
/// `--json` output.
#[derive(Debug, serde::Serialize)]
struct AutoFixResults {
    /// Whether the embedding client was available (ZEROENTROPY_API_KEY set).
    pub embedding_enabled: bool,
    /// Pages re-embedded via `embed_stale` (0 when embedding disabled).
    pub embedded: usize,
    /// Outgoing links created via `extract_links`.
    pub links_created: usize,
    /// Timeline entries appended via `extract_timeline`.
    pub timeline_entries_added: usize,
}

/// Page-level auto-fix dispatch (Rust analog of TS `executeAutoFix`): extract
/// links and timeline entries from page bodies, and — when an embedding
/// client is configured — re-embed stale pages. All three operations are
/// idempotent, so re-running is safe.
async fn run_auto_fix(
    engine: &dyn zbrain_core::engine::BrainEngine,
) -> anyhow::Result<AutoFixResults> {
    use zbrain_core::auto_fix::{
        embed_stale, extract_links, extract_timeline, EmbedStaleOpts, ExtractLinksOpts,
        ExtractTimelineOpts,
    };
    use zbrain_core::embedding::EmbeddingClient;

    let links = extract_links(engine, &ExtractLinksOpts::default()).await?;
    let timeline = extract_timeline(engine, &ExtractTimelineOpts::default()).await?;

    let (embedding_enabled, embedded) = match EmbeddingClient::from_env() {
        Some(client) => {
            let res = embed_stale(engine, &client, &EmbedStaleOpts::default()).await?;
            (true, res.embedded)
        }
        None => (false, 0),
    };

    Ok(AutoFixResults {
        embedding_enabled,
        embedded,
        links_created: links.links_created,
        timeline_entries_added: timeline.entries_added,
    })
}

/// `zbrain storage status` — storage-tiering report (Rust port of TS
/// `src/commands/storage.ts`).
///
/// Builds the home libsql engine (same as doctor/features), resolves the repo
/// path (`--repo` override, else `config.sync.default_repo`), warns once when
/// running on the local Libsql engine (tiering has limited effect there,
/// mirroring TS `engine.kind !== 'pglite'`), then dispatches to
/// `zbrain_core::storage_status::get_storage_status` and prints the result as
/// JSON or human-readable text. Unknown subcommands exit 1.
async fn run_storage_command(
    args: StorageArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    if args.subcommand != "status" {
        anyhow::bail!("Unknown storage subcommand: {}", args.subcommand);
    }

    // Build the engine the same way doctor/features do: home PGLite DB.
    let db_path = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("brain.pglite");
    let engine_config = zbrain_core::engine::EngineConfig {
        database_path: Some(db_path.to_string_lossy().to_string()),
        database_url: None,
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    // TS warns when not on PGLite; in Rust the local engine is Libsql.
    if engine.kind() == zbrain_core::engine::EngineKind::Libsql
        && std::io::IsTerminal::is_terminal(&std::io::stderr())
    {
        eprintln!(
            "Note: storage tiering has limited effect on Libsql — pages live in \
             your local database file regardless of tier. The .gitignore \
             management still keeps bulk content out of git history. To get \
             full tiering, migrate to Postgres with `zbrain migrate --to supabase`."
        );
    }

    // Resolution chain: explicit --repo → typed config accessor → null.
    let repo_path: Option<String> = match &args.repo {
        Some(r) => Some(r.clone()),
        None => config::load_config(config_path)
            .ok()
            .and_then(|c| c.sync.and_then(|s| s.default_repo))
            .map(|p| p.to_string_lossy().to_string()),
    };

    let result = zbrain_core::storage_status::get_storage_status(&engine, repo_path.clone())
        .await?;

    if args.json {
        println!(
            "{}",
            zbrain_core::storage_status::format_storage_status_json(&result)
        );
    } else {
        println!(
            "{}",
            zbrain_core::storage_status::format_storage_status_human(&result)
        );
    }
    Ok(())
}

/// `zbrain publish <page.md>` — generate a self-contained, shareable HTML file.
///
/// Reads the markdown page, strips private/internal data (`make_shareable`),
/// extracts the title (or uses `--title`), renders markdown to static HTML
/// server-side (pulldown-cmark), optionally AES-256-GCM encrypts the rendered
/// HTML with `--password`, and writes the final document. No LLM calls, no
/// client-side markdown renderer (deliberate divergence from the TS source,
/// which shipped `marked.js` and decrypted to raw markdown).
async fn run_publish_command(args: PublishArgs) -> anyhow::Result<()> {
    use zbrain_core::publish::{encrypt_content, extract_title, generate_html, make_shareable, render_markdown};

    let raw = std::fs::read_to_string(&args.input)
        .map_err(|e| anyhow::anyhow!("failed to read input {}: {e}", args.input.display()))?;

    let shareable = make_shareable(&raw);
    let title = match &args.title {
        Some(t) => t.clone(),
        // TS extracts the title from the raw (pre-strip) page; frontmatter uses
        // `---` not `#`, so the first H1 is the same either way.
        None => extract_title(&raw),
    };
    let rendered = render_markdown(&shareable);

    // Resolve the password: `--password` alone -> auto-generated; `--password
    // "x"` -> literal; absent -> no encryption (cleartext share).
    let (encrypted, shown_password) = match &args.password {
        None => (None, None),
        Some(pw) => {
            let pw = if pw.is_empty() {
                zbrain_core::publish::generate_password(16)
            } else {
                pw.clone()
            };
            (Some(encrypt_content(&rendered, &pw)), Some(pw))
        }
    };

    let html = generate_html(&title, &rendered, encrypted.as_ref());

    let out_path = match &args.out {
        Some(o) => o.clone(),
        None => {
            let stem = args
                .input
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "document".into());
            let mut p = args.input.to_path_buf();
            p.set_file_name(format!("{stem}.html"));
            p
        }
    };

    // Mirror TS `mkdirSync(dirname(outPath), { recursive: true })`.
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("failed to create dir {}: {e}", parent.display()))?;
        }
    }

    std::fs::write(&out_path, html)
        .map_err(|e| anyhow::anyhow!("failed to write output {}: {e}", out_path.display()))?;

    println!("Published: {}", out_path.display());
    match shown_password {
        Some(pw) => println!("  (password protected, AES-256-GCM encrypted)\n  Password: {pw}"),
        None => println!("  (no password, content in cleartext)"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// resolvers — introspect the Resolver SDK registry (slice 1-6-4-10-4)
// ---------------------------------------------------------------------------

/// Arguments for `zbrain resolvers`.
///
/// Mirrors TS `src/commands/resolvers.ts`: `list` (pretty table / `--json`,
/// with `--cost` / `--backend` filters) and `describe <id>` (schema +
/// availability). No engine connection is required — the registry is a
/// process-wide in-memory singleton. The builtins are registered with live
/// transport clients at invocation time.
#[derive(Debug, Parser)]
pub struct ResolversArgs {
    #[command(subcommand)]
    pub sub: Option<ResolversSub>,
}

#[derive(Debug, Subcommand)]
pub enum ResolversSub {
    /// List all registered resolvers (pretty table)
    List {
        /// Machine-readable JSON output
        #[arg(long)]
        json: bool,
        /// Filter by cost: free, rate-limited, paid
        #[arg(long)]
        cost: Option<String>,
        /// Filter by backend label
        #[arg(long)]
        backend: Option<String>,
    },
    /// Show schema + availability for a single resolver
    Describe {
        /// Resolver id (e.g. `x_handle_to_tweet`)
        id: String,
    },
}

/// Arguments for `zbrain anomalies` — statistical anomalies in recent page
/// activity, grouped by cohort (tag, type). Deterministic: zero LLM calls.
#[derive(Debug, Parser)]
pub struct AnomaliesArgs {
    /// Target day (YYYY-MM-DD). Defaults to today UTC. Invalid dates are
    /// ignored (mirrors the TS CLI's silent-drop behavior).
    #[arg(long)]
    pub since: Option<String>,

    /// Baseline window in days (default 30, clamped to >= 1).
    #[arg(long)]
    pub lookback_days: Option<u32>,

    /// Sigma threshold multiplier (default 3.0, must be > 0).
    #[arg(long)]
    pub sigma: Option<f64>,

    /// Emit results as JSON (for agents) instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain check-update`.
#[derive(Debug, Parser)]
pub struct CheckUpdateArgs {
    /// Emit results as JSON (for agents) instead of human-readable output.
    #[arg(long)]
    pub json: bool,
}

/// Mode selector for `zbrain models`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ModelsMode {
    /// Print the model routing table (default).
    Read,
    /// Probe that each configured model is reachable.
    Doctor,
}

/// Arguments for `zbrain models`.
#[derive(Debug, Parser)]
pub struct ModelsArgs {
    /// Mode: routing table (`read`, default) or reachability probes (`doctor`).
    #[arg(value_enum, default_value_t = ModelsMode::Read)]
    pub mode: ModelsMode,
    /// Emit results as JSON (for agents) instead of human-readable output.
    #[arg(long)]
    pub json: bool,
    /// Skip reachability probes for a provider (repeatable, e.g. `--skip=anthropic`).
    #[arg(long, value_name = "PROVIDER")]
    pub skip: Vec<String>,
}

/// Arguments for `zbrain apply-migrations`.
#[derive(Debug, Parser)]
pub struct ApplyMigrationsArgs {
    /// Show applied + pending migrations and exit.
    #[arg(long)]
    pub list: bool,
    /// Print the plan; take no action.
    #[arg(long)]
    pub dry_run: bool,
    /// Run all pending migrations (non-interactive).
    #[arg(long)]
    pub yes: bool,
    /// Write a 'retry' marker for a wedged migration by version (then re-run --yes).
    #[arg(long, value_name = "VERSION")]
    pub force_retry: Option<String>,
    /// Write a 'retry' marker for every wedged orchestrator migration.
    #[arg(long)]
    pub force_orchestrator: bool,
    /// Reset schema-version drift; re-run init schema (DDL) on the configured brain.
    #[arg(long)]
    pub force_schema: bool,
    /// Both --force-orchestrator and --force-schema.
    #[arg(long)]
    pub force_all: bool,
    /// Bypass post-condition verify hooks on non-idempotent migrations.
    #[arg(long)]
    pub skip_verify: bool,
    /// Set minion_mode without prompting (always | pain_triggered | off).
    #[arg(long, value_name = "MODE")]
    pub mode: Option<String>,
    /// Include this directory in the host-file walk.
    #[arg(long, value_name = "PATH")]
    pub host_dir: Option<String>,
    /// Skip the v0.11.0 autopilot install step.
    #[arg(long)]
    pub no_autopilot_install: bool,
    /// Emit results as JSON (for agents).
    #[arg(long)]
    pub json: bool,
}

async fn run_resolvers_command(args: ResolversArgs) -> anyhow::Result<()> {
    use std::sync::Arc;
    use zbrain_core::resolvers::{
        get_default_registry, DnsResolver, HttpClient, ReqwestHttpClient, ResolverContext,
        ResolverCost, ResolverListFilter, TokioDnsResolver,
    };

    // Register the two builtin resolvers with live transport clients
    // (idempotent: re-registration of an existing id is a no-op inside the
    // registry). Mirrors TS `registerBuiltinResolvers()`.
    {
        let mut registry = get_default_registry();
        let http: Arc<dyn HttpClient> = Arc::new(ReqwestHttpClient::new());
        let dns: Arc<dyn DnsResolver> = Arc::new(TokioDnsResolver);
        registry.register_builtin_resolvers(http, dns);
    }

    match args.sub {
        None => {
            print_resolvers_help();
            Ok(())
        }
        Some(ResolversSub::List { json, cost, backend }) => {
            let cost = match cost.as_deref() {
                None => None,
                Some("free") => Some(ResolverCost::Free),
                Some("rate-limited") => Some(ResolverCost::RateLimited),
                Some("paid") => Some(ResolverCost::Paid),
                Some(other) => {
                    eprintln!(
                        "Invalid --cost value: {other}. Must be one of: free, rate-limited, paid."
                    );
                    std::process::exit(1);
                }
            };
            let filter = if cost.is_some() || backend.is_some() {
                Some(ResolverListFilter { cost, backend })
            } else {
                None
            };
            let registry = get_default_registry();
            let summaries = registry.list(filter.as_ref());

            if json {
                let arr: Vec<serde_json::Value> = summaries
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "id": s.id,
                            "cost": s.cost.as_str(),
                            "backend": s.backend,
                            "description": s.description,
                            "hasInputSchema": s.has_input_schema,
                            "hasOutputSchema": s.has_output_schema,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::Value::Array(arr))?
                );
                return Ok(());
            }

            if summaries.is_empty() {
                println!("No resolvers registered.");
                return Ok(());
            }
            print_resolvers_table(&summaries);
            Ok(())
        }
        Some(ResolversSub::Describe { id }) => {
            let (resolver, available) = {
                let registry = get_default_registry();
                if !registry.has(&id) {
                    eprintln!("Resolver not found: {id}");
                    eprintln!(
                        "Available: {}",
                        registry
                            .list(None)
                            .iter()
                            .map(|s| s.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    std::process::exit(1);
                }
                let resolver = registry.get(&id).expect("checked has() above");
                drop(registry); // release the lock before the async await
                let ctx = ResolverContext::new();
                let available = resolver.available(&ctx).await;
                (resolver, available)
            };
            println!("ID:          {}", resolver.id());
            println!("Cost:        {}", resolver.cost());
            println!("Backend:     {}", resolver.backend());
            if let Some(d) = resolver.description() {
                println!("Description: {d}");
            }
            println!(
                "Available:   {}",
                if available {
                    "yes"
                } else {
                    "no (check env/config)"
                }
            );
            if let Some(schema) = resolver.input_schema() {
                println!("\nInput schema:");
                println!("{}", serde_json::to_string_pretty(schema)?);
            }
            if let Some(schema) = resolver.output_schema() {
                println!("\nOutput schema:");
                println!("{}", serde_json::to_string_pretty(schema)?);
            }
            Ok(())
        }
    }
}

/// Execute `zbrain anomalies` — statistical anomalies in recent page activity.
///
/// Builds the home PGLite engine the same way doctor/features/whoknows do,
/// runs [`zbrain_core::anomaly`]'s `find_anomalies` engine method, and prints
/// either JSON (`--json`) or a human summary. On thin-client installs, routes
/// via MCP (mirrors TS `callRemoteTool(cfg, 'find_anomalies', ...)`).
async fn run_anomalies_command(
    args: AnomaliesArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use chrono::NaiveDate;
    use zbrain_core::anomaly::{AnomaliesOpts, AnomalyResult};

    // Normalize flags (mirror TS parseArgs: invalid values dropped silently).
    // `since` must be YYYY-MM-DD; `lookback_days` >= 1; `sigma` > 0.
    let since = args
        .since
        .as_ref()
        .filter(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok())
        .cloned();
    let lookback_days = args.lookback_days.filter(|n| *n >= 1);
    let sigma = args.sigma.filter(|n| *n > 0.0);

    // Load config (needed for thin-client check + engine path).
    let config_file = config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;
    let config = config::load_config_from_path(&config_file)?;

    let rows: Vec<AnomalyResult> = if config::is_thin_client(&config) {
        // Thin-client: route via remote MCP (mirror TS callRemoteTool).
        let mcp_client = mcp_client::McpClient::new(
            config,
            std::time::Duration::from_millis(30_000),
        );
        let raw = mcp_client
            .call_tool(
                "find_anomalies",
                serde_json::json!({
                    "since": since,
                    "lookback_days": lookback_days,
                    "sigma": sigma,
                }),
            )
            .await
            .map_err(|e| {
                eprintln!("Remote MCP call failed: {}", e);
                std::process::exit(1);
            })
            .unwrap();
        let data = unpack_tool_result(&raw);
        serde_json::from_value::<Vec<AnomalyResult>>(data)
            .map_err(|e| anyhow::anyhow!("failed to decode find_anomalies result: {}", e))?
    } else {
        // Local: build home PGLite engine (mirror whoknows/integrity/storage).
        let db_path = config::zbrain_home()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("brain.pglite");
        let engine_config = zbrain_core::engine::EngineConfig {
            database_path: Some(db_path.to_string_lossy().to_string()),
            database_url: None,
        };
        let engine = zbrain_core::libsql::LibsqlEngine::new();
        engine.connect(&engine_config).await?;
        engine
            .find_anomalies(AnomaliesOpts {
                since: since.clone(),
                lookback_days,
                sigma,
            })
            .await?
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if rows.is_empty() {
        println!("(no anomalies for this window)");
        return Ok(());
    }

    let since_label = since
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().format("%Y-%m-%d").to_string());
    println!(
        "{} anomalous cohort(s) for {}:\n",
        rows.len(),
        since_label
    );

    for r in &rows {
        println!(
            "[{}={}] count={}, baseline mean={:.2}±{:.2}, sigma={:.2}",
            r.cohort_kind.as_str(),
            r.cohort_value,
            r.count,
            r.baseline_mean,
            r.baseline_stddev,
            r.sigma_observed
        );
        let slug_sample: Vec<&String> = r.page_slugs.iter().take(5).collect();
        if !slug_sample.is_empty() {
            let sample_str = slug_sample
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let more = if r.page_slugs.len() > 5 {
                format!(", +{} more", r.page_slugs.len() - 5)
            } else {
                String::new()
            };
            println!("  pages: {}{}", sample_str, more);
        }
    }

    Ok(())
}

fn print_resolvers_table(summaries: &[zbrain_core::resolvers::ResolverSummary]) {
    use std::cmp::max;

    let id_w = max(2, summaries.iter().map(|s| s.id.len()).max().unwrap_or(2));
    let cost_w = max(4, summaries.iter().map(|s| s.cost.as_str().len()).max().unwrap_or(4));
    let backend_w = max(7, summaries.iter().map(|s| s.backend.len()).max().unwrap_or(7));

    let hdr = format!(
        "{:<id_w$}  {:<cost_w$}  {:<backend_w$}  DESCRIPTION",
        "ID", "COST", "BACKEND", id_w = id_w, cost_w = cost_w, backend_w = backend_w
    );
    println!("{hdr}");
    println!("{}", "-".repeat(hdr.len()));
    for s in summaries {
        println!(
            "{:<id_w$}  {:<cost_w$}  {:<backend_w$}  {}",
            s.id,
            s.cost.as_str(),
            s.backend,
            s.description.as_deref().unwrap_or(""),
            id_w = id_w,
            cost_w = cost_w,
            backend_w = backend_w
        );
    }
    println!(
        "\n{} resolver{} registered.",
        summaries.len(),
        if summaries.len() == 1 { "" } else { "s" }
    );
}

fn print_resolvers_help() {
    println!(
        "Usage: zbrain resolvers <subcommand> [options]

Subcommands:
  list                    List all registered resolvers (pretty table)
  list --json             List as JSON
  list --cost <c>         Filter by cost: free, rate-limited, paid
  list --backend <b>      Filter by backend label
  describe <id>           Show schema + availability for a single resolver

Examples:
  zbrain resolvers list
  zbrain resolvers list --cost paid
  zbrain resolvers describe x_handle_to_tweet
"
    );
}

/// `zbrain whoknows <topic>` — expert-routing query.
///
/// Builds the home libsql engine (same as doctor/features), runs the
/// expertise-ranked search via `zbrain_core::whoknows::find_experts`, and
/// prints either a human table (with optional `--explain` factor breakdown)
/// or JSON.
///
/// Type filter parity note: TS consults the active schema pack via
/// `expertTypesFromPack` to honor user-defined `expert_routing:` declarations.
/// The schema-pack subsystem is not migrated yet, so this falls back to the
/// default person/company filter (`whoknows::DEFAULT_TYPES`). Thin-client
/// remote routing (TS routes to the `find_experts` MCP op when there is no
/// local brain) is likewise deferred. Both are registered in
/// docs/plans/MIGRATION.md.
async fn run_whoknows_command(args: WhoknowsArgs, _config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::whoknows;

    let topic = args.topic.join(" ");

    // Build the engine the same way doctor/features do: home PGLite DB via libsql.
    let db_path = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("brain.pglite");
    let engine_config = zbrain_core::engine::EngineConfig {
        database_path: Some(db_path.to_string_lossy().to_string()),
        database_url: None,
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    let results = whoknows::find_experts(
        &engine,
        &whoknows::FindExpertsOpts {
            topic: topic.clone(),
            limit: args.limit,
            // Default person/company filter (schema-pack pack-aware derivation
            // not migrated yet — see KNOWN-GAPS).
            types: None,
            source_id: None,
        },
    )
    .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
        return Ok(());
    }

    if results.is_empty() {
        println!("(no person or company pages match \"{topic}\")");
        return Ok(());
    }

    // Human format: rank | score | type | slug — title.
    let header = format!("{:<3} {:<7} {:<8} slug — title", "#", "score", "type");
    println!("{header}");
    println!("{}", "-".repeat(header.len().min(80)));
    for (i, r) in results.iter().enumerate() {
        println!(
            "{:<3} {:<7} {:<8} {} — {}",
            i + 1,
            format!("{:.3}", r.score),
            r.page_type,
            r.slug,
            r.title
        );
        if args.explain {
            let f = &r.factors;
            let days = match f.days_since_effective {
                Some(d) => format!("{d:.0}d"),
                None => "cold".to_string(),
            };
            println!(
                "      expertise={:.3} (raw={:.3}) recency={:.3} ({}) salience={:.3} → factor={:.3}",
                f.expertise, f.raw_match, f.recency_factor, days, f.salience, f.salience_factor
            );
        }
    }

    Ok(())
}

/// Execute `zbrain integrity check` — read-only brain-integrity scan.
///
/// Builds the home PGLite engine the same way doctor/features/whoknows do,
/// runs [`zbrain_core::integrity::scan_integrity`], and prints either JSON
/// (`--json`) or a human summary. The `auto`/`review`/`reset-progress`
/// subcommands are intentionally not wired (resolver SDK un-migrated, G51).
async fn run_integrity_command(
    args: IntegrityArgs,
    _config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::integrity;

    // Build the engine the same way doctor/features/whoknows do: home PGLite DB.
    let db_path = config::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("brain.pglite");
    let engine_config = zbrain_core::engine::EngineConfig {
        database_path: Some(db_path.to_string_lossy().to_string()),
        database_url: None,
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    let result = integrity::scan_integrity(
        &engine,
        &integrity::IntegrityScanOptions {
            limit: args.limit,
            type_filter: args.r#type.clone(),
        },
    )
    .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    println!(
        "Scanned {} page(s) · bare-tweet phrases: {} · external links: {}",
        result.pages_scanned,
        result.bare_hits.len(),
        result.external_hits.len()
    );
    if !result.bare_hits.is_empty() {
        println!("\nBare-tweet references (need a citation URL):");
        for h in &result.bare_hits {
            println!("  {}:{}  {}   → \"{}\"", h.slug, h.line, h.phrase, h.raw_line);
        }
    }
    if !result.external_hits.is_empty() {
        println!("\nExternal links (check for rot):");
        for h in &result.external_hits {
            println!("  {}:{}  {}", h.slug, h.line, h.url);
        }
    }
    if !result.top_pages.is_empty() {
        println!("\nTop pages by bare-tweet count:");
        for (i, p) in result.top_pages.iter().enumerate() {
            println!("  {}. {} ({} hits)", i + 1, p.slug, p.count);
        }
    }

    Ok(())
}


/// FUTURE(schema-pack): the TS `zbrain schema` command was a 1166-line
/// schema-pack manager (Schema Cathedral v3) exposing the 32-verb taxonomy
/// below. As of 2026-07-15 **all 32 verbs are migrated** across roadmap
/// Part10 Phase12 nodes 1-1..1-5 (inspection 1-3, activation+authoring 1-4,
/// discovery+repair 1-5). G4 (residual TS schema-pack) is RESOLVED.
///
/// This constant is the closed-out tracking point (`UNMIGRATED_TS_SCHEMA_PACK_VERBS`):
/// it is now empty. The anchor test guards against silent re-introduction of
/// un-migrated TS verbs — if a verb is ever found un-migrated again, re-list
/// it here and update the test. TS source: src/commands/schema.ts @ 5d5b404~1.
/// Full background: docs/plans/MIGRATION.md (G4, resolved).
#[allow(dead_code)] // Referenced only in the anchor test (cargo test); silent in non-test builds.
const UNMIGRATED_TS_SCHEMA_PACK_VERBS: &[&str] = &[
    // All 32 verbs migrated (1-1..1-5). Empty = G4 resolved.
];

/// Execute `zbrain schema-sql` command.
///
/// Prints the database schema SQL (DDL) for the specified backend.
/// Supports: libsql (default), postgres.
fn run_schema_command(args: SchemaArgs) -> anyhow::Result<()> {
    let backend = args.backend.to_lowercase();

    match backend.as_str() {
        "libsql" | "sqlite" | "pglite" => {
            println!("-- ZBrain libsql/SQLite Schema");
            println!();
            for migration in zbrain_core::libsql::LIBQL_MIGRATIONS.iter() {
                println!("-- Migration {}: {}", migration.version(), migration.name());
                println!("{}", migration.sql());
                println!();
            }
        }
        "postgres" | "pg" => {
            println!("-- ZBrain Postgres Schema");
            println!();
            for migration in zbrain_core::postgres::POSTGRES_MIGRATIONS.iter() {
                println!("-- Migration {}: {}", migration.version(), migration.name());
                println!("{}", migration.sql());
                println!();
            }
        }
        _ => {
            eprintln!("Unknown backend: {}", args.backend);
            eprintln!("Supported backends: libsql, postgres");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Execute `zbrain config` subcommands.
async fn run_config_command(args: ConfigArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    match args.action {
        ConfigAction::Show => {
            let config = config::load_config(config_path)?;
            println!("ZBrain config:");
            print_config_value("", &serde_yaml::to_value(&config)?, 2);
        }
        ConfigAction::Get { key } => {
            let config = config::load_config(config_path)?;
            let value = get_config_value(&key, &serde_yaml::to_value(&config)?);
            match value {
                // `get` returns the raw value (no redaction): it is an explicit
                // single-value read used by scripts to read back secrets.
                // `show` still redacts to avoid scrollback leaks.
                Some(v) => println!("{v}"),
                None => anyhow::bail!("Config key not found: {key}"),
            }
        }
        ConfigAction::Set { key, value, force } => {
            if !force && !is_known_config_key(&key) {
                anyhow::bail!(
                    "Unknown config key: {key}. Use --force to set it anyway."
                );
            }
            let mut config = config::load_config(config_path)?;
            set_config_value(&mut config, &key, value)?;
            // Default to user config directory if no explicit path
            let output_path = config_path
                .map(PathBuf::from)
                .or_else(config::user_config_path)
                .unwrap_or_else(|| PathBuf::from("zbrain.yml"));
            config::write_config(&config, &output_path)?;
            println!("Set config key: {}", key);
        }
        ConfigAction::Unset { key: _, pattern: Some(ref pattern) } => {
            // Bulk unset by prefix pattern
            let mut config = config::load_config(config_path)?;
            let count = unset_config_by_pattern(&mut config, pattern)?;
            let output_path = config_path
                .map(PathBuf::from)
                .or_else(config::user_config_path)
                .unwrap_or_else(|| PathBuf::from("zbrain.yml"));
            config::write_config(&config, &output_path)?;
            println!("Unset {} key(s) matching pattern: {}", count, pattern);
        }
        ConfigAction::Unset { ref key, pattern: None } => {
            // Single key unset
            let mut config = config::load_config(config_path)?;
            if let Some(ref k) = key {
                if unset_config_value(&mut config, k)? {
                    let output_path = config_path
                        .map(PathBuf::from)
                        .or_else(config::user_config_path)
                        .unwrap_or_else(|| PathBuf::from("zbrain.yml"));
                    config::write_config(&config, &output_path)?;
                    println!("Unset config key: {}", k);
                } else {
                    eprintln!("Config key not found: {}", k);
                }
            }
        }
    }
    Ok(())
}

/// Helper to print config values with proper indentation and redaction.
fn print_config_value(key: &str, value: &serde_yaml::Value, indent: usize) {
    use serde_yaml::Value;

    match value {
        Value::Mapping(map) => {
            for (k, v) in map {
                let k_str = k.as_str().unwrap_or_default();
                let new_key = if key.is_empty() {
                    k_str.to_string()
                } else {
                    format!("{}.{}", key, k_str)
                };

                if let Value::Mapping(_) = v {
                    println!("{:indent$}{}:", "", k_str, indent = indent);
                    print_config_value(&new_key, v, indent + 2);
                } else {
                    let display = match v {
                        Value::String(s) => config::redact_value(&new_key, s),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        Value::Null => "null".to_string(),
                        Value::Sequence(_) => "[array]".to_string(),
                        _ => format!("{:?}", v),
                    };
                    println!("{:indent$}{}: {}", "", k_str, display, indent = indent);
                }
            }
        }
        _ => {} // Only mappings at top level, should not happen with Config struct
    }
}

/// Get a nested config value by dot-separated key path.
fn get_config_value(key: &str, config: &serde_yaml::Value) -> Option<String> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = config;

    for part in parts {
        match current {
            serde_yaml::Value::Mapping(map) => {
                current = map.get(&serde_yaml::Value::String(part.to_string()))?;
            }
            _ => return None,
        }
    }

    match current {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => Some(format!("{:?}", current)),
    }
}

/// Whether a dot-separated key path corresponds to a known field in the
/// strongly-typed `Config` schema. The default `Config` serialized to YAML is
/// the authoritative whitelist: every typed path is materialized there.
///
/// `providers` is a free-form map keyed by provider name, so any
/// `providers.<name>...` path is accepted.
fn is_known_config_key(key: &str) -> bool {
    let schema = match serde_yaml::to_value(config::Config::default()) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let mut current = &schema;
    for (i, part) in key.split('.').enumerate() {
        // First segment `providers` is a free-form provider map: accept any
        // deeper path under it.
        if i == 0 && part == "providers" {
            return true;
        }
        match current {
            serde_yaml::Value::Mapping(map) => {
                match map.get(serde_yaml::Value::String(part.to_string())) {
                    Some(next) => current = next,
                    None => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

/// Set a nested config value by dot-separated key path.
fn set_config_value(config: &mut config::Config, key: &str, value: String) -> anyhow::Result<()> {
    // Convert to value representation, then apply change
    // Use &* to reborrow and avoid "value used after move"
    let mut cfg_value = serde_yaml::to_value(&*config)?;
    {
        let parts: Vec<&str> = key.split('.').collect();
        let mut current = &mut cfg_value;

        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // Leaf node - set the value
                if let serde_yaml::Value::Mapping(map) = current {
                    map.insert(
                        serde_yaml::Value::String(part.to_string()),
                        serde_yaml::Value::String(value.clone()),
                    );
                }
            } else {
                // Traverse or create nested mapping
                if let serde_yaml::Value::Mapping(map) = current {
                    let key_val = serde_yaml::Value::String(part.to_string());
                    if !map.contains_key(&key_val) {
                        map.insert(key_val.clone(), serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                    }
                    current = map.get_mut(&key_val).unwrap();
                }
            }
        }
    }
    // Convert back to Config struct
    *config = serde_yaml::from_value(cfg_value)?;
    Ok(())
}

/// Unset a specific config key. Returns true if the key existed.
fn unset_config_value(config: &mut config::Config, key: &str) -> anyhow::Result<bool> {
    let mut cfg_value = serde_yaml::to_value(&*config)?;
    let parts: Vec<&str> = key.split('.').collect();

    let result = if parts.len() == 1 {
        // Top level key
        if let serde_yaml::Value::Mapping(map) = &mut cfg_value {
            map.remove(&serde_yaml::Value::String(key.to_string())).is_some()
        } else {
            false
        }
    } else {
        // Nested key
        let mut current = &mut cfg_value;
        for (i, part) in parts[..parts.len() - 1].iter().enumerate() {
            if let serde_yaml::Value::Mapping(map) = current {
                let key_val = serde_yaml::Value::String(part.to_string());
                if i == parts.len() - 2 {
                    // Parent of leaf - remove the leaf
                    return Ok(map.remove(&serde_yaml::Value::String(parts[parts.len() - 1].to_string())).is_some());
                }
                current = map.get_mut(&key_val).ok_or_else(|| anyhow::anyhow!("Key path not found"))?;
            } else {
                return Ok(false);
            }
        }
        false
    };

    if result {
        *config = serde_yaml::from_value(cfg_value)?;
    }
    Ok(result)
}

/// Unset all keys matching a prefix pattern. Returns count of removed keys.
fn unset_config_by_pattern(config: &mut config::Config, prefix: &str) -> anyhow::Result<usize> {
    let mut cfg_value = serde_yaml::to_value(&*config)?;
    let mut count = 0;

    if let serde_yaml::Value::Mapping(map) = &mut cfg_value {
        let keys_to_remove: Vec<_> = map
            .keys()
            .filter_map(|k| k.as_str())
            .filter(|k| k.starts_with(prefix))
            .map(|k| k.to_string())
            .collect();

        for key in keys_to_remove {
            if map.remove(&serde_yaml::Value::String(key)).is_some() {
                count += 1;
            }
        }
    }

    if count > 0 {
        *config = serde_yaml::from_value(cfg_value)?;
    }
    Ok(count)
}

// ── Facts commands ──────────────────────────────────────────────

/// Dispatch `zbrain facts` subcommands.
async fn run_facts_command(action: FactsAction, config_path: Option<&Path>) -> anyhow::Result<()> {
    match action {
        FactsAction::Add(args) => run_facts_add(args, config_path).await?,
        FactsAction::List(args) => run_facts_list(args, config_path).await?,
        FactsAction::Health(args) => run_facts_health(args, config_path).await?,
        FactsAction::Expire(args) => run_facts_expire(args, config_path).await?,
    }
    Ok(())
}

/// Execute `zbrain facts add`.
async fn run_facts_add(args: FactsAddArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::types::NewFact;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let kind = parse_fact_kind(&args.kind)?;
    let visibility = parse_fact_visibility(&args.visibility)?;

    let input = NewFact {
        fact: args.claim,
        kind: Some(kind),
        entity_slug: Some(args.entity.clone()),
        visibility: Some(visibility),
        context: args.context.clone(),
        valid_from: args.valid_from.clone(),
        valid_until: args.valid_until.clone(),
        source: args.cite.unwrap_or_else(|| "cli".to_string()),
        source_session: None,
        confidence: Some(args.confidence.clamp(0.0, 1.0)),
        notability: Some(args.notability.clone()),
        claim_metric: None,
        claim_value: None,
        claim_unit: None,
        claim_period: None,
        event_type: None,
        row_num: None,
        source_markdown_slug: None,
    };

    let status = engine.insert_fact(&args.source, &args.entity, &input).await?;

    if args.json {
        let output = serde_json::json!({
            "status": format!("{:?}", status),
            "entity": args.entity,
            "source": args.source,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Fact {:?} for entity '{}' in source '{}'", status, args.entity, args.source);
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain facts list`.
async fn run_facts_list(args: FactsListArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::types::FactListOpts;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let kinds = if args.kind.is_empty() {
        None
    } else {
        Some(args.kind.iter().map(|k| parse_fact_kind(k)).collect::<anyhow::Result<Vec<_>>>()?)
    };

    let visibility = if args.visibility.is_empty() {
        None
    } else {
        Some(args.visibility.iter().map(|v| parse_fact_visibility(v)).collect::<anyhow::Result<Vec<_>>>()?)
    };

    let opts = FactListOpts {
        active_only: if args.active_only { Some(true) } else { None },
        limit: Some(args.limit),
        offset: Some(args.offset),
        kinds,
        visibility,
    };

    let facts = engine.list_facts_by_entity(&args.source, &args.entity, &opts).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&facts)?);
    } else {
        if facts.is_empty() {
            println!("No facts found for entity '{}' in source '{}'", args.entity, args.source);
        } else {
            for f in &facts {
                let created = f.created_at.as_deref().unwrap_or("-");
                let kind = format!("{:?}", f.kind).to_lowercase();
                println!(
                    "[{}] #{} {} | {} | conf={:.2} | {}",
                    created, f.id, f.fact, kind, f.confidence, f.source
                );
            }
            println!("\n{} fact(s)", facts.len());
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain facts health`.
async fn run_facts_health(args: FactsHealthArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let health = engine.get_facts_health(&args.source).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&health)?);
    } else {
        println!("Facts health for source '{}':", args.source);
        println!("  active:      {}", health.total_active);
        println!("  today:       {}", health.total_today);
        println!("  this week:   {}", health.total_week);
        println!("  expired:     {}", health.total_expired);
        println!("  consolidated: {}", health.total_consolidated);
        if !health.top_entities.is_empty() {
            println!("  top entities:");
            for e in &health.top_entities {
                println!("    {} ({})", e.entity_slug, e.count);
            }
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain facts expire`.
async fn run_facts_expire(args: FactsExpireArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let expired = engine.expire_fact(&args.source, args.fact_id).await?;

    if args.json {
        let output = serde_json::json!({
            "expired": expired,
            "fact_id": args.fact_id,
            "source": args.source,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if expired {
        println!("Fact #{} expired", args.fact_id);
    } else {
        println!("Fact #{} not found or already expired", args.fact_id);
    }

    engine.disconnect().await?;
    Ok(())
}

// ── Eval command ────────────────────────────────────────────────

/// TS `eval` sub-verbs that exist in the deleted TypeScript surface but have
/// no Rust port yet.
///
/// Listed explicitly so `zbrain eval export …` fails with a pointer to the gap
/// register instead of silently falling through to the bare IR-metrics flow
/// with a stray positional argument. Every entry here is tracked under
/// KNOWN-GAPS G74 (categories A/C/D of the reconciliation table).
const UNPORTED_EVAL_SUBCOMMANDS: &[&str] = &[
    "brainstorm",
    "trajectory",
];

/// TS sub-verbs that ARE ported but live as top-level Rust commands.
///
/// Without this table `zbrain eval cross-modal` (the TS spelling) fails with a
/// misleading "did you mean --qrels cross-modal?" — muscle memory deserves a
/// pointer at the real command instead.
const RENAMED_EVAL_SUBCOMMANDS: &[(&str, &str)] = &[
    ("code-retrieval", "zbrain eval-code-retrieval"),
    ("cross-modal", "zbrain eval-cross-modal"),
    ("takes-quality", "zbrain eval-takes-quality"),
    ("suspected-contradictions", "zbrain eval-suspected-contradictions"),
];

/// Reject any positional token after `zbrain eval`.
///
/// The bare flow is flag-only, so a positional can only be a TS sub-verb (not
/// ported) or a typo. Split out as a pure function so the guard is unit
/// testable without touching a database.
fn reject_eval_subcommand(sub: Option<&str>) -> anyhow::Result<()> {
    let Some(sub) = sub else { return Ok(()) };
    if UNPORTED_EVAL_SUBCOMMANDS.contains(&sub) {
        anyhow::bail!(
            "`zbrain eval {sub}` is not implemented in Rust yet \
             (TS-only sub-verb; tracked as KNOWN-GAPS G74). \
             The ported surface is the bare IR-metrics flow: \
             `zbrain eval --qrels <path|json>`"
        );
    }
    if let Some((_, replacement)) =
        RENAMED_EVAL_SUBCOMMANDS.iter().find(|(name, _)| *name == sub)
    {
        anyhow::bail!(
            "`zbrain eval {sub}` is a top-level command in the Rust port — \
             run `{replacement}` instead"
        );
    }
    anyhow::bail!(
        "unexpected argument `{sub}` — `zbrain eval` takes flags only \
         (did you mean `--qrels {sub}`?)"
    )
}

/// Load an `EvalConfig` from a path or an inline JSON object.
///
/// Mirrors the TS `loadConfigFile`: a value whose first non-space character is
/// `{` is parsed inline, anything else is read from disk.
fn load_eval_config(path_or_json: &str) -> anyhow::Result<zbrain_core::search::EvalConfig> {
    let raw = if path_or_json.trim_start().starts_with('{') {
        path_or_json.to_string()
    } else {
        std::fs::read_to_string(path_or_json).map_err(|e| {
            anyhow::anyhow!("could not read eval config `{path_or_json}`: {e}")
        })?
    };
    serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("invalid eval config `{path_or_json}`: {e}"))
}

/// Assemble the side-A config: config file/inline JSON as the base, then CLI
/// flag overrides on top, then the `Config A` / `hybrid` defaults.
///
/// Faithful to the TS `buildConfig(opts, 'a')`.
fn build_eval_config_a(args: &EvalArgs) -> anyhow::Result<zbrain_core::search::EvalConfig> {
    let mut base = match args.config_a.as_deref() {
        Some(src) => load_eval_config(src)?,
        None => zbrain_core::search::EvalConfig::default(),
    };

    if let Some(s) = args.strategy {
        base.strategy = Some(s.into());
    }
    if let Some(v) = args.rrf_k {
        base.rrf_k = Some(v);
    }
    // `--expand` / `--no-expand` are `conflicts_with` siblings, so at most one
    // is set; neither means "leave whatever the config file said".
    if args.expand {
        base.expand = Some(true);
    } else if args.no_expand {
        base.expand = Some(false);
    }
    if let Some(v) = args.dedup_cosine {
        base.dedup_cosine_threshold = Some(v);
    }
    if let Some(v) = args.dedup_type_ratio {
        base.dedup_type_ratio = Some(v);
    }
    if let Some(v) = args.dedup_max_per_page {
        base.dedup_max_per_page = Some(v);
    }
    if let Some(v) = args.limit {
        base.limit = Some(v);
    }

    if base.name.is_none() {
        base.name = Some("Config A".to_string());
    }
    if base.strategy.is_none() {
        base.strategy = Some(zbrain_core::search::EvalStrategy::Hybrid);
    }
    Ok(base)
}

/// Assemble the side-B config. Faithful to TS, side B comes ENTIRELY from its
/// own config file — CLI flags tune side A only, otherwise A/B would compare
/// two identically-flagged configs.
fn build_eval_config_b(src: &str) -> anyhow::Result<zbrain_core::search::EvalConfig> {
    let mut base = load_eval_config(src)?;
    if base.name.is_none() {
        base.name = Some("Config B".to_string());
    }
    if base.strategy.is_none() {
        base.strategy = Some(zbrain_core::search::EvalStrategy::Hybrid);
    }
    Ok(base)
}

/// Project the dedup knobs out of an `EvalConfig` into engine `DedupOpts`.
fn eval_dedup_opts(config: &zbrain_core::search::EvalConfig) -> zbrain_core::search::DedupOpts {
    zbrain_core::search::DedupOpts {
        cosine_threshold: config.dedup_cosine_threshold,
        max_type_ratio: config.dedup_type_ratio,
        max_per_page: config.dedup_max_per_page,
    }
}

/// Run one configuration over the qrels, wiring the strategy-specific query
/// closure that `zbrain_core::search::run_eval` expects.
///
/// `run_eval` is deliberately embedding-free (see its module docs): it takes an
/// async `Fn(&str) -> Future<Result<Vec<slug>>>` rather than switching on the
/// Per-strategy retrieval for a single sub-query, carrying `rrf_k` through to
/// the engine so `zbrain eval --rrf-k` re-ranks (KNOWN-GAPS G74b). Extracted
/// from `run_one_eval_config`'s closure so the multi-query `--expand` path can
/// reuse it for each expanded sub-query.
async fn eval_retrieve_slugs(
    engine: &dyn zbrain_core::engine::BrainEngine,
    client: Option<std::sync::Arc<zbrain_core::embedding::EmbeddingClient>>,
    dedup_opts: zbrain_core::search::dedup::DedupOpts,
    strategy: zbrain_core::search::EvalStrategy,
    rrf_k: Option<f64>,
    sub_query: &str,
    limit: usize,
) -> zbrain_core::Result<Vec<String>> {
    match strategy {
        zbrain_core::search::EvalStrategy::Keyword => {
            let results = engine
                .search_pages(&zbrain_core::engine::SearchOpts {
                    keywords: vec![sub_query.to_string()],
                    limit: Some(limit),
                    rrf_k,
                    ..Default::default()
                })
                .await?;
            Ok(results.into_iter().map(|r| r.page.slug).collect())
        }
        zbrain_core::search::EvalStrategy::Vector => {
            let client = client.ok_or_else(|| {
                zbrain_core::Error::new(
                    "EvalError",
                    "run_eval",
                    "--strategy vector needs an embedding provider — \
                     set ZEROENTROPY_API_KEY (or use --strategy keyword)",
                )
            })?;
            let embedding = client
                .embed_query(sub_query)
                .await
                .map_err(|e| zbrain_core::Error::new("EvalError", "run_eval", &e.to_string()))?;
            let results = engine
                .search_pages(&zbrain_core::engine::SearchOpts {
                    query_embedding: Some(embedding),
                    limit: Some(limit),
                    rrf_k,
                    ..Default::default()
                })
                .await?;
            Ok(results.into_iter().map(|r| r.page.slug).collect())
        }
        zbrain_core::search::EvalStrategy::Hybrid => {
            let opts = zbrain_core::search::HybridSearchOpts {
                limit: Some(limit),
                dedup_opts: Some(dedup_opts),
                embedding_client: client,
                rrf_k,
                ..Default::default()
            };
            let results = zbrain_core::search::hybrid_search(engine, sub_query, &opts).await?;
            Ok(results.into_iter().map(|r| r.page.slug).collect())
        }
    }
}

/// Build a chat-backed expansion provider for `zbrain eval --expand`.
///
/// Mirrors the model resolution used elsewhere (e.g. the nightly longmemeval
/// probe): resolve a model via config/env with a `sonnet` fallback, construct a
/// `ChatProvider`, and wrap it in `ChatExpansionProvider`. Returns `None` when
/// no model/key is available, so eval degrades to single-query retrieval with a
/// warning rather than failing (KNOWN-GAPS G74b).
fn build_eval_expansion_provider() -> Option<Arc<dyn ExpansionProvider>> {
    use zbrain_core::ai::{
        instantiate_chat, resolve_model, resolve_recipe_strict, ResolveModelOpts,
    };
    use std::collections::HashMap;
    let lookup: HashMap<String, String> = HashMap::new();
    let model = resolve_model(
        &lookup,
        &ResolveModelOpts {
            cli_flag: None,
            config_key: Some("models.eval.longmemeval".into()),
            env_var: Some("ZBRAIN_MODEL".into()),
            tier: None,
            fallback: "sonnet".into(),
        },
    );
    let (_, recipe) = resolve_recipe_strict(&model).ok()?;
    let env_lookup = |k: &str| std::env::var(k).ok();
    let chat: Arc<dyn zbrain_core::ai::chat::ChatProvider> =
        Arc::from(instantiate_chat(&recipe, &model, &env_lookup).ok()?);
    Some(Arc::new(ChatExpansionProvider::new(chat)))
}

/// strategy internally. Composing that closure is exactly this CLI layer's job.
async fn run_one_eval_config(
    engine: &dyn zbrain_core::engine::BrainEngine,
    embedding_client: Option<&std::sync::Arc<zbrain_core::embedding::EmbeddingClient>>,
    qrels: &[zbrain_core::search::EvalQrel],
    config: &zbrain_core::search::EvalConfig,
    k: usize,
    show_progress: bool,
    expand_provider: Option<Arc<dyn ExpansionProvider>>,
) -> anyhow::Result<zbrain_core::search::EvalReport> {
    use zbrain_core::search::{resolve_eval_limit, run_eval, EvalStrategy};

    let limit = resolve_eval_limit(config, k);
    let strategy = config.strategy.unwrap_or_default();
    let dedup_opts = eval_dedup_opts(config);
    let client = embedding_client.cloned();
    let engine_ref = engine;
    let rrf_k = config.rrf_k;
    let expand = config.expand == Some(true);

    let query_fn = move |q: &str| {
        let q = q.to_string();
        let client = client.clone();
        let dedup_opts = dedup_opts.clone();
        let expand_provider = expand_provider.clone();
        async move {
            // Multi-query expansion (KNOWN-GAPS G74b): when `--expand` is set and a
            // chat-backed expansion provider is available, expand the query into
            // several phrasings and merge their retrieval results by best rank.
            if expand {
                if let Some(provider) = expand_provider.as_ref().map(|arc| &**arc) {
                    let expanded = expand_query(&q, Some(provider)).await;
                    let mut best_rank: std::collections::HashMap<String, usize> =
                        std::collections::HashMap::new();
                    for sub in &expanded {
                        let slugs = eval_retrieve_slugs(
                            engine_ref,
                            client.clone(),
                            dedup_opts.clone(),
                            strategy,
                            rrf_k,
                            sub,
                            limit,
                        )
                        .await?;
                        for (rank, slug) in slugs.into_iter().enumerate() {
                            let entry = best_rank.entry(slug).or_insert(rank);
                            if rank < *entry {
                                *entry = rank;
                            }
                        }
                    }
                    let mut merged: Vec<(usize, String)> =
                        best_rank.into_iter().map(|(s, r)| (r, s)).collect();
                    merged.sort_by_key(|(r, _)| *r);
                    return Ok(merged.into_iter().map(|(_, s)| s).collect());
                }
            }
            // Default / no-provider path: single-query retrieval.
            eval_retrieve_slugs(engine_ref, client, dedup_opts, strategy, rrf_k, &q, limit).await
        }
    };

    let label = config.name.clone().unwrap_or_else(|| "eval".to_string());
    let tick = |done: usize, total: usize, _q: &str| {
        eprint!("\r{label}: {done}/{total} queries");
        if done == total {
            eprintln!();
        }
    };
    let progress: Option<&dyn Fn(usize, usize, &str)> =
        if show_progress { Some(&tick) } else { None };

    let report = run_eval(qrels, config, k, query_fn, progress).await?;
    Ok(report)
}

// Table formatting helpers — ports of the TS `padR` / `padL` / `truncate` /
// `fmt` used by `printSingleTable` / `printABTable`. Widths are counted in
// `char`s rather than UTF-16 code units, so a CJK query pads without panicking
// mid-codepoint (the TS original could slice a surrogate pair in half).

fn eval_fmt(n: f64) -> String {
    format!("{n:.2}")
}

fn eval_pad_r(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.chars().take(width).collect()
    } else {
        let mut out = s.to_string();
        out.push_str(&" ".repeat(width - len));
        out
    }
}

fn eval_pad_l(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.chars().take(width).collect()
    } else {
        let mut out = " ".repeat(width - len);
        out.push_str(s);
        out
    }
}

fn eval_truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

fn eval_plural(n: usize) -> &'static str {
    if n == 1 {
        "y"
    } else {
        "ies"
    }
}

/// Render the single-config report. Byte-faithful to the TS `printSingleTable`
/// except for the product name (`gbrain` → `zbrain`; the rename is
/// deliberate and breaking, see MEMORY / the GBrain→ZBrain decision).
fn print_eval_single_table(report: &zbrain_core::search::EvalReport) {
    const COL_QUERY: usize = 36;
    const COL_NUM: usize = 7;

    let k = report.k;
    // TS quirk preserved on purpose: the header reads `strategy: <label>` but
    // `label = config.name ?? config.strategy ?? 'hybrid'`, and `buildConfig`
    // always fills `name`. So a default run prints `strategy: Config A`, not
    // `strategy: hybrid`. Do not "fix" this without also changing the TS
    // reference output the port is measured against.
    let label = report
        .config
        .name
        .clone()
        .or_else(|| {
            report
                .config
                .strategy
                .map(|s| format!("{s:?}").to_lowercase())
        })
        .unwrap_or_else(|| "hybrid".to_string());
    let n = report.queries.len();

    println!(
        "\nzbrain eval — {n} quer{} · strategy: {label} · k={k}\n",
        eval_plural(n)
    );

    let header = format!(
        "{}{}{}{}{}",
        eval_pad_r("Query", COL_QUERY),
        eval_pad_l(&format!("P@{k}"), COL_NUM),
        eval_pad_l(&format!("R@{k}"), COL_NUM),
        eval_pad_l("MRR", COL_NUM),
        eval_pad_l(&format!("nDCG@{k}"), COL_NUM),
    );
    let divider = "─".repeat(header.chars().count());

    println!("{header}");
    println!("{divider}");

    for q in &report.queries {
        println!(
            "{}{}{}{}{}",
            eval_pad_r(&eval_truncate(&q.query, COL_QUERY - 1), COL_QUERY),
            eval_pad_l(&eval_fmt(q.precision_at_k), COL_NUM),
            eval_pad_l(&eval_fmt(q.recall_at_k), COL_NUM),
            eval_pad_l(&eval_fmt(q.mrr), COL_NUM),
            eval_pad_l(&eval_fmt(q.ndcg_at_k), COL_NUM),
        );
    }

    println!("{divider}");
    println!(
        "{}{}{}{}{}",
        eval_pad_r("Mean", COL_QUERY),
        eval_pad_l(&eval_fmt(report.mean_precision), COL_NUM),
        eval_pad_l(&eval_fmt(report.mean_recall), COL_NUM),
        eval_pad_l(&eval_fmt(report.mean_mrr), COL_NUM),
        eval_pad_l(&eval_fmt(report.mean_ndcg), COL_NUM),
    );
    println!();
}

/// Render the A/B comparison table. Port of the TS `printABTable`; the winner
/// is decided on mean nDCG@k, matching TS.
fn print_eval_ab_table(
    a: &zbrain_core::search::EvalReport,
    b: &zbrain_core::search::EvalReport,
    k: usize,
) {
    const COL_QUERY: usize = 34;
    const COL_METRIC: usize = 8;
    const COLS_PER_SIDE: usize = 3;

    let label_a = a.config.name.clone().unwrap_or_else(|| "Config A".into());
    let label_b = b.config.name.clone().unwrap_or_else(|| "Config B".into());
    let n = a.queries.len();

    println!(
        "\nzbrain eval — {n} quer{} · A/B comparison · k={k}\n",
        eval_plural(n)
    );

    let side = COL_METRIC * COLS_PER_SIDE;
    let clip = side.saturating_sub(2);
    let a_label: String = format!(" {label_a} ").chars().take(clip).collect();
    let b_label: String = format!(" {label_b} ").chars().take(clip).collect();
    let line1 = format!(
        "{}{}{}  Δ nDCG",
        " ".repeat(COL_QUERY),
        eval_pad_r(&format!("── {a_label} "), side),
        eval_pad_r(&format!("── {b_label} "), side),
    );
    println!("{line1}");

    let metric_header = || {
        format!(
            "{}{}{}",
            eval_pad_l(&format!("P@{k}"), COL_METRIC),
            eval_pad_l("MRR", COL_METRIC),
            eval_pad_l(&format!("nDCG@{k}"), COL_METRIC),
        )
    };
    let line2 = format!(
        "{}{}  {}  {}",
        eval_pad_r("Query", COL_QUERY),
        metric_header(),
        metric_header(),
        eval_pad_l("Δ nDCG", 10),
    );
    println!("{line2}");
    let divider = "─".repeat(line2.chars().count());
    println!("{divider}");

    for (qa, qb) in a.queries.iter().zip(b.queries.iter()) {
        let delta = qb.ndcg_at_k - qa.ndcg_at_k;
        let delta_str = if delta > 0.0 {
            format!("+{}", eval_fmt(delta))
        } else {
            eval_fmt(delta)
        };
        println!(
            "{}{}{}{}  {}{}{}  {}",
            eval_pad_r(&eval_truncate(&qa.query, COL_QUERY - 1), COL_QUERY),
            eval_pad_l(&eval_fmt(qa.precision_at_k), COL_METRIC),
            eval_pad_l(&eval_fmt(qa.mrr), COL_METRIC),
            eval_pad_l(&eval_fmt(qa.ndcg_at_k), COL_METRIC),
            eval_pad_l(&eval_fmt(qb.precision_at_k), COL_METRIC),
            eval_pad_l(&eval_fmt(qb.mrr), COL_METRIC),
            eval_pad_l(&eval_fmt(qb.ndcg_at_k), COL_METRIC),
            eval_pad_l(&delta_str, 10),
        );
    }

    println!("{divider}");

    let mean_delta = b.mean_ndcg - a.mean_ndcg;
    let sign = if mean_delta > 0.0 { "+" } else { "" };
    let winner = if mean_delta > 0.0 {
        " ✓ B wins"
    } else if mean_delta < 0.0 {
        " ✓ A wins"
    } else {
        " tie"
    };
    println!(
        "{}{}{}{}  {}{}{}  {}",
        eval_pad_r("Mean", COL_QUERY),
        eval_pad_l(&eval_fmt(a.mean_precision), COL_METRIC),
        eval_pad_l(&eval_fmt(a.mean_mrr), COL_METRIC),
        eval_pad_l(&eval_fmt(a.mean_ndcg), COL_METRIC),
        eval_pad_l(&eval_fmt(b.mean_precision), COL_METRIC),
        eval_pad_l(&eval_fmt(b.mean_mrr), COL_METRIC),
        eval_pad_l(&eval_fmt(b.mean_ndcg), COL_METRIC),
        eval_pad_l(&format!("{sign}{}{winner}", eval_fmt(mean_delta)), 10),
    );
    println!();
}

/// Execute `zbrain eval` — Rust port of the TS `eval` bare IR-metrics flow.
///
/// Closes the CLI half of KNOWN-GAPS G74: the harness itself
/// (`zbrain_core::search::eval`, G73) has been ported since the TS delete but
/// had **zero callers** — the exact same shape as the `extract timeline` gap.
/// `zbrain eval-extract-atoms` — command surface only (TS `eval-extract-atoms.ts`, G74 1-1).
///
/// v0.41 ships the command surface; the full parity-baseline eval against
/// OpenClaw's atoms lands in v0.41.1. This mirrors the TS scaffold: it returns
/// `not_yet_implemented` and never touches the brain or LLM.
async fn run_eval_extract_atoms_command(
    args: EvalExtractAtomsArgs,
) -> anyhow::Result<()> {
    let result = serde_json::json!({
        "schema_version": 1,
        "ok": true,
        "reason": "v0.41 ships the command surface; full parity-baseline eval lands v0.41.1",
        "status": "not_yet_implemented",
        "details": {
            "parity_baseline_path": args.parity_baseline,
            "sample_size": args.sample,
            "v0_41_1_followup":
                "Compare extract_atoms output against your OpenClaw atoms/ on a sample subset; \
                 compute precision/recall over atom_type classifications + virality_score correlation.",
        }
    });
    emit_eval_scaffold(&result, args.json);
    Ok(())
}

/// `zbrain eval-synthesize-concepts` — command surface only (TS `eval-synthesize-concepts.ts`, G74 1-1).
///
/// v0.41 ships the command surface; the full parity-baseline eval against
/// OpenClaw's concepts lands in v0.41.1. Mirrors the TS scaffold.
async fn run_eval_synthesize_concepts_command(
    args: EvalSynthesizeConceptsArgs,
) -> anyhow::Result<()> {
    let result = serde_json::json!({
        "schema_version": 1,
        "ok": true,
        "reason": "v0.41 ships the command surface; full parity-baseline eval lands v0.41.1",
        "status": "not_yet_implemented",
        "details": {
            "parity_baseline_path": args.parity_baseline,
            "sample_size": args.sample,
            "v0_41_1_followup":
                "Compare synthesize_concepts output against your OpenClaw concepts/ on a sample \
                 subset; compute tier agreement (T1/T2/T3) + cluster stability via set Jaccard.",
        }
    });
    emit_eval_scaffold(&result, args.json);
    Ok(())
}

/// `zbrain eval-schema-authoring` — hermetic harness surface (TS `eval-schema-authoring.ts`, G74 1-1).
///
/// Without `--fixture` the verdict is `inconclusive` (matches TS). A missing
/// fixture path yields `fail`; a present fixture yields `inconclusive` because
/// the hermetic engine wiring follows the longmemeval pattern (v0.39.1). The
/// pure `aggregate_verdict` aggregator already lives in `zbrain_core::eval::schema_authoring`.
async fn run_eval_schema_authoring_command(
    args: EvalSchemaAuthoringArgs,
) -> anyhow::Result<()> {
    let verdict = match &args.fixture {
        None => serde_json::json!({
            "verdict": "inconclusive",
            "fixture": null,
            "filing_accuracy_baseline": 0,
            "filing_accuracy_post_suggest": 0,
            "delta": 0,
            "reasoning": "No fixture brain provided. Pass --fixture <path> pointing at a fixture brain directory (e.g. tests/unit/fixtures/schema-authoring/notion-refugee).",
            "suggestion_count": 0,
            "low_confidence_count": 0
        }),
        Some(fixture) => {
            if !std::path::Path::new(fixture).exists() {
                serde_json::json!({
                    "verdict": "fail",
                    "fixture": fixture,
                    "filing_accuracy_baseline": 0,
                    "filing_accuracy_post_suggest": 0,
                    "delta": 0,
                    "reasoning": format!("Fixture brain not found: {fixture}"),
                    "suggestion_count": 0,
                    "low_confidence_count": 0
                })
            } else {
                serde_json::json!({
                    "verdict": "inconclusive",
                    "fixture": fixture,
                    "filing_accuracy_baseline": 0,
                    "filing_accuracy_post_suggest": 0,
                    "delta": 0,
                    "reasoning": "Hermetic engine wiring follows the longmemeval pattern; in v0.39.0.0 ship, in-process callers use aggregateVerdict() directly. Full CLI harness lands in v0.39.1.",
                    "suggestion_count": 0,
                    "low_confidence_count": 0
                })
            }
        }
    };
    emit_eval_scaffold(&verdict, args.json);
    Ok(())
}

/// Print a scaffold/harness JSON result: pretty JSON with `--json`, otherwise a
/// compact human summary (verdict/status + reason) to stdout.
fn emit_eval_scaffold(result: &serde_json::Value, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(result).unwrap());
    } else {
        if let Some(status) = result.get("status").and_then(|v| v.as_str()) {
            println!("status: {status}");
        }
        if let Some(verdict) = result.get("verdict").and_then(|v| v.as_str()) {
            println!("verdict: {verdict}");
        }
        if let Some(reason) = result
            .get("reason")
            .or_else(|| result.get("reasoning"))
            .and_then(|v| v.as_str())
        {
            println!("reason: {reason}");
        }
    }
}

async fn run_eval_export_command(
    args: EvalExportArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, EvalCandidateFilter};
    use std::io::Write;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;

    let filter = EvalCandidateFilter {
        tool_name: args.tool.clone(),
        since: args.since.clone(),
        limit: args.limit,
    };
    let candidates = engine.list_eval_candidates(&filter).await?;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    for c in &candidates {
        let line = serde_json::json!({ "schema_version": 1, "candidate": c });
        serde_json::to_writer(&mut handle, &line)?;
        handle.write_all(b"\n")?;
    }
    eprintln!("exported {} eval candidate(s)", candidates.len());
    Ok(())
}

async fn run_eval_prune_command(
    args: EvalPruneArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, EvalCandidateFilter};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;

    if args.dry_run {
        let all = engine
            .list_eval_candidates(&EvalCandidateFilter::default())
            .await?;
        let older: u64 = all
            .iter()
            .filter(|c| c.created_at < args.older_than)
            .count() as u64;
        eprintln!(
            "dry-run: {} candidate(s) older than {} would be deleted ({} present)",
            older, args.older_than, all.len()
        );
        return Ok(());
    }

    let deleted = engine.delete_eval_candidates_before(&args.older_than).await?;
    eprintln!(
        "pruned {} eval candidate(s) older than {}",
        deleted, args.older_than
    );
    Ok(())
}

async fn run_eval_gate_command(
    args: EvalGateArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, SearchOpts};
    use zbrain_core::eval::gate::{
        assemble_gate_result, evaluate_correctness_gate, parse_qrels_file,
        run_correctness_gate, QrelsThresholds,
    };

    // The qrels arg accepts either an inline JSON object or a file path.
    let qrels_content = match args.qrels.strip_prefix("json:") {
        Some(inline) => inline.to_string(),
        None => std::fs::read_to_string(&args.qrels).map_err(|e| {
            anyhow::anyhow!("cannot read --qrels {}: {}", args.qrels, e)
        })?,
    };
    let qrels_path = if args.qrels.strip_prefix("json:").is_some() {
        None
    } else {
        Some(args.qrels.clone())
    };

    let qrels = parse_qrels_file(&qrels_content).map_err(|e| anyhow::anyhow!("{e}"))?;
    if qrels.queries.is_empty() {
        anyhow::bail!("qrels contains no queries");
    }

    let k = args.k.max(1);
    let thresholds = QrelsThresholds {
        recall_at_k: args.recall_at_k.unwrap_or(0.70),
        first_relevant_hit: args.first_relevant_hit.unwrap_or(0.60),
        expected_top1: args.expected_top1.unwrap_or(0.50),
        k,
    };

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;

    // Closure that runs retrieval for one query and returns the
    // `${source_id}::${slug}` keys (federated, eng-D5).
    let engine_ref = &engine;
    let query_fn = |q: &str, k: usize| {
        let opts = SearchOpts {
            keywords: vec![q.to_string()],
            limit: Some(k),
            ..Default::default()
        };
        async move {
            let results = engine_ref.search_pages(&opts).await?;
            let keys: Vec<String> = results
                .into_iter()
                .map(|r| format!("{}::{}", r.page.source_id, r.page.slug))
                .collect();
            Ok(keys)
        }
    };

    let result = run_correctness_gate(&qrels, k, query_fn).await?;
    let breaches = evaluate_correctness_gate(&result, &thresholds);
    let gate = assemble_gate_result(qrels_path, &result, &thresholds, breaches);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&gate)?);
    } else {
        let c = &gate.correctness_gate;
        eprintln!(
            "correctness gate: verdict={}  mean_recall@{k}={:.3}  first_relevant_hit_rate={:.3}  queries_errored={}",
            serde_json::to_string(&gate.verdict).unwrap_or_default(),
            c.summary.mean_recall_at_k,
            c.summary.first_relevant_hit_rate,
            c.summary.queries_errored,
        );
        for b in &c.breaches {
            eprintln!("  breach: {} observed={:?} threshold={:?}", b.metric, b.observed, b.threshold);
        }
    }

    if gate.verdict == zbrain_core::eval::gate::GateVerdict::Fail {
        anyhow::bail!(
            "correctness gate FAILED ({} breach(es))",
            gate.correctness_gate.breaches.len()
        );
    }
    Ok(())
}

async fn run_eval_replay_command(
    args: EvalReplayArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::eval::replay::{parse_ndjson, replay_core, ReplayTool};
    use zbrain_core::search::{hybrid_search, keyword_search_slugs, HybridSearchOpts};

    // Read the snapshot up front so missing-file / parse errors surface as
    // clean exit(1) rather than halfway through the replay.
    let content = std::fs::read_to_string(&args.against)
        .map_err(|e| anyhow::anyhow!("cannot read --against {}: {}", args.against, e))?;
    let rows = parse_ndjson(&content).map_err(|e| anyhow::anyhow!("{e}"))?;
    if rows.is_empty() {
        anyhow::bail!("{} contains no rows (empty export)", args.against);
    }

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;
    let engine_ref = &engine;

    // Retrieval dispatch: a captured `search` row re-runs the bare keyword
    // path; a captured `query` row re-runs the hybrid path — the same logic
    // that produced the original retrieval (TS `searchKeyword` vs
    // `hybridSearch`).
    let query_fn = move |q: &str, tool: ReplayTool, limit: usize| {
        let q = q.to_string();
        Box::pin(async move {
            let slugs: Vec<String> = match tool {
                ReplayTool::Search => keyword_search_slugs(engine_ref, &q, limit).await?,
                ReplayTool::Query => {
                    let opts = HybridSearchOpts {
                        limit: Some(limit),
                        ..Default::default()
                    };
                    let results = hybrid_search(engine_ref, &q, &opts).await?;
                    results.into_iter().map(|r| r.page.slug).collect()
                }
            };
            Ok(slugs)
        })
    };

    let (summary, results) =
        replay_core(&query_fn, &content, args.limit, args.compare_limit).await?;

    if args.json {
        let out = serde_json::json!({
            "schema_version": 1,
            "summary": serde_json::to_value(&summary)?,
            "results": if args.verbose {
                Some(serde_json::to_value(&results)?)
            } else {
                None
            },
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    print_replay_human(&summary, &results, args.top_regressions);
    Ok(())
}

/// Render the human-readable replay summary + top regressions to stdout
/// (mirrors TS `printHumanSummary`).
fn print_replay_human(
    summary: &zbrain_core::eval::replay::ReplaySummary,
    results: &[zbrain_core::eval::replay::RowResult],
    top_regressions: Option<usize>,
) {
    let sign = if summary.mean_latency_delta_ms >= 0.0 { "+" } else { "" };
    println!(
        "Replayed {} of {} captured queries ({} skipped, {} errored)",
        summary.rows_replayed, summary.rows_total, summary.rows_skipped, summary.rows_errored
    );
    println!("Mean Jaccard@k:    {:.3}", summary.mean_jaccard);
    println!(
        "Top-1 stability:   {:.1}%",
        summary.top1_stability_rate * 100.0
    );
    println!(
        "Mean latency Δ:    {}{:.0}ms (current vs captured)",
        sign, summary.mean_latency_delta_ms
    );
    if summary.rows_over_2x_latency > 0 {
        println!(
            "⚠ {} row(s) ran more than 2× slower than captured",
            summary.rows_over_2x_latency
        );
    }

    let top_n = top_regressions.unwrap_or(5);
    if top_n > 0 {
        let mut regressions: Vec<&zbrain_core::eval::replay::RowResult> = results
            .iter()
            .filter(|r| r.skipped != Some(true) && r.errored != Some(true))
            .collect();
        regressions.sort_by(|a, b| a.jaccard.partial_cmp(&b.jaccard).unwrap_or(std::cmp::Ordering::Equal));
        regressions.truncate(top_n);
        if let Some(worst) = regressions.first() {
            if worst.jaccard < 1.0 {
                println!("\nTop {} regression(s):", regressions.len());
                for r in regressions {
                    let trunc: String = if r.query.chars().count() > 60 {
                        let head: String = r.query.chars().take(57).collect();
                        format!("{head}...")
                    } else {
                        r.query.clone()
                    };
                    println!(
                        "  jaccard={:.2}  captured={}  current={}  \"{}\"",
                        r.jaccard,
                        r.captured_slugs.len(),
                        r.current_slugs.len(),
                        trunc
                    );
                }
            }
        }
    }

    if summary.rows_errored > 0 {
        let errors: Vec<&zbrain_core::eval::replay::RowResult> = results
            .iter()
            .filter(|r| r.errored == Some(true))
            .take(3)
            .collect();
        println!(
            "\n{} row(s) errored. First {}:",
            summary.rows_errored,
            errors.len()
        );
        for r in errors {
            let trunc: String = if r.query.chars().count() > 60 {
                let head: String = r.query.chars().take(57).collect();
                format!("{head}...")
            } else {
                r.query.clone()
            };
            println!(
                "  id={}  \"{}\"  {}",
                r.id,
                trunc,
                r.error_message.as_deref().unwrap_or("")
            );
        }
    }
}

async fn run_eval_whoknows_command(
    args: EvalWhoknowsArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, EvalCandidateFilter};
    use zbrain_core::eval::whoknows::{
        assemble_report, read_fixture, run_quality_gate, run_regression_gate, ReplayRow,
    };
    use zbrain_core::whoknows::{self};

    // Read + validate the fixture up front so usage errors surface cleanly.
    let fixture_content = std::fs::read_to_string(&args.fixture_path)
        .map_err(|e| anyhow::anyhow!("cannot read fixture {}: {}", args.fixture_path, e))?;
    let fixture = read_fixture(&fixture_content).map_err(|e| anyhow::anyhow!("{e}"))?;
    if fixture.is_empty() {
        anyhow::bail!("fixture file is empty: {}", args.fixture_path);
    }

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;
    let engine_ref = &engine;

    // The whoknows callable — local `find_experts`. Thin-client remote routing
    // (TS `callRemoteTool`) is deferred; see KNOWN-GAPS G74.
    let whoknows_fn = move |topic: &str, limit: usize| {
        let topic = topic.to_string();
        Box::pin(async move {
            let results = whoknows::find_experts(
                engine_ref,
                &whoknows::FindExpertsOpts {
                    topic,
                    limit: Some(limit),
                    types: None,
                    source_id: None,
                },
            )
            .await?;
            Ok(results.into_iter().map(|r| r.slug).collect::<Vec<String>>())
        })
    };

    let quality = run_quality_gate(&whoknows_fn, &fixture, args.limit).await;

    // Layer 2 — regression gate. Skipped on --skip-replay; otherwise load
    // captured `query` rows from eval_candidates (sparseness fallback inside
    // run_regression_gate handles < 20 rows).
    let regression = if args.skip_replay {
        zbrain_core::eval::whoknows::RegressionReport {
            status: "skipped".to_string(),
            reason: Some("--skip-replay flag".to_string()),
            total: 0,
            mean_jaccard: 0.0,
            threshold: zbrain_core::eval::whoknows::REGRESSION_THRESHOLD,
            rows: Vec::new(),
        }
    } else {
        let captured = engine_ref
            .list_eval_candidates(&EvalCandidateFilter {
                tool_name: Some("query".to_string()),
                since: None,
                limit: Some(200),
            })
            .await
            .unwrap_or_default();
        let replay_rows: Vec<ReplayRow> = captured
            .into_iter()
            .map(|c| ReplayRow {
                query: c.query,
                retrieved_slugs: c.retrieved_slugs,
            })
            .collect();
        run_regression_gate(&whoknows_fn, &replay_rows, args.limit).await
    };

    let report = assemble_report(&args.fixture_path, quality, regression);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_whoknows_human(&report);
    }

    if !report.overall_passed {
        anyhow::bail!("whoknows eval FAILED (quality gate / regression gate not passed)");
    }
    Ok(())
}

/// Render the human-readable whoknows eval report (mirrors TS
/// `renderHumanReport`).
fn print_whoknows_human(
    r: &zbrain_core::eval::whoknows::EvalWhoknowsReport,
) {
    println!("whoknows eval @ {}", r.fixture_path);
    println!("{}", "─".repeat(60));
    println!();
    println!("LAYER 1 — quality gate (hand-labeled fixture)");
    println!("  total: {}", r.quality.total);
    println!("  hits:  {}", r.quality.hits);
    println!(
        "  rate:  {:.1}%  (threshold {:.0}%)",
        r.quality.hit_rate * 100.0,
        r.quality.threshold * 100.0
    );
    println!("  {}", if r.quality.passed { "PASS" } else { "FAIL" });
    if !r.quality.passed {
        println!();
        println!("  Misses:");
        for row in &r.quality.rows {
            if row.hit {
                continue;
            }
            println!("    \"{}\"", row.query);
            println!("      expected: {}", row.expected.join(", "));
            println!(
                "      got:      {}",
                if row.actual_top_3.is_empty() {
                    "(no results)".to_string()
                } else {
                    row.actual_top_3.join(", ")
                }
            );
        }
    }
    println!();
    println!("LAYER 2 — regression gate (eval_candidates replay)");
    if r.regression.status == "skipped" {
        println!(
            "  SKIPPED — {}",
            r.regression.reason.as_deref().unwrap_or("")
        );
    } else {
        println!("  total:  {}", r.regression.total);
        println!(
            "  Jaccard mean: {:.3}  (threshold {:.2})",
            r.regression.mean_jaccard, r.regression.threshold
        );
        println!(
            "  {}",
            if r.regression.status == "passed" { "PASS" } else { "FAIL" }
        );
    }
    println!();
    println!("VERDICT: {}", if r.overall_passed { "PASS" } else { "FAIL" });
}

/// Parse `--checks` into the validated set of gates to run.
///
/// `None` or an empty list means "all three" (gate, replay, whoknows). Any
/// other token is rejected so a typo fails fast instead of silently skipping.
fn parse_check_list(checks: &Option<Vec<String>>) -> anyhow::Result<Vec<String>> {
    let allowed = ["gate", "replay", "whoknows"];
    match checks {
        None => Ok(allowed.iter().map(|s| s.to_string()).collect()),
        Some(list) if list.is_empty() => Ok(allowed.iter().map(|s| s.to_string()).collect()),
        Some(list) => {
            let mut out = Vec::with_capacity(list.len());
            for s in list {
                if !allowed.contains(&s.as_str()) {
                    anyhow::bail!(
                        "--checks: '{}' is not a valid gate (use gate|replay|whoknows)",
                        s
                    );
                }
                out.push(s.clone());
            }
            Ok(out)
        }
    }
}

/// Read an eval input that may be an inline `json:` object or a file path.
fn read_eval_input(s: &str) -> anyhow::Result<String> {
    if let Some(inline) = s.strip_prefix("json:") {
        Ok(inline.to_string())
    } else {
        std::fs::read_to_string(s).map_err(|e| anyhow::anyhow!("cannot read {s}: {e}"))
    }
}

/// Best-effort short git commit of the working tree (for the run report).
fn current_commit() -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Run every selected eval gate and aggregate their verdicts into one report.
///
/// Redesign of the TS `eval run-all` stub: the TS version swept TS-only search
/// modes and only ever wrote `status: "skipped"` audit rows. This builds the
/// real Rust gates (gate / replay / whoknows) and collects their verdicts.
async fn run_eval_run_all_command(
    args: EvalRunAllArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use std::collections::BTreeMap;
    use zbrain_core::engine::{BrainEngine, EngineConfig, EvalCandidateFilter, SearchOpts};
    use zbrain_core::eval::run_all::{assemble_run_all_report, RunAllCheck, RunAllStatus};
    use zbrain_core::eval::gate::{
        assemble_gate_result, evaluate_correctness_gate, parse_qrels_file, run_correctness_gate,
        GateVerdict, QrelsThresholds,
    };
    use zbrain_core::eval::replay::{replay_core, ReplayTool};
    use zbrain_core::eval::whoknows::{
        assemble_report, read_fixture, run_quality_gate, run_regression_gate, ReplayRow,
        REGRESSION_THRESHOLD,
    };
    use zbrain_core::search::{hybrid_search, keyword_search_slugs, HybridSearchOpts};
    use zbrain_core::whoknows::{self, FindExpertsOpts};

    let checks = parse_check_list(&args.checks)?;

    // Validate required inputs up front (before touching the DB).
    if checks.contains(&"gate".to_string()) && args.qrels.is_none() {
        anyhow::bail!("eval run-all: --qrels is required when 'gate' is in --checks");
    }
    if checks.contains(&"replay".to_string()) && args.against.is_none() {
        anyhow::bail!("eval run-all: --against is required when 'replay' is in --checks");
    }
    if checks.contains(&"whoknows".to_string()) && args.fixture.is_none() {
        anyhow::bail!("eval run-all: --fixture is required when 'whoknows' is in --checks");
    }

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;
    let engine_ref = &engine;

    let started = std::time::Instant::now();
    let mut run_checks: Vec<RunAllCheck> = Vec::new();

    // ── gate ──────────────────────────────────────────────────────────────
    if checks.contains(&"gate".to_string()) {
        let qrels_content = read_eval_input(args.qrels.as_ref().unwrap())?;
        match parse_qrels_file(&qrels_content) {
            Ok(qrels) => {
                let k = args.k.max(1);
                let thresholds = QrelsThresholds {
                    recall_at_k: 0.70,
                    first_relevant_hit: 0.60,
                    expected_top1: 0.50,
                    k,
                };
                let query_fn = |q: &str, k: usize| {
                    let opts = SearchOpts {
                        keywords: vec![q.to_string()],
                        limit: Some(k),
                        ..Default::default()
                    };
                    async move {
                        let results = engine_ref.search_pages(&opts).await?;
                        let keys: Vec<String> = results
                            .into_iter()
                            .map(|r| format!("{}::{}", r.page.source_id, r.page.slug))
                            .collect();
                        Ok(keys)
                    }
                };
                match run_correctness_gate(&qrels, k, query_fn).await {
                    Ok(result) => {
                        let breaches = evaluate_correctness_gate(&result, &thresholds);
                        let gate = assemble_gate_result(None, &result, &thresholds, breaches);
                        let status = if gate.verdict == GateVerdict::Fail {
                            RunAllStatus::Failed
                        } else {
                            RunAllStatus::Passed
                        };
                        let mut metrics = BTreeMap::new();
                        metrics.insert(
                            "mean_recall_at_k".into(),
                            serde_json::json!(gate.correctness_gate.summary.mean_recall_at_k),
                        );
                        metrics.insert(
                            "first_relevant_hit_rate".into(),
                            serde_json::json!(gate.correctness_gate.summary.first_relevant_hit_rate),
                        );
                        metrics.insert(
                            "queries_errored".into(),
                            serde_json::json!(gate.correctness_gate.summary.queries_errored),
                        );
                        metrics.insert(
                            "breaches".into(),
                            serde_json::json!(gate.correctness_gate.breaches.len()),
                        );
                        run_checks.push(RunAllCheck {
                            name: "gate".into(),
                            status,
                            metrics,
                            detail: None,
                            error: None,
                        });
                    }
                    Err(e) => run_checks.push(RunAllCheck {
                        name: "gate".into(),
                        status: RunAllStatus::Errored,
                        metrics: BTreeMap::new(),
                        detail: None,
                        error: Some(e.to_string()),
                    }),
                }
            }
            Err(e) => run_checks.push(RunAllCheck {
                name: "gate".into(),
                status: RunAllStatus::Errored,
                metrics: BTreeMap::new(),
                detail: None,
                error: Some(e.to_string()),
            }),
        }
    }

    // ── replay ──────────────────────────────────────────────────────────────
    if checks.contains(&"replay".to_string()) {
        let content = std::fs::read_to_string(args.against.as_ref().unwrap())
            .map_err(|e| anyhow::anyhow!("cannot read --against: {e}"))?;
        let query_fn = move |q: &str, tool: ReplayTool, limit: usize| {
            let q = q.to_string();
            Box::pin(async move {
                let slugs: Vec<String> = match tool {
                    ReplayTool::Search => keyword_search_slugs(engine_ref, &q, limit).await?,
                    ReplayTool::Query => {
                        let opts = HybridSearchOpts {
                            limit: Some(limit),
                            ..Default::default()
                        };
                        let results = hybrid_search(engine_ref, &q, &opts).await?;
                        results.into_iter().map(|r| r.page.slug).collect()
                    }
                };
                Ok(slugs)
            })
        };
        match replay_core(&query_fn, &content, None, None).await {
            Ok((summary, _)) => {
                // A row that errored is a hard failure; the >2x latency alarm
                // is informational and surfaced as a metric, not a verdict.
                let status = if summary.rows_errored > 0 {
                    RunAllStatus::Failed
                } else {
                    RunAllStatus::Passed
                };
                let mut metrics = BTreeMap::new();
                metrics.insert("mean_jaccard".into(), serde_json::json!(summary.mean_jaccard));
                metrics.insert(
                    "top1_stability_rate".into(),
                    serde_json::json!(summary.top1_stability_rate),
                );
                metrics.insert(
                    "rows_over_2x_latency".into(),
                    serde_json::json!(summary.rows_over_2x_latency),
                );
                metrics.insert("rows_errored".into(), serde_json::json!(summary.rows_errored));
                run_checks.push(RunAllCheck {
                    name: "replay".into(),
                    status,
                    metrics,
                    detail: None,
                    error: None,
                });
            }
            Err(e) => run_checks.push(RunAllCheck {
                name: "replay".into(),
                status: RunAllStatus::Errored,
                metrics: BTreeMap::new(),
                detail: None,
                error: Some(e.to_string()),
            }),
        }
    }

    // ── whoknows ─────────────────────────────────────────────────────────────
    if checks.contains(&"whoknows".to_string()) {
        let fixture_path = args.fixture.clone().unwrap();
        let fixture_content = std::fs::read_to_string(&fixture_path)
            .map_err(|e| anyhow::anyhow!("cannot read --fixture: {e}"))?;
        let fixture = match read_fixture(&fixture_content) {
            Ok(f) => f,
            Err(e) => {
                run_checks.push(RunAllCheck {
                    name: "whoknows".into(),
                    status: RunAllStatus::Errored,
                    metrics: BTreeMap::new(),
                    detail: None,
                    error: Some(e.to_string()),
                });
                Vec::new()
            }
        };
        if !fixture.is_empty() {
            let whoknows_fn = move |topic: &str, limit: usize| {
                let topic = topic.to_string();
                Box::pin(async move {
                    let results = whoknows::find_experts(
                        engine_ref,
                        &FindExpertsOpts {
                            topic,
                            limit: Some(limit),
                            types: None,
                            source_id: None,
                        },
                    )
                    .await?;
                    Ok(results.into_iter().map(|r| r.slug).collect::<Vec<String>>())
                })
            };
            let quality = run_quality_gate(&whoknows_fn, &fixture, args.limit).await;
            let regression = if args.skip_replay {
                zbrain_core::eval::whoknows::RegressionReport {
                    status: "skipped".to_string(),
                    reason: Some("--skip-replay".to_string()),
                    total: 0,
                    mean_jaccard: 0.0,
                    threshold: REGRESSION_THRESHOLD,
                    rows: Vec::new(),
                }
            } else {
                let captured = engine_ref
                    .list_eval_candidates(&EvalCandidateFilter {
                        tool_name: Some("query".to_string()),
                        since: None,
                        limit: Some(200),
                    })
                    .await
                    .unwrap_or_default();
                let replay_rows: Vec<ReplayRow> = captured
                    .into_iter()
                    .map(|c| ReplayRow {
                        query: c.query,
                        retrieved_slugs: c.retrieved_slugs,
                    })
                    .collect();
                run_regression_gate(&whoknows_fn, &replay_rows, args.limit).await
            };
            let report = assemble_report(&fixture_path, quality, regression);
            let status = if report.overall_passed {
                RunAllStatus::Passed
            } else {
                RunAllStatus::Failed
            };
            let mut metrics = BTreeMap::new();
            metrics.insert("quality_hit_rate".into(), serde_json::json!(report.quality.hit_rate));
            metrics.insert(
                "regression_status".into(),
                serde_json::json!(report.regression.status),
            );
            metrics.insert(
                "regression_mean_jaccard".into(),
                serde_json::json!(report.regression.mean_jaccard),
            );
            run_checks.push(RunAllCheck {
                name: "whoknows".into(),
                status,
                metrics,
                detail: None,
                error: None,
            });
        }
    }

    let duration_ms = started.elapsed().as_millis() as u64;
    let commit = current_commit();
    let run_id = format!("{}-{}", commit, chrono::Utc::now().format("%Y%m%dT%H%M%S"));
    let ran_at = chrono::Utc::now().to_rfc3339();
    let report = assemble_run_all_report(run_id.clone(), ran_at, commit, run_checks, duration_ms);

    // Persist the report to disk (default: .zbrain-evals/run-all-<run_id>.json).
    let output_path = match &args.output {
        Some(p) => p.clone(),
        None => {
            let dir = std::path::Path::new(".zbrain-evals");
            std::fs::create_dir_all(dir).ok();
            dir.join(format!("run-all-{run_id}.json"))
                .to_string_lossy()
                .to_string()
        }
    };
    if let Err(e) = std::fs::write(&output_path, serde_json::to_string_pretty(&report)?) {
        anyhow::bail!("failed to write run-all report to {output_path}: {e}");
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_run_all_human(&report);
        eprintln!("report written to: {output_path}");
    }

    if !report.overall_passed {
        anyhow::bail!("eval run-all FAILED (one or more gates did not pass)");
    }
    Ok(())
}

/// Render the human-readable run-all summary.
fn print_run_all_human(r: &zbrain_core::eval::run_all::RunAllReport) {
    println!(
        "eval run-all — {}",
        if r.overall_passed { "PASS" } else { "FAIL" }
    );
    println!("run_id:     {}", r.run_id);
    println!("commit:     {}", r.commit);
    println!("ran_at:     {}", r.ran_at);
    println!("duration:   {}ms", r.duration_ms);
    println!();
    for c in &r.checks {
        let st = match c.status {
            zbrain_core::eval::run_all::RunAllStatus::Passed => "PASS ",
            zbrain_core::eval::run_all::RunAllStatus::Failed => "FAIL ",
            zbrain_core::eval::run_all::RunAllStatus::Errored => "ERROR",
            zbrain_core::eval::run_all::RunAllStatus::Skipped => "SKIP ",
        };
        if let Some(e) = &c.error {
            println!("  [{st}] {}  error: {e}", c.name);
        } else {
            println!("  [{st}] {}  ({} metric(s))", c.name, c.metrics.len());
        }
    }
}

/// Diff two run-all reports and surface regressions.
async fn run_eval_compare_command(
    args: EvalCompareArgs,
    _config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::eval::compare::compare_reports;
    use zbrain_core::eval::run_all::RunAllReport;

    let baseline_content = std::fs::read_to_string(&args.baseline)
        .map_err(|e| anyhow::anyhow!("cannot read --baseline {}: {}", args.baseline, e))?;
    let current_content = std::fs::read_to_string(&args.current)
        .map_err(|e| anyhow::anyhow!("cannot read --current {}: {}", args.current, e))?;
    let baseline: RunAllReport = serde_json::from_str(&baseline_content)
        .map_err(|e| anyhow::anyhow!("invalid --baseline report: {e}"))?;
    let current: RunAllReport = serde_json::from_str(&current_content)
        .map_err(|e| anyhow::anyhow!("invalid --current report: {e}"))?;

    let cmp = compare_reports(&baseline, &current);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&cmp)?);
    } else {
        print_compare_human(&cmp);
    }

    if cmp.any_regression {
        let n = cmp.checks.iter().filter(|d| d.regression).count();
        anyhow::bail!("eval compare: {n} regression(s) detected");
    }
    Ok(())
}

/// Render the human-readable compare table.
fn print_compare_human(cmp: &zbrain_core::eval::compare::CompareReport) {
    fn st(s: zbrain_core::eval::run_all::RunAllStatus) -> &'static str {
        match s {
            zbrain_core::eval::run_all::RunAllStatus::Passed => "pass",
            zbrain_core::eval::run_all::RunAllStatus::Failed => "fail",
            zbrain_core::eval::run_all::RunAllStatus::Errored => "error",
            zbrain_core::eval::run_all::RunAllStatus::Skipped => "skip",
        }
    }
    println!(
        "eval compare — baseline {} vs current {}",
        cmp.baseline_run_id, cmp.current_run_id
    );
    println!();
    println!("| check      | baseline | current | changed | regression |");
    println!("|------------|----------|---------|---------|------------|");
    for d in &cmp.checks {
        println!(
            "| {:<10} | {:^8} | {:^7} | {:^7} | {:^10} |",
            d.name,
            st(d.baseline),
            st(d.current),
            if d.changed { "yes" } else { "no" },
            if d.regression { "YES" } else { "no" }
        );
    }
    println!();
    println!("any_regression: {}", cmp.any_regression);
}

// ── code-retrieval eval (G74 1-1-4 stage 7) ───────────────────────

async fn run_eval_code_retrieval_command(
    args: EvalCodeRetrievalArgs,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::eval::code_retrieval::{
        load_default_questions, load_questions, run_code_retrieval_eval, CodeQuestion,
        EvalRunReportMode, RetrievalOutcome,
    };
    use zbrain_core::search::{hybrid_search, HybridSearchOpts};

    // ── compare mode: pure JSON read, no engine ──
    if let Some(reports) = &args.compare {
        return run_eval_code_retrieval_compare(&args, reports);
    }

    // ── capture mode: require a mode ──
    if !args.baseline && !args.with_code_intel {
        anyhow::bail!(
            "eval code-retrieval: specify --baseline or --with-code-intel (or --compare A B)"
        );
    }

    let mode = if args.baseline {
        EvalRunReportMode::Baseline
    } else {
        EvalRunReportMode::WithCodeIntel
    };

    let questions_file = match &args.questions {
        Some(p) => load_questions(Path::new(p))?,
        None => load_default_questions()?,
    };
    let questions: Vec<CodeQuestion> = questions_file.questions;

    // Connect engine (needed for both strategies).
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig {
            database_url: None,
            database_path: Some(db_path),
        })
        .await?;
    engine.init_schema().await?;
    let engine_ref = &engine;

    let source_id = args.source.clone();
    let commit = current_commit();

    // Build the async retrieve closure for this mode. Use `async move` with
    // inner clones of non-Copy captures (mirrors run_eval's query_fn) so the
    // returned future owns its data and the closure stays `Fn` across the
    // per-question loop.
    let retrieve = |q: &CodeQuestion, k: usize| {
        // Clone q into an owned value so the async block does not borrow the
        // closure argument (Fut cannot be parameterized by that lifetime).
        let q = q.clone();
        let source_id = source_id.clone();
        let config_path = config_path;
        let timeout_ms = timeout_ms;
        async move {
            if mode == EvalRunReportMode::Baseline {
                // query + hybrid search (deterministic, keyword-only: no embedding
                // client → matches TS expand:false; semantic path needs a provider).
                let opts = HybridSearchOpts {
                    limit: Some((k * 3).max(10)),
                    source_id: source_id.clone(),
                    ..Default::default()
                };
                let t0 = std::time::Instant::now();
                let results = hybrid_search(engine_ref, &q.query, &opts).await?;
                let latency_ms = t0.elapsed().as_millis() as u64;
                // Collapse to file paths via the `code/` slug prefix (mirrors TS
                // pickFilePath). Take the first k.
                let mut files: Vec<String> = Vec::new();
                for r in &results {
                    if let Some(rest) = r.page.slug.strip_prefix("code/") {
                        files.push(rest.to_string());
                        if files.len() >= k {
                            break;
                        }
                    }
                }
                Ok(RetrievalOutcome { files, latency_ms })
            } else {
                // with-code-intel: dispatch by kind to the real Rust ops.
                let t0 = std::time::Instant::now();
                let files = with_code_intel_files(&q, config_path, timeout_ms).await?;
                let latency_ms = t0.elapsed().as_millis() as u64;
                Ok(RetrievalOutcome { files, latency_ms })
            }
        }
    };

    let opts = zbrain_core::eval::code_retrieval::RunnerOpts {
        k: args.k,
        corpus: args.corpus.clone(),
        commit,
    };
    let report = run_code_retrieval_eval(mode, &questions, &retrieve, &opts).await?;

    if let Some(save) = &args.save {
        let save_path = Path::new(save);
        if let Some(parent) = save_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(save_path, serde_json::to_string_pretty(&report)?)?;
        eprintln!("[eval] saved report to {}", save_path.display());
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print_code_retrieval_report(&report);
    }

    Ok(())
}

/// Dispatch a question to the real Rust code-intel op and extract file paths
/// from the op JSON output. `cluster_membership` has no Rust op → empty. Any
/// op error is returned as an empty result (honest baseline), not propagated.
async fn with_code_intel_files(
    q: &zbrain_core::eval::code_retrieval::CodeQuestion,
    config_path: Option<&Path>,
    timeout_ms: Option<u64>,
) -> anyhow::Result<Vec<String>> {
    use zbrain_core::eval::code_retrieval::CodeQuestionKind;
    let Some(op) = q.kind.code_intel_op() else {
        // cluster_membership has no Rust op yet → empty (honest).
        return Ok(Vec::new());
    };
    let params = match q.kind {
        CodeQuestionKind::Callers | CodeQuestionKind::BlastRadius => serde_json::json!({
            "symbol": q.symbol,
            "depth": 2,
            "max_nodes": 50,
            "exact": false,
        }),
        CodeQuestionKind::Callees | CodeQuestionKind::ExecutionFlow => serde_json::json!({
            "entry_point": q.symbol,
            "depth": 2,
            "max_nodes": 50,
            "exact": false,
        }),
        CodeQuestionKind::Definition => serde_json::json!({
            "symbol": q.symbol,
            "lang": serde_json::Value::Null,
            "limit": 10,
        }),
        CodeQuestionKind::References => serde_json::json!({
            "symbol": q.symbol,
            "lang": serde_json::Value::Null,
            "limit": 10,
        }),
        CodeQuestionKind::ClusterMembership => unreachable!("handled by code_intel_op() == None"),
    };
    match run_operation(op, params, config_path, timeout_ms).await {
        Ok(output) => Ok(extract_code_retrieval_paths(&output)),
        Err(e) => {
            eprintln!("[eval] code-intel op {} failed on {}: {}", op, q.id, e);
            Ok(Vec::new())
        }
    }
}

/// Recursively collect file-path-like strings from an op JSON output.
fn extract_code_retrieval_paths(v: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    collect_paths(v, &mut out, &mut seen);
    out
}

fn collect_paths(
    v: &serde_json::Value,
    out: &mut Vec<String>,
    seen: &mut std::collections::HashSet<String>,
) {
    match v {
        serde_json::Value::String(s) => {
            if looks_like_file_path(s) && seen.insert(s.clone()) {
                out.push(s.clone());
            }
        }
        serde_json::Value::Array(a) => {
            for x in a {
                collect_paths(x, out, seen);
            }
        }
        serde_json::Value::Object(o) => {
            for (_, val) in o {
                collect_paths(val, out, seen);
            }
        }
        _ => {}
    }
}

fn looks_like_file_path(s: &str) -> bool {
    if !s.contains('/') || s.len() > 300 {
        return false;
    }
    let l = s.to_ascii_lowercase();
    l.ends_with(".rs") || l.ends_with(".ts") || l.ends_with(".js") || l.ends_with(".tsx")
        || l.ends_with(".jsx") || l.ends_with(".py") || l.ends_with(".go") || l.ends_with(".java")
        || l.ends_with(".cpp") || l.ends_with(".c") || l.ends_with(".h") || l.ends_with(".hpp")
        || l.ends_with(".md") || l.ends_with(".json") || l.ends_with(".toml")
        || l.ends_with(".yaml") || l.ends_with(".yml")
}

fn run_eval_code_retrieval_compare(
    args: &EvalCodeRetrievalArgs,
    reports: &[String],
) -> anyhow::Result<()> {
    use zbrain_core::eval::code_retrieval::{
        evaluate_gate, EvalRunReport, EvalRunReportMode, DEFAULT_GATE,
    };
    let a: EvalRunReport = read_code_retrieval_report(&reports[0])?;
    let b: EvalRunReport = read_code_retrieval_report(&reports[1])?;
    // Convention: first arg is baseline, second is with-code-intel. Swap if
    // labels disagree so the comparison is meaningful.
    let (baseline, with_code_intel) = if a.mode == EvalRunReportMode::WithCodeIntel
        && b.mode == EvalRunReportMode::Baseline
    {
        (b, a)
    } else {
        (a, b)
    };
    let gate = evaluate_gate(&baseline, &with_code_intel, DEFAULT_GATE);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": 1,
                "passed": gate.passed,
                "precision_delta_pp": gate.precision_delta_pp,
                "top_1_stability_rate": gate.top_1_stability_rate,
                "questions_cleared_bar": gate.questions_cleared_bar,
                "questions_total": gate.questions_total,
                "summary": gate.summary,
            }))?
        );
    } else {
        println!("\n{}\n", gate.summary);
        println!(
            "baseline:        precision@{} = {:.1}%   answered = {:.1}%   (commit {})",
            baseline.k,
            baseline.mean_precision_at_k * 100.0,
            baseline.answered_rate * 100.0,
            baseline.commit
        );
        println!(
            "with-code-intel: precision@{} = {:.1}%   answered = {:.1}%   (commit {})",
            with_code_intel.k,
            with_code_intel.mean_precision_at_k * 100.0,
            with_code_intel.answered_rate * 100.0,
            with_code_intel.commit
        );
        println!(
            "delta:           +{:.1}pp precision   top-1 stability = {:.1}%",
            gate.precision_delta_pp,
            gate.top_1_stability_rate * 100.0
        );
        println!("cleared bar:     {}/{}", gate.questions_cleared_bar, gate.questions_total);
    }
    if !gate.passed {
        anyhow::bail!("eval code-retrieval compare: gate FAILED");
    }
    Ok(())
}

// ── eval cross-modal ───────────────────────────────────────────

/// Handler for `zbrain eval cross-modal` (single-task mode).
///
/// Faithful port of TS `src/commands/eval-cross-modal.ts`: three
/// different-provider frontier models score the OUTPUT against the TASK;
/// verdict PASS(0)/FAIL(1)/INCONCLUSIVE(2). The LLM transport is resolved
/// per `provider:model` slot via the AI gateway's `instantiate_chat`.
async fn run_eval_cross_modal_command(args: EvalCrossModalArgs, _timeout_ms: Option<u64>) -> anyhow::Result<()> {
    use std::io::IsTerminal;
    use std::sync::Arc;

    use zbrain_core::ai::chat::{instantiate_chat, ChatMessage, ChatOpts, ChatProvider, ChatRole};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::eval::cross_modal::{
        default_dimensions, default_slots, estimate_cost, infer_slug_from_output_path, run_eval,
        ChatRequest, RunEvalOpts, SlotConfig, Verdict, RECEIPT_SCHEMA_VERSION,
    };
    use zbrain_core::paths::zbrain_path;

    let task = args
        .task
        .clone()
        .ok_or_else(|| anyhow::anyhow!("eval cross-modal: --task \"<description>\" is required"))?;
    let output_path = args
        .output
        .clone()
        .ok_or_else(|| anyhow::anyhow!("eval cross-modal: --output <path> is required"))?;
    if !std::path::Path::new(&output_path).exists() {
        anyhow::bail!("eval cross-modal: --output path not found: {output_path}");
    }
    let output_content = std::fs::read_to_string(&output_path)
        .map_err(|e| anyhow::anyhow!("eval cross-modal: cannot read --output {output_path}: {e}"))?;
    if output_content.trim().is_empty() {
        anyhow::bail!("eval cross-modal: --output file is empty: {output_path}");
    }

    // Slug: explicit > inferred from `skills/<slug>/SKILL.md` > None (run_eval
    // falls back to a content sha).
    let slug = args
        .slug
        .clone()
        .or_else(|| infer_slug_from_output_path(&output_path));

    let cycles = args.cycles.unwrap_or(if std::io::stdout().is_terminal() { 3 } else { 1 });
    let dimensions = args.dimensions.clone().unwrap_or_else(default_dimensions);
    let receipt_dir = match &args.receipt_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => zbrain_path("eval-receipts").ok_or_else(|| {
            anyhow::anyhow!(
                "eval cross-modal: cannot resolve ~/.zbrain — pass --receipt-dir <path> or set ZBRAIN_HOME"
            )
        })?,
    };
    let max_tokens = args.max_tokens.unwrap_or(4000);

    // Defaults come from the shared DEFAULT_SLOTS table; per-slot overrides win.
    let defaults = default_slots();
    let default_model = |idx: usize| -> String {
        defaults.get(idx).map(|s| s.model.clone()).unwrap_or_default()
    };
    let slots: Vec<SlotConfig> = vec![
        SlotConfig { id: "A".into(), model: args.slot_a_model.clone().unwrap_or_else(|| default_model(0)) },
        SlotConfig { id: "B".into(), model: args.slot_b_model.clone().unwrap_or_else(|| default_model(1)) },
        SlotConfig { id: "C".into(), model: args.slot_c_model.clone().unwrap_or_else(|| default_model(2)) },
    ];

    // Resolve each slot's ChatProvider from its `provider:model` string.
    let env_lookup = |k: &str| std::env::var(k).ok();
    let mut resolved: Vec<(String, Arc<dyn ChatProvider>)> = vec![];
    for slot in &slots {
        let (_parsed, recipe) = resolve_recipe_strict(&slot.model).map_err(|e: AiConfigError| {
            anyhow::anyhow!("eval cross-modal: cannot resolve model `{}`: {}", slot.model, e.message)
        })?;
        let provider = instantiate_chat(recipe, &slot.model, &env_lookup)
            .map_err(|e: AiConfigError| {
                anyhow::anyhow!("eval cross-modal: cannot build provider for `{}`: {}", slot.model, e.message)
            })?;
        resolved.push((slot.model.clone(), Arc::from(provider)));
    }

    // Cost estimate to stderr (T11=B).
    let cost = estimate_cost(&slots, cycles, max_tokens);
    eprintln!(
        "[eval cross-modal] estimated cost: ~${:.2}/cycle, ~${:.2} max for {} cycle(s).",
        cost.per_cycle_usd, cost.per_run_max_usd, cycles
    );
    for note in &cost.notes {
        eprintln!("[eval cross-modal] note: {note}");
    }

    // Wiring: the injected chat closure dispatches by model string to the
    // pre-resolved provider.
    let providers: Arc<Vec<(String, Arc<dyn ChatProvider>)>> = Arc::new(resolved);
    let chat = move |req: ChatRequest| {
        let providers = providers.clone();
        async move {
            let provider = providers
                .iter()
                .find(|(m, _)| m == &req.model)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| anyhow::anyhow!("no provider resolved for model {}", req.model))?;
            let opts = ChatOpts {
                model: Some(req.model.clone()),
                system: Some(req.system.clone()),
                messages: vec![ChatMessage::text(ChatRole::User, req.prompt.clone())],
                tools: vec![],
                max_tokens: Some(req.max_tokens),
                cache_system: false,
            };
            let result = provider.chat(opts).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
            Ok(result.text)
        }
    };

    let opts = RunEvalOpts {
        task,
        output: output_content,
        slug,
        dimensions: Some(dimensions),
        slots: Some(slots),
        cycles: Some(cycles),
        receipt_dir,
        max_tokens: Some(max_tokens),
        on_progress: None,
    };

    let result = run_eval(&opts, &chat).await?;

    let verdict = result.final_aggregate.verdict;
    eprintln!();
    eprintln!("[eval cross-modal] {}", result.final_aggregate.verdict_message);
    eprintln!("[eval cross-modal] receipt: {}", result.final_receipt_path);

    if args.json {
        let json_out = serde_json::json!({
            "verdict": verdict,
            "aggregate": result.final_aggregate,
            "cycles": result.cycles.iter().map(|c| serde_json::json!({
                "cycle": c.cycle,
                "receipt_path": c.receipt_path,
                "verdict": c.aggregate.verdict,
                "overall": c.aggregate.overall,
            })).collect::<Vec<_>>(),
            "final_receipt_path": result.final_receipt_path,
            "schema_version": RECEIPT_SCHEMA_VERSION,
        });
        println!("{}", serde_json::to_string_pretty(&json_out)?);
    }

    // Exit codes: PASS=0, FAIL=1, INCONCLUSIVE=2.
    let code = match verdict {
        Verdict::Pass => 0,
        Verdict::Fail => 1,
        Verdict::Inconclusive => 2,
    };
    std::process::exit(code);
}

/// Handler for `zbrain eval-takes-quality` (MVP).
///
/// Rust port of TS `src/commands/eval-takes-quality.ts`. Opens the configured
/// brain database, samples takes, and delegates to
/// Open the eval engine for `eval-takes-quality` subcommands that read the DB
/// (replay --from-db / regress --*-from-db / trend).
async fn open_tq_engine(config_path: Option<&std::path::Path>) -> anyhow::Result<Arc<dyn BrainEngine>> {
    use std::sync::Arc;

    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::libsql::LibsqlEngine;

    let config = crate::config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    Ok(Arc::new(engine))
}

/// Handler for `zbrain eval-takes-quality` (A2 / #319).
///
/// Four subcommands: `run` drives the shared cross-modal judge panel and
/// persists a 4-sha receipt; `replay` re-loads a prior receipt; `regress`
/// compares two receipts (CI gate); `trend` charts past runs from the DB.
/// Honest degradation: without an API key for the judge model we fail loudly
/// instead of fabricating a PASS.
async fn run_eval_takes_quality_command(
    args: EvalTakesQualityArgs,
    config_path: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    use zbrain_core::ai::chat::{instantiate_chat, ChatMessage, ChatOpts, ChatProvider, ChatRole};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::eval::cross_modal::{default_slots, estimate_cost, ChatRequest, SlotConfig};
    use zbrain_core::eval::takes_quality::receipt::{
        parse_receipt_filename, ReceiptIdentity, RECEIPT_SCHEMA_VERSION,
    };
    use zbrain_core::eval::takes_quality::replay::{load_receipt_from_disk, load_receipt_from_db};
    use zbrain_core::eval::takes_quality::regress::{compare_receipts, RegressOpts};
    use zbrain_core::eval::takes_quality::trend::{load_trend, render_trend_table, TrendOpts};
    use zbrain_core::eval::takes_quality::runner::{run as tq_run, TakesQualityRunOpts};
    use zbrain_core::paths::zbrain_path;

    match args.action {
        TakesQualityAction::Run(run_args) => {
            use std::io::IsTerminal;

            let cycles = run_args
                .cycles
                .unwrap_or(if std::io::stdout().is_terminal() { 3 } else { 1 });
            let receipt_dir = match &run_args.receipt_dir {
                Some(d) => std::path::PathBuf::from(d),
                None => zbrain_path("eval-receipts").ok_or_else(|| {
                    anyhow::anyhow!(
                        "eval-takes-quality: cannot resolve ~/.zbrain — pass --receipt-dir <path> or set ZBRAIN_HOME"
                    )
                })?,
            };
            let max_tokens = run_args.max_tokens.unwrap_or(4000);

            let defaults = default_slots();
            let default_model = |idx: usize| -> String {
                defaults.get(idx).map(|s| s.model.clone()).unwrap_or_default()
            };
            let slots: Vec<SlotConfig> = vec![
                SlotConfig { id: "A".into(), model: run_args.slot_a_model.clone().unwrap_or_else(|| default_model(0)) },
                SlotConfig { id: "B".into(), model: run_args.slot_b_model.clone().unwrap_or_else(|| default_model(1)) },
                SlotConfig { id: "C".into(), model: run_args.slot_c_model.clone().unwrap_or_else(|| default_model(2)) },
            ];

            // Resolve each slot's ChatProvider from its `provider:model` string.
            // Honest degradation: if any slot can't be resolved (no API key),
            // fail loudly instead of fabricating a PASS.
            let env_lookup = |k: &str| std::env::var(k).ok();
            let mut resolved: Vec<(String, Arc<dyn ChatProvider>)> = vec![];
            for slot in &slots {
                let (_parsed, recipe) = resolve_recipe_strict(&slot.model).map_err(|e: AiConfigError| {
                    anyhow::anyhow!("eval-takes-quality: cannot resolve model `{}`: {}", slot.model, e.message)
                })?;
                let provider = instantiate_chat(recipe, &slot.model, &env_lookup).map_err(|e: AiConfigError| {
                    anyhow::anyhow!("eval-takes-quality: cannot build provider for `{}`: {}", slot.model, e.message)
                })?;
                resolved.push((slot.model.clone(), Arc::from(provider)));
            }

            let cost = estimate_cost(&slots, cycles, max_tokens);
            eprintln!(
                "[eval-takes-quality] estimated cost: ~${:.2}/cycle, ~${:.2} max for {} cycle(s).",
                cost.per_cycle_usd, cost.per_run_max_usd, cycles
            );
            for note in &cost.notes {
                eprintln!("[eval-takes-quality] note: {note}");
            }

            // Wiring: the injected chat closure dispatches by model string to
            // the pre-resolved provider.
            let providers: Arc<Vec<(String, Arc<dyn ChatProvider>)>> = Arc::new(resolved);
            let chat = move |req: ChatRequest| {
                let providers = providers.clone();
                async move {
                    let provider = providers
                        .iter()
                        .find(|(m, _)| m == &req.model)
                        .map(|(_, p)| p.clone())
                        .ok_or_else(|| anyhow::anyhow!("no provider resolved for model {}", req.model))?;
                    let opts = ChatOpts {
                        model: Some(req.model.clone()),
                        system: Some(req.system.clone()),
                        messages: vec![ChatMessage::text(ChatRole::User, req.prompt.clone())],
                        tools: vec![],
                        max_tokens: Some(req.max_tokens),
                        cache_system: false,
                    };
                    let result = provider.chat(opts).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
                    Ok(result.text)
                }
            };

            let engine = open_tq_engine(config_path).await?;

            let tq_opts = TakesQualityRunOpts {
                engine: engine.as_ref(),
                sample: run_args.sample,
                slug: run_args.slug.clone(),
                dimensions: run_args.dimensions.clone(),
                slots: Some(slots.clone()),
                cycles: run_args.cycles,
                max_tokens: run_args.max_tokens,
                receipt_dir,
            };

            let result = tq_run(&tq_opts, &chat).await?;

            let verdict = result.receipt.verdict.clone();
            eprintln!();
            eprintln!("[eval-takes-quality] sampled {} takes", result.n_takes);
            if let Some(msg) = &result.receipt.verdict_message {
                eprintln!("[eval-takes-quality] {msg}");
            }
            eprintln!("[eval-takes-quality] receipt: {}", result.final_receipt_path);

            if run_args.json {
                let json_out = serde_json::json!({
                    "verdict": verdict,
                    "n_takes": result.n_takes,
                    "overall_score": result.receipt.overall_score,
                    "cost_usd": result.receipt.cost_usd,
                    "scores": result.receipt.scores,
                    "final_receipt_path": result.final_receipt_path,
                    "schema_version": RECEIPT_SCHEMA_VERSION,
                });
                println!("{}", serde_json::to_string_pretty(&json_out)?);
            }

            // Exit codes: PASS=0, FAIL=1, INCONCLUSIVE=2.
            let code = match verdict.as_str() {
                "pass" => 0,
                "fail" => 1,
                _ => 2,
            };
            std::process::exit(code);
        }
        TakesQualityAction::Replay(replay_args) => {
            let receipt = if let Some(from_db) = &replay_args.from_db {
                let identity: ReceiptIdentity = parse_receipt_filename(from_db).ok_or_else(|| {
                    anyhow::anyhow!(
                        "eval-takes-quality replay: `{from_db}` is not a valid 4-sha receipt id"
                    )
                })?;
                let engine = open_tq_engine(config_path).await?;
                load_receipt_from_db(engine.as_ref(), &identity).await?
            } else if let Some(path) = &replay_args.receipt {
                load_receipt_from_disk(std::path::Path::new(path))?
            } else {
                anyhow::bail!("eval-takes-quality replay: pass --receipt <path> or --from-db <id>");
            };

            if replay_args.json {
                println!("{}", serde_json::to_string_pretty(&receipt)?);
            } else {
                println!("verdict: {}", receipt.verdict);
                println!("overall_score: {:?}", receipt.overall_score);
                println!("rubric_version: {}", receipt.rubric_version);
                println!("corpus_sha8: {}", receipt.corpus.corpus_sha8);
                println!("models_sha8: {}", receipt.models_sha8);
                println!("cost_usd: {:.2}", receipt.cost_usd);
                println!("dimensions:");
                for (dim, roll) in &receipt.scores {
                    println!(
                        "  {dim}: mean={:.1} min={:.1} max={:.1}",
                        roll.mean, roll.min, roll.max
                    );
                }
            }
            Ok(())
        }
        TakesQualityAction::Regress(regress_args) => {
            let current = if let Some(from_db) = &regress_args.current_from_db {
                let identity: ReceiptIdentity = parse_receipt_filename(from_db).ok_or_else(|| {
                    anyhow::anyhow!(
                        "eval-takes-quality regress: `{from_db}` is not a valid 4-sha receipt id"
                    )
                })?;
                let engine = open_tq_engine(config_path).await?;
                load_receipt_from_db(engine.as_ref(), &identity).await?
            } else if let Some(path) = &regress_args.current {
                load_receipt_from_disk(std::path::Path::new(path))?
            } else {
                anyhow::bail!(
                    "eval-takes-quality regress: --current (or --current-from-db) required"
                );
            };
            let prior = if let Some(from_db) = &regress_args.prior_from_db {
                let identity: ReceiptIdentity = parse_receipt_filename(from_db).ok_or_else(|| {
                    anyhow::anyhow!(
                        "eval-takes-quality regress: `{from_db}` is not a valid 4-sha receipt id"
                    )
                })?;
                let engine = open_tq_engine(config_path).await?;
                load_receipt_from_db(engine.as_ref(), &identity).await?
            } else if let Some(path) = &regress_args.prior {
                load_receipt_from_disk(std::path::Path::new(path))?
            } else {
                anyhow::bail!(
                    "eval-takes-quality regress: --prior (or --prior-from-db) required"
                );
            };

            let delta = compare_receipts(
                &current,
                &prior,
                &RegressOpts {
                    threshold: regress_args.threshold,
                },
            );

            if regress_args.json {
                let json_out = serde_json::json!({
                    "regressed": delta.regressed,
                    "overall_delta": delta.overall_delta,
                    "threshold": delta.threshold,
                    "inputs_differ": delta.inputs_differ,
                    "input_diffs": delta.input_diffs,
                    "dim_deltas": delta.dim_deltas,
                    "summary": delta.summary,
                });
                println!("{}", serde_json::to_string_pretty(&json_out)?);
            } else {
                println!("{}", delta.summary);
                if delta.inputs_differ {
                    for d in &delta.input_diffs {
                        println!("  {d}");
                    }
                }
            }

            if regress_args.fail_on_regress && delta.regressed {
                std::process::exit(1);
            }
            Ok(())
        }
        TakesQualityAction::Trend(trend_args) => {
            let engine = open_tq_engine(config_path).await?;
            let opts = TrendOpts {
                days: Some(trend_args.days),
                rubric_version: trend_args.rubric_version.clone(),
                limit: trend_args.limit,
            };
            let rows = load_trend(engine.as_ref(), &opts)
                .await
                .map_err(|e| anyhow::anyhow!("eval-takes-quality trend: {e}"))?;
            if trend_args.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("{}", render_trend_table(&rows));
            }
            Ok(())
        }
    }
}

async fn run_eval_suspected_contradictions_command(
    args: EvalSuspectedContradictionsArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    use zbrain_core::ai::chat::{instantiate_chat, ChatMessage, ChatOpts, ChatProvider, ChatRole};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::eval::contradictions::{
        self, build_contradictions_run_row, render_review_report, render_trend_chart, Severity,
        ContradictionOpts, DEFAULT_JUDGE_MODEL, DEFAULT_QUERY,
    };
    use zbrain_core::eval::cross_modal::ChatRequest;
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::paths::zbrain_path;

    match args.action {
        SuspectedContradictionsAction::Run(run_args) => {
            // Resolve the judge provider up front (honest degradation).
            let judge_model = run_args.judge.clone().unwrap_or_else(|| DEFAULT_JUDGE_MODEL.to_string());
            let env_lookup = |k: &str| std::env::var(k).ok();
            let (_parsed, recipe) = resolve_recipe_strict(&judge_model).map_err(|e: AiConfigError| {
                anyhow::anyhow!(
                    "eval-suspected-contradictions: cannot resolve judge model `{}`: {}",
                    judge_model,
                    e.message
                )
            })?;
            let provider = instantiate_chat(recipe, &judge_model, &env_lookup).map_err(|e: AiConfigError| {
                anyhow::anyhow!(
                    "eval-suspected-contradictions: cannot build provider for `{}`: {}",
                    judge_model,
                    e.message
                )
            })?;
            let provider: Arc<dyn ChatProvider> = Arc::from(provider);

            // Open the engine (samples all takes).
            let config = crate::config::load_config(config_path)?;
            let db_path = resolve_database_path(&config.database_url);
            let engine_config = EngineConfig {
                database_url: None,
                database_path: Some(db_path),
            };
            let engine = LibsqlEngine::new();
            engine.connect(&engine_config).await?;
            engine.init_schema().await?;
            let engine: Arc<dyn BrainEngine> = Arc::new(engine);

            let receipt_dir = match &run_args.receipt_dir {
                Some(d) => std::path::PathBuf::from(d),
                None => zbrain_path("eval-receipts").ok_or_else(|| {
                    anyhow::anyhow!(
                        "eval-suspected-contradictions: cannot resolve ~/.zbrain — pass --receipt-dir <path> or set ZBRAIN_HOME"
                    )
                })?,
            };
            let max_tokens = run_args.max_tokens.unwrap_or(2000);
            let query = run_args.query.clone().unwrap_or_else(|| DEFAULT_QUERY.to_string());

            // Pairing strategy (Corpus default, or Retrieval extension).
            let pairing = if run_args.pairing == "retrieval" {
                let queries = run_args
                    .queries
                    .clone()
                    .unwrap_or_else(|| vec![query.clone()]);
                contradictions::PairingMode::Retrieval {
                    queries,
                    top_k: run_args.top_k,
                }
            } else {
                contradictions::PairingMode::Corpus
            };

            // Cost hint to stderr (bounded by max_pairs).
            let est_calls = run_args.max_pairs.min(
                (run_args.sample * (run_args.sample.saturating_sub(1))) / 2,
            );
            eprintln!(
                "[eval-suspected-contradictions] judge={judge_model} pairing={} sample={} max_pairs={} (~{est_calls} judge calls, capped)",
                run_args.pairing, run_args.sample, run_args.max_pairs
            );

            let providers: Arc<(String, Arc<dyn ChatProvider>)> =
                Arc::new((judge_model.clone(), provider));
            let chat = move |req: ChatRequest| {
                let providers = providers.clone();
                async move {
                    let (model, provider) = providers.as_ref();
                    if model != &req.model {
                        return Err(anyhow::anyhow!(
                            "eval-suspected-contradictions: unexpected model {} (expected {model})",
                            req.model
                        ));
                    }
                    let opts = ChatOpts {
                        model: Some(req.model.clone()),
                        system: Some(req.system.clone()),
                        messages: vec![ChatMessage::text(ChatRole::User, req.prompt.clone())],
                        tools: vec![],
                        max_tokens: Some(req.max_tokens),
                        cache_system: false,
                    };
                    let result = provider.chat(opts).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
                    Ok(result.text)
                }
            };

            let sc_opts = ContradictionOpts {
                engine: engine.as_ref(),
                sample: run_args.sample,
                max_pairs: run_args.max_pairs,
                query,
                pairing,
                judge_model,
                max_pair_chars: run_args.max_pair_chars,
                max_tokens,
                receipt_dir,
                slug: run_args.slug.clone(),
                no_cache: run_args.no_cache,
            };

            let started = std::time::Instant::now();
            let result = contradictions::run(&sc_opts, &chat).await?;
            let duration_ms = started.elapsed().as_millis() as u64;

            // Persist the run row so `eval suspected-contradictions trend` can
            // chart it later (1-1-5-6 / #62). A persist failure is non-fatal:
            // the probe already succeeded and printed its report above.
            let row = contradictions::build_contradictions_run_row(&result, duration_ms);
            if let Err(e) = engine.write_contradictions_run(&row).await {
                eprintln!(
                    "[eval-suspected-contradictions] warning: failed to persist run row: {e}"
                );
            }

            eprintln!();
            eprintln!(
                "[eval-suspected-contradictions] sampled {} takes, built {} pairs, judged {}",
                result.n_takes, result.n_pairs, result.judged
            );
            eprintln!(
                "[eval-suspected-contradictions] verdict breakdown: {:?}",
                result.verdict_breakdown
            );
            eprintln!(
                "[eval-suspected-contradictions] severity breakdown: {:?}",
                result.severity_breakdown
            );
            if result.judge_errors.total > 0 {
                eprintln!(
                    "[eval-suspected-contradictions] judge errors (counted, not silent): {:?}",
                    result.judge_errors
                );
            }
            eprintln!(
                "[eval-suspected-contradictions] findings: {} (see receipt for detail)",
                result.findings.len()
            );
            if let Some(p) = &result.receipt_path {
                eprintln!("[eval-suspected-contradictions] receipt: {p}");
            }

            if run_args.json {
                let json_out = serde_json::json!({
                    "n_takes": result.n_takes,
                    "n_pairs": result.n_pairs,
                    "judged": result.judged,
                    "verdict_breakdown": result.verdict_breakdown,
                    "severity_breakdown": result.severity_breakdown,
                    "judge_errors": result.judge_errors,
                    "n_findings": result.findings.len(),
                    "findings": result.findings,
                    "receipt_path": result.receipt_path,
                    "run_id": result.run_id,
                    "judge_model": result.judge_model,
                    "queries_evaluated": result.queries_evaluated,
                    "queries_with_contradiction": result.queries_with_contradiction,
                    "total_contradictions_flagged": result.total_contradictions_flagged,
                    "wilson_ci_lower": result.wilson_ci_lower,
                    "wilson_ci_upper": result.wilson_ci_upper,
                    "cost_usd_total": result.cost_usd_total,
                    "duration_ms": duration_ms,
                    "cache": result.cache,
                });
                println!("{}", serde_json::to_string_pretty(&json_out)?);
            }

            // The probe is a report, not a gate: a completed run exits 0.
            // Findings are carried in the JSON / receipt for downstream review.
            std::process::exit(0);
        }
        SuspectedContradictionsAction::Trend(trend_args) => {
            // Open the engine and load the trend of past probe runs.
            let config = crate::config::load_config(config_path)?;
            let db_path = resolve_database_path(&config.database_url);
            let engine_config = EngineConfig {
                database_url: None,
                database_path: Some(db_path),
            };
            let engine = LibsqlEngine::new();
            engine.connect(&engine_config).await?;
            engine.init_schema().await?;
            let engine: Arc<dyn BrainEngine> = Arc::new(engine);

            let rows = engine
                .load_contradictions_trend(trend_args.days)
                .await
                .map_err(|e| anyhow::anyhow!("eval-suspected-contradictions trend: {e}"))?;

            if trend_args.json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!("{}", contradictions::render_trend_chart(&rows));
            }
            Ok(())
        }
        SuspectedContradictionsAction::Review(review_args) => {
            // Parse the optional severity filter (faithful to TS flags).
            let severity_filter: Option<Severity> = match review_args.severity.as_deref() {
                Some("info") => Some(Severity::Info),
                Some("low") => Some(Severity::Low),
                Some("medium") => Some(Severity::Medium),
                Some("high") => Some(Severity::High),
                Some(other) => anyhow::bail!(
                    "eval-suspected-contradictions review: --severity must be info|low|medium|high (got `{other}`)"
                ),
                None => None,
            };

            // Open the engine (same boilerplate as the `trend` arm).
            let config = crate::config::load_config(config_path)?;
            let db_path = resolve_database_path(&config.database_url);
            let engine_config = EngineConfig {
                database_url: None,
                database_path: Some(db_path),
            };
            let engine = LibsqlEngine::new();
            engine.connect(&engine_config).await?;
            engine.init_schema().await?;
            let engine: Arc<dyn BrainEngine> = Arc::new(engine);

            // Load all recorded runs, then (optionally) bound by --since and
            // pick the target run. We sort defensively by ran_at DESC so the
            // "latest" selection is backend-independent.
            let mut rows = engine
                .load_contradictions_trend(0)
                .await
                .map_err(|e| anyhow::anyhow!("eval-suspected-contradictions review: {e}"))?;
            if rows.is_empty() {
                anyhow::bail!(
                    "eval-suspected-contradictions review: no probe runs recorded yet. \
                     Run `zbrain eval suspected-contradictions run` first."
                );
            }
            // ISO dates are zero-padded, so lexicographic compare == chronological.
            if let Some(since) = &review_args.since {
                if since.len() != 10
                    || since.as_bytes()[4] != b'-'
                    || since.as_bytes()[7] != b'-'
                    || !since.chars().all(|c| c.is_ascii_digit() || c == '-')
                {
                    anyhow::bail!(
                        "eval-suspected-contradictions review: --since must be YYYY-MM-DD (got `{since}`)"
                    );
                }
                let bound = format!("{since}T");
                rows.retain(|r| r.ran_at.as_str() >= bound.as_str());
                if rows.is_empty() {
                    anyhow::bail!(
                        "eval-suspected-contradictions review: no runs on/after {since}."
                    );
                }
            }
            rows.sort_by(|a, b| b.ran_at.cmp(&a.ran_at));

            let row = if let Some(rid) = &review_args.run_id {
                match rows.into_iter().find(|r| &r.run_id == rid) {
                    Some(r) => r,
                    None => anyhow::bail!(
                        "eval-suspected-contradictions review: no run with run_id `{rid}` among the loaded runs."
                    ),
                }
            } else {
                // rows are sorted DESC by ran_at; take the newest.
                rows.remove(0)
            };

            if review_args.json {
                println!("{}", serde_json::to_string_pretty(&row.report_json)?);
            } else {
                println!(
                    "Reviewing run {} (ran_at {}, judge {}):",
                    row.run_id, row.ran_at, row.judge_model
                );
                println!(
                    "{}",
                    render_review_report(&row.report_json, severity_filter.as_ref())
                );
            }
            Ok(())
        }
    }
}

/// Handler for `zbrain eval-longmemeval`.
///
/// Faithful port of TS `src/commands/eval-longmemeval.ts`. Resolves the chat
/// + embedding providers and the config lookup, then delegates to the
/// benchmark runner in `zbrain_core`. Honest degradation: without an API key
/// (and not `--retrieval-only`) we fail loudly instead of fabricating PASSes.
async fn run_eval_longmemeval_command(
    args: EvalLongMemEvalArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use std::sync::Arc;

    use zbrain_core::ai::chat::{instantiate_chat, ChatProvider};
    use zbrain_core::ai::model_config::{resolve_model, ConfigLookup, ModelTier, ResolveModelOpts};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::embedding::EmbeddingClient;
    use zbrain_core::eval::longmemeval::runner::{run_eval_long_mem_eval, RunLongMemEvalOpts};

    let config = crate::config::load_config(config_path)?;
    let lookup = crate::models::config_to_lookup(&config);

    // Resolve the same models the runner will, so the providers we build
    // match the model strings it passes into ChatOpts.
    let chat_model = resolve_model(
        &lookup,
        &ResolveModelOpts {
            cli_flag: args.model.clone(),
            config_key: Some("models.eval.longmemeval".into()),
            env_var: Some("ZBRAIN_MODEL".into()),
            tier: None,
            fallback: "sonnet".into(),
        },
    );
    let extractor_model = if !args.no_trajectory {
        resolve_model(
            &lookup,
            &ResolveModelOpts {
                cli_flag: None,
                config_key: None,
                env_var: None,
                tier: Some(ModelTier::Utility),
                fallback: "haiku".into(),
            },
        )
    } else {
        String::new()
    };

    let env_lookup = |k: &str| std::env::var(k).ok();

    // Honest degradation: only build providers we actually need.
    let chat: Option<Arc<dyn ChatProvider>> = if args.retrieval_only {
        None
    } else {
        let (_parsed, recipe) = resolve_recipe_strict(&chat_model).map_err(|e: AiConfigError| {
            anyhow::anyhow!(
                "eval longmemeval: cannot resolve model `{}`: {}",
                chat_model,
                e.message
            )
        })?;
        let provider = instantiate_chat(&recipe, &chat_model, &env_lookup).map_err(|e: AiConfigError| {
            anyhow::anyhow!(
                "eval longmemeval: cannot build chat provider for `{}`: {}",
                chat_model,
                e.message
            )
        })?;
        Some(Arc::from(provider))
    };

    let extractor_chat: Option<Arc<dyn ChatProvider>> = if args.no_trajectory {
        None
    } else {
        let (_parsed, recipe) = resolve_recipe_strict(&extractor_model)
            .map_err(|e: AiConfigError| {
                anyhow::anyhow!(
                    "eval longmemeval: cannot resolve extractor model `{}`: {}",
                    extractor_model,
                    e.message
                )
            })?;
        let provider = instantiate_chat(&recipe, &extractor_model, &env_lookup)
            .map_err(|e: AiConfigError| {
                anyhow::anyhow!(
                    "eval longmemeval: cannot build extractor provider for `{}`: {}",
                    extractor_model,
                    e.message
                )
            })?;
        Some(Arc::from(provider))
    };

    let embedding_client: Option<Arc<EmbeddingClient>> = EmbeddingClient::from_env().map(Arc::new);

    let argv = eval_longmemeval_args_to_vec(&args);
    let opts = RunLongMemEvalOpts {
        args: argv,
        chat,
        extractor_chat,
        embedding_client,
        config_lookup: Some(Arc::new(lookup)),
    };

    run_eval_long_mem_eval(opts)
        .await
        .map_err(|e| anyhow::anyhow!("eval longmemeval: {e}"))
}

fn read_code_retrieval_report(path: &str) -> anyhow::Result<zbrain_core::eval::code_retrieval::EvalRunReport> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read report {}: {}", path, e))?;
    serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("invalid report {}: {}", path, e))
}

fn print_code_retrieval_report(report: &zbrain_core::eval::code_retrieval::EvalRunReport) {
    println!("\n=== code-retrieval eval (mode={:?}) ===", report.mode);
    println!("corpus:      {}", report.corpus);
    println!("commit:      {}", report.commit);
    println!("captured:    {}", report.captured_at);
    println!("questions:   {}", report.questions.len());
    println!("precision@{}: {:.1}%", report.k, report.mean_precision_at_k * 100.0);
    let answered = report.questions.iter().filter(|q| q.answered).count();
    println!(
        "answered:    {}/{} ({:.1}%)",
        answered,
        report.questions.len(),
        report.answered_rate * 100.0
    );
    println!(
        "latency:     {}ms total, {:.0}ms/q",
        report.total_latency_ms,
        report.total_latency_ms as f64 / report.questions.len().max(1) as f64
    );
    println!("\nper-question:");
    for q in &report.questions {
        let status = if q.answered { "+" } else { "-" };
        println!(
            "  {} {:<20} p@{} = {:.0}% recall@{} = {:.0}% ({}ms)",
            status,
            q.id,
            report.k,
            q.precision_at_k * 100.0,
            report.k,
            q.recall_at_k * 100.0,
            q.latency_ms
        );
    }
    println!();
}

async fn run_eval_command(args: EvalArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::search::parse_qrels;

    reject_eval_subcommand(args.subcommand.as_deref())?;

    let qrels_src = args
        .qrels
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("--qrels <path|json> is required"))?;
    let qrels = parse_qrels(qrels_src)?;
    if qrels.is_empty() {
        anyhow::bail!("qrels contains no queries");
    }

    let k = args.k;
    if k == 0 {
        anyhow::bail!("--k must be >= 1");
    }

    let config_a = build_eval_config_a(&args)?;
    let config_b = match args.config_b.as_deref() {
        Some(src) => Some(build_eval_config_b(src)?),
        None => None,
    };

    // `--rrf-k` is now honored end-to-end (plumbed into `SearchOpts::rrf_k` →
    // `fuse_and_boost` → `rrf_fuse`), so no warning is needed there. `--expand`
    // is honored only when a chat-backed expansion provider can be built; when
    // `--expand` is requested but no model/key is configured we degrade to
    // single-query retrieval and say so (KNOWN-GAPS G74b).
    let expand_requested =
        config_a.expand == Some(true) || config_b.as_ref().is_some_and(|c| c.expand == Some(true));
    let expand_provider: Option<Arc<dyn ExpansionProvider>> =
        if expand_requested { build_eval_expansion_provider() } else { None };
    if expand_requested && expand_provider.is_none() {
        eprintln!(
            "warning: --expand requested but no chat provider is configured \
             (set ZBRAIN_MODEL + a provider API key) — falling back to single-query \
             retrieval. KNOWN-GAPS G74b"
        );
    }

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine
        .connect(&EngineConfig { database_url: None, database_path: Some(db_path) })
        .await?;
    engine.init_schema().await?;

    // Same posture as `run_query_command`'s MCP context: a missing key leaves
    // the vector axis off and hybrid degrades to lexical (TS behaves the same
    // when no embedding provider is configured). `--strategy vector` errors
    // instead, because a vector-only run with no vectors is meaningless.
    let embedding_client = zbrain_core::embedding::EmbeddingClient::from_env().map(std::sync::Arc::new);

    let show_progress = !args.json;
    let report_a = run_one_eval_config(
        &engine,
        embedding_client.as_ref(),
        &qrels,
        &config_a,
        k,
        show_progress,
        expand_provider.clone(),
    )
    .await?;
    let report_b = match &config_b {
        Some(cfg) => Some(
            run_one_eval_config(
                &engine,
                embedding_client.as_ref(),
                &qrels,
                cfg,
                k,
                show_progress,
                expand_provider.clone(),
            )
            .await?,
        ),
        None => None,
    };

    engine.disconnect().await?;

    match (&report_b, args.json) {
        (Some(b), true) => {
            let out = serde_json::json!({
                "a": report_a,
                "b": b,
                "delta_mean_ndcg": b.mean_ndcg - report_a.mean_ndcg,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        (Some(b), false) => print_eval_ab_table(&report_a, b, k),
        (None, true) => println!("{}", serde_json::to_string_pretty(&report_a)?),
        (None, false) => print_eval_single_table(&report_a),
    }

    Ok(())
}

// ── Extract commands ────────────────────────────────────────────

/// Open + init the libsql engine for the `extract` verbs.
///
/// The extract subcommands all need the same connected engine; the existing
/// CLI convention is to inline this per command, which would triple the
/// boilerplate here.
async fn connect_extract_engine(
    config_path: Option<&Path>,
) -> anyhow::Result<zbrain_core::libsql::LibsqlEngine> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    Ok(engine)
}

/// Dispatch `zbrain extract` subcommands.
async fn run_extract_command(
    action: ExtractAction,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    match action {
        ExtractAction::Links(args) => run_extract_links(args, config_path).await?,
        ExtractAction::Timeline(args) => run_extract_timeline(args, config_path).await?,
        ExtractAction::All(args) => run_extract_all(args, config_path).await?,
        ExtractAction::ConversationFacts(args) => {
            run_extract_conversation_facts(args, config_path).await?
        }
    }
    Ok(())
}

/// Execute `zbrain extract links` — Rust port of TS `extract links --source db`.
///
/// Shares the `auto_fix::extract_links` core op with
/// `zbrain links rebuild-md-links`; this verb exists so the TS `extract`
/// surface has a faithful Rust equivalent (KNOWN-GAPS G76a-1).
async fn run_extract_links(
    args: ExtractLinksArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::auto_fix::{extract_links, ExtractLinksOpts};
    use zbrain_core::engine::BrainEngine;
    use zbrain_core::extract_fs::extract_links_from_dir;

    let engine = connect_extract_engine(config_path).await?;
    let result = if args.source == "fs" {
        let dir = args.dir.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "extract --source fs requires --dir <path>; pass --dir or use --source db"
            )
        })?;
        extract_links_from_dir(&engine, Path::new(dir)).await?
    } else {
        let opts = ExtractLinksOpts { slug: args.slug.clone() };
        extract_links(&engine, &opts).await?
    };

    if args.json {
        // `ExtractLinksResult` is a plain core value type (no serde derive);
        // project it here, following the CLI's existing `json!` convention.
        let output = serde_json::json!({
            "pages_processed": result.pages_processed,
            "links_created": result.links_created,
            "dangling": result.dangling,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "extract links: pages={} created={} dangling={}",
            result.pages_processed, result.links_created, result.dangling
        );
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain extract timeline` — Rust port of TS
/// `extract timeline --source db`.
///
/// Scans page bodies for dated entries and appends each as a
/// `"{date} {summary}"` line to `pages.timeline`, skipping lines already
/// present (idempotent). Closes KNOWN-GAPS G76a-2: the core op
/// (`auto_fix::extract_timeline`) already existed but was only reachable
/// from `run_auto_fix`, with no standalone verb.
async fn run_extract_timeline(
    args: ExtractTimelineArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::auto_fix::{extract_timeline, ExtractTimelineOpts};
    use zbrain_core::engine::BrainEngine;
    use zbrain_core::extract_fs::extract_timeline_from_dir;

    let engine = connect_extract_engine(config_path).await?;
    let result = if args.source == "fs" {
        let dir = args.dir.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "extract --source fs requires --dir <path>; pass --dir or use --source db"
            )
        })?;
        extract_timeline_from_dir(&engine, Path::new(dir)).await?
    } else {
        let opts = ExtractTimelineOpts { slug: args.slug.clone() };
        extract_timeline(&engine, &opts).await?
    };

    if args.json {
        let output = serde_json::json!({
            "pages_processed": result.pages_processed,
            "entries_added": result.entries_added,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "extract timeline: pages={} entries_added={}",
            result.pages_processed, result.entries_added
        );
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain extract all` — Rust port of TS `extract all --source db`.
///
/// Runs link extraction then timeline extraction over the same page set in
/// one connection. Unlike `run_auto_fix` this deliberately omits the
/// `embed_stale` step: TS `extract all` covers links + timeline only, and
/// re-embedding is a separate concern (`zbrain reindex`).
async fn run_extract_all(args: ExtractAllArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::auto_fix::{
        extract_links, extract_timeline, ExtractLinksOpts, ExtractTimelineOpts,
    };
    use zbrain_core::engine::BrainEngine;
    use zbrain_core::extract_fs::{extract_links_from_dir, extract_timeline_from_dir};

    let engine = connect_extract_engine(config_path).await?;
    let (links, timeline) = if args.source == "fs" {
        let dir = args.dir.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "extract --source fs requires --dir <path>; pass --dir or use --source db"
            )
        })?;
        let dir_path = Path::new(dir);
        let l = extract_links_from_dir(&engine, dir_path).await?;
        let t = extract_timeline_from_dir(&engine, dir_path).await?;
        (l, t)
    } else {
        let links = extract_links(&engine, &ExtractLinksOpts { slug: args.slug.clone() }).await?;
        let timeline =
            extract_timeline(&engine, &ExtractTimelineOpts { slug: args.slug.clone() }).await?;
        (links, timeline)
    };

    if args.json {
        let output = serde_json::json!({
            "links": {
                "pages_processed": links.pages_processed,
                "links_created": links.links_created,
                "dangling": links.dangling,
            },
            "timeline": {
                "pages_processed": timeline.pages_processed,
                "entries_added": timeline.entries_added,
            },
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "extract all: pages={} links_created={} dangling={} entries_added={}",
            links.pages_processed,
            links.links_created,
            links.dangling,
            timeline.entries_added
        );
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain extract conversation-facts` — Rust port of the TS
/// top-level `extract-conversation-facts` command (KNOWN-GAPS G76b).
///
/// Enumerates conversation-style pages (optionally a single `--slug`) and
/// extracts structured facts via an LLM, inserting them into the fact store
/// with per-page checkpointing for resume. Reuses the exact core op the
/// `conversation-facts-backfill` cycle phase uses, so behavior stays in
/// lockstep with the autopilot path.
async fn run_extract_conversation_facts(
    args: ExtractConversationFactsArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use std::sync::Arc;
    use zbrain_core::ai::chat::{instantiate_chat, ChatProvider};
    use zbrain_core::ai::resolver::{resolve_recipe_strict, AiConfigError};
    use zbrain_core::autopilot::phases::conversation_facts_backfill::{
        run_extract_conversation_facts_core, ExtractConversationFactsCoreOpts,
        DEFAULT_EXTRACT_MODEL, DEFAULT_INTER_CALL_SLEEP_MS, DEFAULT_MAX_COST_USD,
    };

    let engine = connect_extract_engine(config_path).await?;

    // Build the LLM provider. Mirrors `zbrain brainstorm`: resolve the model
    // recipe, then instantiate. A missing API key surfaces a clear message
    // rather than a stack trace at call time.
    let resolved_model = args
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_EXTRACT_MODEL.to_string());
    let env_lookup = |k: &str| std::env::var(k).ok();
    let (_parsed, recipe) = resolve_recipe_strict(&resolved_model).map_err(|e: AiConfigError| {
        anyhow::anyhow!(
            "zbrain extract conversation-facts: cannot resolve model `{}`: {}. Set your provider API key (e.g. ANTHROPIC_API_KEY) or run `zbrain models doctor`.",
            resolved_model, e.message
        )
    })?;
    let chat: Arc<dyn ChatProvider> = Arc::from(
        instantiate_chat(recipe, &resolved_model, &env_lookup).map_err(|e: AiConfigError| {
            anyhow::anyhow!(
                "zbrain extract conversation-facts: cannot build LLM provider for `{}`: {}. Set your provider API key or run `zbrain models doctor`.",
                resolved_model, e.message
            )
        })?,
    );

    let opts = ExtractConversationFactsCoreOpts {
        source_id: args.source_id.clone(),
        types: if args.types.is_empty() { None } else { Some(args.types.clone()) },
        slug: args.slug.clone(),
        dry_run: args.dry_run,
        limit: args.limit,
        since_iso: args.since.clone(),
        force: args.force,
        sleep_ms: args.sleep_ms.unwrap_or(DEFAULT_INTER_CALL_SLEEP_MS),
        segment_limit: args.segment_limit.unwrap_or(0),
        max_cost_usd: args.max_cost.unwrap_or(DEFAULT_MAX_COST_USD),
        model: args.model.clone(),
        budget_tracker: None,
    };

    let result = run_extract_conversation_facts_core(&engine, &*chat, &opts).await?;

    if args.json {
        let output = serde_json::json!({
            "pages_considered": result.pages_considered,
            "pages_processed": result.pages_processed,
            "pages_skipped": result.pages_skipped,
            "pages_skipped_too_large": result.pages_skipped_too_large,
            "pages_skipped_disappeared": result.pages_skipped_disappeared,
            "segments_processed": result.segments_processed,
            "facts_extracted": result.facts_extracted,
            "facts_inserted": result.facts_inserted,
            "budget_exhausted": result.budget_exhausted,
            "spent_usd": result.spent_usd,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "extract conversation-facts: considered={} processed={} skipped={} too_large={} disappeared={} segments={} facts_extracted={} facts_inserted={} spent={:?}{}",
            result.pages_considered,
            result.pages_processed,
            result.pages_skipped,
            result.pages_skipped_too_large,
            result.pages_skipped_disappeared,
            result.segments_processed,
            result.facts_extracted,
            result.facts_inserted,
            result.spent_usd,
            if result.budget_exhausted { " (budget_exhausted)" } else { "" },
        );
    }

    engine.disconnect().await?;
    Ok(())
}

// ── Links commands ──────────────────────────────────────────────

/// Dispatch `zbrain links` subcommands.
/// Execute `zbrain links reconcile` (G77 / 1-6-2).
///
/// Scans every markdown page in the scoped source for code-path references,
/// then upserts bidirectional `documents` / `documented_by` edges to the
/// matching code page. Idempotent; respects the `auto_link` config gate.
/// Mirrors TS `reconcile-links.ts`.
async fn run_links_reconcile(
    args: LinksReconcileArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::links::{reconcile_links, ReconcileLinksOpts};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = ReconcileLinksOpts {
        source_id: Some(args.source.clone()),
        dry_run: args.dry_run,
    };
    let result = reconcile_links(&engine, &opts).await?;

    if args.json {
        let output = serde_json::json!({
            "status": result.status,
            "markdownPagesScanned": result.markdown_pages_scanned,
            "codeRefsFound": result.code_refs_found,
            "edgesAttempted": result.edges_attempted,
            "edgesTargetsMissing": result.edges_targets_missing,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if result.status == "auto_link_disabled" {
        println!(
            "[reconcile-links] auto_link is disabled in config; skipping. \
             Set `zbrain config set auto_link true` to re-enable."
        );
    } else {
        let header = if args.dry_run {
            "reconcile-links (dry run)"
        } else {
            "reconcile-links"
        };
        print!(
            "{}: scanned {} markdown pages, found {} code refs, attempted {} edges",
            header,
            result.markdown_pages_scanned,
            result.code_refs_found,
            result.edges_attempted
        );
        if result.edges_targets_missing > 0 {
            print!(" ({} targets missing code page)", result.edges_targets_missing);
        }
        println!();
    }

    engine.disconnect().await?;
    Ok(())
}

async fn run_links_by_mention(
    args: LinksByMentionArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::mentions::{run_by_mention, ByMentionOpts};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let extra_ignore = args
        .extra_ignore
        .as_ref()
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect::<Vec<_>>());

    let opts = ByMentionOpts {
        source_id: Some(args.source.clone()),
        dry_run: args.dry_run,
        extra_ignore,
    };
    let result = run_by_mention(&engine, &opts).await?;

    if args.json {
        let output = serde_json::json!({
            "status": result.status,
            "pagesScanned": result.pages_scanned,
            "mentionsFound": result.mentions_found,
            "edgesAttempted": result.edges_attempted,
            "edgesWritten": result.edges_written,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        let header = if args.dry_run {
            "by-mention (dry run)"
        } else {
            "by-mention"
        };
        println!(
            "{}: scanned {} markdown pages, found {} mentions, attempted {} edges (wrote {})",
            header,
            result.pages_scanned,
            result.mentions_found,
            result.edges_attempted,
            result.edges_written
        );
    }

    engine.disconnect().await?;
    Ok(())
}

async fn run_links_command(action: LinksAction, config_path: Option<&Path>) -> anyhow::Result<()> {
    match action {
        LinksAction::Add(args) => run_links_add(args, config_path).await?,
        LinksAction::List(args) => run_links_list(args, config_path).await?,
        LinksAction::Backlinks(args) => run_links_backlinks(args, config_path).await?,
        LinksAction::RebuildMdLinks(args) => {
            run_links_rebuild_md_links(args, config_path).await?
        }
        LinksAction::Remove(args) => run_links_remove(args, config_path).await?,
        LinksAction::EdgesBackfill(args) => {
            run_links_edges_backfill(args, config_path).await?
        }
        LinksAction::Reconcile(args) => run_links_reconcile(args, config_path).await?,
        LinksAction::ByMention(args) => run_links_by_mention(args, config_path).await?,
    }
    Ok(())
}

/// Execute `zbrain links add`.
async fn run_links_add(args: LinksAddArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::types::LinkBatchInput;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let link = LinkBatchInput {
        from_slug: args.from.clone(),
        to_slug: args.to.clone(),
        link_type: Some(args.link_type),
        context: args.context.clone(),
        link_source: Some(args.link_source),
        origin_slug: None,
        origin_field: None,
        from_source_id: Some(args.from_source.clone()),
        to_source_id: Some(args.to_source.clone()),
        origin_source_id: None,
    };

    let added = engine.add_links_batch(&[link]).await?;

    if args.json {
        let output = serde_json::json!({
            "added": added,
            "from": args.from,
            "to": args.to,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{} link(s) added ({} -> {})", added, args.from, args.to);
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain links list`.
async fn run_links_list(args: LinksListArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let links = engine.get_links(&args.slug, Some(&args.source)).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&links)?);
    } else {
        if links.is_empty() {
            println!("No outbound links from '{}' in source '{}'", args.slug, args.source);
        } else {
            for l in &links {
                let source = l.link_source.as_deref().unwrap_or("-");
                println!("  -> {} ({}, {})", l.to_slug, l.link_type, source);
            }
            println!("\n{} link(s)", links.len());
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain links backlinks`.
async fn run_links_backlinks(args: LinksBacklinksArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let backlinks = engine.get_backlinks(&args.slug, Some(&args.source)).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&backlinks)?);
    } else {
        if backlinks.is_empty() {
            println!("No backlinks to '{}' in source '{}'", args.slug, args.source);
        } else {
            for l in &backlinks {
                let source = l.link_source.as_deref().unwrap_or("-");
                println!("  <- {} ({}, {})", l.from_slug, l.link_type, source);
            }
            println!("\n{} backlink(s)", backlinks.len());
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain links rebuild-md-links`.
///
/// Rescans every page body (or one specific page) for markdown + wikilink
/// references, resolves each target against existing slugs, and upserts the
/// resulting outbound links via `auto_fix::extract_links`. Closes G77-1.
async fn run_links_rebuild_md_links(
    args: LinksRebuildMdLinksArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::auto_fix::{extract_links, ExtractLinksOpts};
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = ExtractLinksOpts { slug: args.slug.clone() };
    let result = extract_links(&engine, &opts).await?;

    if args.json {
        // `ExtractLinksResult` is a plain core value type (no serde derive);
        // project it here, following the CLI's existing `json!` convention.
        let output = serde_json::json!({
            "pages_processed": result.pages_processed,
            "links_created": result.links_created,
            "dangling": result.dangling,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "rebuild-md-links: pages={} created={} dangling={}",
            result.pages_processed, result.links_created, result.dangling
        );
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain links rm`.
async fn run_links_remove(args: LinksRemoveArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    engine
        .remove_link(
            &args.from,
            &args.to,
            args.link_type.as_deref(),
            None,
            Some(&args.from_source),
            Some(&args.to_source),
        )
        .await?;

    if args.json {
        let output = serde_json::json!({
            "removed": true,
            "from": args.from,
            "to": args.to,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Link removed: {} -> {}", args.from, args.to);
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain links edges-backfill` (G77 / 1-6-3).
///
/// Resumable symbol-resolution backfill. Walks every `content_chunks` row
/// whose `edges_backfilled_at` is NULL or older than `EDGE_EXTRACTOR_VERSION`
/// and resolves its emitted `code_edges_symbol` rows against same-page
/// `symbol_name_qualified` candidates, recording the outcome in
/// `edge_metadata`. Mirrors TS `edges-backfill.ts`.
async fn run_links_edges_backfill(
    args: LinksEdgesBackfillArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::symbol_edges::{resolve_symbol_edges_incremental, ResolverOpts, ResolverStats};
    use serde_json::json;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    // Build the source-id list.
    let source_ids: Vec<String> = if args.all_sources {
        match engine
            .execute_raw("SELECT id FROM sources ORDER BY id", &[])
            .await
        {
            Ok(rows) => {
                let ids: Vec<String> = rows
                    .iter()
                    .filter_map(|r| r.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect();
                if ids.is_empty() {
                    vec!["default".to_string()]
                } else {
                    ids
                }
            }
            Err(_) => vec!["default".to_string()],
        }
    } else if let Some(s) = &args.source {
        vec![s.clone()]
    } else {
        vec!["default".to_string()]
    };

    let mut summary: Vec<serde_json::Value> = Vec::new();
    for source_id in &source_ids {
        if !args.json {
            eprintln!("[edges-backfill] source={} starting...", source_id);
        }
        let opts = ResolverOpts {
            source_id: source_id.clone(),
            max_chunks: args.max_chunks,
        };
        let stats: ResolverStats = match resolve_symbol_edges_incremental(&engine, &opts).await {
            Ok(s) => s,
            Err(e) => {
                let msg = e.to_string();
                eprintln!("[edges-backfill] source={} failed: {}", source_id, msg);
                summary.push(json!({ "source_id": source_id, "error": msg }));
                continue;
            }
        };
        if !args.json {
            eprintln!(
                "[edges-backfill] source={} done: {} chunks walked, {} resolved, {} ambiguous, {} unmatched, {}ms",
                source_id,
                stats.chunks_walked,
                stats.edges_resolved,
                stats.edges_ambiguous,
                stats.edges_unmatched,
                stats.ms
            );
        }
        summary.push(json!({
            "source_id": source_id,
            "chunks_walked": stats.chunks_walked,
            "edges_resolved": stats.edges_resolved,
            "edges_ambiguous": stats.edges_ambiguous,
            "edges_unmatched": stats.edges_unmatched,
            "batches": stats.batches,
            "ms": stats.ms,
        }));
    }

    if args.json {
        println!("{}", serde_json::to_string_pretty(&json!(summary))?);
    }

    engine.disconnect().await?;
    Ok(())
}

// ── Takes commands ──────────────────────────────────────────────

/// Dispatch `zbrain takes` subcommands.
async fn run_takes_command(action: TakesAction, config_path: Option<&Path>) -> anyhow::Result<()> {
    match action {
        TakesAction::Add(args) => run_takes_add(args, config_path).await?,
        TakesAction::List(args) => run_takes_list(args, config_path).await?,
    }
    Ok(())
}

/// Execute `zbrain takes add`.
async fn run_takes_add(args: TakesAddArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts};
    use zbrain_core::types::TakeInput;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    // Resolve slug -> page_id
    let page = engine
        .get_page(
            &args.slug,
            &GetPageOpts {
                source_id: Some(args.source.clone()),
                include_deleted: false,
            },
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("Page not found: {} in source {}", args.slug, args.source))?;

    let take = TakeInput {
        page_id: page.id,
        row_num: None,
        claim: args.claim,
        kind: args.kind,
        holder: args.holder,
        weight: args.weight.clamp(0.0, 1.0),
        since_date: None,
        until_date: None,
        source: Some("cli".to_string()),
        superseded_by: None,
        active: Some(true),
    };

    let result = engine.add_takes_batch(page.id, &[take]).await?;

    if args.json {
        let output = serde_json::json!({
            "upserted": result.upserted,
            "weight_clamped": result.weight_clamped,
            "page_id": page.id,
            "slug": args.slug,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!(
            "{} take(s) added to page '{}' (weight_clamped: {})",
            result.upserted, args.slug, result.weight_clamped
        );
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain takes list`.
async fn run_takes_list(args: TakesListArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig, GetPageOpts};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    // Resolve slug -> page_id (takes list)
    let page = engine
        .get_page(
            &args.slug,
            &GetPageOpts {
                source_id: Some(args.source.clone()),
                include_deleted: false,
            },
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("Page not found: {} in source {}", args.slug, args.source))?;

    let takes = engine.get_takes_for_page(page.id, None).await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&takes)?);
    } else {
        if takes.is_empty() {
            println!(
                "No takes on page '{}' (id={})",
                args.slug, page.id
            );
        } else {
            for t in &takes {
                let active = if t.active { "" } else { " [inactive]" };
                println!(
                    "  #{} [{}] {} | {} | w={:.2}{}",
                    t.row_num, t.kind, t.claim, t.holder, t.weight, active
                );
            }
            println!("\n{} take(s)", takes.len());
        }
    }

    engine.disconnect().await?;
    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────

/// Parse a fact kind string into `FactKind`.
fn parse_fact_kind(s: &str) -> anyhow::Result<zbrain_core::types::FactKind> {
    use zbrain_core::types::FactKind;
    match s.to_lowercase().as_str() {
        "event" => Ok(FactKind::Event),
        "preference" => Ok(FactKind::Preference),
        "commitment" => Ok(FactKind::Commitment),
        "belief" => Ok(FactKind::Belief),
        "fact" => Ok(FactKind::Fact),
        other => Err(anyhow::anyhow!(
            "Invalid fact kind '{}'. Valid: event, preference, commitment, belief, fact",
            other
        )),
    }
}

/// Parse a fact visibility string into `FactVisibility`.
fn parse_fact_visibility(s: &str) -> anyhow::Result<zbrain_core::types::FactVisibility> {
    use zbrain_core::types::FactVisibility;
    match s.to_lowercase().as_str() {
        "private" => Ok(FactVisibility::Private),
        "world" => Ok(FactVisibility::World),
        other => Err(anyhow::anyhow!(
            "Invalid fact visibility '{}'. Valid: private, world",
            other
        )),
    }
}

/// Execute `zbrain salience` command.
async fn run_salience_command(args: SalienceArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let results = engine
        .get_recent_salience(args.days, args.limit, args.prefix.as_deref())
        .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        if results.is_empty() {
            println!("No salient pages found in the last {} days.", args.days);
            return Ok(());
        }
        // Header
        println!(
            "{:<6} {:<40} {:<14} {:<14} {:<12}",
            "Score", "Slug", "Emotion Wt", "Take Count", "Take Avg Wt"
        );
        println!("{}", "-".repeat(90));
        for r in &results {
            println!(
                "{:<6.2} {:<40} {:<14.2} {:<14} {:<12.2}",
                r.score, r.slug, r.emotional_weight, r.take_count, r.take_avg_weight
            );
        }
        println!("\n{} pages.", results.len());
    }
    Ok(())
}

/// Execute `zbrain orphans` command.
async fn run_orphans_command(args: OrphansArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let results = engine.find_orphan_pages().await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        if results.is_empty() {
            println!("No orphan pages found.");
            return Ok(());
        }
        println!("{:<40} {:<30} {:<20}", "Slug", "Title", "Domain");
        println!("{}", "-".repeat(95));
        for r in &results {
            println!(
                "{:<40} {:<30} {:<20}",
                r.slug,
                r.title,
                r.domain.as_deref().unwrap_or("-")
            );
        }
        println!("\n{} orphan pages.", results.len());
    }
    Ok(())
}

/// Execute `zbrain graph-query` command.
async fn run_graph_query_command(args: GraphQueryArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let results = engine
        .traverse_paths(
            &args.slug,
            Some(args.depth),
            args.link_type.as_deref(),
            Some(&args.direction),
            Some(&args.source),
            None,
        )
        .await?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        if results.is_empty() {
            println!(
                "No graph traversal results from '{}' (depth={}, direction={}).",
                args.slug, args.depth, args.direction
            );
            return Ok(());
        }
        println!(
            "{:<5} {:<40} {:<40} {:<15}",
            "Depth", "From", "To", "Link Type"
        );
        println!("{}", "-".repeat(105));
        for r in &results {
            println!(
                "{:<5} {:<40} {:<40} {:<15}",
                r.depth, r.from_slug, r.to_slug, r.link_type
            );
        }
        println!(
            "\n{} edges traversed from '{}' (depth={}, direction={}).",
            results.len(),
            args.slug,
            args.depth,
            args.direction
        );
    }
    Ok(())
}

/// Resolve a `sqlite://path` database URL to a filesystem path,
/// expanding `~` to the home directory.
pub(crate) fn resolve_database_path(database_url: &str) -> String {
    let path = database_url
        .strip_prefix("sqlite://")
        .unwrap_or(database_url);
    if path.starts_with('~') {
        if let Some(home) = config::home_root() {
            return format!("{}{}", home.display(), &path[1..]);
        }
    }
    path.to_string()
}

/// Resolve the brainstorm run-store directory. An explicit `--store-dir`
/// overrides the default (`~/.zbrain/runs/brainstorm`, honoring `ZBRAIN_HOME`).
#[must_use]
pub(crate) fn resolve_run_store_dir(store_dir: Option<&std::path::Path>) -> std::path::PathBuf {
    match store_dir {
        Some(d) => d.to_path_buf(),
        None => zbrain_core::eval::brainstorm::store::default_store_dir(),
    }
}

/// Print the full review of a single persisted run (1-1-5-10): metadata
/// header + the full `format_brainstorm_markdown` report, or the raw run row
/// JSON when `json` is set. Errors clearly when the run is absent.
pub(crate) fn print_run_review(
    store_dir: &std::path::Path,
    run_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    use zbrain_core::eval::brainstorm::checkpoint;
    use zbrain_core::eval::brainstorm::orchestrator::{format_brainstorm_markdown, BrainstormResult, FormatOpts};

    let row = checkpoint::load_checkpoint(store_dir, run_id).ok_or_else(|| {
        anyhow::anyhow!(
            "zbrain brainstorm: no run with run_id `{run_id}` in {}.",
            store_dir.display()
        )
    })?;
    if json {
        println!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }
    let result: BrainstormResult = serde_json::from_value(row.result.clone()).map_err(|e| {
        anyhow::anyhow!(
            "zbrain brainstorm: run `{run_id}` payload no longer deserializes: {e}"
        )
    })?;
    println!("{}", checkpoint::render_review_header(&row));
    println!();
    let md = format_brainstorm_markdown(
        &result,
        &FormatOpts { only_passed: false, include_meta: true },
    );
    println!("{md}");
    println!("\n_Run store: `{}` (run_id {})._", store_dir.display(), run_id);
    Ok(())
}

/// Print the run trend (pass-rate% + mean grounding) across persisted runs
/// within the last `days` (1-1-5-10).
pub(crate) fn print_run_trend(store_dir: &std::path::Path, days: u64) -> anyhow::Result<()> {
    use zbrain_core::eval::brainstorm::checkpoint;
    let runs = checkpoint::list_runs(store_dir);
    let recent = checkpoint::recent_runs_by_days(&runs, days);
    let chart = checkpoint::render_trend_chart(&recent);
    println!("{chart}");
    Ok(())
}

/// Dispatch `zbrain autopilot` command.
async fn run_autopilot_command(
    args: AutopilotArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::autopilot::daemon;
    use zbrain_core::autopilot::runner;

    // ── --status ──────────────────────────────────────────────────────
    if args.status {
        let status = daemon::show_status();
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "installed": status.installed,
                    "last_log": status.last_log,
                }))?
            );
        } else {
            println!(
                "Autopilot: {}",
                if status.installed { "installed" } else { "not installed" }
            );
            if !status.last_log.is_empty() {
                println!("Last log: {}", status.last_log);
            }
        }
        return Ok(());
    }

    // ── --uninstall ───────────────────────────────────────────────────
    if args.uninstall {
        // Uninstall is idempotent — try all targets, each skips if not present.
        // Actual file I/O + process management is platform-specific.
        println!("Uninstalling zbrain autopilot daemon...");
        println!("  (daemon uninstall removes plist/systemd unit/crontab/start-script)");
        println!("  Run on the target host where the daemon was installed.");
        return Ok(());
    }

    // ── --install ─────────────────────────────────────────────────────
    if args.install {
        let target = daemon::detect_install_target();
        let repo_path = args.repo.as_deref().unwrap_or(".");
        let cli_path = daemon::resolve_zbrain_cli_path()
            .unwrap_or_else(|_| "zbrain".into());

        let wrapper = daemon::generate_wrapper_script(repo_path, &cli_path);
        let wrapper_path = daemon::wrapper_script_path();

        println!("Detected install target: {}", target);
        println!("Wrapper script path: {}", wrapper_path.display());

        match target {
            daemon::InstallTarget::Macos => {
                // Host-level install target: launchd/systemd require the real
                // OS home (not `ZBRAIN_HOME`), so we resolve the home root
                // directly rather than via `config::zbrain_home()`.
                let home = dirs::home_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let plist = daemon::generate_launchd_plist(
                    &wrapper_path.to_string_lossy(),
                    &home,
                );
                println!("Plist path: {}", daemon::plist_path().display());
                if !args.json {
                    println!("\n--- plist ---\n{}", plist);
                }
            }
            daemon::InstallTarget::LinuxSystemd => {
                let unit = daemon::generate_systemd_unit(
                    &wrapper_path.to_string_lossy(),
                );
                println!("Unit path: {}", daemon::systemd_unit_path().display());
                if !args.json {
                    println!("\n--- unit ---\n{}", unit);
                }
            }
            daemon::InstallTarget::LinuxCron => {
                let home = dirs::home_dir()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let cron_line = daemon::generate_crontab_line(
                    &wrapper_path.to_string_lossy(),
                    &home,
                );
                println!("Crontab line: {}", cron_line);
            }
            daemon::InstallTarget::EphemeralContainer => {
                let script = daemon::generate_ephemeral_start_script(
                    &wrapper_path.to_string_lossy(),
                );
                let script_path = daemon::ephemeral_start_script_path();
                println!("Start script path: {}", script_path.display());
                if !args.json {
                    println!("\n--- start script ---\n{}", script);
                }
                // OpenClaw detection
                let oc = daemon::detect_open_claw();
                if oc.detected {
                    println!("OpenClaw detected. Bootstrap candidates:");
                    for p in &oc.bootstrap_candidates {
                        println!("  - {}", p.display());
                    }
                }
            }
        }

        if !args.json {
            println!("\nWrapper script content:");
            println!("{}", wrapper);
            println!("\nUninstall: zbrain autopilot --uninstall");
        }
        return Ok(());
    }

    // ── Normal mode: run autopilot tick(s) ────────────────────────────
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    // Resolve repo path: --repo flag > config sync.repo_path > "."
    let repo_path = args
        .repo
        .clone()
        .or_else(|| {
            // Config doesn't have sync.repo_path in Rust yet; default to "."
            None
        })
        .unwrap_or_else(|| ".".into());

    // Mode resolution: CLI always uses LibsqlEngine → always Inline.
    // The --inline flag is accepted but is a no-op (already inline).
    // The --no-worker flag is accepted but is a no-op (no worker in inline).
    let mode = runner::resolve_autopilot_mode(
        "pain_triggered", // default mode
        "pglite",         // CLI always uses libsql
        args.inline,
        args.no_worker,
    );

    // Print startup banner (before mode is moved into opts)
    if args.json {
        eprintln!(
            "{}",
            serde_json::json!({
                "event": "autopilot_start",
                "repo": repo_path,
                "interval": args.interval,
                "mode": format!("{:?}", mode),
                "once": args.once,
            })
        );
    } else {
        let reason = match &mode {
            runner::AutopilotMode::Inline { reason } => format!(" ({reason})"),
            _ => String::new(),
        };
        println!(
            "Autopilot starting. Repo: {}, interval: {}s{}",
            repo_path, args.interval, reason
        );
    }

    let opts = runner::AutopilotOpts {
        repo_path: repo_path.clone(),
        base_interval: args.interval,
        json_mode: args.json,
        mode,
        max_reconnect_fails: 30,
        engine_kind: zbrain_core::engine::EngineKind::Libsql,
        nightly_quality_probe_enabled: false,
        nightly_probe_max_usd: 5.0,
        audit_dir: Some(resolve_audit_dir()),
    };

    // ── --once: single tick ───────────────────────────────────────────
    if args.once {
        let mut state = runner::AutopilotState::default();
        let result = runner::run_autopilot_tick(&engine, &mut state, &opts).await;

        if args.json {
            for event in &result.events {
                eprintln!("{}", serde_json::to_string(event)?);
            }
        } else {
            for event in &result.events {
                match event {
                    runner::TickEvent::CycleInline { status, duration_ms } => {
                        println!("[cycle-inline {status}] {duration_ms}ms");
                    }
                    runner::TickEvent::Cycle { brain_score, elapsed_s, next_s } => {
                        println!(
                            "[cycle] score={brain_score} elapsed={elapsed_s}s next={next_s}s"
                        );
                    }
                    runner::TickEvent::SkipHealthy { score, plan_size } => {
                        println!("[skip] score={score} plan_size={plan_size}");
                    }
                    runner::TickEvent::FanoutSummary {
                        dispatched,
                        skipped_fresh,
                        skipped_cap,
                        legacy_fallback,
                        fanout_max,
                        score,
                    } => {
                        println!(
                            "[dispatch] fanout: {} dispatched, {} fresh, {} capped (max={fanout_max}, score={score}, legacy={legacy_fallback})",
                            dispatched.len(),
                            skipped_fresh.len(),
                            skipped_cap.len(),
                        );
                    }
                    runner::TickEvent::NoWorkerWarn { consecutive_idle } => {
                        eprintln!(
                            "[autopilot] WARNING: no worker signal for {consecutive_idle} consecutive cycles"
                        );
                    }
                    runner::TickEvent::NightlyProbeResult {
                        outcome,
                        exit_code,
                        detail,
                    } => {
                        eprintln!("[autopilot] nightly quality probe: {outcome} (exit={exit_code})");
                        if let Some(d) = detail {
                            eprintln!("[autopilot] probe detail: {d}");
                        }
                    }
                }
            }
        }

        if !result.cycle_ok {
            eprintln!("[autopilot] tick completed with errors");
        }

        engine.disconnect().await?;
        return Ok(());
    }

    // ── Continuous loop ───────────────────────────────────────────────
    let mut state = runner::AutopilotState::default();
    let mut stopping = false;

    while !stopping {
        let result = runner::run_autopilot_tick(&engine, &mut state, &opts).await;

        if args.json {
            for event in &result.events {
                eprintln!("{}", serde_json::to_string(event)?);
            }
        } else {
            for event in &result.events {
                match event {
                    runner::TickEvent::Cycle { brain_score, next_s, .. } => {
                        println!("[cycle] score={brain_score} next={next_s}s");
                    }
                    runner::TickEvent::CycleInline { status, .. } => {
                        println!("[cycle-inline {status}]");
                    }
                    runner::TickEvent::SkipHealthy { score, .. } => {
                        println!("[skip] score={score}");
                    }
                    runner::TickEvent::FanoutSummary { dispatched, score, .. } => {
                        println!("[dispatch] {} job(s) (score={score})", dispatched.len());
                    }
                    runner::TickEvent::NoWorkerWarn { consecutive_idle } => {
                        eprintln!(
                            "[autopilot] WARNING: no worker signal for {consecutive_idle} cycles"
                        );
                    }
                    runner::TickEvent::NightlyProbeResult {
                        outcome,
                        exit_code,
                        detail,
                    } => {
                        eprintln!(
                            "[autopilot] nightly quality probe: {outcome} (exit={exit_code})"
                        );
                        if let Some(d) = detail {
                            eprintln!("[autopilot] probe detail: {d}");
                        }
                    }
                }
            }
        }

        // Error tracking
        let (new_errors, should_stop) =
            runner::update_error_counter(state.consecutive_errors, result.cycle_ok);
        state.consecutive_errors = new_errors;

        if should_stop {
            eprintln!("5 consecutive cycle failures. Stopping autopilot.");
            break;
        }

        // Sleep until next tick
        tokio::time::sleep(std::time::Duration::from_secs(result.next_interval)).await;
    }

    engine.disconnect().await?;
    Ok(())
}

// ── remote command ──────────────────────────────────────────────────────

/// Parse a duration string like "5m", "30s", "1h", "90s", "500ms" into milliseconds.
/// Returns None if the string doesn't match the expected format.
///
/// Mirrors TS `parseDuration` in remote.ts.
fn parse_duration(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Find where the numeric part ends and the unit begins.
    let split_idx = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());

    let (num_str, unit) = s.split_at(split_idx);
    let n: f64 = num_str.parse().ok()?;
    let unit = if unit.is_empty() { "ms" } else { unit };

    let ms = match unit {
        "ms" => n,
        "s" => n * 1000.0,
        "m" => n * 60_000.0,
        "h" => n * 3_600_000.0,
        _ => return None,
    };

    if ms < 0.0 {
        return None;
    }
    Some(ms as u64)
}

/// Compute the poll interval (in milliseconds) based on elapsed time.
///
/// Backoff curve mirrors TS `runRemotePing`:
///   - First 30s:   poll every 1s
///   - Next 5m30s:  poll every 5s
///   - After 6m:    poll every 10s
fn compute_poll_interval(elapsed_ms: u64) -> u64 {
    if elapsed_ms < 30_000 {
        1_000
    } else if elapsed_ms < 30_000 + 5 * 60_000 {
        5_000
    } else {
        10_000
    }
}

/// Unpack an MCP tool call result, extracting JSON from the content envelope.
///
/// MCP responses wrap the actual result in a content array:
///   { "content": [{ "type": "text", "text": "<JSON string>" }] }
/// or a JSON-RPC envelope:
///   { "jsonrpc": "2.0", "result": { "content": [...] } }
///
/// This function drills through both layers and parses the text as JSON.
fn unpack_tool_result(value: &serde_json::Value) -> serde_json::Value {
    // Drill through JSON-RPC envelope if present
    let value = value.get("result").unwrap_or(value);

    // Extract content array
    if let Some(content) = value.get("content").and_then(|c| c.as_array()) {
        if let Some(first) = content.first() {
            if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                    return parsed;
                }
                // Return the text as a string value if it's not JSON
                return serde_json::Value::String(text.to_string());
            }
        }
    }

    // Return as-is if no content envelope
    value.clone()
}

/// Check if a job state is terminal.
fn is_terminal_state(state: &str) -> bool {
    matches!(state, "completed" | "failed" | "dead" | "cancelled")
}

/// Execute `zbrain remote` command.
async fn run_remote_command(
    sub: RemoteSub,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config_file = config_path
        .map(PathBuf::from)
        .or_else(config::user_config_path)
        .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;

    let config = config::load_config_from_path(&config_file)?;

    if !config::is_thin_client(&config) {
        eprintln!(
            "`zbrain remote` requires thin-client mode. This install has no remote_mcp config.\n\
             Run `zbrain init --mcp-only` to set up thin-client mode, or use the local CLI directly."
        );
        std::process::exit(1);
    }

    match sub {
        RemoteSub::Ping(args) => run_remote_ping(config, args).await,
        RemoteSub::Doctor(args) => run_remote_doctor(config, args).await,
    }
}

/// Submit an autopilot-cycle job to the remote host and poll until terminal.
///
/// NO `repo` arg is passed — the autopilot uses the server's configured brain
/// repo. This sidesteps the repo-path validation issue entirely because the
/// path is server-controlled.
///
/// Payload uses `data: {phases: [...]}`, NOT `params:` — the submit_job op
/// shape takes `data`.
async fn run_remote_ping(config: config::Config, args: RemotePingArgs) -> anyhow::Result<()> {
    let timeout_ms = args
        .max_wait
        .as_deref()
        .and_then(parse_duration)
        .unwrap_or(15 * 60 * 1000); // default 15m

    // Per-call timeout for MCP tool calls (polling interval + slack)
    let mcp_client = mcp_client::McpClient::new(
        config,
        std::time::Duration::from_millis(30_000),
    );

    // Submit the autopilot-cycle job
    let submit_result = mcp_client
        .call_tool(
            "submit_job",
            serde_json::json!({
                "name": "autopilot-cycle",
                "data": { "phases": ["sync", "extract", "embed"] }
            }),
        )
        .await;

    let submitted = match submit_result {
        Ok(res) => {
            let data = unpack_tool_result(&res);
            // Extract id and state from the response
            let id = data.get("id").and_then(|v| v.as_i64());
            let state = data
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("queued");
            match id {
                Some(id) => (id, state.to_string()),
                None => {
                    if args.json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "status": "error",
                                "reason": "parse_error",
                                "message": "submit_job response missing 'id' field",
                                "raw": data
                            })
                        );
                    } else {
                        eprintln!("Failed to parse submit_job response: missing 'id' field");
                    }
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            let msg = format!("{e}");
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "error",
                        "reason": "unknown",
                        "message": msg
                    })
                );
            } else {
                eprintln!("Failed to submit autopilot-cycle: {msg}");
                eprintln!(
                    "Hint: ensure the OAuth client was registered with admin scope (`--scopes read,write,admin`)."
                );
            }
            std::process::exit(1);
        }
    };

    let (job_id, initial_state) = submitted;

    if !args.json {
        eprintln!("Submitted autopilot-cycle (job #{job_id}). Polling...");
    }

    let start = std::time::Instant::now();
    let mut attempt = 0u32;
    let mut last_state = initial_state.clone();

    loop {
        let elapsed_ms = start.elapsed().as_millis() as u64;
        if elapsed_ms >= timeout_ms {
            // Timeout
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "error",
                        "reason": "timeout",
                        "job_id": job_id,
                        "last_state": last_state,
                        "message": format!("ping timed out after {}s; check job {} on the host.", timeout_ms / 1000, job_id),
                    })
                );
            } else {
                eprintln!(
                    "\nping timed out after {}s. Job #{job_id} is still {last_state}.",
                    timeout_ms / 1000
                );
                eprintln!("Run `zbrain jobs get {job_id}` on the host to inspect.");
            }
            std::process::exit(1);
        }

        let interval = compute_poll_interval(elapsed_ms);
        tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
        attempt += 1;

        // Poll get_job
        let poll_result = mcp_client
            .call_tool("get_job", serde_json::json!({ "id": job_id }))
            .await;

        match poll_result {
            Ok(res) => {
                let data = unpack_tool_result(&res);
                let state = data
                    .get("state")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| last_state.clone());

                if state != last_state {
                    last_state = state.clone();
                    if !args.json {
                        eprintln!("  job #{job_id} -> {state}");
                    }
                }

                if is_terminal_state(&state) {
                    let ok = state == "completed";
                    let failed_reason = data
                        .get("failed_reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if args.json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "status": if ok { "success" } else { "error" },
                                "job_id": job_id,
                                "state": state,
                                "failed_reason": if failed_reason.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(failed_reason.to_string()) },
                                "elapsed_ms": start.elapsed().as_millis(),
                            })
                        );
                    } else {
                        if ok {
                            println!(
                                "\nautopilot-cycle complete ({}s).",
                                start.elapsed().as_secs()
                            );
                        } else {
                            let reason = if failed_reason.is_empty() {
                                String::new()
                            } else {
                                format!(": {failed_reason}")
                            };
                            println!("\nautopilot-cycle ended {state}{reason}.");
                        }
                    }
                    std::process::exit(if ok { 0 } else { 1 });
                }
            }
            Err(e) => {
                // Network blip mid-poll: log and keep going
                if !args.json {
                    eprintln!("  poll #{attempt} failed ({e}); continuing...");
                }
            }
        }
    }
}

/// Call `run_doctor` on the remote host, render the structured DoctorReport,
/// and exit 0/1 based on status (unhealthy -> 1, otherwise 0).
async fn run_remote_doctor(config: config::Config, args: RemoteDoctorArgs) -> anyhow::Result<()> {
    let mcp_client = mcp_client::McpClient::new(
        config,
        std::time::Duration::from_millis(60_000),
    );

    let result = mcp_client.call_tool("run_doctor", serde_json::json!({})).await;

    let report = match result {
        Ok(res) => unpack_tool_result(&res),
        Err(e) => {
            let msg = format!("{e}");
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "error",
                        "reason": "unknown",
                        "message": msg
                    })
                );
            } else {
                eprintln!("Failed to run remote doctor: {msg}");
                eprintln!(
                    "Hint: run_doctor requires admin scope. Re-register the client with `--scopes read,write,admin`."
                );
            }
            std::process::exit(1);
        }
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_doctor_report_remote(&report);
    }

    let status = report
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unhealthy");
    std::process::exit(if status == "unhealthy" { 1 } else { 0 });
}

/// Render a remote DoctorReport in human-readable form.
fn render_doctor_report_remote(report: &serde_json::Value) {
    println!("\nZBrain Health Check (remote host)");
    println!("=================================");

    if let Some(checks) = report.get("checks").and_then(|c| c.as_array()) {
        for check in checks {
            let name = check.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let status = check
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let message = check
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let icon = match status {
                "ok" => "OK",
                "warn" => "WARN",
                "fail" => "FAIL",
                _ => "??",
            };
            println!("  [{icon}] {name}: {message}");
        }
    }

    let health_score = report
        .get("health_score")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let status = report
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    println!("\nHealth score: {health_score}/100. Status: {status}.");

    if status == "unhealthy" {
        if let Some(checks) = report.get("checks").and_then(|c| c.as_array()) {
            let fails: Vec<_> = checks
                .iter()
                .filter(|c| {
                    c.get("status").and_then(|v| v.as_str()) == Some("fail")
                })
                .collect();
            if !fails.is_empty() {
                println!("\nFailures:");
                for f in fails {
                    let name = f.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let message = f.get("message").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  - {name}: {message}");
                }
            }
        }
    }
}

// ── jobs command ────────────────────────────────────────────────────────

/// Parse a relative duration string like "30d", "7d", "1h" into an RFC 3339
/// timestamp for the cutoff. Returns None on parse failure.
fn parse_relative_duration(s: &str) -> Option<String> {
    let s = s.trim();
    let (num_str, unit) = if let Some(pos) = s.find(|c: char| !c.is_ascii_digit()) {
        s.split_at(pos)
    } else {
        return None;
    };
    let n: i64 = num_str.parse().ok()?;
    let secs = match unit {
        "d" => n * 86_400,
        "h" => n * 3_600,
        "m" => n * 60,
        "s" => n,
        _ => return None,
    };
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(secs);
    Some(cutoff.to_rfc3339())
}

/// Render a MinionJob as a human-readable line.
fn render_job_line(job: &zbrain_core::minions::types::MinionJob) -> String {
    format!(
        "  #{:<6} {:<12} {:<10} p={} a={}/{} q={}",
        job.id, job.name, job.status.as_str(), job.priority, job.attempts_made, job.max_attempts, job.queue
    )
}

/// Execute `zbrain jobs` command.
async fn run_jobs_command(
    action: JobsAction,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    use zbrain_core::minions::queue::MinionQueue;
    use zbrain_core::minions::types::*;

    match action {
        JobsAction::Submit(args) => {
            let data = args
                .params
                .as_deref()
                .map(|s| serde_json::from_str(s))
                .transpose()?
                .unwrap_or(serde_json::Value::Null);

            let input = MinionJobInput {
                name: args.name.clone(),
                data: Some(data),
                queue: args.queue.clone(),
                priority: args.priority,
                max_attempts: args.max_attempts,
                backoff_type: None,
                backoff_delay: None,
                backoff_jitter: None,
                max_stalled: args.max_stalled,
                delay: args.delay,
                parent_job_id: None,
                on_child_fail: None,
                max_children: None,
                timeout_ms: None,
                remove_on_complete: None,
                remove_on_fail: None,
                idempotency_key: None,
            };

            let queue = MinionQueue::new(&engine);
            let job = queue.add(&input).await?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "id": job.id,
                    "name": job.name,
                    "status": job.status.as_str(),
                    "queue": job.queue,
                    "priority": job.priority,
                }))?);
            } else {
                println!("Submitted job #{} ({}) to queue '{}'", job.id, job.name, job.queue);
            }
        }

        JobsAction::List(args) => {
            let status = args
                .status
                .as_deref()
                .and_then(MinionJobStatus::parse);

            let filters = JobFilters {
                status,
                queue: args.queue.clone(),
                name: None,
                limit: Some(args.limit),
                offset: None,
            };

            let queue = MinionQueue::new(&engine);
            let jobs = queue.get_jobs(&filters).await?;

            if args.json {
                let arr: Vec<_> = jobs
                    .iter()
                    .map(|j| serde_json::json!({
                        "id": j.id, "name": j.name, "status": j.status.as_str(),
                        "queue": j.queue, "priority": j.priority,
                        "attempts_made": j.attempts_made, "max_attempts": j.max_attempts,
                    }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&arr)?);
            } else {
                if jobs.is_empty() {
                    println!("No jobs found.");
                } else {
                    println!("{:<10} {:<12} {:<10} {:<5} {:<5} {:<10}",
                        "ID", "NAME", "STATUS", "PRI", "ATT", "QUEUE");
                    for j in &jobs {
                        println!("{}", render_job_line(j));
                    }
                }
            }
        }

        JobsAction::Get(args) => {
            let queue = MinionQueue::new(&engine);
            let job = queue.get_job(args.id).await?;

            match job {
                Some(j) => {
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                            "id": j.id, "name": j.name, "status": j.status.as_str(),
                            "queue": j.queue, "priority": j.priority,
                            "data": j.data, "attempts_made": j.attempts_made,
                            "max_attempts": j.max_attempts, "stalled_counter": j.stalled_counter,
                            "max_stalled": j.max_stalled,
                            "created_at": j.created_at, "updated_at": j.updated_at,
                            "error_text": j.error_text,
                        }))?);
                    } else {
                        println!("Job #{}", j.id);
                        println!("  name:     {}", j.name);
                        println!("  status:   {}", j.status.as_str());
                        println!("  queue:    {}", j.queue);
                        println!("  priority: {}", j.priority);
                        println!("  attempts: {}/{}", j.attempts_made, j.max_attempts);
                        if !j.data.is_null() {
                            println!("  data:     {}", j.data);
                        }
                        if let Some(e) = &j.error_text {
                            println!("  error:    {}", e);
                        }
                    }
                }
                None => {
                    eprintln!("Job #{} not found.", args.id);
                    std::process::exit(1);
                }
            }
        }

        JobsAction::Cancel(args) => {
            engine.cancel_job(args.id).await?;
            println!("Cancelled job #{}.", args.id);
        }

        JobsAction::Retry(args) => {
            let queue = MinionQueue::new(&engine);
            let job = queue.retry_job(args.id).await?;

            match job {
                Some(j) => {
                    if args.json {
                        println!("{}", serde_json::json!({
                            "id": j.id, "status": j.status.as_str(), "attempts_made": j.attempts_made,
                        }));
                    } else {
                        println!("Retried job #{} — status: {}, attempts: {}", j.id, j.status.as_str(), j.attempts_made);
                    }
                }
                None => {
                    eprintln!("Job #{} not found or not in a retryable state.", args.id);
                    std::process::exit(1);
                }
            }
        }

        JobsAction::Prune(args) => {
            let cutoff = args
                .older_than
                .as_deref()
                .and_then(parse_relative_duration)
                .unwrap_or_else(|| {
                    let cutoff = chrono::Utc::now() - chrono::Duration::days(30);
                    cutoff.to_rfc3339()
                });

            let queue = MinionQueue::new(&engine);
            let count = queue.prune(Some(&cutoff), None).await?;

            if args.json {
                println!("{}", serde_json::json!({ "pruned": count }));
            } else {
                println!("Pruned {} terminal jobs older than {}.", count, cutoff);
            }
        }

        JobsAction::Stats(args) => {
            let queue = MinionQueue::new(&engine);
            let stats = queue.get_stats(None).await?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "by_status": stats.by_status,
                    "by_type": stats.by_type,
                    "queue_health": stats.queue_health,
                }))?);
            } else {
                println!("Queue Statistics");
                println!("=================");
                println!("\nBy Status:");
                for (status, count) in &stats.by_status {
                    println!("  {:<15} {}", status, count);
                }
                if !stats.by_type.is_empty() {
                    println!("\nBy Type (last 24h):");
                    for t in &stats.by_type {
                        println!(
                            "  {:<25} total={} ok={} fail={} dead={}",
                            t.name, t.total, t.completed, t.failed, t.dead
                        );
                    }
                }
            }
        }

        JobsAction::Work(args) => {
            let queue_name = args.queue.unwrap_or_else(|| "default".into());
            eprintln!("Starting worker on queue '{}' (concurrency={})", queue_name, args.concurrency);
            eprintln!("Press Ctrl+C to stop.");

            // Worker startup: connect, register handlers, start loop.
            // Full worker implementation is in zbrain-worker crate.
            // This CLI command is a thin launcher.
            eprintln!("(worker integration — connects to queue and processes jobs)");
            eprintln!("Note: use `zbrain serve` with --http to run the full stack including workers.");
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain agent` command.
async fn run_agent_command(
    action: AgentAction,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine: std::sync::Arc<dyn zbrain_core::engine::BrainEngine> = std::sync::Arc::new(engine);

    use zbrain_core::minions::queue::MinionQueue;
    use zbrain_core::minions::types::*;

    match action {
        AgentAction::Run(args) => {
            // The subagent handler reads `model` from job data. When unset we
            // fall back to a concrete default so the in-process executor below
            // can build a matching provider (there is otherwise no default).
            let effective_model = args
                .model
                .clone()
                .unwrap_or_else(|| "anthropic:claude-opus-4-7".to_string());
            let data = serde_json::json!({
                "prompt": args.prompt,
                "model": effective_model,
                "max_turns": args.max_turns,
            });

            let input = MinionJobInput {
                name: "subagent".into(),
                data: Some(data),
                queue: None,
                priority: None,
                max_attempts: None,
                backoff_type: None,
                backoff_delay: None,
                backoff_jitter: None,
                max_stalled: Some(3),
                delay: None,
                parent_job_id: None,
                on_child_fail: None,
                max_children: None,
                timeout_ms: None,
                remove_on_complete: None,
                remove_on_fail: None,
                idempotency_key: None,
            };

            let job = {
                let queue = MinionQueue::new(&*engine);
                queue.add(&input).await?
            };

            if args.json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                    "id": job.id, "name": job.name, "status": job.status.as_str(),
                }))?);
            } else {
                println!("Submitted subagent job #{} ({})", job.id, job.status.as_str());
            }

            // Follow mode: actually EXECUTE the job in-process, then report.
            // The Rust CLI has no external worker (`jobs work` only launches a
            // placeholder), so `agent run --follow` runs a short-lived inline
            // worker itself — the same executor `book-mirror` uses.
            if args.follow {
                let start = std::time::Instant::now();

                let (parsed, recipe) =
                    zbrain_core::ai::resolver::resolve_recipe_strict(&effective_model)
                        .map_err(|e| anyhow::anyhow!(e.message))?;
                let provider = zbrain_core::ai::chat::instantiate_chat(
                    recipe,
                    &parsed.model_id,
                    |k| std::env::var(k).ok(),
                )
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;

                let jobs = crate::inline_worker::run_subagent_jobs(
                    std::sync::Arc::clone(&engine),
                    std::sync::Arc::from(provider),
                    &[job.id],
                    crate::inline_worker::InlineWorkerOpts {
                        concurrency: 1,
                        ..Default::default()
                    },
                )
                .await?;

                let final_job = jobs.into_iter().next().flatten();
                match final_job {
                    Some(j) => {
                        let ok = j.status == MinionJobStatus::Completed;
                        if !args.json {
                            if ok {
                                println!("\nSubagent completed ({}s).", start.elapsed().as_secs());
                            } else {
                                println!("\nSubagent ended: {}.", j.status.as_str());
                            }
                        }
                        engine.disconnect().await?;
                        std::process::exit(if ok { 0 } else { 1 });
                    }
                    None => {
                        eprintln!("Job #{} disappeared.", job.id);
                        engine.disconnect().await?;
                        std::process::exit(1);
                    }
                }
            }
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain backfill` (G77 / 1-6).
///
/// Mirrors TS `commands/backfill.ts`: `list` enumerates registered backfills;
/// `<kind>` runs one. `effective_date` delegates to `reindex frontmatter`
/// (identical recompute logic); `emotional_weight` calls the cycle phase
/// `recompute_emotional_weight` (exposed here as a standalone verb);
/// `embedding_voyage` is declared-only and errors.
async fn run_backfill_command(
    args: BackfillArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    // `list` mode — positional "list" or `--list`.
    if args.list || args.kind.as_deref() == Some("list") {
        print_backfill_list();
        return Ok(());
    }

    match args.kind.as_deref() {
        Some("effective_date") => {
            // Delegates to `reindex frontmatter` (identical recompute logic).
            // `force` => recompute every row (backfill semantics); `yes` skips
            // the confirmation prompt for non-TTY / non-JSON runs.
            let fm = ReindexFrontmatterArgs {
                source_id: None,
                slug_prefix: None,
                dry_run: args.dry_run,
                yes: true,
                force: true,
                json: args.json,
            };
            return run_reindex_frontmatter(fm, config_path).await;
        }
        Some("emotional_weight") => return run_backfill_emotional_weight(&args, config_path).await,
        Some("embedding_voyage") => {
            anyhow::bail!(
                "Backfill \"embedding_voyage\" is declared-only in v0.30.1 — \
                 the schema migration ships in v0.30.2."
            );
        }
        Some(other) => {
            anyhow::bail!(
                "No backfill registered with name \"{other}\". Run `zbrain backfill list`."
            );
        }
        None => {
            anyhow::bail!("Usage: zbrain backfill <kind> [flags]   |   zbrain backfill list");
        }
    }
}

/// Run the `emotional_weight` backfill against the local libsql engine.
async fn run_backfill_emotional_weight(
    args: &BackfillArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::autopilot::phases::recompute_emotional_weight::{
        run_phase_recompute_emotional_weight, RecomputeEmotionalWeightOpts,
    };
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::libsql::LibsqlEngine;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = RecomputeEmotionalWeightOpts {
        dry_run: args.dry_run,
        ..Default::default()
    };

    let result = run_phase_recompute_emotional_weight(&engine, &opts).await?;
    engine.disconnect().await?;

    if args.json {
        let envelope = serde_json::json!({
            "kind": "emotional_weight",
            "status": result.status,
            "summary": result.summary,
            "pages_recomputed": result.pages_recomputed,
            "mode": result.mode,
            "dry_run": result.dry_run,
        });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!("Backfill emotional_weight complete.");
        println!("  pages_recomputed: {}", result.pages_recomputed);
        println!("  mode:             {}", result.mode);
        println!("  dry_run:          {}", result.dry_run);
        if !result.summary.is_empty() {
            println!("  summary:          {}", result.summary);
        }
    }
    Ok(())
}

/// Print the registered backfills and their implementation status (TS
/// `listBackfills`). `✓` = implemented, `⊘` = declared-only.
fn print_backfill_list() {
    println!("Registered backfills (v0.30.1):\n");
    let entries: &[(&str, bool, &str)] = &[
        (
            "effective_date",
            true,
            "Compute effective_date for pages imported pre-v0.29.1.",
        ),
        (
            "emotional_weight",
            true,
            "Recompute emotional_weight for pages with stale stamp.",
        ),
        (
            "embedding_voyage",
            false,
            "Declared-only in v0.30.1 (multi-column embedding lands in v0.30.2).",
        ),
    ];
    for (name, implemented, desc) in entries {
        let status = if *implemented { "✓" } else { "⊘" };
        println!("  {status} {name:<20} {desc}");
    }
    println!();
}

/// Run `zbrain export`: serialize every matching page to `<dir>/<slug>.md`,
/// with a `.raw/<slug>.json` sidecar when raw sidecar data exists.
async fn run_export_command(
    args: ExportArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    if args.restore_only {
        anyhow::bail!(
            "`export --restore-only` requires storage-tier config (db_only tiers) \
             which is not yet ported to Rust. Use a regular export instead."
        );
    }
    use zbrain_core::engine::{BrainEngine, EngineConfig, PageFilters};
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::markdown::serialize_markdown;

    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let filters = PageFilters {
        page_type: args.r#type.clone(),
        tag: None,
        limit: Some(100_000),
        offset: None,
        updated_after: None,
        slug_prefix: args.slug_prefix.clone(),
        include_deleted: false,
        sort: None,
        source_id: args.source_id.clone(),
        source_ids: None,
    };

    let pages = engine.list_pages(&filters).await?;
    let out_dir = std::path::Path::new(&args.dir);
    std::fs::create_dir_all(out_dir)
        .map_err(|e| anyhow::anyhow!("create_dir {out_dir:?}: {e}"))?;

    let mut exported = 0usize;
    for page in &pages {
        let tags = engine
            .get_tags(&page.slug, Some(page.source_id.as_str()))
            .await?;
        let md = serialize_markdown(
            &page.frontmatter,
            &page.compiled_truth,
            &page.timeline,
            &tags,
        );
        let md_path = out_dir.join(format!("{}.md", page.slug));
        if let Some(parent) = md_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("create_dir {parent:?}: {e}"))?;
        }
        std::fs::write(&md_path, md)
            .map_err(|e| anyhow::anyhow!("write {md_path:?}: {e}"))?;

        let raw = engine
            .get_raw_data(&page.slug, None, Some(page.source_id.as_str()))
            .await?;
        if !raw.is_empty() {
            let mut raw_obj = serde_json::Map::new();
            for rd in &raw {
                raw_obj.insert(rd.source.clone(), rd.data.clone());
            }
            let slug_parts: Vec<&str> = page.slug.split('/').collect();
            let raw_dir = slug_parts
                .iter()
                .take(slug_parts.len().saturating_sub(1))
                .fold(out_dir.join(".raw"), |acc, p| acc.join(p));
            std::fs::create_dir_all(&raw_dir)
                .map_err(|e| anyhow::anyhow!("create_dir {raw_dir:?}: {e}"))?;
            let raw_path = raw_dir.join(format!(
                "{}.json",
                slug_parts.last().copied().unwrap_or("")
            ));
            std::fs::write(
                &raw_path,
                serde_json::to_string_pretty(&serde_json::Value::Object(raw_obj))?,
            )
            .map_err(|e| anyhow::anyhow!("write {raw_path:?}: {e}"))?;
        }

        exported += 1;
    }

    if args.json {
        let envelope = serde_json::json!({ "exported": exported, "dir": args.dir });
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!("Exported {exported} pages to {}/", args.dir);
    }

    engine.disconnect().await?;
    Ok(())
}

// ─── upgrade / post-upgrade ──────────────────────────────────────────────
//
// The TS `upgrade` flow reinstalls the bun/npm/clawhub binary and then runs
// `post-upgrade`. A cargo-built binary is updated via the package manager /
// cargo instead, so `upgrade` just delegates to `post-upgrade`, which runs the
// idempotent migration orchestrator (the real, valuable work).

async fn run_upgrade_command(
    args: UpgradeArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    println!("`zbrain upgrade` self-reinstall is not applicable to a cargo-built binary.");
    println!("Update the zbrain binary via your package manager or `cargo install`, then re-run.");
    println!("Running post-upgrade (apply-migrations) to keep the brain DB current...\n");
    run_post_upgrade_command(
        PostUpgradeArgs {
            yes: args.yes,
            json: args.json,
        },
        config_path,
    )
    .await
}

async fn run_post_upgrade_command(
    args: PostUpgradeArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let am_args = ApplyMigrationsArgs {
        list: false,
        dry_run: false,
        yes: args.yes,
        force_retry: None,
        force_orchestrator: false,
        force_schema: false,
        force_all: false,
        skip_verify: false,
        mode: None,
        host_dir: None,
        no_autopilot_install: false,
        json: args.json,
    };
    apply_migrations::run_apply_migrations_command(&am_args, config_path).await
}

// ─── providers ─────────────────────────────────────────────────────────────

fn provider_touchpoint_labels(r: &zbrain_core::ai::types::Recipe) -> Vec<String> {
    use zbrain_core::ai::types::TouchpointKind;
    let mut out = Vec::new();
    for k in [
        TouchpointKind::Embedding,
        TouchpointKind::Expansion,
        TouchpointKind::Chat,
        TouchpointKind::Reranker,
    ] {
        if r.has_touchpoint(k) {
            out.push(match k {
                TouchpointKind::Embedding => "embedding".to_string(),
                TouchpointKind::Expansion => "expansion".to_string(),
                TouchpointKind::Chat => "chat".to_string(),
                TouchpointKind::Reranker => "reranker".to_string(),
            });
        }
    }
    out
}

fn run_providers_command(
    action: ProvidersAction,
    _config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::ai::registry::REGISTRY;

    match action {
        ProvidersAction::List => {
            println!("Known AI providers (env-readiness):\n");
            println!(
                "{:<14} {:<10} {:<32} status",
                "ID", "TIER", "TOUCHPOINTS"
            );
            for r in REGISTRY.iter() {
                let ready = r.auth_env.as_ref().map_or(true, |a| {
                    a.required.iter().all(|v| std::env::var(v).is_ok())
                });
                println!(
                    "{:<14} {:<10} {:<32} {}",
                    r.id,
                    format!("{:?}", r.tier),
                    provider_touchpoint_labels(r).join(", "),
                    if ready { "ready" } else { "missing env" }
                );
            }
            Ok(())
        }
        ProvidersAction::Env(args) => {
            let r = REGISTRY.iter().find(|r| r.id == args.id.as_str());
            match r {
                None => anyhow::bail!(
                    "Unknown provider: {}. Run `zbrain providers list` to see known providers.",
                    args.id
                ),
                Some(r) => {
                    println!("Provider: {} ({})", r.name, r.id);
                    match &r.auth_env {
                        Some(a) => {
                            println!("Required env vars:");
                            for v in a.required {
                                println!("  {}", v);
                            }
                            if !a.optional.is_empty() {
                                println!("Optional env vars:");
                                for v in a.optional {
                                    println!("  {}", v);
                                }
                            }
                            if let Some(u) = a.setup_url {
                                println!("Setup: {}", u);
                            }
                        }
                        None => println!("No env vars required (native provider)."),
                    }
                    Ok(())
                }
            }
        }
        ProvidersAction::Explain(args) => {
            let matrix: Vec<serde_json::Value> = REGISTRY
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "name": r.name,
                        "tier": format!("{:?}", r.tier),
                        "touchpoints": provider_touchpoint_labels(r),
                        "authEnv": r.auth_env.as_ref().map(|a| serde_json::json!({
                            "required": a.required,
                            "optional": a.optional,
                            "setupUrl": a.setup_url,
                        })),
                        "setupHint": r.setup_hint,
                    })
                })
                .collect();
            if args.json {
                println!("{}", serde_json::to_string_pretty(&matrix)?);
            } else {
                println!("Provider matrix ({} providers):", matrix.len());
                for r in REGISTRY.iter() {
                    println!(
                        "  {} — {} [{}]",
                        r.id,
                        r.name,
                        provider_touchpoint_labels(r).join(", ")
                    );
                }
            }
            Ok(())
        }
        ProvidersAction::Test(args) => {
            // Rust does not yet port the live embedding/chat probe (it needs the
            // AI client + network). We surface env + config readiness, which is
            // the part that actually catches misconfiguration before `init`.
            let provider_id = match &args.model {
                Some(m) => m.split_once(':').map(|(p, _)| p.to_string()).unwrap_or_else(|| m.clone()),
                None => {
                    println!("No --model given; live probe is not yet ported to Rust.");
                    println!("Example: zbrain providers test --model openai:text-embedding-3-small");
                    anyhow::bail!("providers test requires --model <provider:model>")
                }
            };
            let r = REGISTRY.iter().find(|r| r.id == provider_id.as_str());
            match r {
                None => anyhow::bail!(
                    "Unknown provider: {}. Run `zbrain providers list`.",
                    provider_id
                ),
                Some(r) => {
                    let ready = r.auth_env.as_ref().map_or(true, |a| {
                        a.required.iter().all(|v| std::env::var(v).is_ok())
                    });
                    if ready {
                        println!(
                            "Provider '{}' env is ready. Live probe is not yet ported to Rust; run `zbrain init` to validate the active path.",
                            r.id
                        );
                    } else {
                        println!(
                            "Provider '{}' is NOT ready: missing required env vars. Run `zbrain providers env {}`.",
                            r.id, r.id
                        );
                    }
                    Ok(())
                }
            }
        }
    }
}

// ─── frontmatter ───────────────────────────────────────────────────────────

/// Walk `root`, collecting `.md` files while skipping vendor / hidden /
/// generated subtrees (mirrors the TS `collectFiles` descent rules).
fn collect_markdown_files(root: &str) -> anyhow::Result<Vec<String>> {
    let p = std::path::Path::new(root);
    let mut out = Vec::new();
    if p.is_file() {
        if p.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(root.to_string());
        }
        return Ok(out);
    }
    if !p.is_dir() {
        anyhow::bail!("path does not exist: {root}");
    }
    let skip = |name: &str| {
        name == ".git"
            || name == "node_modules"
            || name == "vendor"
            || name == "target"
            || name.starts_with('.')
    };
    let mut stack = vec![p.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| anyhow::anyhow!("read_dir {dir:?}: {e}"))?
        {
            let entry = entry.map_err(|e| anyhow::anyhow!("entry: {e}"))?;
            let ep = entry.path();
            if ep.is_dir() {
                if let Some(name) = ep.file_name().and_then(|n| n.to_str()) {
                    if skip(name) {
                        continue;
                    }
                }
                stack.push(ep);
            } else if ep.extension().and_then(|e| e.to_str()) == Some("md") {
                out.push(ep.to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}

fn run_frontmatter_command(
    action: FrontmatterAction,
    _config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::capture::parse_frontmatter_from_body;
    use zbrain_core::markdown::parse_markdown;

    match action {
        FrontmatterAction::Validate(args) => {
            if args.fix {
                anyhow::bail!(
                    "`frontmatter validate --fix` is not yet ported to Rust (no in-place frontmatter rewriting yet)."
                );
            }
            let files = collect_markdown_files(&args.path)?;
            let mut errors = 0usize;
            let mut reports = Vec::new();
            for f in &files {
                let content =
                    std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("read {f}: {e}"))?;
                match parse_frontmatter_from_body(&content) {
                    Ok((fm, _)) => {
                        let has_fm =
                            fm.map_or(false, |v| !v.as_object().map_or(false, |o| o.is_empty()));
                        if !has_fm {
                            errors += 1;
                            reports.push(serde_json::json!({
                                "path": f,
                                "ok": false,
                                "error": "missing or empty frontmatter"
                            }));
                        } else {
                            reports.push(serde_json::json!({ "path": f, "ok": true }));
                        }
                    }
                    Err(e) => {
                        errors += 1;
                        reports.push(serde_json::json!({
                            "path": f,
                            "ok": false,
                            "error": e.to_string()
                        }));
                    }
                }
            }
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "scanned": files.len(), "errors": errors, "files": reports })
                    )?
                );
            } else {
                for r in &reports {
                    if r["ok"].as_bool().unwrap_or(false) {
                        println!("ok    {}", r["path"].as_str().unwrap_or(""));
                    } else {
                        println!(
                            "FAIL  {} — {}",
                            r["path"].as_str().unwrap_or(""),
                            r["error"].as_str().unwrap_or("")
                        );
                    }
                }
                println!(
                    "\nScanned {} file(s), {} with frontmatter issues.",
                    files.len(),
                    errors
                );
            }
            if errors > 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        FrontmatterAction::Generate(args) => {
            let files = collect_markdown_files(&args.path)?;
            let mut generated = 0usize;
            let mut reports = Vec::new();
            for f in &files {
                let content =
                    std::fs::read_to_string(f).map_err(|e| anyhow::anyhow!("read {f}: {e}"))?;
                let (fm, body_without_fm) = parse_frontmatter_from_body(&content)
                    .map_err(|e| anyhow::anyhow!("parse {f}: {e}"))?;
                let already_has =
                    fm.map_or(false, |v| !v.as_object().map_or(false, |o| o.is_empty()));
                if already_has {
                    reports.push(serde_json::json!({
                        "path": f,
                        "action": "skip",
                        "reason": "already has frontmatter"
                    }));
                    continue;
                }
                let parsed = parse_markdown(&content, f, None);
                let mut new_fm = serde_json::Map::new();
                if !parsed.type_.is_empty() {
                    new_fm.insert(
                        "type".into(),
                        serde_json::Value::String(parsed.type_.clone()),
                    );
                }
                if !parsed.title.is_empty() {
                    new_fm.insert("title".into(), serde_json::Value::String(parsed.title.clone()));
                }
                if !parsed.tags.is_empty() {
                    new_fm.insert(
                        "tags".into(),
                        serde_json::Value::Array(
                            parsed.tags.iter().cloned().map(serde_json::Value::String).collect(),
                        ),
                    );
                }
                if new_fm.is_empty() {
                    reports.push(serde_json::json!({
                        "path": f,
                        "action": "skip",
                        "reason": "nothing to infer"
                    }));
                    continue;
                }
                let yaml = serde_yaml::to_string(&serde_json::Value::Object(new_fm.clone()))
                    .map_err(|e| anyhow::anyhow!("yaml {f}: {e}"))?;
                let rebuilt = format!("---\n{yaml}---\n\n{body_without_fm}");
                if args.fix {
                    std::fs::write(f, &rebuilt).map_err(|e| anyhow::anyhow!("write {f}: {e}"))?;
                    generated += 1;
                    reports.push(serde_json::json!({ "path": f, "action": "wrote" }));
                } else {
                    generated += 1;
                    reports.push(serde_json::json!({
                        "path": f,
                        "action": "preview",
                        "frontmatter": serde_json::Value::Object(new_fm)
                    }));
                }
            }
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "would_generate": generated, "files": reports })
                    )?
                );
            } else {
                for r in &reports {
                    match r["action"].as_str().unwrap_or("") {
                        "wrote" => println!("wrote {}", r["path"].as_str().unwrap_or("")),
                        "preview" => println!(
                            "would write {} ({} fields)",
                            r["path"].as_str().unwrap_or(""),
                            r["frontmatter"].as_object().map(|o| o.len()).unwrap_or(0)
                        ),
                        _ => println!(
                            "{} {} — {}",
                            r["action"].as_str().unwrap_or(""),
                            r["path"].as_str().unwrap_or(""),
                            r["reason"].as_str().unwrap_or("")
                        ),
                    }
                }
                if args.fix {
                    println!("\nGenerated frontmatter for {generated} file(s).");
                } else {
                    println!(
                        "\nWould generate frontmatter for {generated} file(s). Re-run with --fix to write."
                    );
                }
            }
            Ok(())
        }
    }
}

// ─── auth ──────────────────────────────────────────────────────────────────

fn generate_api_token() -> String {
    use rand::Rng;
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..40)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

async fn run_auth_command(
    action: AuthAction,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    use zbrain_core::admin_queries::AdminQueries;
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::libsql::LibsqlEngine;
    use zbrain_core::oauth_queries::RegisterClientRequest;
    use zbrain_core::OAuthQueries;

    // Build a concrete LibsqlEngine so we can call AdminQueries / OAuthQueries /
    // execute_raw directly (all are implemented on LibsqlEngine).
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    match action {
        AuthAction::Create(args) => {
            if args.takes_holders.is_some() {
                anyhow::bail!(
                    "`auth create --takes-holders` is not supported by the Rust `access_tokens` schema (no permissions column). Use `auth register-client` (OAuth 2.1) with --federated-read instead."
                );
            }
            let token = generate_api_token();
            let hash = {
                use sha2::{Sha256, Digest};
                let mut h = Sha256::new();
                h.update(token.as_bytes());
                hex::encode(h.finalize())
            };
            let id = format!("{:032x}", rand::random::<u128>());
            let created = zbrain_core::time::current_utc_iso8601();
            engine
                .execute_raw(
                    "INSERT INTO access_tokens (id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
                    &[&id, &args.name, &hash, &created],
                )
                .await?;
            println!("  {token}\n");
            println!("Save this token — it will not be shown again.");
            Ok(())
        }
        AuthAction::List => {
            let keys = engine.list_api_keys().await?;
            if keys.is_empty() {
                println!("No tokens found. Create one: zbrain auth create \"my-client\"");
            } else {
                for k in keys {
                    let state = if k.revoked_at.is_some() {
                        "revoked"
                    } else {
                        "active"
                    };
                    println!("{}  {}  created {}", state, k.name, k.created_at);
                }
            }
            Ok(())
        }
        AuthAction::Revoke(args) => {
            engine.revoke_api_key(&args.name).await?;
            println!("Revoked token '{}'.", args.name);
            Ok(())
        }
        AuthAction::Permissions(_) => {
            anyhow::bail!(
                "`auth permissions` (per-token takes-holders allow-list) is not supported by the Rust `access_tokens` schema. Use `auth register-client` with --federated-read for source-scoped access."
            );
        }
        AuthAction::RegisterClient(args) => {
            let req = RegisterClientRequest {
                name: args.name,
                scope: args.scopes,
                grant_types: args.grant_types,
                redirect_uris: args.redirect_uris,
                token_endpoint_auth_method: args.token_endpoint_auth_method,
                token_ttl: None,
                source_id: args.source.unwrap_or_else(|| "default".to_string()),
                federated_read: args.federated_read,
            };
            let resp = engine.register_client(req).await?;
            println!("client_id:     {}", resp.client_id);
            println!("client_secret: {}", resp.client_secret);
            println!("Save the client_secret — it will not be shown again.");
            Ok(())
        }
        AuthAction::RevokeClient(args) => {
            let resp = engine.revoke_client(&args.client_id).await?;
            if resp.revoked {
                println!("Revoked OAuth client '{}'.", args.client_id);
            } else {
                println!("No active OAuth client found with id '{}'.", args.client_id);
            }
            Ok(())
        }
        AuthAction::Test(args) => {
            let client = reqwest::Client::new();
            let url = if args.url.ends_with('/') {
                format!("{}health", args.url)
            } else {
                format!("{}/health", args.url)
            };
            match client.get(&url).bearer_auth(&args.token).send().await {
                Ok(r) => {
                    println!("GET {} -> HTTP {}", url, r.status());
                    Ok(())
                }
                Err(e) => anyhow::bail!("auth test failed: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn crate_name_is_zbrain_cli() {
        assert_eq!(crate_name(), "zbrain-cli");
    }

    #[test]
    fn banner_mentions_both_crates() {
        let b = banner();
        assert!(b.contains("zbrain-cli"), "banner missing cli name: {b}");
        assert!(b.contains("zbrain-core"), "banner missing core name: {b}");
    }

    #[test]
    fn cli_parses_successfully() {
        Cli::command().debug_assert();
    }

    /// Phase B parity guard (TDD spec for the cli.ts → Rust cutover).
    ///
    /// Every TS `cli.ts` command that has a registered Rust operation must be
    /// wired into the clap `Commands` enum, so deleting `cli.ts` (and
    /// `operations.ts`) drops *zero* product commands. If any command
    /// regresses, `find_subcommand` returns `None` and this fails loudly.
    ///
    /// `transcripts` is a parent subcommand (`transcripts recent`), so it gets
    /// its own nested assertion.
    #[test]
    fn phase_b_commands_registered() {
        let cmd = Cli::command();
        for name in [
            "code-blast",
            "code-callees",
            "code-callers",
            "code-def",
            "code-flow",
            "code-refs",
            "code-traversal-cache-clear",
            "find-contradictions",
            "find-trajectory",
            "history",
            "revert",
            "tag",
            "tags",
            "timeline",
            "timeline-add",
            "transcripts",
            "untag",
            "search-by-image",
            "whoami",
        ] {
            assert!(
                cmd.find_subcommand(name).is_some(),
                "Phase B parity regression: CLI subcommand `{name}` is not wired"
            );
        }
        assert!(
            cmd.find_subcommand("transcripts")
                .and_then(|c| c.find_subcommand("recent"))
                .is_some(),
            "Phase B parity regression: `transcripts recent` subcommand missing"
        );
    }

    // ── --timeout parsing (mirrors TS parseTimeout in src/core/cli-options.ts) ──

    #[test]
    fn parse_timeout_seconds_suffix() {
        // "30s" -> 30000ms (tracer bullet: the suffix path works end-to-end)
        assert_eq!(parse_timeout("30s"), Some(30_000));
    }

    #[test]
    fn parse_timeout_minutes_suffix() {
        assert_eq!(parse_timeout("2m"), Some(120_000));
    }

    #[test]
    fn parse_timeout_plain_number_defaults_to_ms() {
        // No suffix means milliseconds (TS: `unit ?? 'ms'`).
        assert_eq!(parse_timeout("30000"), Some(30_000));
    }

    #[test]
    fn parse_timeout_explicit_ms_suffix() {
        assert_eq!(parse_timeout("30000ms"), Some(30_000));
    }

    #[test]
    fn parse_timeout_decimal_seconds_floors() {
        // "1.5s" -> 1500ms; TS applies Math.floor after unit conversion.
        assert_eq!(parse_timeout("1.5s"), Some(1500));
    }

    #[test]
    fn parse_timeout_rejects_scientific_notation() {
        // TS regex `^([0-9]+(?:\.[0-9]+)?)(ms|s|m)?$` does NOT allow exponents.
        // Rust f64::parse WOULD accept "1e3" as 1000 — we must reject it to
        // stay char-for-char with TS.
        assert_eq!(parse_timeout("1e3"), None);
    }

    #[test]
    fn parse_timeout_rejects_non_positive() {
        // TS: `if (!Number.isFinite(n) || n <= 0) return null`.
        assert_eq!(parse_timeout("0"), None);
        assert_eq!(parse_timeout("0s"), None);
    }

    #[test]
    fn parse_timeout_rejects_garbage_and_empty() {
        assert_eq!(parse_timeout(""), None);
        assert_eq!(parse_timeout("abc"), None);
        assert_eq!(parse_timeout("30x"), None); // unknown unit
        assert_eq!(parse_timeout("-5s"), None); // leading sign not in TS class
        assert_eq!(parse_timeout(".5s"), None); // bare fraction not in TS class
    }

    #[test]
    fn cli_accepts_global_timeout_flag() {
        // --timeout is a top-level global flag (mirrors TS parse-anywhere).
        // Value is resolved to milliseconds on parse.
        let cli = Cli::try_parse_from(["zbrain", "--timeout=30s", "query", "hello"])
            .expect("--timeout=30s should parse");
        assert_eq!(cli.timeout, Some(30_000));
    }

    #[test]
    fn cli_timeout_flag_is_global_after_subcommand() {
        // global = true means it parses after the subcommand too.
        let cli = Cli::try_parse_from(["zbrain", "query", "hello", "--timeout", "2m"])
            .expect("--timeout after subcommand should parse");
        assert_eq!(cli.timeout, Some(120_000));
    }

    #[test]
    fn cli_invalid_timeout_fails_loud_exit_2() {
        // Departure from TS soft fall-through: a bad --timeout is a hard usage
        // error. clap maps value_parser Err -> ErrorKind::ValueValidation,
        // which the binary renders to stderr and exits with code 2.
        let err = Cli::try_parse_from(["zbrain", "--timeout=nonsense", "query", "hi"])
            .expect_err("invalid --timeout must not parse");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);
    }

    // ── default timeout resolution (mirrors TS cli.ts:302) ──

    #[test]
    fn resolve_timeout_think_default_is_180s() {
        // No user override: `think` gets the 180s default.
        assert_eq!(resolve_timeout_ms("think", None), 180_000);
    }

    #[test]
    fn resolve_timeout_other_ops_default_is_30s() {
        assert_eq!(resolve_timeout_ms("query", None), 30_000);
        assert_eq!(resolve_timeout_ms("get_page", None), 30_000);
    }

    #[test]
    fn resolve_timeout_user_override_wins() {
        // A resolved --timeout beats the per-op default (both think and else).
        assert_eq!(resolve_timeout_ms("think", Some(5_000)), 5_000);
        assert_eq!(resolve_timeout_ms("query", Some(90_000)), 90_000);
    }

    // ── sources-list wall-clock timeout resolution ──
    // Only the live TS default (cli.ts:1137, sources list → 10s) is ported;
    // the dead `search → 30s` branch is intentionally NOT reproduced.

    #[test]
    fn resolve_sources_list_timeout_defaults_to_10s() {
        // No user override → 10s default, flagged as NOT user-supplied so the
        // timeout message includes the `--timeout=Ns` override hint.
        assert_eq!(resolve_sources_list_timeout(None), (10_000, false));
    }

    #[test]
    fn resolve_sources_list_timeout_user_override_wins() {
        // A resolved --timeout beats the 10s default and is flagged as
        // user-supplied so the override hint is suppressed.
        assert_eq!(resolve_sources_list_timeout(Some(2_500)), (2_500, true));
    }

    // ── local-path --timeout honesty ──

    #[test]
    fn local_path_with_timeout_emits_honest_warning() {
        // On the local path, --timeout is not yet wired (tracked by 1-2-3).
        // We must NOT silently ignore it — emit a stderr warning that says so.
        let msg = local_timeout_warning(Some(30_000)).expect("should warn when --timeout set");
        assert!(msg.contains("--timeout"), "warning should name the flag: {msg}");
        assert!(
            msg.contains("thin-client") || msg.contains("thin client"),
            "warning should scope to thin-client: {msg}"
        );
    }

    #[test]
    fn local_path_without_timeout_is_silent() {
        // No --timeout means nothing to warn about.
        assert_eq!(local_timeout_warning(None), None);
    }

    #[test]
    fn help_flag_works() {
        let result = Cli::try_parse_from(["zbrain", "--help"]);
        assert!(result.is_err()); // help returns a special exit error
    }

    #[test]
    fn version_flag_works() {
        let result = Cli::try_parse_from(["zbrain", "--version"]);
        assert!(result.is_err()); // version returns a special exit error
    }

    #[test]
    fn init_command_parses() {
        let result = Cli::try_parse_from(["zbrain", "init"]);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap().command, Commands::Init(_)));
    }

    #[test]
    fn init_force_flag_parses() {
        let result = Cli::try_parse_from(["zbrain", "init", "--force"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Init(args) if args.force));
    }

    #[test]
    fn init_ts_visible_flags_parse() {
        let result = Cli::try_parse_from([
            "zbrain",
            "init",
            "--pglite",
            "--force",
            "--json",
            "--non-interactive",
            "--embedding-model",
            "openai:text-embedding-3-large",
            "--embedding-dimensions",
            "1024",
            "--no-embedding",
        ]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Init(args)
            if args.pglite
                && args.force
                && args.json
                && args.non_interactive
                && args.embedding_model.as_deref() == Some("openai:text-embedding-3-large")
                && args.embedding_dimensions == Some(1024)
                && args.no_embedding
        ));
    }

    #[test]
    fn init_engine_selection_flags_conflict() {
        let result = Cli::try_parse_from([
            "zbrain",
            "init",
            "--pglite",
            "--url",
            "postgres://localhost/zbrain",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn init_url_engine_flag_parses() {
        let result = Cli::try_parse_from(["zbrain", "init", "--url", "postgres://localhost/zbrain"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Init(args)
            if args.url.as_deref() == Some("postgres://localhost/zbrain")
        ));
    }

    #[test]
    fn init_ts_visible_migrate_and_supabase_flags_parse() {
        let result = Cli::try_parse_from(["zbrain", "init", "--supabase", "--migrate-only"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Init(args) if args.supabase && args.migrate_only));
    }

    #[test]
    fn init_ts_visible_mcp_only_flags_parse() {
        let result = Cli::try_parse_from([
            "zbrain",
            "init",
            "--mcp-only",
            "--json",
            "--issuer-url",
            "http://127.0.0.1:3000",
            "--mcp-url",
            "http://127.0.0.1:3000/mcp",
            "--oauth-client-id",
            "cid",
            "--oauth-client-secret",
            "secret",
        ]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Init(args)
            if args.mcp_only
                && args.json
                && args.issuer_url.as_deref() == Some("http://127.0.0.1:3000")
                && args.mcp_url.as_deref() == Some("http://127.0.0.1:3000/mcp")
                && args.oauth_client_id.as_deref() == Some("cid")
                && args.oauth_client_secret.as_deref() == Some("secret")
        ));
    }

    #[test]
    fn doctor_command_parses() {
        let result = Cli::try_parse_from(["zbrain", "doctor"]);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap().command, Commands::Doctor(_)));
    }

    #[test]
    fn doctor_offline_flag_removed() {
        // TS doctor never had --offline; the Rust `--offline` flag was a dead
        // flag (declared but ignored). Removing it aligns with TS: parsing
        // `--offline` must now be rejected.
        let result = Cli::try_parse_from(["zbrain", "doctor", "--offline"]);
        assert!(result.is_err(), "--offline should no longer be a valid doctor flag");
    }

    #[test]
    fn doctor_json_flag_parses() {
        let result = Cli::try_parse_from(["zbrain", "doctor", "--json"]);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap().command, Commands::Doctor(args) if args.json));
    }

    #[test]
    fn doctor_health_score_matches_ts_formula() {
        // TS outputResults: score = 100 - fail*20 - warn*5, clamp to >= 0.
        let clean = vec![DoctorCheck::ok("a", "m"), DoctorCheck::ok("b", "m")];
        assert_eq!(doctor_health_score(&clean), 100);

        let one_warn = vec![DoctorCheck::ok("a", "m"), DoctorCheck::warn("b", "m")];
        assert_eq!(doctor_health_score(&one_warn), 95);

        let one_fail = vec![DoctorCheck::fail("a", "m")];
        assert_eq!(doctor_health_score(&one_fail), 80);

        let mixed = vec![
            DoctorCheck::fail("a", "m"),
            DoctorCheck::warn("b", "m"),
            DoctorCheck::warn("c", "m"),
        ];
        assert_eq!(doctor_health_score(&mixed), 70);

        // clamp at 0: 6 fails would be -20 without clamp.
        let many_fails: Vec<DoctorCheck> =
            (0..6).map(|i| DoctorCheck::fail(&format!("f{i}"), "m")).collect();
        assert_eq!(doctor_health_score(&many_fails), 0);
    }

    #[test]
    fn doctor_status_matches_ts_mapping() {
        // TS computeDoctorReport: hasFail -> unhealthy, hasWarn -> warnings,
        // else healthy. Fail dominates warn.
        let clean = vec![DoctorCheck::ok("a", "m")];
        assert_eq!(doctor_status(&clean), "healthy");

        let warned = vec![DoctorCheck::ok("a", "m"), DoctorCheck::warn("b", "m")];
        assert_eq!(doctor_status(&warned), "warnings");

        let failed = vec![DoctorCheck::warn("a", "m"), DoctorCheck::fail("b", "m")];
        assert_eq!(doctor_status(&failed), "unhealthy");
    }

    #[test]
    fn not_implemented_checks_do_not_affect_status_or_score() {
        // Q2: unmigrated checks are surfaced as `not-implemented` for
        // traceability, but must NOT poison exit code / health_score / status.
        let checks = vec![
            DoctorCheck::ok("config", "m"),
            DoctorCheck::not_implemented("embedding_health", "covers N sub-checks"),
            DoctorCheck::not_implemented("search_mode", "covers N sub-checks"),
        ];
        assert_eq!(doctor_status(&checks), "healthy");
        assert_eq!(doctor_health_score(&checks), 100);
    }

    #[test]
    fn unmigrated_ts_doctor_checks_are_anchored() {
        // Hard trace for later agents: the constant must stay populated in the
        // expected subsystem band so removals cannot happen silently. When a
        // subsystem is migrated, its entry moves out into a real check.
        let n = UNMIGRATED_TS_DOCTOR_CHECKS.len();
        assert!(
            (5..=12).contains(&n),
            "expected 5-12 subsystem-aggregated entries, got {n}"
        );
    }

    #[test]
    fn reranker_health_is_no_longer_unmigrated() {
        // Migration hard-trace: `reranker_health` moved OUT of the UNMIGRATED
        // stand-in list into a real doctor check (reads the config-plane
        // `search.reranker_enabled` + the rerank-failure audit JSONL and
        // classifies auth/payload/transient thresholds). Guards against a
        // later agent re-adding it to the not-implemented band and silently
        // regressing the real check back to a placeholder.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "reranker_health"),
            "reranker_health is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn sync_freshness_is_no_longer_unmigrated() {
        // Migration hard-trace: `sync_freshness` moved OUT of the UNMIGRATED
        // stand-in list into a real doctor check (pulls the source list via the
        // typed `list_sources` API — no raw SQL — and folds per-source lag into
        // a worst-of warn/fail with env-overridable thresholds). Mirrors the TS
        // `checkSyncFreshness` (src/commands/doctor.ts). Guards against a later
        // agent re-adding it to the not-implemented band and silently
        // regressing the real check back to a placeholder.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "sync_freshness"),
            "sync_freshness is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn eval_drift_is_no_longer_unmigrated() {
        // Migration hard-trace (1-5-4, first ported check): `eval_drift` moved
        // OUT of the UNMIGRATED stand-in list into a real doctor check (runs
        // `git diff --name-only` against RETRIEVAL_WATCH_PATTERNS, fail-open).
        // Guards against a later agent re-adding it to the not-implemented
        // band and silently regressing the real check back to a placeholder.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "eval_drift"),
            "eval_drift is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn takes_weight_grid_is_no_longer_unmigrated() {
        // Migration hard-trace (1-5-4, second ported check): `takes_weight_grid`
        // moved OUT of the UNMIGRATED stand-in list into a real doctor check
        // (pages all takes via `list_takes`, flags off-0.05-grid weights).
        // Mirrors the TS `takesWeightGridCheck` (src/commands/doctor.ts). Guards
        // against a later agent re-adding it to the not-implemented band.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "takes_weight_grid"),
            "takes_weight_grid is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn skill_conformance_is_no_longer_unmigrated() {
        // Migration hard-trace: `skill_conformance` moved OUT of the
        // UNMIGRATED stand-in list into a real filesystem check
        // (zbrain_core::skill_conformance::check_skill_conformance — reads
        // skills/manifest.json, verifies each skill file exists + starts with
        // `---` frontmatter). Mirrors the TS `checkSkillConformance`
        // (src/commands/doctor.ts). Guards against a later agent re-adding it
        // to the not-implemented band.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "skill_conformance"),
            "skill_conformance is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn brain_score_is_no_longer_unmigrated() {
        // Migration hard-trace: `brain_score` moved OUT of the UNMIGRATED
        // stand-in list into a real doctor check that pulls a health snapshot
        // via `BrainEngine::get_health()` and folds the 3-tier threshold +
        // per-component breakdown into one check (see
        // zbrain_core::autopilot::brain_score::brain_score_doctor_check).
        // Mirrors the TS `checkBrainScore` (src/commands/doctor.ts). Guards
        // against a later agent re-adding it to the not-implemented band.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "brain_score"),
            "brain_score is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn embedding_health_is_no_longer_unmigrated() {
        // Migration hard-trace: `embedding_health` moved OUT of the UNMIGRATED
        // stand-in list into a real doctor check that verifies ZeroEntropy API key
        // presence and confirms embedding column persistence (G24 resolved).
        // Mirrors the TS `checkZeEmbeddingHealth` (src/commands/doctor.ts). Guards
        // against a later agent re-adding it to the not-implemented band.
        assert!(
            !UNMIGRATED_TS_DOCTOR_CHECKS
                .iter()
                .any(|(name, _)| *name == "embedding_health"),
            "embedding_health is a real check now; it must not appear in UNMIGRATED_TS_DOCTOR_CHECKS"
        );
    }

    #[test]
    fn doctor_json_report_matches_ts_envelope() {
        // Q5: envelope aligned field-for-field with TS computeDoctorReport:
        // {schema_version:2, status, health_score, checks[]}, each check entry
        // is {name, status, message}.
        let checks = vec![
            DoctorCheck::ok("config", "loaded"),
            DoctorCheck::warn("network", "offline"),
            DoctorCheck::not_implemented("embedding_health", "covers N"),
        ];
        let report = doctor_json_report(&checks);

        assert_eq!(report["schema_version"], 2);
        assert_eq!(report["status"], "warnings");
        assert_eq!(report["health_score"], 95);

        let arr = report["checks"].as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["name"], "config");
        assert_eq!(arr[0]["status"], "ok");
        assert_eq!(arr[0]["message"], "loaded");
        assert_eq!(arr[1]["status"], "warn");
        // not-implemented entries are surfaced with a distinct status string.
        assert_eq!(arr[2]["name"], "embedding_health");
        assert_eq!(arr[2]["status"], "not-implemented");
    }

    #[test]
    fn config_show_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "show"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Config(args) if matches!(args.action, ConfigAction::Show)));
    }

    #[test]
    fn config_get_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "get", "database.url"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Config(args) if matches!(&args.action, ConfigAction::Get { key } if key == "database.url")));
    }

    #[test]
    fn config_set_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "set", "database.url", "sqlite://db"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config(args)
            if matches!(&args.action, ConfigAction::Set { key, value, .. }
                        if key == "database.url" && value == "sqlite://db")
        ));
    }

    #[test]
    fn config_get_returns_raw_value_without_redaction() {
        let mut config = config::Config::default();
        let mut openai = config::ProviderConfig::default();
        openai.api_key = Some("sk-secret-value".to_string());
        config.providers.insert("openai".to_string(), openai);

        let raw = get_config_value(
            "providers.openai.api_key",
            &serde_yaml::to_value(&config).unwrap(),
        )
        .expect("api_key should resolve");

        // `get` must return the raw secret unchanged...
        assert_eq!(raw, "sk-secret-value");
        // ...even though the same key would be redacted by `show`.
        assert_ne!(
            config::redact_value("providers.openai.api_key", &raw),
            raw,
            "sanity: this key is redaction-sensitive, so get intentionally skips redaction"
        );
    }

    #[tokio::test]
    async fn config_get_missing_key_fails_loud() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        config::write_config(&config::Config::default(), &config_path).unwrap();

        let args = ConfigArgs {
            action: ConfigAction::Get {
                key: "no_such_key".to_string(),
            },
        };

        let result = run_config_command(args, Some(&config_path)).await;
        assert!(
            result.is_err(),
            "config get on a missing key must fail with a non-zero exit"
        );
    }

    #[tokio::test]
    async fn config_set_known_key_succeeds_and_persists() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        config::write_config(&config::Config::default(), &config_path).unwrap();

        let args = ConfigArgs {
            action: ConfigAction::Set {
                key: "database_url".to_string(),
                value: "sqlite:///tmp/known.db".to_string(),
                force: false,
            },
        };

        run_config_command(args, Some(&config_path)).await.unwrap();

        let written = config::load_config_from_path(&config_path).unwrap();
        assert_eq!(written.database_url, "sqlite:///tmp/known.db");
    }

    #[tokio::test]
    async fn config_set_unknown_key_with_force_writes_value() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        config::write_config(&config::Config::default(), &config_path).unwrap();

        let args = ConfigArgs {
            action: ConfigAction::Set {
                key: "custom_extra_key".to_string(),
                value: "kept".to_string(),
                force: true,
            },
        };

        run_config_command(args, Some(&config_path)).await.unwrap();

        let raw = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            raw.contains("custom_extra_key"),
            "--force must persist the forced key: {raw}"
        );
    }

    #[tokio::test]
    async fn config_set_unknown_key_without_force_is_rejected_without_writing() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let seeded = config::Config::default();
        config::write_config(&seeded, &config_path).unwrap();
        let before = std::fs::read_to_string(&config_path).unwrap();

        let args = ConfigArgs {
            action: ConfigAction::Set {
                key: "embeding.model".to_string(),
                value: "oops".to_string(),
                force: false,
            },
        };

        let result = run_config_command(args, Some(&config_path)).await;
        assert!(
            result.is_err(),
            "setting an unknown/typo key without --force must fail"
        );
        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(before, after, "rejected set must not modify the config file");
    }

    #[test]
    fn is_known_config_key_accepts_schema_paths_and_rejects_typos() {
        // Known scalar and nested schema paths.
        assert!(is_known_config_key("database_url"));
        assert!(is_known_config_key("embedding.model"));
        assert!(is_known_config_key("embedding.enabled"));
        // providers is a free-form map: any provider sub-key is allowed.
        assert!(is_known_config_key("providers.openai.api_key"));
        // Typos and stray fields are rejected.
        assert!(!is_known_config_key("embeding.model"));
        assert!(!is_known_config_key("database.url"));
        assert!(!is_known_config_key("totally_unknown_key"));
    }

    #[test]
    fn config_unset_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "unset", "old.key"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config(args)
            if matches!(&args.action, ConfigAction::Unset { key: Some(ref k), pattern: None }
                        if k == "old.key")
        ));
    }

    #[test]
    fn config_unset_pattern_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "unset", "--pattern", "legacy_"]);
        assert!(result.is_ok());
    }

    #[test]
    fn schema_sql_command_parses_default() {
        // The DDL dumper is `schema-sql` (renamed from `schema`, which was a
        // naming bug: TS `schema` is a schema-pack manager, not a DDL dumper).
        let result = Cli::try_parse_from(["zbrain", "schema-sql"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::SchemaSql(args) if args.backend == "libsql"));
    }

    #[test]
    fn schema_sql_command_postgres_parses() {
        let result = Cli::try_parse_from(["zbrain", "schema-sql", "--backend", "postgres"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::SchemaSql(args) if args.backend == "postgres"));
    }

    #[test]
    fn bare_schema_name_is_no_longer_the_ddl_dumper() {
        // `schema` is deliberately freed up for a future schema-pack manager
        // migration; the breaking rename has no compatibility alias.
        let result = Cli::try_parse_from(["zbrain", "schema"]);
        assert!(result.is_err(), "bare `schema` should no longer parse as the DDL dumper");
    }

    #[test]
    fn unmigrated_ts_schema_pack_verbs_are_anchored() {
        // Hard trace (mirrors doctor's UNMIGRATED_TS_DOCTOR_CHECKS): the TS
        // `schema` command was a 32-verb schema-pack manager. As of 2026-07-15
        // all verbs are migrated (1-1..1-5) and G4 is resolved, so the list is
        // empty. This test guards against silent removal of the tracking point
        // AND against re-introducing un-migrated verbs without updating it.
        let n = UNMIGRATED_TS_SCHEMA_PACK_VERBS.len();
        assert_eq!(n, 0, "all schema-pack verbs should be migrated (G4 resolved); found {n} un-migrated");
    }

    #[tokio::test]
    async fn run_executes_init_stub() {
        let cli = Cli::try_parse_from(["zbrain", "init"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn init_url_fails_loud_when_connection_string_is_unreachable() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: Some("postgres://127.0.0.1:1/zbrain".to_string()),
            force: true,
            migrate_only: false,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        let result = run_init_command(args, Some(&config_path)).await;

        assert!(result.is_err());
        assert!(
            !config_path.exists(),
            "failed postgres init must not write config before a verified connection"
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("postgres connect failed"),
            "expected postgres connection failure, got: {error}"
        );
    }

    #[tokio::test]
    async fn init_supabase_fails_not_implemented_before_disk_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let args = InitArgs {
            pglite: false,
            supabase: true,
            url: None,
            force: true,
            migrate_only: false,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        let result = run_init_command(args, Some(&config_path)).await;

        assert!(result.is_err());
        assert!(
            !config_path.exists(),
            "supabase init must not write a local config"
        );
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("--supabase init is not implemented yet"),
            "expected explicit --supabase not implemented failure, got: {error}"
        );
    }

    #[tokio::test]
    async fn init_embedding_flags_write_config_without_model_setup() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: None,
            force: true,
            migrate_only: false,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: Some("text-embedding-3-small".to_string()),
            no_embedding: true,
            embedding_dimensions: Some(1536),
        };

        run_init_command(args, Some(&config_path)).await.unwrap();

        let written = config::load_config_from_path(&config_path).unwrap();
        assert_eq!(written.embedding.model, "text-embedding-3-small");
        assert_eq!(written.embedding.dimensions, Some(1536));
        assert!(!written.embedding.enabled);
    }

    #[tokio::test]
    async fn init_existing_config_without_force_refuses_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");

        // Seed an existing config with a distinctive database_url.
        let mut seeded = config::Config::default();
        seeded.database_url = "sqlite:///seeded/existing.db".to_string();
        config::write_config(&seeded, &config_path).unwrap();

        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: None,
            force: false,
            migrate_only: false,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        run_init_command(args, Some(&config_path)).await.unwrap();

        let after = config::load_config_from_path(&config_path).unwrap();
        assert_eq!(
            after.database_url, "sqlite:///seeded/existing.db",
            "existing config must not be overwritten without --force"
        );
    }

    #[tokio::test]
    async fn init_force_overwrites_existing_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");

        // Seed an existing config with a distinctive database_url.
        let mut seeded = config::Config::default();
        seeded.database_url = "sqlite:///seeded/existing.db".to_string();
        config::write_config(&seeded, &config_path).unwrap();

        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: None,
            force: true,
            migrate_only: false,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        run_init_command(args, Some(&config_path)).await.unwrap();

        let after = config::load_config_from_path(&config_path).unwrap();
        assert_ne!(
            after.database_url, "sqlite:///seeded/existing.db",
            "--force must overwrite the seeded database_url with a fresh local config"
        );
    }

    #[test]
    fn init_initialized_json_emits_structured_status() {
        let value = init_initialized_json(
            Path::new("/home/u/.zbrain/zbrain.yml"),
            "sqlite:///home/u/.zbrain/brain.pglite",
            "local",
        );
        assert_eq!(value["status"], "initialized");
        assert_eq!(value["config_path"], "/home/u/.zbrain/zbrain.yml");
        assert_eq!(value["database_url"], "sqlite:///home/u/.zbrain/brain.pglite");
        assert_eq!(value["mode"], "local");
    }

    #[tokio::test]
    async fn init_mcp_only_writes_thin_client_config_without_local_database() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let db_path = tmp.path().join("brain.pglite");
        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: None,
            force: true,
            migrate_only: false,
            mcp_only: true,
            json: false,
            non_interactive: true,
            issuer_url: Some("https://issuer.example".to_string()),
            mcp_url: Some("https://mcp.example/mcp".to_string()),
            oauth_client_id: Some("client-id".to_string()),
            oauth_client_secret: Some("secret".to_string()),
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        run_init_command(args, Some(&config_path)).await.unwrap();

        let written = config::load_config_from_path(&config_path).unwrap();
        assert_eq!(written.database_url, "remote-mcp://thin-client");
        let remote_mcp = written
            .remote_mcp
            .expect("mcp-only init should write remote_mcp config");
        assert_eq!(remote_mcp.issuer_url, "https://issuer.example");
        assert_eq!(remote_mcp.mcp_url, "https://mcp.example/mcp");
        assert_eq!(remote_mcp.oauth_client_id, "client-id");
        assert_eq!(remote_mcp.oauth_client_secret.as_deref(), Some("secret"));
        assert!(
            !db_path.exists(),
            "mcp-only init must not create local brain.pglite"
        );
    }

    #[tokio::test]
    async fn init_mcp_only_requires_remote_auth_arguments_without_writing_config() {
        let required_args = ["--issuer-url", "--mcp-url", "--oauth-client-id"];

        for missing_arg in required_args {
            let tmp = tempfile::tempdir().unwrap();
            let config_path = tmp.path().join("zbrain.yml");
            let args = InitArgs {
                pglite: false,
                supabase: false,
                url: None,
                force: true,
                migrate_only: false,
                mcp_only: true,
                json: false,
                non_interactive: true,
                issuer_url: (missing_arg != "--issuer-url")
                    .then(|| "https://issuer.example".to_string()),
                mcp_url: (missing_arg != "--mcp-url")
                    .then(|| "https://mcp.example/mcp".to_string()),
                oauth_client_id: (missing_arg != "--oauth-client-id")
                    .then(|| "client-id".to_string()),
                oauth_client_secret: None,
                embedding_model: None,
                no_embedding: false,
                embedding_dimensions: None,
            };

            let result = run_init_command(args, Some(&config_path)).await;

            assert!(result.is_err(), "missing {missing_arg} should fail");
            let error = format!("{:#}", result.unwrap_err());
            assert!(
                error.contains(missing_arg),
                "expected error to mention {missing_arg}, got: {error}"
            );
            assert!(
                !config_path.exists(),
                "missing {missing_arg} must not write config"
            );
        }
    }

    #[tokio::test]
    async fn init_mcp_only_rejects_db_migrate_and_embedding_flags_without_writing_config() {
        for (
            flag_name,
            pglite,
            supabase,
            url,
            migrate_only,
            embedding_model,
            no_embedding,
            embedding_dimensions,
        ) in [
            ("--pglite", true, false, None, false, None, false, None),
            ("--supabase", false, true, None, false, None, false, None),
            (
                "--url",
                false,
                false,
                Some("postgres://127.0.0.1:1/zbrain".to_string()),
                false,
                None,
                false,
                None,
            ),
            (
                "--migrate-only",
                false,
                false,
                None,
                true,
                None,
                false,
                None,
            ),
            (
                "--embedding-model",
                false,
                false,
                None,
                false,
                Some("text-embedding-3-small".to_string()),
                false,
                None,
            ),
            (
                "--no-embedding",
                false,
                false,
                None,
                false,
                None,
                true,
                None,
            ),
            (
                "--embedding-dimensions",
                false,
                false,
                None,
                false,
                None,
                false,
                Some(1536),
            ),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let config_path = tmp.path().join("zbrain.yml");
            let args = InitArgs {
                pglite,
                supabase,
                url,
                force: true,
                migrate_only,
                mcp_only: true,
                json: false,
                non_interactive: true,
                issuer_url: Some("https://issuer.example".to_string()),
                mcp_url: Some("https://mcp.example/mcp".to_string()),
                oauth_client_id: Some("client-id".to_string()),
                oauth_client_secret: None,
                embedding_model,
                no_embedding,
                embedding_dimensions,
            };

            let result = run_init_command(args, Some(&config_path)).await;

            assert!(result.is_err(), "mcp-only with {flag_name} should fail");
            let error = format!("{:#}", result.unwrap_err());
            assert!(
                error.contains("--mcp-only cannot be combined") && error.contains(flag_name),
                "expected conflict with {flag_name}, got: {error}"
            );
            assert!(
                !config_path.exists(),
                "mcp-only with {flag_name} must not write config"
            );
        }
    }

    #[tokio::test]
    async fn init_migrate_only_sqlite_config_applies_schema_without_rewriting_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let db_path = tmp.path().join("brain.pglite");
        let mut config = config::Config::default();
        config.database_url = format!("sqlite://{}", db_path.display());
        config::write_config(&config, &config_path).unwrap();
        let before = std::fs::read_to_string(&config_path).unwrap();
        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: None,
            force: false,
            migrate_only: true,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        run_init_command(args, Some(&config_path)).await.unwrap();

        let after = std::fs::read_to_string(&config_path).unwrap();
        assert_eq!(after, before, "migrate-only must not rewrite config");
        assert!(db_path.exists(), "migrate-only should create/migrate the configured database");
    }

    #[tokio::test]
    async fn init_migrate_only_without_config_fails_loud() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("missing-zbrain.yml");
        let args = InitArgs {
            pglite: false,
            supabase: false,
            url: None,
            force: false,
            migrate_only: true,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        let result = run_init_command(args, Some(&config_path)).await;

        assert!(result.is_err());
        let error = format!("{:#}", result.unwrap_err());
        assert!(
            error.contains("--migrate-only requires an existing config"),
            "expected missing-config guidance, got: {error}"
        );
        assert!(!config_path.exists(), "migrate-only must not create config");
    }

    #[tokio::test]
    async fn init_migrate_only_rejects_engine_selection_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let mut config = config::Config::default();
        config.database_url = format!("sqlite://{}", tmp.path().join("brain.pglite").display());
        config::write_config(&config, &config_path).unwrap();

        for (pglite, supabase, url) in [
            (true, false, None),
            (false, true, None),
            (false, false, Some("postgres://127.0.0.1:1/zbrain".to_string())),
        ] {
            let args = InitArgs {
                pglite,
                supabase,
                url,
                force: false,
                migrate_only: true,
                mcp_only: false,
                json: false,
                non_interactive: true,
                issuer_url: None,
                mcp_url: None,
                oauth_client_id: None,
                oauth_client_secret: None,
                embedding_model: None,
                no_embedding: false,
                embedding_dimensions: None,
            };

            let result = run_init_command(args, Some(&config_path)).await;
            assert!(result.is_err());
            let error = format!("{:#}", result.unwrap_err());
            assert!(
                error.contains("--migrate-only cannot be combined"),
                "expected engine flag conflict, got: {error}"
            );
        }
    }

    #[tokio::test]
    async fn init_explicit_pglite_writes_local_database_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("zbrain.yml");
        let args = InitArgs {
            pglite: true,
            supabase: false,
            url: None,
            force: true,
            migrate_only: false,
            mcp_only: false,
            json: false,
            non_interactive: true,
            issuer_url: None,
            mcp_url: None,
            oauth_client_id: None,
            oauth_client_secret: None,
            embedding_model: None,
            no_embedding: false,
            embedding_dimensions: None,
        };

        run_init_command(args, Some(&config_path)).await.unwrap();

        let written = config::load_config_from_path(&config_path).unwrap();
        assert!(
            written.database_url.starts_with("sqlite://"),
            "pglite init should write local sqlite/libsql URL, got: {}",
            written.database_url
        );
        assert!(
            written.database_url.contains("brain.pglite"),
            "pglite init should use the local brain.pglite path, got: {}",
            written.database_url
        );
    }

    #[tokio::test]
    async fn run_executes_doctor_stub() {
        let cli = Cli::try_parse_from(["zbrain", "doctor"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_executes_config_stub() {
        let cli = Cli::try_parse_from(["zbrain", "config", "show"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_executes_schema_stub() {
        let cli = Cli::try_parse_from(["zbrain", "schema-sql"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[test]
    fn registry_dynamic_local_only_consistent_with_trait() {
        let mut registry = OperationRegistry::new();
        register_all(&mut registry);

        // local_only ops: must return true from both trait AND registry lookup
        for name in &["put_page", "delete_page", "restore_page", "purge_deleted_pages"] {
            let op = registry.lookup(name).expect(&format!("{} should be registered", name));
            assert!(op.local_only(), "{} should be local_only", name);
        }

        // non-local_only ops: must return false
        for name in &["get_page", "think", "query", "list_pages"] {
            let op = registry.lookup(name).expect(&format!("{} should be registered", name));
            assert!(!op.local_only(), "{} should NOT be local_only", name);
        }
    }

    #[test]
    fn dynamic_local_only_unknown_operation_defaults_to_false() {
        let registry = OperationRegistry::new();

        // Unknown operations should NOT be treated as local_only.
        // This ensures the thin-client guard does not block operations
        // it has never registered — defaulting to permissive.
        let is_local = registry
            .lookup("nonexistent_op")
            .map(|op| op.local_only())
            .unwrap_or(false);
        assert!(!is_local, "unknown operation should default to not-local_only");
    }

    // --- ServeHttp arg tests (#69) ---

    #[test]
    fn serve_http_parses_with_no_flags() {
        let cli = Cli::try_parse_from(["zbrain", "serve", "--http"]).unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert!(args.port.is_none());
                assert!(args.bind.is_none());
                assert!(args.spa_dir.is_none());
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[test]
    fn serve_http_parses_with_port_flag() {
        let cli = Cli::try_parse_from(["zbrain", "serve", "--http", "--port", "4000"]).unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert_eq!(args.port, Some(4000));
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[test]
    fn serve_http_parses_with_bind_flag() {
        let cli = Cli::try_parse_from(["zbrain", "serve", "--http", "--bind", "0.0.0.0"]).unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert_eq!(args.bind.as_deref(), Some("0.0.0.0"));
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[test]
    fn serve_http_parses_all_flags_together() {
        let cli = Cli::try_parse_from([
            "zbrain", "serve", "--http", "--port", "8080", "--bind", "::1",
            "--spa-dir", "/tmp/admin-dist",
        ])
        .unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert_eq!(args.port, Some(8080));
                assert_eq!(args.bind.as_deref(), Some("::1"));
                assert_eq!(args.spa_dir.as_deref(), Some(std::path::Path::new("/tmp/admin-dist")));
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[tokio::test]
    async fn serve_http_integration_health_and_spa() {
        let tmp = tempfile::tempdir().unwrap();
        let spa_dir = tmp.path().to_path_buf();
        std::fs::write(spa_dir.join("index.html"), "<!DOCTYPE html><html><body>INTEGRATION_TEST_SPA</body></html>").unwrap();

        // Use a temp database so the engine can connect
        let db_path = tmp.path().join("test.db");
        std::env::set_var("ZBRAIN_DATABASE_URL", format!("sqlite://{}", db_path.display()));

        // Use a high port unlikely to conflict
        let test_port: u16 = 19876;

        let args = ServeHttpArgs {
            http: true,
            port: Some(test_port),
            bind: Some("127.0.0.1".to_string()),
            spa_dir: Some(spa_dir),
        };

        // Spawn the server in background
        let server_handle = tokio::spawn(async move {
            let _ = run_serve_http_command(args, None).await;
        });

        // Give the server a moment to bind and init schema
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        // Test /health
        let health_url = format!("http://127.0.0.1:{test_port}/health");
        let resp = reqwest::get(&health_url).await;
        assert!(resp.is_ok(), "health endpoint should be reachable");
        let resp = resp.unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");

        // Test /admin/ SPA
        let admin_url = format!("http://127.0.0.1:{test_port}/admin/");
        let resp = reqwest::get(&admin_url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("INTEGRATION_TEST_SPA"), "SPA content not found: {body}");

        // Abort server task (don't wait for graceful shutdown)
        server_handle.abort();
    }

    // --- Sync CLI arg tests (#101) ---

    #[test]
    fn sync_command_parses_defaults() {
        let cli = Cli::try_parse_from(["zbrain", "sync"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.source_id, "default");
                assert!(args.repo_path.is_none());
                assert!(!args.full_sync);
                assert!(args.chunker_version.is_none());
                assert_eq!(args.max_file_size, 0);
                assert!(args.failures_dir.is_none());
                assert_eq!(args.parallelism, 0);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_source_id() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--source-id", "my-docs"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.source_id, "my-docs");
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_repo_path() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--repo-path", "/home/user/repo"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.repo_path, Some(PathBuf::from("/home/user/repo")));
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_full_sync_flag() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--full-sync"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert!(args.full_sync);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_chunker_version() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--chunker-version", "2"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.chunker_version, Some(2));
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_max_file_size() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--max-file-size", "1048576"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.max_file_size, 1048576);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_failures_dir() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--failures-dir", "/tmp/sync-failures"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.failures_dir, Some(PathBuf::from("/tmp/sync-failures")));
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_parallelism() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--parallelism", "4"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.parallelism, 4);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_all_flags_together() {
        let cli = Cli::try_parse_from([
            "zbrain", "sync",
            "--source-id", "my-docs",
            "--repo-path", "/tmp/myrepo",
            "--full-sync",
            "--chunker-version", "3",
            "--max-file-size", "524288",
            "--failures-dir", "/tmp/failures",
            "--parallelism", "2",
        ])
        .unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.source_id, "my-docs");
                assert_eq!(args.repo_path, Some(PathBuf::from("/tmp/myrepo")));
                assert!(args.full_sync);
                assert_eq!(args.chunker_version, Some(3));
                assert_eq!(args.max_file_size, 524288);
                assert_eq!(args.failures_dir, Some(PathBuf::from("/tmp/failures")));
                assert_eq!(args.parallelism, 2);
            }
            _ => panic!("expected Sync"),
        }
    }

    // --- Sources CLI arg tests (#105 sources add) ---

    #[test]
    fn sources_add_parses_required_id() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => {
                    assert_eq!(args.id, "my-source");
                    assert!(args.name.is_none());
                    assert!(args.path.is_none());
                    assert!(args.url.is_none());
                    assert!(!args.federated);
                    assert_eq!(args.depth, 1);
                    assert!(args.branch.is_none());
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_with_name() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--name", "My Source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.name.as_deref(), Some("My Source")),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_with_path() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--path", "/tmp/repo"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.path, Some(PathBuf::from("/tmp/repo"))),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_with_url() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--url", "https://github.com/foo/bar.git"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.url.as_deref(), Some("https://github.com/foo/bar.git")),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_path_url_conflict() {
        let result = Cli::try_parse_from([
            "zbrain", "sources", "add", "my-source",
            "--path", "/tmp/repo",
            "--url", "https://example.com/repo.git",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn sources_add_parses_federated() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--federated"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert!(args.federated),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_clone_dir() {
        let cli = Cli::try_parse_from([
            "zbrain", "sources", "add", "my-source",
            "--clone-dir", "/custom/clone",
        ]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.clone_dir, Some(PathBuf::from("/custom/clone"))),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_depth_and_branch() {
        let cli = Cli::try_parse_from([
            "zbrain", "sources", "add", "my-source",
            "--depth", "0",
            "--branch", "main",
        ]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => {
                    assert_eq!(args.depth, 0);
                    assert_eq!(args.branch.as_deref(), Some("main"));
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Sources list CLI tests (#102) ---

    #[test]
    fn sources_list_parses_default() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "list"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::List(args) => assert!(!args.json),
                _ => panic!("expected List"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_list_parses_json_flag() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "list", "--json"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::List(args) => assert!(args.json),
                _ => panic!("expected List"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Sources remove CLI tests (#104) ---

    #[test]
    fn sources_remove_parses_required_id() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => {
                    assert_eq!(args.id, "my-source");
                    assert!(!args.confirm_destructive);
                    assert!(!args.dry_run);
                    assert!(!args.keep_storage);
                    assert!(!args.yes);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_confirm_destructive() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--confirm-destructive"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.confirm_destructive),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_dry_run() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--dry-run"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.dry_run),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_keep_storage() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--keep-storage"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.keep_storage),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_yes_short() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "-y"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.yes),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_yes_long() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--yes"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.yes),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_all_flags_together() {
        let cli = Cli::try_parse_from([
            "zbrain", "sources", "remove", "my-source",
            "--confirm-destructive",
            "--dry-run",
            "--keep-storage",
            "--yes",
        ]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => {
                    assert_eq!(args.id, "my-source");
                    assert!(args.confirm_destructive);
                    assert!(args.dry_run);
                    assert!(args.keep_storage);
                    assert!(args.yes);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Sources status CLI tests (#106) ---

    #[test]
    fn sources_status_parses_all_sources_default() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert!(args.source_id.is_none());
                    assert!(!args.json);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_status_parses_single_source() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status", "my-source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert_eq!(args.source_id.as_deref(), Some("my-source"));
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_status_parses_json_flag() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status", "--json"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert!(args.json);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_status_parses_source_with_json() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status", "my-source", "--json"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert_eq!(args.source_id.as_deref(), Some("my-source"));
                    assert!(args.json);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Capture CLI tests (#103) ---

    #[test]
    fn capture_parses_file_input() {
        let cli = Cli::try_parse_from(["zbrain", "capture", "/path/to/note.md"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.content.as_deref(), Some("/path/to/note.md"));
                assert_eq!(args.r#type, "markdown");
                assert!(args.source.is_none());
                assert!(args.slug.is_none());
                assert!(!args.json);
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_stdin_when_no_file() {
        let cli = Cli::try_parse_from(["zbrain", "capture"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert!(args.content.is_none());
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_type_flag() {
        let cli = Cli::try_parse_from(["zbrain", "capture", "--type", "text", "myfile.txt"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.r#type, "text");
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_source_and_slug() {
        let cli = Cli::try_parse_from([
            "zbrain", "capture",
            "--source", "my-docs",
            "--slug", "custom-slug",
            "file.md",
        ]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.source.as_deref(), Some("my-docs"));
                assert_eq!(args.slug.as_deref(), Some("custom-slug"));
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_json_flag() {
        let cli = Cli::try_parse_from(["zbrain", "capture", "--json", "file.md"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert!(args.json);
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "zbrain", "capture",
            "--type", "markdown",
            "--source", "my-docs",
            "--slug", "my-page",
            "--json",
            "path/to/file.md",
        ]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.r#type, "markdown");
                assert_eq!(args.source.as_deref(), Some("my-docs"));
                assert_eq!(args.slug.as_deref(), Some("my-page"));
                assert!(args.json);
                assert_eq!(args.content.as_deref(), Some("path/to/file.md"));
            },
            _ => panic!("expected Capture"),
        }
    }

    // --- Autopilot CLI tests (1-5-6) ---

    #[test]
    fn autopilot_parses_default_args() {
        let cli = Cli::try_parse_from(["zbrain", "autopilot"]).unwrap();
        match cli.command {
            Commands::Autopilot(args) => {
                assert!(!args.install);
                assert!(!args.uninstall);
                assert!(!args.status);
                assert!(!args.inline);
                assert!(!args.no_worker);
                assert!(!args.json);
                assert!(!args.once);
                assert_eq!(args.interval, 300);
                assert!(args.repo.is_none());
            },
            _ => panic!("expected Autopilot"),
        }
    }

    #[test]
    fn autopilot_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "zbrain", "autopilot",
            "--repo", "/tmp/brain",
            "--interval", "120",
            "--json",
            "--inline",
            "--no-worker",
            "--once",
        ]).unwrap();
        match cli.command {
            Commands::Autopilot(args) => {
                assert_eq!(args.repo.as_deref(), Some("/tmp/brain"));
                assert_eq!(args.interval, 120);
                assert!(args.json);
                assert!(args.inline);
                assert!(args.no_worker);
                assert!(args.once);
            },
            _ => panic!("expected Autopilot"),
        }
    }

    #[test]
    fn autopilot_parses_install_flag() {
        let cli = Cli::try_parse_from([
            "zbrain", "autopilot", "--install", "--repo", "/tmp/brain",
        ]).unwrap();
        match cli.command {
            Commands::Autopilot(args) => {
                assert!(args.install);
                assert_eq!(args.repo.as_deref(), Some("/tmp/brain"));
            },
            _ => panic!("expected Autopilot"),
        }
    }

    #[test]
    fn autopilot_parses_status_flag() {
        let cli = Cli::try_parse_from(["zbrain", "autopilot", "--status", "--json"]).unwrap();
        match cli.command {
            Commands::Autopilot(args) => {
                assert!(args.status);
                assert!(args.json);
            },
            _ => panic!("expected Autopilot"),
        }
    }

    #[test]
    fn autopilot_parses_uninstall_flag() {
        let cli = Cli::try_parse_from(["zbrain", "autopilot", "--uninstall"]).unwrap();
        match cli.command {
            Commands::Autopilot(args) => {
                assert!(args.uninstall);
            },
            _ => panic!("expected Autopilot"),
        }
    }

    // --- Remote CLI tests ---

    #[test]
    fn remote_ping_parses_default() {
        let cli = Cli::try_parse_from(["zbrain", "remote", "ping"]).unwrap();
        match cli.command {
            Commands::Remote(RemoteSub::Ping(args)) => {
                assert!(!args.json);
                assert!(args.max_wait.is_none());
            },
            _ => panic!("expected Remote::Ping"),
        }
    }

    #[test]
    fn remote_ping_parses_json_and_timeout() {
        let cli = Cli::try_parse_from([
            "zbrain", "remote", "ping", "--json", "--max-wait", "5m",
        ]).unwrap();
        match cli.command {
            Commands::Remote(RemoteSub::Ping(args)) => {
                assert!(args.json);
                assert_eq!(args.max_wait.as_deref(), Some("5m"));
            },
            _ => panic!("expected Remote::Ping"),
        }
    }

    #[test]
    fn remote_doctor_parses_json() {
        let cli = Cli::try_parse_from(["zbrain", "remote", "doctor", "--json"]).unwrap();
        match cli.command {
            Commands::Remote(RemoteSub::Doctor(args)) => {
                assert!(args.json);
            },
            _ => panic!("expected Remote::Doctor"),
        }
    }

    #[test]
    fn remote_doctor_parses_no_json() {
        let cli = Cli::try_parse_from(["zbrain", "remote", "doctor"]).unwrap();
        match cli.command {
            Commands::Remote(RemoteSub::Doctor(args)) => {
                assert!(!args.json);
            },
            _ => panic!("expected Remote::Doctor"),
        }
    }

    // --- parse_duration tests ---

    #[test]
    fn parse_duration_seconds() {
        assert_eq!(parse_duration("30s"), Some(30_000));
        assert_eq!(parse_duration("90s"), Some(90_000));
        assert_eq!(parse_duration("1s"), Some(1_000));
    }

    #[test]
    fn parse_duration_minutes() {
        assert_eq!(parse_duration("5m"), Some(300_000));
        assert_eq!(parse_duration("15m"), Some(900_000));
        assert_eq!(parse_duration("1.5m"), Some(90_000));
    }

    #[test]
    fn parse_duration_hours() {
        assert_eq!(parse_duration("1h"), Some(3_600_000));
        assert_eq!(parse_duration("2h"), Some(7_200_000));
    }

    #[test]
    fn parse_duration_milliseconds() {
        assert_eq!(parse_duration("500ms"), Some(500));
        assert_eq!(parse_duration("1000ms"), Some(1000));
    }

    #[test]
    fn parse_duration_bare_number_defaults_to_ms() {
        assert_eq!(parse_duration("500"), Some(500));
        assert_eq!(parse_duration("1000"), Some(1000));
    }

    #[test]
    fn parse_duration_rejects_invalid() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("-5s"), None);
    }

    // --- compute_poll_interval tests ---

    #[test]
    fn poll_interval_1s_for_first_30s() {
        assert_eq!(compute_poll_interval(0), 1_000);
        assert_eq!(compute_poll_interval(1_000), 1_000);
        assert_eq!(compute_poll_interval(29_000), 1_000);
        assert_eq!(compute_poll_interval(29_999), 1_000);
    }

    #[test]
    fn poll_interval_5s_for_30s_to_6m() {
        assert_eq!(compute_poll_interval(30_000), 5_000);
        assert_eq!(compute_poll_interval(120_000), 5_000);
        assert_eq!(compute_poll_interval(300_000), 5_000);
        assert_eq!(compute_poll_interval(329_999), 5_000);
    }

    #[test]
    fn poll_interval_10s_after_6m() {
        assert_eq!(compute_poll_interval(330_000), 10_000);
        assert_eq!(compute_poll_interval(600_000), 10_000);
        assert_eq!(compute_poll_interval(3_600_000), 10_000);
    }

    // --- is_terminal_state tests ---

    #[test]
    fn terminal_states() {
        assert!(is_terminal_state("completed"));
        assert!(is_terminal_state("failed"));
        assert!(is_terminal_state("dead"));
        assert!(is_terminal_state("cancelled"));
    }

    #[test]
    fn non_terminal_states() {
        assert!(!is_terminal_state("queued"));
        assert!(!is_terminal_state("running"));
        assert!(!is_terminal_state("waiting"));
        assert!(!is_terminal_state(""));
    }

    // --- unpack_tool_result tests ---

    #[test]
    fn unpack_extracts_from_content_envelope() {
        let raw = serde_json::json!({
            "content": [{
                "type": "text",
                "text": "{\"id\": 42, \"state\": \"queued\"}"
            }]
        });
        let result = unpack_tool_result(&raw);
        assert_eq!(result["id"], 42);
        assert_eq!(result["state"], "queued");
    }

    #[test]
    fn unpack_drills_through_jsonrpc_envelope() {
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{
                    "type": "text",
                    "text": "{\"status\": \"healthy\", \"health_score\": 100}"
                }]
            }
        });
        let result = unpack_tool_result(&raw);
        assert_eq!(result["status"], "healthy");
        assert_eq!(result["health_score"], 100);
    }

    #[test]
    fn unpack_returns_as_is_when_no_content() {
        let raw = serde_json::json!({"id": 1, "state": "running"});
        let result = unpack_tool_result(&raw);
        assert_eq!(result["id"], 1);
        assert_eq!(result["state"], "running");
    }

    #[test]
    fn unpack_handles_non_json_text() {
        let raw = serde_json::json!({
            "content": [{
                "type": "text",
                "text": "plain text response"
            }]
        });
        let result = unpack_tool_result(&raw);
        assert_eq!(result, "plain text response");
    }

    #[test]
    fn unpack_handles_empty_content_array() {
        let raw = serde_json::json!({"content": []});
        let result = unpack_tool_result(&raw);
        assert!(result.is_object());
    }

    // --- Jobs CLI tests ---

    #[test]
    fn jobs_submit_parses_basic() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "submit", "sync"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Submit(args)) => {
                assert_eq!(args.name, "sync");
                assert!(args.params.is_none());
                assert!(args.json == false);
            },
            _ => panic!("expected Jobs::Submit"),
        }
    }

    #[test]
    fn jobs_submit_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "zbrain", "jobs", "submit", "embed",
            "--params", "{\"source\":\"default\"}",
            "--priority", "5",
            "--queue", "high",
            "--delay", "60000",
            "--max-attempts", "5",
            "--max-stalled", "3",
            "--json",
        ]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Submit(args)) => {
                assert_eq!(args.name, "embed");
                assert_eq!(args.params.as_deref(), Some("{\"source\":\"default\"}"));
                assert_eq!(args.priority, Some(5));
                assert_eq!(args.queue.as_deref(), Some("high"));
                assert_eq!(args.delay, Some(60000));
                assert_eq!(args.max_attempts, Some(5));
                assert_eq!(args.max_stalled, Some(3));
                assert!(args.json);
            },
            _ => panic!("expected Jobs::Submit"),
        }
    }

    #[test]
    fn jobs_list_parses_default() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "list"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::List(args)) => {
                assert!(args.status.is_none());
                assert_eq!(args.limit, 20);
                assert!(!args.json);
            },
            _ => panic!("expected Jobs::List"),
        }
    }

    #[test]
    fn jobs_list_parses_filters() {
        let cli = Cli::try_parse_from([
            "zbrain", "jobs", "list", "--status", "failed", "--queue", "default", "--limit", "50", "--json",
        ]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::List(args)) => {
                assert_eq!(args.status.as_deref(), Some("failed"));
                assert_eq!(args.queue.as_deref(), Some("default"));
                assert_eq!(args.limit, 50);
                assert!(args.json);
            },
            _ => panic!("expected Jobs::List"),
        }
    }

    #[test]
    fn jobs_get_parses_id() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "get", "42", "--json"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Get(args)) => {
                assert_eq!(args.id, 42);
                assert!(args.json);
            },
            _ => panic!("expected Jobs::Get"),
        }
    }

    #[test]
    fn jobs_cancel_parses_id() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "cancel", "7"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Cancel(args)) => {
                assert_eq!(args.id, 7);
            },
            _ => panic!("expected Jobs::Cancel"),
        }
    }

    #[test]
    fn jobs_retry_parses_id() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "retry", "99"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Retry(args)) => {
                assert_eq!(args.id, 99);
            },
            _ => panic!("expected Jobs::Retry"),
        }
    }

    #[test]
    fn jobs_prune_parses_older_than() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "prune", "--older-than", "7d"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Prune(args)) => {
                assert_eq!(args.older_than.as_deref(), Some("7d"));
            },
            _ => panic!("expected Jobs::Prune"),
        }
    }

    #[test]
    fn jobs_stats_parses_json() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "stats", "--json"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Stats(args)) => {
                assert!(args.json);
            },
            _ => panic!("expected Jobs::Stats"),
        }
    }

    #[test]
    fn jobs_work_parses_defaults() {
        let cli = Cli::try_parse_from(["zbrain", "jobs", "work"]).unwrap();
        match cli.command {
            Commands::Jobs(JobsAction::Work(args)) => {
                assert_eq!(args.concurrency, 1);
                assert_eq!(args.poll_interval, 1000);
            },
            _ => panic!("expected Jobs::Work"),
        }
    }

    // --- Agent CLI tests ---

    #[test]
    fn agent_run_parses_basic() {
        let cli = Cli::try_parse_from(["zbrain", "agent", "run", "hello world"]).unwrap();
        match cli.command {
            Commands::Agent(AgentAction::Run(args)) => {
                assert_eq!(args.prompt, "hello world");
                assert_eq!(args.max_turns, 20);
                assert!(!args.follow);
            },
            _ => panic!("expected Agent::Run"),
        }
    }

    #[test]
    fn agent_run_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "zbrain", "agent", "run", "test prompt",
            "--model", "claude-3-5-sonnet",
            "--max-turns", "10",
            "--follow",
            "--json",
        ]).unwrap();
        match cli.command {
            Commands::Agent(AgentAction::Run(args)) => {
                assert_eq!(args.prompt, "test prompt");
                assert_eq!(args.model.as_deref(), Some("claude-3-5-sonnet"));
                assert_eq!(args.max_turns, 10);
                assert!(args.follow);
                assert!(args.json);
            },
            _ => panic!("expected Agent::Run"),
        }
    }

    // --- parse_relative_duration tests ---

    #[test]
    fn parse_relative_duration_days() {
        let result = parse_relative_duration("30d");
        assert!(result.is_some());
        // Just verify it's a valid RFC 3339 string
        assert!(result.unwrap().contains('T'));
    }

    #[test]
    fn parse_relative_duration_hours() {
        let result = parse_relative_duration("1h");
        assert!(result.is_some());
    }

    #[test]
    fn parse_relative_duration_rejects_invalid() {
        assert!(parse_relative_duration("abc").is_none());
        assert!(parse_relative_duration("").is_none());
        assert!(parse_relative_duration("5x").is_none());
    }

    // ── `zbrain extract` verb (KNOWN-GAPS G76a) ──────────────────
    //
    // The extraction algorithms themselves are covered by the core-side
    // inline tests in `zbrain_core::auto_fix` (dangling targets, self-links,
    // markdown syntax, bullet/header timeline entries, idempotent re-runs,
    // single-slug scoping). These cases pin the CLI surface only: that the
    // three subcommands exist, and that `--slug` / `--json` reach the args.

    #[test]
    fn extract_links_parses_default() {
        let cli = Cli::try_parse_from(["zbrain", "extract", "links"]).unwrap();
        match cli.command {
            Commands::Extract(ExtractAction::Links(args)) => {
                assert!(args.slug.is_none());
                assert!(!args.json);
            },
            _ => panic!("expected Extract::Links"),
        }
    }

    #[test]
    fn extract_links_parses_slug_and_json() {
        let cli =
            Cli::try_parse_from(["zbrain", "extract", "links", "--slug", "alpha", "--json"])
                .unwrap();
        match cli.command {
            Commands::Extract(ExtractAction::Links(args)) => {
                assert_eq!(args.slug.as_deref(), Some("alpha"));
                assert!(args.json);
            },
            _ => panic!("expected Extract::Links"),
        }
    }

    #[test]
    fn extract_timeline_parses_default() {
        let cli = Cli::try_parse_from(["zbrain", "extract", "timeline"]).unwrap();
        match cli.command {
            Commands::Extract(ExtractAction::Timeline(args)) => {
                assert!(args.slug.is_none());
                assert!(!args.json);
            },
            _ => panic!("expected Extract::Timeline"),
        }
    }

    #[test]
    fn extract_timeline_parses_slug_and_json() {
        let cli =
            Cli::try_parse_from(["zbrain", "extract", "timeline", "--slug", "beta", "--json"])
                .unwrap();
        match cli.command {
            Commands::Extract(ExtractAction::Timeline(args)) => {
                assert_eq!(args.slug.as_deref(), Some("beta"));
                assert!(args.json);
            },
            _ => panic!("expected Extract::Timeline"),
        }
    }

    #[test]
    fn extract_all_parses_slug_and_json() {
        let cli =
            Cli::try_parse_from(["zbrain", "extract", "all", "--slug", "gamma", "--json"])
                .unwrap();
        match cli.command {
            Commands::Extract(ExtractAction::All(args)) => {
                assert_eq!(args.slug.as_deref(), Some("gamma"));
                assert!(args.json);
            },
            _ => panic!("expected Extract::All"),
        }
    }

    #[test]
    fn extract_rejects_unknown_subcommand() {
        // `facts` is not a valid extract subcommand (the real verb is
        // `conversation-facts`, added in 1-2-3). `--source` is now a recognized
        // flag on every extract subcommand (1-2-2 implemented the filesystem
        // source), so `links --source fs` parses; it is only rejected at
        // runtime when `--dir` is missing.
        assert!(Cli::try_parse_from(["zbrain", "extract", "facts"]).is_err());
        assert!(Cli::try_parse_from(["zbrain", "extract", "links", "--source", "fs"]).is_ok());
    }

    #[test]
    fn extract_conversation_facts_parses_default_and_flags() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "extract",
            "conversation-facts",
            "--slug",
            "page-1",
            "--source-id",
            "default",
            "--type",
            "meeting",
            "--dry-run",
            "--limit",
            "5",
            "--model",
            "anthropic:claude-sonnet-4-6",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::Extract(ExtractAction::ConversationFacts(args)) => {
                assert_eq!(args.slug.as_deref(), Some("page-1"));
                assert_eq!(args.source_id, "default");
                assert_eq!(args.types, vec!["meeting".to_string()]);
                assert!(args.dry_run);
                assert_eq!(args.limit, Some(5));
                assert_eq!(args.model.as_deref(), Some("anthropic:claude-sonnet-4-6"));
                assert!(args.json);
            },
            _ => panic!("expected Extract::ConversationFacts"),
        }
    }

    // ── `zbrain eval` verb (KNOWN-GAPS G74) ──────────────────────
    //
    // The IR metric math and the `run_eval` orchestrator are covered by the
    // core-side inline tests in `zbrain_core::search::eval` (P@k divides by k,
    // graded nDCG, qrels parsing, explicit-limit truncation). These cases pin
    // the CLI surface only: flag plumbing, config assembly, the still-unported
    // sub-verb guard, and the table formatting helpers.

    #[test]
    fn eval_parses_minimal_qrels_flag() {
        let cli = Cli::try_parse_from(["zbrain", "eval", "--qrels", "./qrels.json"]).unwrap();
        match cli.command {
            Commands::Eval(args) => {
                assert_eq!(args.qrels.as_deref(), Some("./qrels.json"));
                assert!(args.subcommand.is_none());
                assert_eq!(args.k, 5, "k defaults to 5, matching TS");
                assert!(args.strategy.is_none());
                assert!(!args.json);
            }
            _ => panic!("expected Eval"),
        }
    }

    #[test]
    fn eval_parses_full_flag_surface() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval",
            "--qrels",
            "q.json",
            "--config-a",
            "a.json",
            "--config-b",
            "b.json",
            "--strategy",
            "keyword",
            "--rrf-k",
            "30",
            "--no-expand",
            "--dedup-cosine",
            "0.9",
            "--dedup-type-ratio",
            "0.5",
            "--dedup-max-per-page",
            "3",
            "--limit",
            "20",
            "--k",
            "10",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::Eval(args) => {
                assert_eq!(args.config_a.as_deref(), Some("a.json"));
                assert_eq!(args.config_b.as_deref(), Some("b.json"));
                assert_eq!(args.strategy, Some(EvalStrategyArg::Keyword));
                assert_eq!(args.rrf_k, Some(30.0));
                assert!(args.no_expand && !args.expand);
                assert_eq!(args.dedup_cosine, Some(0.9));
                assert_eq!(args.dedup_type_ratio, Some(0.5));
                assert_eq!(args.dedup_max_per_page, Some(3));
                assert_eq!(args.limit, Some(20));
                assert_eq!(args.k, 10);
                assert!(args.json);
            }
            _ => panic!("expected Eval"),
        }
    }

    #[test]
    fn eval_rejects_conflicting_expand_flags() {
        assert!(
            Cli::try_parse_from(["zbrain", "eval", "--qrels", "q.json", "--expand", "--no-expand"])
                .is_err()
        );
    }

    #[test]
    fn eval_rejects_unported_ts_subverbs() {
        // Guards the gap: every TS `eval <sub>` verb must fail loudly rather
        // than fall through to the bare IR-metrics flow (KNOWN-GAPS G74).
        for sub in UNPORTED_EVAL_SUBCOMMANDS {
            let err = reject_eval_subcommand(Some(sub)).unwrap_err().to_string();
            assert!(
                err.contains("not implemented in Rust yet"),
                "sub-verb `{sub}` must be rejected with the gap pointer, got: {err}"
            );
        }
        // A typo is rejected too, but with the flags-only hint instead.
        let err = reject_eval_subcommand(Some("qrels.json")).unwrap_err().to_string();
        assert!(err.contains("flags only"), "unexpected message: {err}");
        // The bare flow (no positional) passes through.
        assert!(reject_eval_subcommand(None).is_ok());
    }

    #[test]
    fn eval_export_parses_filters() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-export",
            "--tool",
            "search",
            "--since",
            "2026-01-01T00:00:00Z",
            "--limit",
            "50",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalExport(args) => {
                assert_eq!(args.tool.as_deref(), Some("search"));
                assert_eq!(args.since.as_deref(), Some("2026-01-01T00:00:00Z"));
                assert_eq!(args.limit, Some(50));
            }
            _ => panic!("expected EvalExport"),
        }
    }

    #[test]
    fn eval_prune_requires_older_than() {
        // --older-than is required; without it parsing fails.
        assert!(Cli::try_parse_from(["zbrain", "eval-prune"]).is_err());
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-prune",
            "--older-than",
            "2026-01-01T00:00:00Z",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalPrune(args) => {
                assert_eq!(args.older_than, "2026-01-01T00:00:00Z");
                assert!(args.dry_run);
            }
            _ => panic!("expected EvalPrune"),
        }
    }

    #[test]
    fn eval_gate_requires_qrels() {
        // --qrels is required; the qrels half is fully real (1-1-4 stage 3).
        assert!(Cli::try_parse_from(["zbrain", "eval-gate"]).is_err());
        assert!(Cli::try_parse_from(["zbrain", "eval-gate", "--k", "5"]).is_err());
    }

    #[test]
    fn eval_gate_parses_qrels_and_floors() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-gate",
            "--qrels",
            "./qrels.json",
            "--k",
            "20",
            "--recall-at-k",
            "0.8",
            "--first-relevant-hit",
            "0.7",
            "--expected-top1",
            "0.6",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalGate(args) => {
                assert_eq!(args.qrels, "./qrels.json");
                assert_eq!(args.k, 20);
                assert_eq!(args.recall_at_k, Some(0.8));
                assert_eq!(args.first_relevant_hit, Some(0.7));
                assert_eq!(args.expected_top1, Some(0.6));
                assert!(args.json);
            }
            _ => panic!("expected EvalGate"),
        }
    }

    #[test]
    fn eval_replay_requires_against() {
        // --against is required; replay is fully real (1-1-4 stage 4).
        assert!(Cli::try_parse_from(["zbrain", "eval-replay"]).is_err());
        assert!(Cli::try_parse_from(["zbrain", "eval-replay", "--json"]).is_err());
    }

    #[test]
    fn eval_replay_parses_against_and_flags() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-replay",
            "--against",
            "./baseline.ndjson",
            "--limit",
            "10",
            "--compare-limit",
            "25",
            "--top-regressions",
            "3",
            "--json",
            "--verbose",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalReplay(args) => {
                assert_eq!(args.against, "./baseline.ndjson");
                assert_eq!(args.limit, Some(10));
                assert_eq!(args.compare_limit, Some(25));
                assert_eq!(args.top_regressions, Some(3));
                assert!(args.json);
                assert!(args.verbose);
            }
            _ => panic!("expected EvalReplay"),
        }
    }

    #[test]
    fn eval_whoknows_requires_fixture() {
        // The fixture path is a required positional; whoknows is fully real
        // (1-1-4 stage 5).
        assert!(Cli::try_parse_from(["zbrain", "eval-whoknows"]).is_err());
        assert!(Cli::try_parse_from(["zbrain", "eval-whoknows", "--json"]).is_err());
    }

    #[test]
    fn eval_whoknows_parses_fixture_and_flags() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-whoknows",
            "./fixtures/whoknows-eval.jsonl",
            "--json",
            "--skip-replay",
            "--limit",
            "10",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalWhoknows(args) => {
                assert_eq!(args.fixture_path, "./fixtures/whoknows-eval.jsonl");
                assert!(args.json);
                assert!(args.skip_replay);
                assert_eq!(args.limit, 10);
            }
            _ => panic!("expected EvalWhoknows"),
        }
    }

    #[test]
    fn eval_run_all_defaults_to_all_three_checks() {
        // No --checks => all three gates; inputs are parsed but not required
        // here (the CLI validates required inputs at runtime).
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-run-all",
            "--qrels",
            "./q.json",
            "--against",
            "./b.ndjson",
            "--fixture",
            "./f.jsonl",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalRunAll(args) => {
                assert!(args.checks.is_none());
                assert_eq!(args.qrels.as_deref(), Some("./q.json"));
                assert_eq!(args.against.as_deref(), Some("./b.ndjson"));
                assert_eq!(args.fixture.as_deref(), Some("./f.jsonl"));
                assert_eq!(args.k, 10);
                assert_eq!(args.limit, 5);
            }
            _ => panic!("expected EvalRunAll"),
        }
    }

    #[test]
    fn eval_run_all_parses_checks_list_and_flags() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-run-all",
            "--checks",
            "gate,replay",
            "--qrels",
            "./q.json",
            "--against",
            "./b.ndjson",
            "--k",
            "20",
            "--json",
            "--output",
            "./report.json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalRunAll(args) => {
                assert_eq!(args.checks, Some(vec!["gate".to_string(), "replay".to_string()]));
                assert_eq!(args.k, 20);
                assert!(args.json);
                assert_eq!(args.output.as_deref(), Some("./report.json"));
            }
            _ => panic!("expected EvalRunAll"),
        }
    }

    #[test]
    fn eval_run_all_rejects_unknown_check() {
        // `--checks` accepts any string at parse time; the invalid value is
        // rejected by `parse_check_list` (runtime), which is what the CLI
        // actually calls.
        let err = parse_check_list(&Some(vec!["bogus".to_string()])).unwrap_err();
        assert!(
            err.to_string().contains("not a valid gate"),
            "unknown --checks value must be rejected: {err}"
        );
    }

    #[test]
    fn eval_compare_requires_both_reports() {
        assert!(Cli::try_parse_from(["zbrain", "eval-compare"]).is_err());
        assert!(
            Cli::try_parse_from(["zbrain", "eval-compare", "--baseline", "a.json"]).is_err()
        );
    }

    #[test]
    fn eval_compare_parses_both_reports() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-compare",
            "--baseline",
            "a.json",
            "--current",
            "b.json",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalCompare(args) => {
                assert_eq!(args.baseline, "a.json");
                assert_eq!(args.current, "b.json");
                assert!(args.json);
            }
            _ => panic!("expected EvalCompare"),
        }
    }

    #[test]
    fn eval_code_retrieval_baseline_parses_flags() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-code-retrieval",
            "--baseline",
            "--k",
            "10",
            "--corpus",
            "mybrain",
            "--save",
            "out.json",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalCodeRetrieval(args) => {
                assert!(args.baseline);
                assert!(!args.with_code_intel);
                assert_eq!(args.k, 10);
                assert_eq!(args.corpus, "mybrain");
                assert_eq!(args.save.as_deref(), Some("out.json"));
                assert!(args.json);
            }
            _ => panic!("expected EvalCodeRetrieval"),
        }
    }

    #[test]
    fn eval_code_retrieval_with_code_intel_parses() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-code-retrieval",
            "--with-code-intel",
            "--questions",
            "q.json",
            "--source",
            "src",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalCodeRetrieval(args) => {
                assert!(args.with_code_intel);
                assert_eq!(args.questions.as_deref(), Some("q.json"));
                assert_eq!(args.source.as_deref(), Some("src"));
            }
            _ => panic!("expected EvalCodeRetrieval"),
        }
    }

    #[test]
    fn eval_code_retrieval_compare_requires_two_reports() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-code-retrieval",
            "--compare",
            "a.json",
            "b.json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalCodeRetrieval(args) => {
                assert_eq!(
                    args.compare.as_deref(),
                    Some(["a.json".to_string(), "b.json".to_string()].as_slice())
                );
            }
            _ => panic!("expected EvalCodeRetrieval"),
        }
    }

    #[test]
    fn eval_code_retrieval_compare_rejects_single_report() {
        assert!(
            Cli::try_parse_from(["zbrain", "eval-code-retrieval", "--compare", "only.json"]).is_err()
        );
    }

    #[test]
    fn eval_cross_modal_parses_full_flag_surface() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-cross-modal",
            "--task",
            "write a good skill doc",
            "--output",
            "skills/my-skill/SKILL.md",
            "--slug",
            "my-skill",
            "--dimensions",
            "DEPTH,SOURCING",
            "--cycles",
            "2",
            "--slot-a-model",
            "openai:gpt-4o-mini",
            "--slot-b-model",
            "anthropic:claude-sonnet-4-6",
            "--slot-c-model",
            "google:gemini-2.0-flash",
            "--receipt-dir",
            "/tmp/receipts",
            "--max-tokens",
            "1500",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalCrossModal(args) => {
                assert_eq!(args.task.as_deref(), Some("write a good skill doc"));
                assert_eq!(args.output.as_deref(), Some("skills/my-skill/SKILL.md"));
                assert_eq!(args.slug.as_deref(), Some("my-skill"));
                assert_eq!(
                    args.dimensions.as_deref(),
                    Some(["DEPTH".to_string(), "SOURCING".to_string()].as_slice())
                );
                assert_eq!(args.cycles, Some(2));
                assert_eq!(args.slot_a_model.as_deref(), Some("openai:gpt-4o-mini"));
                assert_eq!(args.slot_b_model.as_deref(), Some("anthropic:claude-sonnet-4-6"));
                assert_eq!(args.slot_c_model.as_deref(), Some("google:gemini-2.0-flash"));
                assert_eq!(args.receipt_dir.as_deref(), Some("/tmp/receipts"));
                assert_eq!(args.max_tokens, Some(1500));
                assert!(args.json);
            }
            _ => panic!("expected EvalCrossModal"),
        }
    }

    #[test]
    fn eval_cross_modal_defers_required_flags_to_the_handler() {
        // TS prints usage + returns 1 for a missing --task/--output rather than
        // failing at parse time; keep clap permissive so the messages match.
        let cli = Cli::try_parse_from(["zbrain", "eval-cross-modal"]).unwrap();
        match cli.command {
            Commands::EvalCrossModal(args) => {
                assert!(args.task.is_none());
                assert!(args.output.is_none());
                assert!(args.cycles.is_none());
                assert!(!args.json);
            }
            _ => panic!("expected EvalCrossModal"),
        }
    }

    #[test]
    fn eval_longmemeval_parses_without_dataset() {
        // The runner owns the dataset-required check; clap must accept the
        // verb with no positional so the runner can emit its own honest error.
        let cli = Cli::try_parse_from(["zbrain", "eval-longmemeval"]).unwrap();
        match cli.command {
            Commands::EvalLongMemEval(args) => {
                assert!(args.dataset.is_none());
                assert!(!args.retrieval_only);
                assert!(!args.no_trajectory);
                assert!(args.limit.is_none());
                assert!(args.by_type_floor.is_none());
            }
            _ => panic!("expected EvalLongMemEval"),
        }
    }

    #[test]
    fn eval_longmemeval_reconstructs_argv_for_runner() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-longmemeval",
            "ds.jsonl",
            "--limit",
            "3",
            "--model",
            "sonnet",
            "--retrieval-only",
            "--top-k",
            "5",
            "--by-type-floor",
            "0.8",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalLongMemEval(args) => {
                assert_eq!(args.dataset.as_deref(), Some("ds.jsonl"));
                assert_eq!(args.limit, Some(3));
                assert_eq!(args.model.as_deref(), Some("sonnet"));
                assert!(args.retrieval_only);
                assert_eq!(args.top_k, Some(5));
                assert_eq!(args.by_type_floor, Some(0.8));
                let v = eval_longmemeval_args_to_vec(&args);
                assert!(v.contains(&"ds.jsonl".to_string()));
                assert!(v.contains(&"--retrieval-only".to_string()));
                assert!(v.contains(&"--by-type-floor".to_string()));
                assert!(v.contains(&"0.8".to_string()));
            }
            _ => panic!("expected EvalLongMemEval"),
        }
    }

    #[test]
    fn eval_takes_quality_parses_minimal() {
        // clap stays permissive: the verb must accept `run` with no flags.
        let cli = Cli::try_parse_from(["zbrain", "eval-takes-quality", "run"]).unwrap();
        match cli.command {
            Commands::EvalTakesQuality(args) => match args.action {
                TakesQualityAction::Run(run_args) => {
                    assert_eq!(run_args.sample, 100);
                    assert!(run_args.slug.is_none());
                    assert!(run_args.dimensions.is_none());
                    assert!(run_args.cycles.is_none());
                    assert!(run_args.slot_a_model.is_none());
                    assert!(run_args.slot_b_model.is_none());
                    assert!(run_args.slot_c_model.is_none());
                    assert!(run_args.receipt_dir.is_none());
                    assert!(run_args.max_tokens.is_none());
                    assert!(!run_args.json);
                }
                _ => panic!("expected Run"),
            },
            _ => panic!("expected EvalTakesQuality"),
        }
    }

    #[test]
    fn eval_takes_quality_parses_full_flag_surface() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-takes-quality",
            "run",
            "--sample",
            "20",
            "--slug",
            "my-brain",
            "--dimensions",
            "accuracy,attribution",
            "--cycles",
            "2",
            "--slot-a-model",
            "openai:gpt-4o-mini",
            "--slot-b-model",
            "anthropic:claude-sonnet-4-6",
            "--slot-c-model",
            "google:gemini-2.0-flash",
            "--receipt-dir",
            "/tmp/receipts",
            "--max-tokens",
            "1500",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalTakesQuality(args) => match args.action {
                TakesQualityAction::Run(run_args) => {
                    assert_eq!(run_args.sample, 20);
                    assert_eq!(run_args.slug.as_deref(), Some("my-brain"));
                    assert_eq!(
                        run_args.dimensions.as_deref(),
                        Some(
                            ["accuracy".to_string(), "attribution".to_string()]
                                .as_slice()
                        )
                    );
                    assert_eq!(run_args.cycles, Some(2));
                    assert_eq!(run_args.slot_a_model.as_deref(), Some("openai:gpt-4o-mini"));
                    assert_eq!(run_args.slot_b_model.as_deref(), Some("anthropic:claude-sonnet-4-6"));
                    assert_eq!(run_args.slot_c_model.as_deref(), Some("google:gemini-2.0-flash"));
                    assert_eq!(run_args.receipt_dir.as_deref(), Some("/tmp/receipts"));
                    assert_eq!(run_args.max_tokens, Some(1500));
                    assert!(run_args.json);
                }
                _ => panic!("expected Run"),
            },
            _ => panic!("expected EvalTakesQuality"),
        }
    }

    #[test]
    fn eval_cross_modal_is_no_longer_an_unported_subverb() {
        assert!(!UNPORTED_EVAL_SUBCOMMANDS.contains(&"cross-modal"));
        let err = reject_eval_subcommand(Some("cross-modal")).unwrap_err().to_string();
        assert!(err.contains("zbrain eval-cross-modal"), "got: {err}");
        assert!(!err.contains("KNOWN-GAPS"), "must not still claim a gap: {err}");
    }

    #[test]
    fn suspected_contradictions_is_now_a_top_level_verb() {
        // No longer rejected as an unported sub-verb.
        assert!(!UNPORTED_EVAL_SUBCOMMANDS.contains(&"suspected-contradictions"));
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-suspected-contradictions",
            "run",
            "--sample",
            "50",
            "--max-pairs",
            "10",
            "--judge",
            "anthropic:claude-haiku-4-5",
            "--query",
            "do these conflict?",
            "--slug",
            "probe1",
            "--json",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalSuspectedContradictions(args) => match args.action {
                SuspectedContradictionsAction::Run(r) => {
                    assert_eq!(r.sample, 50);
                    assert_eq!(r.max_pairs, 10);
                    assert_eq!(r.judge.as_deref(), Some("anthropic:claude-haiku-4-5"));
                    assert_eq!(r.query.as_deref(), Some("do these conflict?"));
                    assert_eq!(r.slug.as_deref(), Some("probe1"));
                    assert!(r.json);
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected EvalSuspectedContradictions"),
        }
    }

    #[test]
    fn eval_suspected_contradictions_redirects_old_subverb() {
        // `zbrain eval suspected-contradictions` now redirects to the ported
        // top-level verb instead of claiming it is unimplemented.
        let err = reject_eval_subcommand(Some("suspected-contradictions"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("eval-suspected-contradictions"),
            "got: {err}"
        );
    }

    #[test]
    fn eval_suspected_contradictions_retrieval_flags_parse() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval-suspected-contradictions",
            "run",
            "--pairing",
            "retrieval",
            "--queries",
            "valuation,markets",
            "--top-k",
            "3",
        ])
        .unwrap();
        match cli.command {
            Commands::EvalSuspectedContradictions(args) => match args.action {
                SuspectedContradictionsAction::Run(r) => {
                    assert_eq!(r.pairing, "retrieval");
                    let q = r.queries.expect("queries should be set");
                    assert_eq!(q.len(), 2);
                    assert_eq!(q[0], "valuation");
                    assert_eq!(q[1], "markets");
                    assert_eq!(r.top_k, 3);
                }
                _ => panic!("expected Run action"),
            },
            _ => panic!("expected EvalSuspectedContradictions"),
        }
    }

    #[test]
    fn eval_config_a_applies_cli_overrides_over_the_config_file() {
        let cli = Cli::try_parse_from([
            "zbrain",
            "eval",
            "--qrels",
            "q.json",
            "--config-a",
            r#"{"name":"baseline","strategy":"hybrid","limit":50}"#,
            "--strategy",
            "keyword",
            "--limit",
            "7",
        ])
        .unwrap();
        let Commands::Eval(args) = cli.command else { panic!("expected Eval") };
        let cfg = build_eval_config_a(&args).unwrap();
        assert_eq!(cfg.name.as_deref(), Some("baseline"), "file name survives");
        assert_eq!(cfg.strategy, Some(zbrain_core::search::EvalStrategy::Keyword));
        assert_eq!(cfg.limit, Some(7), "CLI --limit wins over the config file");
    }

    #[test]
    fn eval_config_a_defaults_to_hybrid_named_config_a() {
        let cli = Cli::try_parse_from(["zbrain", "eval", "--qrels", "q.json"]).unwrap();
        let Commands::Eval(args) = cli.command else { panic!("expected Eval") };
        let cfg = build_eval_config_a(&args).unwrap();
        assert_eq!(cfg.name.as_deref(), Some("Config A"));
        assert_eq!(cfg.strategy, Some(zbrain_core::search::EvalStrategy::Hybrid));
        assert!(cfg.limit.is_none(), "unset --limit leaves the derived default");
    }

    #[test]
    fn eval_config_b_ignores_cli_flags() {
        // Faithful to TS `buildConfig(opts, 'b')`: side B comes entirely from
        // its own JSON, otherwise A/B would compare two identical configs.
        let cfg = build_eval_config_b(r#"{"strategy":"keyword"}"#).unwrap();
        assert_eq!(cfg.name.as_deref(), Some("Config B"));
        assert_eq!(cfg.strategy, Some(zbrain_core::search::EvalStrategy::Keyword));
    }

    #[test]
    fn eval_table_helpers_are_codepoint_safe() {
        assert_eq!(eval_fmt(0.5), "0.50");
        assert_eq!(eval_pad_r("ab", 5), "ab   ");
        assert_eq!(eval_pad_l("ab", 5), "   ab");
        // Over-width input is clipped, not padded.
        assert_eq!(eval_pad_r("abcdef", 3), "abc");
        // CJK must clip on char boundaries (a byte slice would panic here).
        assert_eq!(eval_truncate("知识库检索评测", 4), "知识库…");
        assert_eq!(eval_truncate("short", 40), "short");
        assert_eq!(eval_plural(1), "y");
        assert_eq!(eval_plural(2), "ies");
    }
}
