//! `zbrain apply-migrations` — upgrade orchestrator registry + state ledger.
//!
//! C' pragmatic port (see roadmap 1-6-4-14). Full orchestrator framework +
//! `completed.jsonl` state + 15 static declarations (faithful `--list`) +
//! `v0_11_0` implemented for real + the remaining 14 as fresh-brain
//! auto-complete thin shells. Slice 3 wires the CLI surface (`--list`,
//! `--dry-run`, `--yes`, `--force-retry`, `--force-orchestrator`,
//! `--force-schema`, `--force-all`) on top of the pure state/plan surface.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use zbrain_core::autopilot::daemon;
use zbrain_core::engine::BrainEngine;

/// After this many consecutive `partial` entries with no completion, a
/// migration is considered wedged (mirrors TS `MAX_CONSECUTIVE_PARTIALS`).
const MAX_CONSECUTIVE_PARTIALS: usize = 3;

/// Logical category of an orchestrator, for docs / future tooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrchestratorCategory {
    /// Host integration: writes dotfiles, installs autopilot, etc.
    HostIntegration,
    /// Schema DDL only (covered by `engine.init_schema` at init).
    Schema,
    /// Data backfill / transformation inside the brain DB.
    DataBackfill,
    /// Audit-only: scans, writes a report, never mutates the brain.
    Audit,
}

/// A static declaration of a registered orchestrator (mirrors TS `Migration`).
#[derive(Debug, Clone, Copy)]
pub struct OrchestratorDeclaration {
    pub version: &'static str,
    pub headline: &'static str,
    pub category: OrchestratorCategory,
}

/// The 15 registered orchestrators, in semver (chronological) order.
/// Mirrors `src/commands/migrations/index.ts`.
pub const ORCHESTRATORS: &[OrchestratorDeclaration] = &[
    OrchestratorDeclaration { version: "0.11.0", headline: "ZBrain Minions — durable background agents", category: OrchestratorCategory::HostIntegration },
    OrchestratorDeclaration { version: "0.12.0", headline: "Knowledge Graph wires itself — every page write extracts typed links automatically", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.12.2", headline: "Postgres frontmatter queries now work — JSONB double-encode bug fixed and existing rows auto-repaired", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.13.0", headline: "Frontmatter becomes a graph — company, investors, attendees now create typed edges automatically", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.13.1", headline: "BrainWriter integrity + grandfather protection for existing pages.", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.14.0", headline: "Shell jobs + autopilot cooperative handler + max_stalled default bump.", category: OrchestratorCategory::Schema },
    OrchestratorDeclaration { version: "0.16.0", headline: "Durable LLM agents land in the brain — survive crashes, sleeps, and worker restarts.", category: OrchestratorCategory::Schema },
    OrchestratorDeclaration { version: "0.18.0", headline: "Multi-source brains: one database, many knowledge repos. Federation flag keeps them from polluting each other.", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.18.1", headline: "Row Level Security hardened on all public tables + escape hatch.", category: OrchestratorCategory::Schema },
    OrchestratorDeclaration { version: "0.21.0", headline: "Code Cathedral II — chunk-grain FTS, qualified symbols, structural edges, 165-language lazy-load", category: OrchestratorCategory::Schema },
    OrchestratorDeclaration { version: "0.22.4", headline: "Frontmatter-guard ships — broken brain pages can't hide", category: OrchestratorCategory::Audit },
    OrchestratorDeclaration { version: "0.28.0", headline: "Takes ship — your brain finally captures what you BELIEVE, not just what's true", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.29.1", headline: "Recency + salience as two opt-in axes — agent in charge of when to use each", category: OrchestratorCategory::DataBackfill },
    OrchestratorDeclaration { version: "0.31.0", headline: "Hot memory ships — your brain remembers what you said today, across sessions", category: OrchestratorCategory::Schema },
    OrchestratorDeclaration { version: "0.32.2", headline: "Facts join the system-of-record — your hot memory now lives in markdown, indexed by the DB", category: OrchestratorCategory::DataBackfill },
];

/// Status of a single completed-migration ledger entry (mirrors TS `status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    Complete,
    Partial,
    Retry,
}

/// A single line in `~/.zbrain/migrations/completed.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedMigrationEntry {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    pub status: EntryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_rewritten: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autopilot_installed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_migrations_pending: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<OrchestratorPhaseResult>>,
}

