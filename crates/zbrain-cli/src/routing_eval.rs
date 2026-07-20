//! `zbrain routing-eval` — Check 5 standalone CLI verb (Rust port).
//!
//! Slice 1-6-5-6-6. Parity with `src/commands/routing-eval.ts` on exit codes:
//!   0  all fixtures pass (top-1 accuracy = 1.0, no ambiguity, no false
//!      positives, no lint issues, no malformed lines)
//!   1  any failure (miss / ambiguous / false positive / lint / malformed)
//!   2  setup error (no skills dir, no resolver file)
//!
//! Layer B (`--llm`) is NOT accepted: the Rust CLI refuses the flag (exit 1)
//! for honesty instead of silently ignoring it. The TS line accepted `--llm`
//! as a forward-compat placeholder; we deliberately do not (roadmap
//! decision 1-6-5-6-6).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use zbrain_core::skill_resolver::repo_root::{
    auto_detect_hint_read_only, auto_detect_skills_dir_read_only, SkillsDirSource,
};
use zbrain_core::skill_resolver::routing_eval::{
    index_resolver_triggers, lint_routing_fixtures, load_routing_fixtures, run_routing_eval,
    FixtureLintIssue, MalformedFixture, RoutingCaseResult, RoutingFixture, RoutingOutcome,
    RoutingReport,
};
use zbrain_core::skill_resolver::trigger_index::{
    entries_to_resolver_content, find_primary_resolver_path, load_skill_trigger_index,
};

/// Top-level `--json` envelope (mirrors the TS `RoutingEvalEnvelope` shape).
#[derive(Debug, Clone, Serialize)]
struct Envelope {
    ok: bool,
    #[serde(rename = "skillsDir")]
    skills_dir: Option<String>,
    #[serde(rename = "resolverFile")]
    resolver_file: Option<String>,
    report: Option<RoutingReport>,
    #[serde(rename = "lintIssues")]
    lint_issues: Vec<FixtureLintIssue>,
    #[serde(rename = "malformedFixtures")]
    malformed_fixtures: Vec<MalformedFixture>,
    error: Option<String>,
    message: Option<String>,
}

/// `zbrain routing-eval` flags.
#[derive(Debug, clap::Parser)]
pub struct RoutingEvalArgs {
    /// Emit a stable machine-readable JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,
    /// Placeholder for Layer B LLM tie-break. NOT implemented in Rust — the
    /// flag is refused (exit 1) for honesty. See roadmap decision 1-6-5-6-6.
    #[arg(long)]
    pub llm: bool,
    /// Override the auto-detected skills/ directory.
    #[arg(long)]
    pub skills_dir: Option<PathBuf>,
}

/// Dispatch `zbrain routing-eval`.
///
/// Returns `Result<()>` for early I/O errors; the normal resolution path ends
/// via `std::process::exit` so the exit code reflects the eval outcome.
pub async fn run_routing_eval_command(
    args: &RoutingEvalArgs,
    _config_path: Option<&Path>,
) -> Result<()> {
    // --llm is refused (honesty; not a lying no-op). Exit 1 so CI treats a
    // request to run the unbuilt Layer B as a failure, not a silent pass.
    if args.llm {
        eprintln!(
            "zbrain routing-eval --llm is not implemented in the Rust port.\n\
             Layer B (LLM tie-break) is not yet built; the flag is refused for honesty.\n\
             Re-run without --llm to run the structural (Layer A) eval only."
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
            resolver_file: None,
            report: None,
            lint_issues: Vec::new(),
            malformed_fixtures: Vec::new(),
            error: error.clone(),
            message: message.clone(),
        };
        if args.json {
            println!("{}", serde_json::to_string_pretty(&env_el)?);
        } else {
            eprintln!("{}", message.unwrap_or_default());
        }
        std::process::exit(2);
    }

    let skills_dir = dir.expect("skills dir present when error is None");
    if !args.json && source != Some(SkillsDirSource::EnvExplicit) {
        if let Some(msg) = &message {
            println!("{msg}");
        }
    }

    // v0.41.11: route through the shared `load_skill_trigger_index` primitive
    // so this CLI sees the same merged index that `check-resolvable` sees
    // (UNION of frontmatter `triggers:` + RESOLVER.md / AGENTS.md rows).
    let trigger_entries = load_skill_trigger_index(&skills_dir);
    let resolver_file = find_primary_resolver_path(&skills_dir);
    // Allow operation when frontmatter triggers populate the index even if
    // no RESOLVER.md / AGENTS.md exists. Only fail when BOTH surfaces empty.
    if resolver_file.is_none() && trigger_entries.is_empty() {
        let msg = format!(
            "RESOLVER.md / AGENTS.md not found in {} or its parent (and no SKILL.md frontmatter declares triggers:).",
            skills_dir.display()
        );
        let env_el = Envelope {
            ok: false,
            skills_dir: Some(skills_dir.to_string_lossy().into_owned()),
            resolver_file: None,
            report: None,
            lint_issues: Vec::new(),
            malformed_fixtures: Vec::new(),
            error: Some("no_resolver".to_string()),
            message: Some(msg.clone()),
        };
        if args.json {
            println!("{}", serde_json::to_string_pretty(&env_el)?);
        } else {
            eprintln!("{}", msg);
        }
        std::process::exit(2);
    }

    // Synthesize a single resolver-content string from the unified entry list
    // so `run_routing_eval`'s public string-content API is unchanged.
    let resolver_content = entries_to_resolver_content(&trigger_entries);
    let index = index_resolver_triggers(&resolver_content);

    let loaded = load_routing_fixtures(&skills_dir);
    let lint_issues = lint_routing_fixtures(&loaded.fixtures, &index);
    let report = run_routing_eval(&resolver_content, &loaded.fixtures);

    let clean = lint_issues.is_empty()
        && loaded.malformed.is_empty()
        && report.missed == 0
        && report.ambiguous == 0
        && report.false_positives == 0;

    let env_el = Envelope {
        ok: clean,
        skills_dir: Some(skills_dir.to_string_lossy().into_owned()),
        resolver_file: resolver_file.map(|p| p.to_string_lossy().into_owned()),
        report: Some(report.clone()),
        lint_issues: lint_issues.clone(),
        malformed_fixtures: loaded.malformed.clone(),
        error: None,
        message: None,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&env_el)?);
    } else {
        render_human(&report, &lint_issues, &loaded.malformed);
    }

    std::process::exit(if clean { 0 } else { 1 });
}

