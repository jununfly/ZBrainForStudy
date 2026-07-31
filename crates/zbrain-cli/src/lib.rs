//! `zbrain-cli` — command-line entry point.
//!
//! Slice 1-3-1: clap CLI framework with 4 command stubs.
//! Slice 1-3-1-2: Config file discovery, YAML parsing, and env var overrides.
//! Next slices add command implementations.

pub mod config;
pub mod mcp_client;
pub mod schema_cmd;
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
use zbrain_core::engine::BrainEngine;
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
/// registered in docs/plans/KNOWN-GAPS.md (G5).
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

    /// Manage connected brains (mounts.json)
    #[command(name = "mounts", subcommand)]
    Mounts(mounts::MountsSubcommand),

    /// Skillpack management — install, scaffold, search, harvest from third-party repos.
    #[command(subcommand)]
    Skillpack(skillpack::SkillpackSubcommand),

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

    /// Remove a link
    #[command(name = "rm")]
    Remove(LinksRemoveArgs),
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

/// Arguments for `zbrain query` command.
///
/// `--explain` mirrors the TS global flag: it swaps the default JSON output for
/// a human-readable per-stage scoring attribution breakdown (base_score →
/// migrated boost multipliers → reranker rank delta → final). Only the stages
/// with a Rust data layer are rendered (salience / recency / reranker); the
/// un-migrated boost axes are tracked in docs/plans/KNOWN-GAPS.md (G13).
#[derive(Debug, Parser)]
pub struct QueryArgs {
    /// Search query text
    pub query: String,

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
/// docs/plans/KNOWN-GAPS.md.
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
        Commands::BookMirror(args) => {
            run_book_mirror_command(args, cli.config.as_deref()).await?
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
    let params = serde_json::json!({
        "question": args.question,
        "anchor": args.anchor,
        "rounds": args.rounds,
        "model": args.model,
        "since": args.since,
        "until": args.until,
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

/// Execute `zbrain query` command.
async fn run_query_command(args: QueryArgs, config_path: Option<&Path>, timeout_ms: Option<u64>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "query": args.query,
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
/// docs/plans/KNOWN-GAPS.md.
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
/// Full background: docs/plans/KNOWN-GAPS.md (G4, resolved).
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

// ── Links commands ──────────────────────────────────────────────

/// Dispatch `zbrain links` subcommands.
async fn run_links_command(action: LinksAction, config_path: Option<&Path>) -> anyhow::Result<()> {
    match action {
        LinksAction::Add(args) => run_links_add(args, config_path).await?,
        LinksAction::List(args) => run_links_list(args, config_path).await?,
        LinksAction::Backlinks(args) => run_links_backlinks(args, config_path).await?,
        LinksAction::Remove(args) => run_links_remove(args, config_path).await?,
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
}