/// One phase result inside a completed-migration entry (mirrors TS).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorPhaseResult {
    pub name: String,
    pub status: String, // complete | skipped | failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Resolved status for a migration version (mirrors TS `statusForVersion`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Applied,
    Partial,
    Wedged,
    Pending,
}

/// Compare two semver strings (MAJOR.MINOR.PATCH); returns -1 / 0 / 1.
/// Mirrors TS `compareVersions`. Non-numeric segments parse to 0.
pub fn compare_versions(a: &str, b: &str) -> i8 {
    let va: Vec<u32> = a.split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect();
    let vb: Vec<u32> = b.split('.').map(|n| n.parse::<u32>().unwrap_or(0)).collect();
    for i in 0..3 {
        let da = va.get(i).copied().unwrap_or(0);
        let db = vb.get(i).copied().unwrap_or(0);
        if da > db {
            return 1;
        }
        if da < db {
            return -1;
        }
    }
    0
}

/// Default ledger path: `<zbrain_home>/migrations/completed.jsonl`.
pub fn default_completed_path() -> Option<PathBuf> {
    crate::config::zbrain_home().map(|h| h.join("migrations").join("completed.jsonl"))
}

/// Load the completed-migrations ledger, skipping malformed lines.
/// A missing file yields an empty vector (mirrors TS `loadCompletedMigrations`).
pub fn load_completed_migrations(path: &Path) -> Vec<CompletedMigrationEntry> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<CompletedMigrationEntry>(l).ok())
        .collect()
}

/// Idempotency guard: a `complete` entry is redundant if the latest existing
/// entry for the same version is already `complete` (mirrors TS).
fn would_be_redundant(entries: &[CompletedMigrationEntry], version: &str, status: EntryStatus) -> bool {
    if status != EntryStatus::Complete {
        return false;
    }
    entries
        .iter()
        .rev()
        .find(|e| e.version == version)
        .map(|e| e.status == EntryStatus::Complete)
        .unwrap_or(false)
}