/// Resolve the skills directory from `--skills-dir` (explicit) or the
/// read-only auto-detect. Returns `(dir, error, message, source)` where
/// `error`/`message` are set together when auto-detect fails.
fn resolve_skills_dir(
    args: &RoutingEvalArgs,
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

/// Human-readable output, mirroring the TS `runRoutingEvalCli` formatting.
fn render_human(report: &RoutingReport, lint_issues: &[FixtureLintIssue], malformed: &[MalformedFixture]) {
    let pct = (report.top1_accuracy * 100.0).round();
    let clean = lint_issues.is_empty()
        && malformed.is_empty()
        && report.missed == 0
        && report.ambiguous == 0
        && report.false_positives == 0;
    let header = if clean { "routing-eval: OK" } else { "routing-eval: ISSUES" };
    println!(
        "{} — {} case(s), {}% top-1 accuracy",
        header, report.total_cases, pct
    );
    if report.missed > 0 {
        println!("  • {} missed", report.missed);
    }
    if report.ambiguous > 0 {
        println!("  • {} ambiguous", report.ambiguous);
    }
    if report.false_positives > 0 {
        println!(
            "  • {} false positives (negative cases that matched)",
            report.false_positives
        );
    }
    for d in report.details.iter().filter(|x| x.outcome != RoutingOutcome::Pass) {
        let exp = d
            .fixture
            .expected_skill
            .clone()
            .unwrap_or_else(|| "none".to_string());
        let note = d
            .note
            .as_ref()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        println!(
            "  [{}] \"{}\" (expected={}){}",
            d.outcome.as_str(),
            d.fixture.intent,
            exp,
            note
        );
    }
    for lint in lint_issues {
        println!(
            "  [lint:{}] \"{}\" — {}",
            lint.reason.as_str(),
            lint.fixture.intent,
            lint.detail
        );
    }
    for m in malformed {
        println!("  [malformed] {}:{} — {}", m.file, m.line, m.error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_camel_case_fields() {
        let report = RoutingReport {
            total_cases: 1,
            top1_accuracy: 1.0,
            passed: 1,
            missed: 0,
            ambiguous: 0,
            false_positives: 0,
            details: vec![RoutingCaseResult {
                fixture: RoutingFixture {
                    intent: "intent".to_string(),
                    expected_skill: Some("skill".to_string()),
                    ambiguous_with: None,
                    source: None,
                },
                outcome: RoutingOutcome::Pass,
                matched_skills: vec!["skill".to_string()],
                note: None,
            }],
        };
        let env = Envelope {
            ok: true,
            skills_dir: Some("/x".to_string()),
            resolver_file: Some("/x/RESOLVER.md".to_string()),
            report: Some(report),
            lint_issues: vec![],
            malformed_fixtures: vec![],
            error: None,
            message: None,
        };
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"skillsDir\""));
        assert!(s.contains("\"resolverFile\""));
        assert!(s.contains("\"lintIssues\""));
        assert!(s.contains("\"malformedFixtures\""));
        assert!(s.contains("\"totalCases\""));
        assert!(s.contains("\"top1Accuracy\""));
    }
}
