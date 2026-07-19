//! `zbrain check-resolvable` — skill-tree integrity gate (Rust port).
//!
//! Thin wrapper over `zbrain_core::skill_resolver::check_resolvable`.
//! Exit contract (D-CX-3, mirrored from the TS command):
//!   default:  exit 0 unless there are error-severity issues
//!   --strict: exit 0 unless there are errors OR warnings
//!
//! Slice 1-6-5-4 covers checks 1-4 (reachability, MECE overlap/gap, DRY).
//! `--fix` / `--dry-run` (the dry-fix write path, roadmap 1-6-5-8) are NOT
//! yet wired in Rust — passing them now prints a clear "not yet implemented"
//! message and exits 1, rather than exposing a lying no-op interface.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;
use zbrain_core::skill_resolver::check_resolvable::{check_resolvable, ResolvableIssue, ResolvableReport};
use zbrain_core::skill_resolver::repo_root::{
    auto_detect_hint_read_only, auto_detect_skills_dir_read_only, SkillsDirSource,
};

/// Stable JSON envelope field. Kept for parity with the TS `--json` output so
/// downstream consumers that read `.deferred[]` keep working. Currently empty
/// (Checks 5/6 shipped as real implementations; nothing deferred).
#[derive(Debug, Clone, serde::Serialize)]
struct DeferredCheck {
    check: u8,
    name: String,
    issue: String,
}

/// Top-level `--json` envelope (mirrors the TS `Envelope` shape).
#[derive(Debug, Clone, serde::Serialize)]
struct Envelope {
    ok: bool,
    #[serde(rename = "skillsDir")]
    skills_dir: Option<String>,
    report: Option<ResolvableReport>,
    /// DRY auto-fix outcome. Always `null` until roadmap 1-6-5-8 wires --fix.
    #[serde(rename = "autoFix")]
    auto_fix: Option<Value>,
    deferred: Vec<DeferredCheck>,
    #[serde(rename = "error")]
    error_field: Option<String>,
    message: Option<String>,
}

/// `zbrain check-resolvable` flags.
#[derive(Debug, clap::Parser)]
pub struct CheckResolvableArgs {
    /// Emit a stable machine-readable JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,
    /// Apply DRY auto-fixes before checking. NOT YET IMPLEMENTED in the Rust
    /// port (see roadmap 1-6-5-8). Passing it exits 1 with a clear message.
    #[arg(long)]
    pub fix: bool,
    /// With --fix, preview only (no writes). NOT YET IMPLEMENTED.
    #[arg(long)]
    pub dry_run: bool,
    /// Show passing checks and the deferred-check note.
    #[arg(long)]
    pub verbose: bool,
    /// Treat warnings as errors (promote warnings to exit 1).
    #[arg(long)]
    pub strict: bool,
    /// Override the auto-detected skills/ directory.
    #[arg(long)]
    pub skills_dir: Option<PathBuf>,
}

/// Dispatch `zbrain check-resolvable`.
///
/// Returns `Result<()>` for early I/O errors; the normal resolution path ends
/// via `std::process::exit` (mirrors `doctor`) so the exit code reflects the
/// report's error/warning severity under `--strict`.
pub async fn run_check_resolvable_command(
    args: &CheckResolvableArgs,
    _config_path: Option<&Path>,
) -> Result<()> {
    // --fix / --dry-run are not yet implemented (roadmap 1-6-5-8). Refuse
    // rather than expose a lying no-op flag.
    if args.fix || args.dry_run {
        eprintln!(
            "zbrain check-resolvable --fix / --dry-run is not yet implemented in the Rust port.\n\
             It is tracked as roadmap 1-6-5-8 (dry-fix slice). Re-run without --fix."
        );
        std::process::exit(1);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env: HashMap<String, String> = std::env::vars().collect();

    let (dir, error, message, source) = resolve_skills_dir(args, &cwd, &env);

    if error.is_some() {
        let env_el = Envelope {
            ok: false,
            skills_dir: None,
            report: None,
            auto_fix: None,
            deferred: Vec::new(),
            error_field: error.clone(),
            message: message.clone(),
        };
        if args.json {
            println!("{}", serde_json::to_string_pretty(&env_el)?);
        } else {
            eprintln!("{}", message.unwrap_or_default());
        }
        std::process::exit(1);
    }

    let skills_dir = dir.expect("skills dir present when error is None");
    if !args.json && source != Some(SkillsDirSource::EnvExplicit) {
        if let Some(msg) = &message {
            println!("{msg}");
        }
    }

    let report = check_resolvable(&skills_dir);

    let env_el = Envelope {
        ok: resolve_exit_code(&report, args.strict) == 0,
        skills_dir: Some(skills_dir.to_string_lossy().into_owned()),
        report: Some(report.clone()),
        auto_fix: None,
        deferred: Vec::new(),
        error_field: None,
        message: None,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&env_el)?);
    } else {
        render_human(&report, args.verbose, args.strict);
    }

    std::process::exit(resolve_exit_code(&report, args.strict));
}

/// Pure exit-code decision (D-CX-3):
///   default: 0 iff no errors (warnings alone never fail)
///   --strict: 0 iff no errors AND no warnings
/// Returns 0 (clean) or 1 (fail) so the caller can `std::process::exit(code)`.
fn resolve_exit_code(report: &ResolvableReport, strict: bool) -> i32 {
    let ok = if strict {
        report.errors.is_empty() && report.warnings.is_empty()
    } else {
        report.errors.is_empty()
    };
    if ok {
        0
    } else {
        1
    }
}