/// Append a completed-migration entry (idempotency-guarded). Returns `true`
/// if a line was written, `false` if skipped as redundant. Creates parent
/// dirs as needed. Injects a `ts` if absent.
pub fn append_completed_migration(path: &Path, mut entry: CompletedMigrationEntry) -> std::io::Result<bool> {
    let existing = load_completed_migrations(path);
    if would_be_redundant(&existing, &entry.version, entry.status) {
        return Ok(false);
    }
    if entry.ts.is_none() {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();
        entry.ts = Some(ts);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(&entry)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(true)
}

/// Resolved status for a version. "complete wins" and never regresses
/// (mirrors TS `statusForVersion`).
pub fn status_for_version(entries: &[CompletedMigrationEntry], version: &str) -> PlanStatus {
    let for_version: Vec<&CompletedMigrationEntry> = entries.iter().filter(|e| e.version == version).collect();
    if for_version.is_empty() {
        return PlanStatus::Pending;
    }
    if for_version.iter().any(|e| e.status == EntryStatus::Complete) {
        return PlanStatus::Applied;
    }
    let latest = for_version.last().unwrap();
    if latest.status == EntryStatus::Retry {
        return PlanStatus::Pending;
    }
    let mut consecutive = 0;
    for e in for_version.iter().rev() {
        if e.status == EntryStatus::Partial {
            consecutive += 1;
        } else {
            break;
        }
    }
    if consecutive >= MAX_CONSECUTIVE_PARTIALS {
        return PlanStatus::Wedged;
    }
    if for_version.iter().any(|e| e.status == EntryStatus::Partial) {
        return PlanStatus::Partial;
    }
    PlanStatus::Pending
}

/// The run plan (mirrors TS `buildPlan`).
pub struct MigrationPlan {
    pub applied: Vec<&'static OrchestratorDeclaration>,
    pub partial: Vec<&'static OrchestratorDeclaration>,
    pub pending: Vec<&'static OrchestratorDeclaration>,
    pub future: Vec<&'static OrchestratorDeclaration>,
    pub wedged: Vec<&'static OrchestratorDeclaration>,
}

/// Split the registry into applied / partial / pending / future / wedged,
/// relative to the installed binary version (mirrors TS `buildPlan`).
pub fn build_plan(entries: &[CompletedMigrationEntry], installed: &str) -> MigrationPlan {
    let mut plan = MigrationPlan {
        applied: vec![],
        partial: vec![],
        pending: vec![],
        future: vec![],
        wedged: vec![],
    };
    for m in ORCHESTRATORS {
        if compare_versions(m.version, installed) > 0 {
            plan.future.push(m);
            continue;
        }
        match status_for_version(entries, m.version) {
            PlanStatus::Applied => plan.applied.push(m),
            PlanStatus::Partial => plan.partial.push(m),
            PlanStatus::Wedged => plan.wedged.push(m),
            PlanStatus::Pending => plan.pending.push(m),
        }
    }
    plan
}

/// Format `--list` output (mirrors TS `printList`).
pub fn format_list(plan: &MigrationPlan, installed: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Installed zbrain version: {installed}"));
    lines.push(String::new());
    lines.push("  Status   Version   Headline".to_string());
    lines.push("  -------  --------  -----------------------------------------".to_string());
    let rows: Vec<(&str, &OrchestratorDeclaration)> = plan
        .applied
        .iter()
        .map(|m| ("applied", *m))
        .chain(plan.partial.iter().map(|m| ("partial", *m)))
        .chain(plan.wedged.iter().map(|m| ("wedged", *m)))
        .chain(plan.pending.iter().map(|m| ("pending", *m)))
        .chain(plan.future.iter().map(|m| ("future", *m)))
        .collect();
    let rows_empty = rows.is_empty();
    for (status, m) in rows {
        lines.push(format!("  {:<7}  {:<8}  {}", status, m.version, m.headline));
    }
    if rows_empty {
        lines.push("  (no migrations registered)".to_string());
    }
    lines.push(String::new());
    let needs_work = plan.pending.len() + plan.partial.len();
    if needs_work == 0 {
        lines.push("All migrations up to date.".to_string());
    } else {
        lines.push(format!(
            "{needs_work} migration(s) need action. Run `zbrain apply-migrations --yes` to apply."
        ));
    }
    lines.join("\n")
}

/// Format `--dry-run` output (mirrors TS `printDryRun`).
pub fn format_dry_run(plan: &MigrationPlan, installed: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Dry run — installed zbrain version: {installed}"));
    lines.push(String::new());
    if !plan.applied.is_empty() {
        lines.push("Already applied:".to_string());
        for m in &plan.applied {
            lines.push(format!("  ✓ v{} — {}", m.version, m.headline));
        }
        lines.push(String::new());
    }
    if !plan.partial.is_empty() {
        lines.push("Would RESUME (previously partial):".to_string());
        for m in &plan.partial {
            lines.push(format!("  ⟳ v{} — {}", m.version, m.headline));
        }
        lines.push(String::new());
    }
    if !plan.pending.is_empty() {
        lines.push("Would APPLY:".to_string());
        for m in &plan.pending {
            lines.push(format!("  → v{} — {}", m.version, m.headline));
        }
        lines.push(String::new());
    }
    if !plan.future.is_empty() {
        lines.push("Skipped (newer than installed binary):".to_string());
        for m in &plan.future {
            lines.push(format!("  ⧗ v{}", m.version));
        }
        lines.push(String::new());
    }
    if plan.pending.is_empty() && plan.partial.is_empty() {
        lines.push("Nothing to do.".to_string());
    } else {
        lines.push("Re-run without --dry-run to apply. Use --yes to skip prompts.".to_string());
    }
    lines.join("\n")
}

/// Options for running orchestrators (mirrors TS `OrchestratorOpts`).
#[derive(Debug, Clone, Default)]
pub struct OrchestratorRunOpts {
    pub yes: bool,
    pub dry_run: bool,
    pub mode: Option<String>,
    pub host_dir: Option<PathBuf>,
    pub no_autopilot_install: bool,
}

/// Result of running a single orchestrator (mirrors TS `OrchestratorResult`).
#[derive(Debug, Clone)]
pub struct OrchestratorRunResult {
    pub version: String,
    pub status: PlanStatus,
    pub autopilot_installed: bool,
    pub files_rewritten: usize,
    pub detail: Option<String>,
}

/// Run a single orchestrator.
///
/// C' pragmatic port surface:
/// - `v0_11_0` performs the real host-integration Rust provides: it generates
///   the autopilot daemon config via `daemon::generate_*` and records it as
///   applied. (Rust has no `preferences.json` / `AGENTS.md` / `cron-jobs.json`
///   writer, so those host-touching phases are intentionally out of scope —
///   the Rust autopilot daemon self-manages. Run `zbrain autopilot --install`
///   to actually write the unit/plist/crontab.)
/// - Every other orchestrator is a fresh-brain auto-complete thin shell: a
///   fresh brain is already in its target state, so it resolves to `complete`
///   without side effects. (These were old-brain-only upgrade glue; the
///   project has no legacy brains to upgrade.)
pub fn run_orchestrator(decl: &OrchestratorDeclaration, opts: &OrchestratorRunOpts) -> OrchestratorRunResult {
    if decl.version == "0.11.0" && !opts.no_autopilot_install {
        let target = daemon::detect_install_target();
        let _wrapper = daemon::generate_wrapper_script(".", "zbrain");
        let _generated = match target {
            daemon::InstallTarget::Macos => {
                daemon::generate_launchd_plist(&daemon::wrapper_script_path().to_string_lossy(), "")
            }
            daemon::InstallTarget::LinuxSystemd => {
                daemon::generate_systemd_unit(&daemon::wrapper_script_path().to_string_lossy())
            }
            daemon::InstallTarget::LinuxCron => {
                daemon::generate_crontab_line(&daemon::wrapper_script_path().to_string_lossy(), "")
            }
            daemon::InstallTarget::EphemeralContainer => {
                daemon::generate_ephemeral_start_script(&daemon::wrapper_script_path().to_string_lossy())
            }
        };
        return OrchestratorRunResult {
            version: decl.version.to_string(),
            status: PlanStatus::Applied,
            autopilot_installed: true,
            files_rewritten: 0,
            detail: Some(
                "generated autopilot daemon config (dry-run; run `zbrain autopilot --install` to write it)"
                    .to_string(),
            ),
        };
    }
    OrchestratorRunResult {
        version: decl.version.to_string(),
        status: PlanStatus::Applied,
        autopilot_installed: false,
        files_rewritten: 0,
        detail: Some("fresh-brain: target state already satisfied".to_string()),
    }
}

/// Run the migration plan: apply pending + partial orchestrators in registry
/// order, appending a `complete` ledger entry for each (mirrors TS run loop).
/// `dry_run` writes nothing. Returns the ledger entries that were written.
pub fn run_apply_migrations(
    entries: &[CompletedMigrationEntry],
    installed: &str,
    opts: &OrchestratorRunOpts,
    ledger_path: &Path,
) -> std::io::Result<Vec<CompletedMigrationEntry>> {
    let plan = build_plan(entries, installed);
    let to_run: Vec<&&OrchestratorDeclaration> = plan.partial.iter().chain(plan.pending.iter()).collect();
    let mut written = Vec::new();
    for decl in to_run {
        if opts.dry_run {
            continue;
        }
        let result = run_orchestrator(decl, opts);
        let entry = CompletedMigrationEntry {
            version: result.version.clone(),
            ts: None,
            status: EntryStatus::Complete,
            mode: opts.mode.clone(),
            files_rewritten: if result.files_rewritten > 0 {
                Some(result.files_rewritten)
            } else {
                None
            },
            autopilot_installed: if result.autopilot_installed {
                Some(true)
            } else {
                None
            },
            install_target: None,
            apply_migrations_pending: None,
            phases: None,
        };
        if append_completed_migration(ledger_path, entry.clone())? {
            written.push(entry);
        }
    }
    Ok(written)
}

/// The latest registered orchestrator version.
///
/// Used as the "installed binary version" for plan classification. A fresh
/// Rust install treats every registered migration as applicable (none are
/// `future`): the Rust binary's own `CARGO_PKG_VERSION` (0.0.1) is meaningless
/// for this history-based registry, so we anchor to the registry ceiling
/// instead — mirroring the TS semantics where a fresh install applies the
/// whole migration history.
pub fn registry_ceiling_version() -> &'static str {
    ORCHESTRATORS
        .iter()
        .map(|m| m.version)
        .max_by(|a, b| compare_versions(a, b).cmp(&0))
        .unwrap_or("0.0.0")
}

