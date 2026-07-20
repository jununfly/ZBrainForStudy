//! `zbrain check-resolvable` — skill-tree integrity gate (Rust port).
//!
//! Thin wrapper over `zbrain_core::skill_resolver::check_resolvable`.
//! Exit contract (D-CX-3, mirrored from the TS command):
//!   default:  exit 0 unless there are error-severity issues
//!   --strict: exit 0 unless there are errors OR warnings
//!
//! Slice 1-6-5-4 covers checks 1-4 (reachability, MECE overlap/gap, DRY).
//! `--fix` / `--dry-run` (the dry-fix write path, roadmap 1-6-5-8) ARE wired:
//! `--fix` writes approved DRY/INSERT fixes back to SKILL.md under safety
//! gates; `--dry-run` previews them without writing. Both then run the
//! read-only `check_resolvable` on the (possibly mutated) skills dir.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use zbrain_core::skill_resolver::check_resolvable::{check_resolvable, ResolvableIssue, ResolvableReport};
use zbrain_core::skill_resolver::dry_fix::{auto_fix_dry_violations, AutoFixOptions, AutoFixReport, FixStatus};
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
    /// DRY auto-fix outcome (null unless --fix / --dry-run was passed).
    #[serde(rename = "autoFix")]
    auto_fix: Option<AutoFixReport>,
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
    /// Apply DRY auto-fixes (cross-cutting DRY REPLACE + brain-first INSERT)
    /// before checking. Writes are gated by safety checks (working-tree dirt,
    /// code fences, existing delegation, ambiguous match). See roadmap 1-6-5-8.
    #[arg(long)]
    pub fix: bool,
    /// With --fix, preview only (no writes).
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

    let (env_el, code) = run_check_resolvable_core(&skills_dir, args);

    if args.json {
        println!("{}", serde_json::to_string_pretty(&env_el)?);
    } else {
        let report = env_el.report.as_ref().expect("report present on success");
        render_human(report, args.verbose, args.strict);
        if let Some(af) = &env_el.auto_fix {
            render_autofix_human(af);
        }
    }

    std::process::exit(code);
}

/// Pure resolution + auto-fix step (no `std::process::exit`), so it is
/// unit-testable. Runs the `--fix`/`--dry-run` write path when requested,
/// then runs the read-only `check_resolvable` on the (possibly mutated)
/// skills dir and returns the envelope + exit code.
fn run_check_resolvable_core(skills_dir: &Path, args: &CheckResolvableArgs) -> (Envelope, i32) {
    // `--fix` runs the write path; `--dry-run` previews without writing.
    let auto_fix = if args.fix || args.dry_run {
        Some(auto_fix_dry_violations(
            skills_dir,
            &AutoFixOptions {
                dry_run: args.dry_run,
            },
        ))
    } else {
        None
    };

    let report = check_resolvable(skills_dir);

    let env_el = Envelope {
        ok: resolve_exit_code(&report, args.strict) == 0,
        skills_dir: Some(skills_dir.to_string_lossy().into_owned()),
        report: Some(report.clone()),
        auto_fix,
        deferred: Vec::new(),
        error_field: None,
        message: None,
    };

    (env_el, resolve_exit_code(&report, args.strict))
}

/// Human-readable summary of the `--fix` / `--dry-run` outcome.
fn render_autofix_human(af: &AutoFixReport) {
    if af.fixed.is_empty() && af.skipped.is_empty() {
        println!("\nauto_fix: nothing to do");
        return;
    }
    let applied = af
        .fixed
        .iter()
        .filter(|o| o.status == FixStatus::Applied)
        .count();
    let proposed = af
        .fixed
        .iter()
        .filter(|o| o.status == FixStatus::Proposed)
        .count();
    let skipped = af.skipped.len();
    if applied > 0 {
        println!("\nauto_fix: applied {applied} fix(es)");
    }
    if proposed > 0 {
        println!("\nauto_fix: proposed {proposed} fix(es) (dry-run, no writes)");
    }
    if skipped > 0 {
        println!("auto_fix: skipped {skipped} (safety gate / ambiguous / already delegated)");
    }
    for o in af.fixed.iter() {
        let verb = if o.status == FixStatus::Proposed {
            "proposed"
        } else {
            "applied"
        };
        println!("  • {verb:<8} {} — {}", o.skill, o.pattern_label);
    }
    for o in af.skipped.iter() {
        let reason = o
            .reason
            .map(|r| serde_json::to_string(&r).unwrap_or_default())
            .unwrap_or_default();
        println!("  • skipped   {} — {} ({reason})", o.skill, o.pattern_label);
    }
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

    // --- 1-6-5-8-4: --fix / --dry-run wiring ---

    fn cli_scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_cli_{}", name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cli_git_committed(root: &Path, rel: &str, content: &str) {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
        let run = |a: &[&str]| {
            std::process::Command::new("git")
                .args(a)
                .current_dir(root)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["add", rel]);
        run(&[
            "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "c",
        ]);
    }

    #[test]
    fn fix_applies_dry_violation_and_populates_envelope() {
        let root = cli_scratch("fixapply");
        cli_git_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nWe follow the iron law of back-linking here.\n",
        );
        let args = CheckResolvableArgs {
            json: false,
            fix: true,
            dry_run: false,
            verbose: false,
            strict: false,
            skills_dir: Some(root.clone()),
        };
        let (env_el, _code) = run_check_resolvable_core(&root, &args);
        let af = env_el.auto_fix.expect("autoFix populated");
        assert!(!af.fixed.is_empty(), "expected an applied fix: {:?}", af);
        assert_eq!(af.fixed[0].status, FixStatus::Applied);
        let new = std::fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        assert!(new.contains("Convention"));
    }

    #[test]
    fn dry_run_proposes_only_and_leaves_file() {
        let root = cli_scratch("fixdry");
        cli_git_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nWe follow the iron law of back-linking here.\n",
        );
        let args = CheckResolvableArgs {
            json: false,
            fix: false,
            dry_run: true,
            verbose: false,
            strict: false,
            skills_dir: Some(root.clone()),
        };
        let (env_el, _code) = run_check_resolvable_core(&root, &args);
        let af = env_el.auto_fix.expect("autoFix populated");
        assert!(!af.fixed.is_empty());
        assert_eq!(af.fixed[0].status, FixStatus::Proposed);
        // File untouched in dry-run.
        let new = std::fs::read_to_string(root.join("a/SKILL.md")).unwrap();
        assert!(!new.contains("Convention"));
    }

    #[test]
    fn no_fix_flag_leaves_auto_fix_null() {
        let root = cli_scratch("fixnone");
        cli_git_committed(
            &root,
            "a/SKILL.md",
            "---\nname: a\n---\n\nWe follow the iron law of back-linking here.\n",
        );
        let args = CheckResolvableArgs {
            json: false,
            fix: false,
            dry_run: false,
            verbose: false,
            strict: false,
            skills_dir: Some(root.clone()),
        };
        let (env_el, _code) = run_check_resolvable_core(&root, &args);
        assert!(env_el.auto_fix.is_none());
    }
}
