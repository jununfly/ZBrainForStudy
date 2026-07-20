//! `zbrain check-brain-first` — brain-first compliance gate for a single
//! SKILL.md (Rust port of the `skill-brain-first.ts` pure analyzer + the
//! `skillify-check` item-12 gate).
//!
//! Exit contract (mirrors `skillify-check` item 12, which is a REQUIRED gate):
//!   ok   → exit 0
//!   warn → exit 1   (skill must declare brain-first stance)
//!   error (missing/unreadable SKILL.md) → exit 1
//!
//! The `--json` envelope is consumed by `skillify-check` (1-6-5-9-4), which
//! spawns this command and maps `status=="ok"` → passed, else detail =
//! `summary_line` + " — " + `fix_hint`. Field names/casing match the TS
//! `BrainFirstAnalysis` contract so downstream code stays source-compatible.

use std::path::{Path, PathBuf};

use anyhow::Result;
use zbrain_core::skill_resolver::brain_first::{
    analyze_skill_brain_first, build_brain_first_fix_hint, build_brain_first_summary_line,
    BrainFirstStatus,
};
use zbrain_core::skill_resolver::skill_frontmatter::parse_skill_frontmatter;

/// Top-level `--json` envelope for `zbrain check-brain-first`.
#[derive(Debug, Clone, serde::Serialize)]
struct BrainFirstEnvelope {
    /// True iff `status == Ok` (no compliance violation).
    ok: bool,
    /// Skill name (basename of the SKILL.md's parent dir), or null on error.
    #[serde(skip_serializing_if = "Option::is_none")]
    skill: Option<String>,
    /// "ok" | "warn", or null on error.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<BrainFirstStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<zbrain_core::skill_resolver::brain_first::BrainFirstReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_patterns_matched: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    typo_hint: Option<String>,
    formerly_hardcoded_exempt: bool,
    /// Human summary line (mirrors TS `buildBrainFirstSummaryLine`).
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_line: Option<String>,
    /// Stable "Fix:" guidance, present only on warn.
    #[serde(skip_serializing_if = "Option::is_none")]
    fix_hint: Option<String>,
    /// Error code when the SKILL.md is missing/unreadable.
    #[serde(rename = "error")]
    error_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// `zbrain check-brain-first` flags.
#[derive(Debug, clap::Parser)]
pub struct CheckBrainFirstArgs {
    /// Path to the SKILL.md to check.
    #[arg(default_value = "SKILL.md")]
    pub skill_path: PathBuf,
    /// Emit a stable machine-readable JSON envelope instead of human output.
    #[arg(long)]
    pub json: bool,
}

/// Derive the skill name from the SKILL.md path: basename of its parent dir.
/// Falls back to the file stem when there is no parent (e.g. a bare SKILL.md).
fn skill_name_from_path(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string())
}

/// Dispatch `zbrain check-brain-first`.
///
/// Reads the SKILL.md, runs the pure analyzer, prints the envelope / summary
/// line, and exits via `std::process::exit` so the code reflects compliance
/// (warn → 1, error → 1, ok → 0). Mirrors `run_check_resolvable_command`.
pub fn run_check_brain_first_command(args: &CheckBrainFirstArgs) -> Result<()> {
    let path = &args.skill_path;

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            let skill = skill_name_from_path(path);
            let envelope = BrainFirstEnvelope {
                ok: false,
                skill: Some(skill),
                status: None,
                reason: None,
                external_patterns_matched: None,
                typo_hint: None,
                formerly_hardcoded_exempt: false,
                summary_line: None,
                fix_hint: None,
                error_field: Some("no_skill_md".to_string()),
                message: Some(format!("could not read SKILL.md at {}: {}", path.display(), e)),
            };
            if args.json {
                println!("{}", serde_json::to_string_pretty(&envelope)?);
            } else {
                eprintln!("{}", envelope.message.clone().unwrap_or_default());
            }
            std::process::exit(1);
        }
    };

    let skill_name = skill_name_from_path(path);
    let fm = parse_skill_frontmatter(&content);
    let analysis = analyze_skill_brain_first(&content, &skill_name, fm.as_ref());

    let is_ok = matches!(analysis.status, BrainFirstStatus::Ok);
    let summary_line = build_brain_first_summary_line(&analysis);
    let fix_hint = if is_ok {
        None
    } else {
        Some(build_brain_first_fix_hint())
    };

    let envelope = BrainFirstEnvelope {
        ok: is_ok,
        skill: Some(analysis.skill.clone()),
        status: Some(analysis.status),
        reason: Some(analysis.reason),
        external_patterns_matched: Some(analysis.external_patterns_matched.clone()),
        typo_hint: analysis.typo_hint.clone(),
        formerly_hardcoded_exempt: analysis.formerly_hardcoded_exempt,
        summary_line: Some(summary_line.clone()),
        fix_hint,
        error_field: None,
        message: None,
    };

    if args.json {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
    } else {
        println!("{summary_line}");
    }

    std::process::exit(if is_ok { 0 } else { 1 });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_skill(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_bf_{}_{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("SKILL.md");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn ok_skill_emits_ok_envelope() {
        let p = scratch_skill(
            "ok",
            "---\nname: a\n---\n\nJust does local stuff.\n",
        );
        let args = CheckBrainFirstArgs {
            skill_path: p,
            json: true,
        };
        // run_check_brain_first_command exits the process, so we test the
        // envelope shape indirectly via the analyzer + builder contract here.
        let content = std::fs::read_to_string(&args.skill_path).unwrap();
        let fm = parse_skill_frontmatter(&content);
        let a = analyze_skill_brain_first(&content, &skill_name_from_path(&args.skill_path), fm.as_ref());
        assert!(matches!(a.status, BrainFirstStatus::Ok));
        let env = BrainFirstEnvelope {
            ok: true,
            skill: Some(a.skill.clone()),
            status: Some(a.status),
            reason: Some(a.reason),
            external_patterns_matched: Some(a.external_patterns_matched.clone()),
            typo_hint: a.typo_hint.clone(),
            formerly_hardcoded_exempt: a.formerly_hardcoded_exempt,
            summary_line: Some(build_brain_first_summary_line(&a)),
            fix_hint: None,
            error_field: None,
            message: None,
        };
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains("\"ok\":true"));
        assert!(s.contains("\"status\":\"ok\""));
    }
}