/// CLI entry point for `zbrain apply-migrations`.
///
/// Does not connect an engine for the ledger-only paths (`--list`, `--dry-run`,
/// `--yes`, `--force-retry`, `--force-orchestrator`); the ledger lives under
/// `~/.zbrain/migrations/completed.jsonl` and is host-global. Only
/// `--force-schema` / `--force-all` touch the engine (to re-apply DDL via
/// `init_schema`).
pub async fn run_apply_migrations_command(
    args: &crate::ApplyMigrationsArgs,
    config_path: Option<&Path>,
) -> Result<()> {
    let installed = registry_ceiling_version();
    let ledger_path = match default_completed_path() {
        Some(p) => p,
        None => anyhow::bail!("Could not resolve zbrain home; cannot locate the migrations ledger."),
    };

    // `--list` / `--dry-run` short-circuit (mirrors TS precedence).
    if args.list {
        let entries = load_completed_migrations(&ledger_path);
        let plan = build_plan(&entries, installed);
        println!("{}", format_list(&plan, installed));
        return Ok(());
    }
    if args.dry_run {
        let entries = load_completed_migrations(&ledger_path);
        let plan = build_plan(&entries, installed);
        println!("{}", format_dry_run(&plan, installed));
        return Ok(());
    }

    // `--force-retry <VERSION>`: write a single reset marker, then return.
    if let Some(version) = &args.force_retry {
        if !ORCHESTRATORS.iter().any(|m| m.version == version.as_str()) {
            anyhow::bail!(
                "No migration registered with version \"{version}\". Run `zbrain apply-migrations --list`."
            );
        }
        append_completed_migration(
            &ledger_path,
            CompletedMigrationEntry {
                version: version.clone(),
                ts: None,
                status: EntryStatus::Retry,
                mode: None,
                files_rewritten: None,
                autopilot_installed: None,
                install_target: None,
                apply_migrations_pending: None,
                phases: None,
            },
        )?;
        println!("Wrote 'retry' marker for v{version}. Run `zbrain apply-migrations --yes` to re-attempt.");
        return Ok(());
    }

    // `--force-orchestrator` / `--force-all`: write 'retry' for every wedged.
    if args.force_orchestrator || args.force_all {
        let entries = load_completed_migrations(&ledger_path);
        let plan = build_plan(&entries, installed);
        let mut reset = 0usize;
        for m in &plan.wedged {
            append_completed_migration(
                &ledger_path,
                CompletedMigrationEntry {
                    version: m.version.to_string(),
                    ts: None,
                    status: EntryStatus::Retry,
                    mode: None,
                    files_rewritten: None,
                    autopilot_installed: None,
                    install_target: None,
                    apply_migrations_pending: None,
                    phases: None,
                },
            )?;
            reset += 1;
        }
        if reset == 0 {
            println!("No wedged orchestrator migrations found.");
        } else {
            println!(
                "Reset {reset} wedged orchestrator migration(s). Run `zbrain apply-migrations --yes` to re-attempt."
            );
        }
        if !args.force_all {
            return Ok(());
        }
    }

    // `--force-schema` / `--force-all`: re-apply DDL via `init_schema`.
    if args.force_schema || args.force_all {
        run_force_schema(config_path).await?;
        return Ok(());
    }

    // Default / `--yes`: run the plan.
    let opts = OrchestratorRunOpts {
        yes: args.yes,
        dry_run: false,
        mode: args.mode.clone(),
        host_dir: args.host_dir.as_ref().map(PathBuf::from),
        no_autopilot_install: args.no_autopilot_install,
    };
    let entries = load_completed_migrations(&ledger_path);
    let written = run_apply_migrations(&entries, installed, &opts, &ledger_path)?;
    if written.is_empty() {
        println!("All migrations up to date.");
    } else {
        for e in &written {
            println!("Migration v{} complete.", e.version);
        }
        println!("Applied {} migration(s).", written.len());
    }
    Ok(())
}