/// Resolve the skills directory from `--skills-dir` (explicit) or the
/// read-only auto-detect (which adds the install-path fallback). Returns
/// `(dir, error, message, source)` where `error`/`message` are set together
/// when auto-detect fails.
fn resolve_skills_dir(
    args: &CheckResolvableArgs,
    cwd: &Path,
    env: &HashMap<String, String>,
) -> (Option<PathBuf>, Option<String>, Option<String>, Option<SkillsDirSource>) {
    if let Some(sd) = &args.skills_dir {
        let dir = if sd.is_absolute() {
            sd.clone()
        } else {
            cwd.join(sd)
        };
        return (Some(dir), None, None, Some(SkillsDirSource::EnvExplicit));
    }

    let detected = auto_detect_skills_dir_read_only(cwd, env);
    if detected.dir.is_none() {
        let message = format!(
            "Could not auto-detect skills/ with a RESOLVER.md or AGENTS.md.\n\
             Priority order:\n{}\n\
             Fix: export ZBRAIN_SKILLS_DIR=<path>, OPENCLAW_WORKSPACE=<path>, or pass --skills-dir <path>.",
            auto_detect_hint_read_only()
        );
        return (None, Some("no_skills_dir".to_string()), Some(message), None);
    }

    let source_label = match detected.source {
        Some(SkillsDirSource::EnvExplicit) => "$ZBRAIN_SKILLS_DIR (explicit operator override)",
        Some(SkillsDirSource::RepoRoot) => "repo root skills/",
        Some(SkillsDirSource::OpenclawWorkspaceEnv) => "$OPENCLAW_WORKSPACE/skills",
        Some(SkillsDirSource::OpenclawWorkspaceEnvRoot) => {
            "$OPENCLAW_WORKSPACE (AGENTS.md at workspace root)"
        }
        Some(SkillsDirSource::OpenclawWorkspaceHome) => "~/.openclaw/workspace/skills",
        Some(SkillsDirSource::OpenclawWorkspaceHomeRoot) => {
            "~/.openclaw/workspace (AGENTS.md at workspace root)"
        }
        Some(SkillsDirSource::CwdWalkUp) => "skills/ found by walking up from cwd (v0.33)",
        Some(SkillsDirSource::CwdSkills) => "./skills",
        Some(SkillsDirSource::InstallPath) => "zbrain install path (read-only fallback)",
        None => "unknown",
    };
    let dir = detected.dir.clone().expect("dir present in this branch");
    let msg = format!(
        "Auto-detected skills directory from {source_label}: {}",
        dir.display()
    );
    (Some(dir), None, Some(msg), detected.source)
}

/// Human-readable output, mirroring the TS `renderHuman` formatting.
fn render_human(report: &ResolvableReport, verbose: bool, strict: bool) {
    if report.errors.is_empty() && report.warnings.is_empty() {
        println!(
            "resolver_health: OK — {} skills, all reachable",
            report.summary.total_skills
        );
    } else {
        let status = if !report.errors.is_empty() {
            "FAIL"
        } else if strict && !report.warnings.is_empty() {
            "FAIL (strict: warnings promoted)"
        } else {
            "WARN"
        };
        let total = report.errors.len() + report.warnings.len();
        println!(
            "resolver_health: {} — {} issue(s): {} error(s), {} warning(s)",
            status,
            total,
            report.errors.len(),
            report.warnings.len()
        );
        for iss in report.errors.iter().chain(report.warnings.iter()) {
            println!("{}", format_issue_line(iss));
        }
        if report.errors.is_empty() && !report.warnings.is_empty() && !strict {
            println!("\n(warnings are advisory; run with --strict to fail CI on warnings.)");
        }
    }

    if verbose {
        println!("Deferred: (none — Checks 5/6 are implemented and not deferred)");
    }
}

/// One-line rendering of an issue: `  • <type> <skill> <action>`.
fn format_issue_line(iss: &ResolvableIssue) -> String {
    let type_s = iss.issue_type.as_str();
    format!("  • {type_s:<18} {:<24} {}", iss.skill, iss.action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbrain_core::skill_resolver::check_resolvable::{IssueType, ResolvableIssue, Severity};

    fn issue(severity: Severity) -> ResolvableIssue {
        ResolvableIssue {
            issue_type: IssueType::MeceGap,
            severity,
            skill: "x".to_string(),
            message: "m".to_string(),
            action: "a".to_string(),
            fix: None,
        }
    }

    fn report_with(errors: usize, warnings: usize) -> ResolvableReport {
        ResolvableReport {
            ok: errors == 0 && warnings == 0,
            errors: (0..errors).map(|_| issue(Severity::Error)).collect(),
            warnings: (0..warnings).map(|_| issue(Severity::Warning)).collect(),
            issues: Vec::new(),
            summary: Default::default(),
        }
    }

    #[test]
    fn exit_code_default_fails_only_on_errors() {
        // clean
        assert_eq!(resolve_exit_code(&report_with(0, 0), false), 0);
        // warnings only -> exit 0 in default mode (advisory)
        assert_eq!(resolve_exit_code(&report_with(0, 3), false), 0);
        // any error -> exit 1
        assert_eq!(resolve_exit_code(&report_with(1, 0), false), 1);
        assert_eq!(resolve_exit_code(&report_with(2, 5), false), 1);
    }

    #[test]
    fn exit_code_strict_promotes_warnings() {
        assert_eq!(resolve_exit_code(&report_with(0, 0), true), 0);
        // warnings flip the exit code under --strict
        assert_eq!(resolve_exit_code(&report_with(0, 1), true), 1);
        // errors still flip it
        assert_eq!(resolve_exit_code(&report_with(1, 0), true), 1);
    }
}