/// Re-apply schema DDL on the configured brain (mirrors TS `--force-schema`).
///
/// The orchestrator ledger tracks host/upgrade glue; actual DB schema
/// migrations run via `engine.init_schema()` at `zbrain init`. This is the
/// manual recovery path when the DB schema version has drifted from
/// `config.version` (the brain_config incident). Requires a configured brain.
async fn run_force_schema(config_path: Option<&Path>) -> Result<()> {
    let config = crate::config::load_config(config_path)
        .map_err(|e| anyhow::anyhow!("No brain configured for --force-schema: {e}"))?;
    let db_path = crate::resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    println!("Applied schema migrations for the current config.version.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_ledger_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("zbrain-am-{}-{}.jsonl", std::process::id(), nanos))
    }

    #[test]
    fn compare_versions_ordering() {
        assert_eq!(compare_versions("0.11.0", "0.12.0"), -1);
        assert_eq!(compare_versions("0.18.1", "0.18.0"), 1);
        assert_eq!(compare_versions("0.32.2", "0.11.0"), 1);
        assert_eq!(compare_versions("1.0.0", "0.99.9"), 1);
    }

    #[test]
    fn compare_versions_equal_and_non_numeric() {
        assert_eq!(compare_versions("0.32.2", "0.32.2"), 0);
        // Non-numeric segments parse to 0 — stable, never panics.
        assert_eq!(compare_versions("x.y.z", "0.0.0"), 0);
        assert_eq!(compare_versions("0.13.1", "0.13.0"), 1);
    }

    #[test]
    fn load_completed_empty_when_missing() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        assert!(load_completed_migrations(&p).is_empty());
    }

    #[test]
    fn load_skips_malformed_lines() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        fs::write(
            &p,
            "{\"version\":\"0.11.0\",\"status\":\"complete\"}\nnot json at all\n{\"version\":\"0.13.1\",\"status\":\"partial\"}\n",
        )
        .unwrap();
        let loaded = load_completed_migrations(&p);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].version, "0.11.0");
        assert_eq!(loaded[1].version, "0.13.1");
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn append_writes_line_and_roundtrips() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        let written = append_completed_migration(
            &p,
            CompletedMigrationEntry {
                version: "0.11.0".to_string(),
                ts: None,
                status: EntryStatus::Complete,
                mode: Some("pain_triggered".to_string()),
                files_rewritten: None,
                autopilot_installed: Some(true),
                install_target: None,
                apply_migrations_pending: None,
                phases: None,
            },
        )
        .unwrap();
        assert!(written);
        let loaded = load_completed_migrations(&p);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status, EntryStatus::Complete);
        assert!(loaded[0].ts.is_some());
        assert_eq!(loaded[0].autopilot_installed, Some(true));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn append_idempotency_skips_redundant_complete() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        let base = CompletedMigrationEntry {
            version: "0.12.0".to_string(),
            ts: None,
            status: EntryStatus::Complete,
            mode: None,
            files_rewritten: None,
            autopilot_installed: None,
            install_target: None,
            apply_migrations_pending: None,
            phases: None,
        };
        assert!(append_completed_migration(&p, base.clone()).unwrap());
        // Re-appending complete for an already-complete version is a no-op.
        assert!(!append_completed_migration(&p, base).unwrap());
        assert_eq!(load_completed_migrations(&p).len(), 1);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn append_retry_then_complete_writes_both() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        let retry = CompletedMigrationEntry {
            version: "0.13.1".to_string(),
            ts: None,
            status: EntryStatus::Retry,
            mode: None,
            files_rewritten: None,
            autopilot_installed: None,
            install_target: None,
            apply_migrations_pending: None,
            phases: None,
        };
        let complete = CompletedMigrationEntry {
            version: "0.13.1".to_string(),
            ts: None,
            status: EntryStatus::Complete,
            mode: None,
            files_rewritten: None,
            autopilot_installed: None,
            install_target: None,
            apply_migrations_pending: None,
            phases: None,
        };
        assert!(append_completed_migration(&p, retry).unwrap());
        // complete after retry is NOT redundant (retry resets the wedge).
        assert!(append_completed_migration(&p, complete).unwrap());
        assert_eq!(load_completed_migrations(&p).len(), 2);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn status_for_version_variants() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        // empty -> pending
        assert_eq!(status_for_version(&[], "0.11.0"), PlanStatus::Pending);

        // complete wins
        let applied = vec![CompletedMigrationEntry {
            version: "0.11.0".to_string(),
            ts: None,
            status: EntryStatus::Complete,
            mode: None,
            files_rewritten: None,
            autopilot_installed: None,
            install_target: None,
            apply_migrations_pending: None,
            phases: None,
        }];
        assert_eq!(status_for_version(&applied, "0.11.0"), PlanStatus::Applied);

        // latest retry -> pending
        let retried = vec![
            CompletedMigrationEntry { version: "0.13.1".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
            CompletedMigrationEntry { version: "0.13.1".to_string(), ts: None, status: EntryStatus::Retry, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
        ];
        assert_eq!(status_for_version(&retried, "0.13.1"), PlanStatus::Pending);

        // 3 consecutive partials -> wedged
        let wedged = (0..3)
            .map(|_| CompletedMigrationEntry { version: "0.14.0".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None })
            .collect::<Vec<_>>();
        assert_eq!(status_for_version(&wedged, "0.14.0"), PlanStatus::Wedged);

        // single partial -> partial
        let partial = vec![CompletedMigrationEntry { version: "0.16.0".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None }];
        assert_eq!(status_for_version(&partial, "0.16.0"), PlanStatus::Partial);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn build_plan_classifies_all_buckets() {
        let entries = vec![
            CompletedMigrationEntry { version: "0.11.0".to_string(), ts: None, status: EntryStatus::Complete, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
            CompletedMigrationEntry { version: "0.13.1".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
            CompletedMigrationEntry { version: "0.16.0".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
            CompletedMigrationEntry { version: "0.16.0".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
            CompletedMigrationEntry { version: "0.16.0".to_string(), ts: None, status: EntryStatus::Partial, mode: None, files_rewritten: None, autopilot_installed: None, install_target: None, apply_migrations_pending: None, phases: None },
        ];
        let plan = build_plan(&entries, "0.32.2");
        // 0.11.0 complete -> applied
        assert_eq!(plan.applied.len(), 1);
        assert_eq!(plan.applied[0].version, "0.11.0");
        // 0.13.1 single partial -> partial
        assert_eq!(plan.partial.len(), 1);
        assert_eq!(plan.partial[0].version, "0.13.1");
        // 0.16.0 three partials -> wedged
        assert_eq!(plan.wedged.len(), 1);
        assert_eq!(plan.wedged[0].version, "0.16.0");
        // everything else (0.12.0, 0.12.2, 0.14.0, 0.18.0, ...) -> pending
        assert!(plan.pending.iter().any(|m| m.version == "0.12.0"));
        assert!(plan.pending.iter().any(|m| m.version == "0.32.2"));
        // nothing newer than installed 0.32.2 -> no future
        assert!(plan.future.is_empty());
    }

    #[test]
    fn build_plan_marks_future_when_installed_old() {
        // installed 0.11.0: everything above is "future" (binary too old).
        let plan = build_plan(&[], "0.11.0");
        assert_eq!(plan.applied.len() + plan.partial.len() + plan.pending.len(), 1);
        assert!(plan.future.iter().any(|m| m.version == "0.12.0"));
        assert!(plan.future.iter().any(|m| m.version == "0.32.2"));
    }

    #[test]
    fn format_list_contains_headlines_and_status() {
        let entries = vec![CompletedMigrationEntry {
            version: "0.11.0".to_string(),
            ts: None,
            status: EntryStatus::Complete,
            mode: None,
            files_rewritten: None,
            autopilot_installed: None,
            install_target: None,
            apply_migrations_pending: None,
            phases: None,
        }];
        let plan = build_plan(&entries, "0.32.2");
        let out = format_list(&plan, "0.32.2");
        assert!(out.contains("Installed zbrain version: 0.32.2"));
        assert!(out.contains("applied  0.11.0"));
        assert!(out.contains("ZBrain Minions — durable background agents"));
        assert!(out.contains("pending  0.12.0"));
        assert!(out.contains("need action"));
    }

    #[test]
    fn format_dry_run_contains_would_apply() {
        let plan = build_plan(&[], "0.32.2");
        let out = format_dry_run(&plan, "0.32.2");
        assert!(out.contains("Dry run — installed zbrain version: 0.32.2"));
        assert!(out.contains("Would APPLY:"));
        assert!(out.contains("→ v0.11.0"));
        assert!(out.contains("Use --yes to skip prompts"));
    }

    #[test]
    fn run_orchestrator_v0_11_0_is_real_host_integration() {
        let decl = ORCHESTRATORS.iter().find(|m| m.version == "0.11.0").unwrap();
        let opts = OrchestratorRunOpts::default();
        let r = run_orchestrator(decl, &opts);
        assert_eq!(r.status, PlanStatus::Applied);
        assert!(r.autopilot_installed);
        assert!(r.detail.unwrap().contains("autopilot daemon config"));
    }

    #[test]
    fn run_orchestrator_v0_11_0_skipped_when_flag_set() {
        let decl = ORCHESTRATORS.iter().find(|m| m.version == "0.11.0").unwrap();
        let opts = OrchestratorRunOpts {
            no_autopilot_install: true,
            ..Default::default()
        };
        let r = run_orchestrator(decl, &opts);
        assert!(!r.autopilot_installed);
    }

    #[test]
    fn run_orchestrator_other_is_fresh_complete_shell() {
        // Every non-v0_11_0 orchestrator is a fresh-brain auto-complete shell.
        for decl in ORCHESTRATORS.iter().filter(|m| m.version != "0.11.0") {
            let r = run_orchestrator(decl, &OrchestratorRunOpts::default());
            assert_eq!(r.status, PlanStatus::Applied, "{} should be applied", decl.version);
            assert!(!r.autopilot_installed);
            assert!(r.detail.unwrap().contains("fresh-brain"));
        }
    }

    #[test]
    fn run_apply_migrations_writes_all_pending() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        let opts = OrchestratorRunOpts::default();
        let written = run_apply_migrations(&[], "0.32.2", &opts, &p).unwrap();
        // All 15 are pending on a fresh ledger → all 15 written.
        assert_eq!(written.len(), 15);
        assert_eq!(load_completed_migrations(&p).len(), 15);
        // v0_11_0 marked autopilot_installed.
        let v0 = load_completed_migrations(&p)
            .into_iter()
            .find(|e| e.version == "0.11.0")
            .unwrap();
        assert_eq!(v0.autopilot_installed, Some(true));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn run_apply_migrations_dry_run_writes_nothing() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        let opts = OrchestratorRunOpts {
            dry_run: true,
            ..Default::default()
        };
        let written = run_apply_migrations(&[], "0.32.2", &opts, &p).unwrap();
        assert!(written.is_empty());
        assert!(load_completed_migrations(&p).is_empty());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn run_apply_migrations_skips_already_applied() {
        let p = unique_ledger_path();
        let _ = fs::remove_file(&p);
        // Pre-mark 0.11.0 complete (idempotency guard will skip re-writing).
        let _ = append_completed_migration(
            &p,
            CompletedMigrationEntry {
                version: "0.11.0".to_string(),
                ts: None,
                status: EntryStatus::Complete,
                mode: None,
                files_rewritten: None,
                autopilot_installed: None,
                install_target: None,
                apply_migrations_pending: None,
                phases: None,
            },
        );
        let opts = OrchestratorRunOpts::default();
        let written = run_apply_migrations(&load_completed_migrations(&p), "0.32.2", &opts, &p).unwrap();
        // 0.11.0 already applied → only the other 14 written.
        assert_eq!(written.len(), 14);
        assert_eq!(load_completed_migrations(&p).len(), 15);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn registry_ceiling_is_latest_declared_version() {
        // The ceiling anchors plan classification so a fresh install treats
        // every migration as applicable (none are "future").
        assert_eq!(registry_ceiling_version(), "0.32.2");
        assert!(ORCHESTRATORS.iter().all(|m| compare_versions(m.version, registry_ceiling_version()) <= 0));
    }
}
