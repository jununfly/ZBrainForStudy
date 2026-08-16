//! check-resolvable — core resolver validation (checks 1-4).
//!
//! Ported from `src/core/check-resolvable.ts`. Validates that all skills
//! are reachable from RESOLVER.md / frontmatter triggers, detects MECE
//! violations, and checks for DRY issues + SKILLIFY_STUB sentinels.
//!
//! Not yet ported (tracked as later slices of roadmap 1-6-5, registered in
//! `docs/plans/MIGRATION.md`): Check 5 (trigger routing eval) and Check 6
//! (brain filing audit) surface as warnings; `--fix` auto-repair of DRY
//! violations is a separate write path. This core covers the deterministic,
//! read-only structural checks so `zbrain check-resolvable` is useful
//! end-to-end for the primary "is my skill tree resolvable?" question.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::skill_resolver::resolver_filenames::RESOLVER_FILENAMES_LABEL;
use crate::skill_resolver::skill_frontmatter::parse_skill_frontmatter;
use crate::skill_resolver::skill_manifest::{load_or_derive_manifest, ManifestEntry};
use crate::skill_resolver::trigger_index::{
    entries_to_resolver_content, find_primary_resolver_path, load_skill_trigger_index,
    SkillTriggerEntry,
};
use crate::skill_resolver::routing_eval::{
    index_resolver_triggers, lint_routing_fixtures, load_routing_fixtures, run_routing_eval,
    RoutingOutcome,
};
use crate::skill_resolver::filing_audit::{run_filing_audit, FilingIssueType};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixType {
    AddTrigger,
    RemoveTrigger,
    AddFrontmatter,
    CreateStub,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvableFix {
    /// Serialized as `type` to match the TS envelope field name.
    #[serde(rename = "type")]
    pub fix_type: FixType,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(rename = "skill_path", skip_serializing_if = "Option::is_none")]
    pub skill_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    Unreachable,
    MeceOverlap,
    MeceGap,
    DryViolation,
    MissingFile,
    OrphanTrigger,
    RoutingMiss,
    RoutingAmbiguous,
    RoutingFalsePositive,
    RoutingFixtureLint,
    FilingMissingWritesTo,
    FilingUnknownDirectory,
    SkillifyStubUnreplaced,
}

impl IssueType {
    /// Stable lower-case string form, used for human-readable output and
    /// matching the TS `issue.type` display.
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueType::Unreachable => "unreachable",
            IssueType::MeceOverlap => "mece_overlap",
            IssueType::MeceGap => "mece_gap",
            IssueType::DryViolation => "dry_violation",
            IssueType::MissingFile => "missing_file",
            IssueType::OrphanTrigger => "orphan_trigger",
            IssueType::RoutingMiss => "routing_miss",
            IssueType::RoutingAmbiguous => "routing_ambiguous",
            IssueType::RoutingFalsePositive => "routing_false_positive",
            IssueType::RoutingFixtureLint => "routing_fixture_lint",
            IssueType::FilingMissingWritesTo => "filing_missing_writes_to",
            IssueType::FilingUnknownDirectory => "filing_unknown_directory",
            IssueType::SkillifyStubUnreplaced => "skillify_stub_unreplaced",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvableIssue {
    /// Serialized as `type` to match the TS envelope field name.
    #[serde(rename = "type")]
    pub issue_type: IssueType,
    pub severity: Severity,
    pub skill: String,
    pub message: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<ResolvableFix>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Summary {
    pub total_skills: usize,
    pub reachable: usize,
    pub unreachable: usize,
    pub overlaps: usize,
    pub gaps: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ResolvableReport {
    pub ok: bool,
    pub errors: Vec<ResolvableIssue>,
    pub warnings: Vec<ResolvableIssue>,
    /// Deprecated: `[...errors, ...warnings]`. Kept for envelope parity.
    pub issues: Vec<ResolvableIssue>,
    pub summary: Summary,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Skills that intentionally overlap with many others (always-on, routers).
const OVERLAP_WHITELIST: [&str; 3] = ["ingest", "signal-detector", "brain-ops"];

/// Sentinel emitted by `zbrain skillify scaffold`; presence means a scaffolded
/// skill shipped without a real implementation.
const SKILLIFY_STUB_SENTINEL: &str =
    "SKILLIFY_STUB: replace before running check-resolvable --strict";

/// Proximity window (lines) within which a delegation reference suppresses a
/// DRY match.
pub const DRY_PROXIMITY_LINES: usize = 40;

pub struct CrossCuttingPattern {
    pub pattern: &'static str,
    pub conventions: &'static [&'static str],
    pub label: &'static str,
}

pub const CROSS_CUTTING_PATTERNS: &[CrossCuttingPattern] = &[
    CrossCuttingPattern {
        pattern: r"iron\s*law.*back-?link",
        conventions: &["conventions/quality.md"],
        label: "Iron Law back-linking",
    },
    CrossCuttingPattern {
        pattern: r"citation.*format.*\[Source:",
        conventions: &["conventions/quality.md"],
        label: "citation format rules",
    },
    CrossCuttingPattern {
        pattern: r"notability.*gate",
        conventions: &["conventions/quality.md", "_brain-filing-rules.md"],
        label: "notability gate",
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationRef {
    /// normalized relative path, e.g. "conventions/quality.md"
    pub convention: String,
    /// 1-indexed line number
    pub line: usize,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fence_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^---\n([\s\S]*?)\n---").unwrap())
}
fn triggers_block_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?m)^triggers:\s*\n((?:\s+-\s+.+\n?)*)").unwrap())
}
fn delegation_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"`skills/((?:conventions/[^`]+\.md)|(?:_brain-filing-rules\.md))`").unwrap()
    })
}

/// Simple YAML frontmatter parser — extracts the `triggers:` array if present.
pub fn extract_triggers(skill_content: &str) -> Vec<String> {
    let fm = match fence_re().captures(skill_content) {
        Some(c) => c.get(1).unwrap().as_str().to_string(),
        None => return Vec::new(),
    };
    let block = match triggers_block_re().captures(&fm) {
        Some(c) => c.get(1).unwrap().as_str().to_string(),
        None => return Vec::new(),
    };
    block
        .split('\n')
        .map(|l| {
            l.trim()
                .trim_start_matches(|c: char| c == '-' || c.is_whitespace())
                .trim()
                .trim_matches(|c| c == '"' || c == '\u{27}')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// Extract delegation references to known convention files.
pub fn extract_delegation_targets(content: &str) -> Vec<DelegationRef> {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut refs = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        for c in delegation_re().captures_iter(line) {
            if let Some(m) = c.get(1) {
                refs.push(DelegationRef {
                    convention: m.as_str().to_string(),
                    line: i + 1,
                });
            }
        }
    }
    refs
}

// ---------------------------------------------------------------------------
// Main function
// ---------------------------------------------------------------------------

/// Validate that all skills are reachable from RESOLVER.md / frontmatter
/// triggers, detect MECE violations, and check for DRY issues.
pub fn check_resolvable(skills_dir: &Path) -> ResolvableReport {
    let mut issues: Vec<ResolvableIssue> = Vec::new();

    let trigger_entries: Vec<SkillTriggerEntry> = load_skill_trigger_index(skills_dir);

    // Primary RESOLVER.md path is still needed for error messages and
    // --fix targets. When neither RESOLVER.md / AGENTS.md exists AND no
    // skill ships frontmatter triggers, the resolver tree is fully empty.
    let resolver_path_or_null = find_primary_resolver_path(skills_dir);
    let resolver_path: PathBuf = match &resolver_path_or_null {
        Some(p) => p.clone(),
        None => skills_dir.join("RESOLVER.md"),
    };
    if resolver_path_or_null.is_none() && trigger_entries.is_empty() {
        let suggested = skills_dir.join("RESOLVER.md");
        let missing = ResolvableIssue {
            issue_type: IssueType::MissingFile,
            severity: Severity::Error,
            skill: RESOLVER_FILENAMES_LABEL.to_string(),
            message: format!(
                "{} not found in {} or its parent (and no SKILL.md frontmatter declares triggers:)",
                RESOLVER_FILENAMES_LABEL,
                skills_dir.display()
            ),
            action: format!(
                "Create {} with skill routing tables, or add 'triggers:' to each SKILL.md frontmatter",
                suggested.display()
            ),
            fix: Some(ResolvableFix {
                fix_type: FixType::CreateStub,
                file: suggested.to_string_lossy().to_string(),
                section: None,
                skill_path: None,
            }),
        };
        return ResolvableReport {
            ok: false,
            errors: vec![missing.clone()],
            warnings: Vec::new(),
            issues: vec![missing],
            summary: Summary::default(),
        };
    }

    let resolver_content = entries_to_resolver_content(&trigger_entries);
    let manifest: Vec<ManifestEntry> = load_or_derive_manifest(skills_dir).skills;

    // Build lookup sets.
    let resolver_skill_paths: std::collections::HashSet<String> = trigger_entries
        .iter()
        .filter(|e| !e.is_gstack)
        .map(|e| e.skill_path.clone())
        .collect();

    // 1. Check every manifest skill is reachable.
    let mut reachable = 0usize;
    let mut unreachable = 0usize;
    for skill in &manifest {
        let expected_path = format!("skills/{}", skill.path);
        if resolver_skill_paths.contains(&expected_path) {
            reachable += 1;
            continue;
        }
        let name_in_resolver = trigger_entries.iter().any(|e| {
            e.skill_path.contains(&skill.name) || e.trigger.contains(&skill.name)
        });
        if name_in_resolver {
            reachable += 1;
        } else {
            unreachable += 1;
            issues.push(ResolvableIssue {
                issue_type: IssueType::Unreachable,
                severity: Severity::Error,
                skill: skill.name.clone(),
                message: format!(
                    "Skill '{}' is in manifest but has no trigger row in {}",
                    skill.name, RESOLVER_FILENAMES_LABEL
                ),
                action: format!(
                    "Add a trigger row for 'skills/{}' in RESOLVER.md under Brain operations",
                    skill.path
                ),
                fix: Some(ResolvableFix {
                    fix_type: FixType::AddTrigger,
                    file: resolver_path.to_string_lossy().to_string(),
                    section: Some("Brain operations".to_string()),
                    skill_path: Some(format!("skills/{}", skill.path)),
                }),
            });
        }
    }

    // 2. Check every resolver entry points to a file that exists.
    for entry in &trigger_entries {
        if entry.is_gstack {
            continue;
        }
        let rel_path = entry.skill_path.trim_start_matches("skills/");
        let full_path = skills_dir.join(rel_path);
        if !full_path.exists() {
            issues.push(ResolvableIssue {
                issue_type: IssueType::MissingFile,
                severity: Severity::Error,
                skill: entry.skill_path.clone(),
                message: format!(
                    "{} references '{}' but the file doesn't exist",
                    RESOLVER_FILENAMES_LABEL, entry.skill_path
                ),
                action: format!(
                    "Create the skill at '{}' or remove the resolver entry",
                    full_path.display()
                ),
                fix: Some(ResolvableFix {
                    fix_type: FixType::CreateStub,
                    file: full_path.to_string_lossy().to_string(),
                    section: None,
                    skill_path: None,
                }),
            });
            continue;
        }
        let skill_name = rel_path.trim_end_matches("/SKILL.md");
        let in_manifest = manifest.iter().any(|s| s.name == skill_name);
        if !in_manifest {
            issues.push(ResolvableIssue {
                issue_type: IssueType::OrphanTrigger,
                severity: Severity::Warning,
                skill: skill_name.to_string(),
                message: format!(
                    "{} has a trigger for '{}' which is not in manifest.json",
                    RESOLVER_FILENAMES_LABEL, skill_name
                ),
                action: format!(
                    "Register '{}' in skills/manifest.json or remove from {}",
                    skill_name, RESOLVER_FILENAMES_LABEL
                ),
                fix: Some(ResolvableFix {
                    fix_type: FixType::RemoveTrigger,
                    file: resolver_path.to_string_lossy().to_string(),
                    section: None,
                    skill_path: Some(entry.skill_path.clone()),
                }),
            });
        }
    }

    // 3. MECE overlap detection (from SKILL.md frontmatter triggers).
    let mut overlaps = 0usize;
    let mut trigger_map: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for skill in &manifest {
        let skill_path = skills_dir.join(&skill.path);
        if !skill_path.exists() {
            continue;
        }
        let content = match fs::read_to_string(&skill_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for t in extract_triggers(&content) {
            let normalized = t.to_lowercase().trim().to_string();
            trigger_map.entry(normalized).or_default().push(skill.name.clone());
        }
    }
    for (trigger, skills) in &trigger_map {
        if skills.len() <= 1 {
            continue;
        }
        let non_whitelisted: Vec<&String> =
            skills.iter().filter(|s| !OVERLAP_WHITELIST.contains(&s.as_str())).collect();
        if non_whitelisted.len() <= 1 {
            continue;
        }
        overlaps += 1;
        issues.push(ResolvableIssue {
            issue_type: IssueType::MeceOverlap,
            severity: Severity::Warning,
            skill: non_whitelisted
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            message: format!(
                "Trigger '{}' matches multiple skills: {}",
                trigger,
                non_whitelisted
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            action: "Add disambiguation rule in RESOLVER.md or narrow triggers in one skill's frontmatter".to_string(),
            fix: None,
        });
    }

    // 4. Gap detection — skills with no triggers in frontmatter.
    let mut gaps = 0usize;
    for skill in &manifest {
        if OVERLAP_WHITELIST.contains(&skill.name.as_str()) {
            continue;
        }
        let skill_path = skills_dir.join(&skill.path);
        if !skill_path.exists() {
            continue;
        }
        let content = match fs::read_to_string(&skill_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if extract_triggers(&content).is_empty() {
            gaps += 1;
            issues.push(ResolvableIssue {
                issue_type: IssueType::MeceGap,
                severity: Severity::Warning,
                skill: skill.name.clone(),
                message: format!(
                    "Skill '{}' has no triggers: field in its SKILL.md frontmatter",
                    skill.name
                ),
                action: format!(
                    "Add a triggers: array to the frontmatter of skills/{}",
                    skill.path
                ),
                fix: Some(ResolvableFix {
                    fix_type: FixType::AddFrontmatter,
                    file: skill_path.to_string_lossy().to_string(),
                    section: None,
                    skill_path: Some(format!("skills/{}", skill.path)),
                }),
            });
        }
    }

    // 5. DRY detection — inlined cross-cutting rules.
    for skill in &manifest {
        let skill_path = skills_dir.join(&skill.path);
        if !skill_path.exists() {
            continue;
        }
        let content = match fs::read_to_string(&skill_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let delegations = extract_delegation_targets(&content);
        for pat in CROSS_CUTTING_PATTERNS {
            let re = match regex::Regex::new(pat.pattern) {
                Ok(r) => r,
                Err(_) => continue,
            };
            for m in re.find_iter(&content) {
                let match_line = content[..m.start()].matches('\n').count() + 1;
                let suppressed = delegations.iter().any(|d| {
                    pat.conventions.contains(&d.convention.as_str())
                        && d.line.abs_diff(match_line) <= DRY_PROXIMITY_LINES
                });
                if suppressed {
                    continue;
                }
                issues.push(ResolvableIssue {
                    issue_type: IssueType::DryViolation,
                    severity: Severity::Warning,
                    skill: skill.name.clone(),
                    message: format!(
                        "Skill '{}' inlines {} instead of delegating to a convention file",
                        skill.name, pat.label
                    ),
                    action: format!(
                        "Replace inlined rules with a reference to one of: {}",
                        pat.conventions.join(", ")
                    ),
                    fix: None,
                });
                break; // one issue per pattern per skill
            }
        }
    }

    // 5b. Check 5 (routing eval, W2): structural routing evaluation of
    // routing-eval.jsonl fixtures. Surfaces as warnings only — advisory.
    // Mirrors `src/core/check-resolvable.ts` Check 5 wiring.
    let loaded = load_routing_fixtures(skills_dir);
    if !loaded.fixtures.is_empty() {
        let trigger_index = index_resolver_triggers(&resolver_content);
        for lint in lint_routing_fixtures(&loaded.fixtures, &trigger_index) {
            issues.push(ResolvableIssue {
                issue_type: IssueType::RoutingFixtureLint,
                severity: Severity::Warning,
                skill: lint
                    .fixture
                    .expected_skill
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                message: format!(
                    "Routing fixture lint ({}): \"{}\"",
                    lint.reason.as_str(),
                    lint.fixture.intent
                ),
                action: format!(
                    "Edit skills/<skill>/routing-eval.jsonl to fix: {}",
                    lint.detail
                ),
                fix: None,
            });
        }
        let routing_report = run_routing_eval(&resolver_content, &loaded.fixtures);
        for d in &routing_report.details {
            let outcome = d.outcome;
            if outcome == RoutingOutcome::Pass {
                continue;
            }
            let (kind, skill_name) = match outcome {
                RoutingOutcome::Missed => (
                    IssueType::RoutingMiss,
                    d.fixture
                        .expected_skill
                        .clone()
                        .unwrap_or_else(|| "negative-case".to_string()),
                ),
                RoutingOutcome::Ambiguous => (
                    IssueType::RoutingAmbiguous,
                    d.fixture
                        .expected_skill
                        .clone()
                        .unwrap_or_else(|| "negative-case".to_string()),
                ),
                RoutingOutcome::FalsePositive => (
                    IssueType::RoutingFalsePositive,
                    d.fixture
                        .expected_skill
                        .clone()
                        .unwrap_or_else(|| "negative-case".to_string()),
                ),
                RoutingOutcome::Pass => unreachable!(),
            };
            let edit_target = match &d.fixture.expected_skill {
                Some(s) => format!(
                    "skills/{}/SKILL.md frontmatter triggers: (canonical) or skills/RESOLVER.md row (dispatcher map)",
                    s
                ),
                None => "the relevant skill's SKILL.md frontmatter triggers:".to_string(),
            };
            issues.push(ResolvableIssue {
                issue_type: kind,
                severity: Severity::Warning,
                skill: skill_name,
                message: format!("Routing {} for intent \"{}\"", outcome.as_str(), d.fixture.intent),
                action: format!(
                    "Update routing-eval.jsonl fixture or broaden {} ({})",
                    edit_target,
                    d.note.clone().unwrap_or_else(|| "no additional detail".to_string())
                ),
                fix: None,
            });
        }
    }
    for m in &loaded.malformed {
        issues.push(ResolvableIssue {
            issue_type: IssueType::RoutingFixtureLint,
            severity: Severity::Warning,
            skill: "routing-eval".to_string(),
            message: format!("Malformed routing fixture {}:{}", m.file, m.line),
            action: format!(
                "Fix the JSONL in routing-eval.jsonl at line {}: {}",
                m.line, m.error
            ),
            fix: None,
        });
    }

    // 5c. Check 6 (filing audit, W3): brain-filing audit findings. Warning-only
    // (D-CX-3 + D-CX-5) — does not break CI for workspaces that haven't adopted
    // writes_pages:/writes_to: yet. Mirrors `src/core/check-resolvable.ts` Check 6.
    match run_filing_audit(skills_dir) {
        Ok(report) => {
            for issue in &report.issues {
                issues.push(ResolvableIssue {
                    issue_type: match issue.issue_type {
                        FilingIssueType::FilingMissingWritesTo => IssueType::FilingMissingWritesTo,
                        FilingIssueType::FilingUnknownDirectory => IssueType::FilingUnknownDirectory,
                    },
                    severity: Severity::Warning,
                    skill: issue.skill.clone(),
                    message: issue.message.clone(),
                    action: issue.action.clone(),
                    fix: None,
                });
            }
        }
        Err(e) => {
            // TS reuses the `filing_unknown_directory` type for a malformed
            // rules doc (a quirk of the TS catch block); mirror it so the
            // issue list stays byte-for-byte equivalent post-migration.
            issues.push(ResolvableIssue {
                issue_type: IssueType::FilingUnknownDirectory,
                severity: Severity::Warning,
                skill: "brain-filing-rules".to_string(),
                message: "_brain-filing-rules.json failed to load".to_string(),
                action: format!("Fix skills/_brain-filing-rules.json: {}", e),
                fix: None,
            });
        }
    }

    // 6. SKILLIFY_STUB sentinel check.
    for skill in &manifest {
        let skill_dir = skills_dir.join(skill.path.trim_end_matches("/SKILL.md"));
        let mut candidates = vec![skills_dir.join(&skill.path)];
        let script_dir = skill_dir.join("scripts");
        if script_dir.exists() {
            if let Ok(entries) = fs::read_dir(&script_dir) {
                for f in entries.flatten() {
                    let name = f.file_name().to_string_lossy().to_string();
                    if name.ends_with(".ts") || name.ends_with(".mjs") || name.ends_with(".js")
                        || name.ends_with(".py")
                    {
                        candidates.push(f.path());
                    }
                }
            }
        }
        for candidate in candidates {
            if let Ok(content) = fs::read_to_string(&candidate) {
                if content.contains(SKILLIFY_STUB_SENTINEL) {
                    issues.push(ResolvableIssue {
                        issue_type: IssueType::SkillifyStubUnreplaced,
                        severity: Severity::Warning,
                        skill: skill.name.clone(),
                        message: format!(
                            "Skill '{}' still contains the SKILLIFY_STUB sentinel in {}",
                            skill.name,
                            candidate.strip_prefix(skills_dir).unwrap_or(&candidate).display()
                        ),
                        action: format!(
                            "Replace the SKILLIFY_STUB sentinel in {} with a real implementation or remove the file.",
                            candidate.display()
                        ),
                        fix: None,
                    });
                    break;
                }
            }
        }
    }

    let errors: Vec<ResolvableIssue> = issues
        .iter()
        .filter(|i| i.severity == Severity::Error)
        .cloned()
        .collect();
    let warnings: Vec<ResolvableIssue> = issues
        .iter()
        .filter(|i| i.severity == Severity::Warning)
        .cloned()
        .collect();
    let all: Vec<ResolvableIssue> = errors.iter().chain(warnings.iter()).cloned().collect();

    ResolvableReport {
        ok: errors.is_empty(),
        errors,
        warnings,
        issues: all,
        summary: Summary {
            total_skills: manifest.len(),
            reachable,
            unreachable,
            overlaps,
            gaps,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_cr_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, name: &str, fm: &str) {
        let sub = dir.join(name);
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("SKILL.md"), format!("---\n{}\n---\nbody", fm)).unwrap();
    }

    #[test]
    fn empty_tree_is_missing_file_error() {
        let dir = scratch("empty");
        let report = check_resolvable(&dir);
        assert!(!report.ok);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].issue_type, IssueType::MissingFile);
    }

    #[test]
    fn reachable_skill_passes() {
        let dir = scratch("reachable");
        write_skill(&dir, "query", "name: query\ntriggers:\n  - harvest this skill");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| harvest this skill | `skills/query/SKILL.md` |",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(report.ok, "issues: {:?}", report.issues);
        assert_eq!(report.summary.reachable, 1);
    }

    #[test]
    fn unreachable_skill_errors() {
        let dir = scratch("unreachable");
        // `query` has NO frontmatter triggers and NO resolver row -> unreachable.
        write_skill(&dir, "query", "name: query");
        // RESOLVER.md exists (so tree is non-empty) but references a different skill.
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| something else | `skills/other/SKILL.md` |",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(!report.ok);
        assert!(report
            .errors
            .iter()
            .any(|i| i.issue_type == IssueType::Unreachable));
    }

    #[test]
    fn missing_file_for_bad_resolver_row() {
        let dir = scratch("missingfile");
        write_skill(&dir, "query", "name: query\ntriggers:\n  - harvest this skill");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| harvest this skill | `skills/ghost/SKILL.md` |",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(report
            .errors
            .iter()
            .any(|i| i.issue_type == IssueType::MissingFile));
    }

    #[test]
    fn mece_overlap_warns() {
        let dir = scratch("overlap");
        write_skill(&dir, "a", "name: a\ntriggers:\n  - same trigger");
        write_skill(&dir, "b", "name: b\ntriggers:\n  - same trigger");
        let report = check_resolvable(&dir);
        assert!(report
            .warnings
            .iter()
            .any(|i| i.issue_type == IssueType::MeceOverlap));
    }

    #[test]
    fn mece_gap_warns() {
        let dir = scratch("gap");
        // `a` is reachable (resolver row) but has no frontmatter triggers.
        write_skill(&dir, "a", "name: a");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| t | `skills/a/SKILL.md` |",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(report
            .warnings
            .iter()
            .any(|i| i.issue_type == IssueType::MeceGap));
    }

    #[test]
    fn dry_violation_warns_without_delegation() {
        let dir = scratch("dry");
        write_skill(
            &dir,
            "a",
            "name: a\ntriggers:\n  - t\n\nWe follow the iron law of back-linking here.",
        );
        let report = check_resolvable(&dir);
        assert!(report
            .warnings
            .iter()
            .any(|i| i.issue_type == IssueType::DryViolation));
    }

    #[test]
    fn dry_violation_suppressed_by_delegation() {
        let dir = scratch("dryok");
        write_skill(
            &dir,
            "a",
            "name: a\ntriggers:\n  - t\n\nSee `skills/conventions/quality.md` for the iron law of back-linking.\n\nWe follow the iron law of back-linking here.",
        );
        let report = check_resolvable(&dir);
        assert!(!report
            .warnings
            .iter()
            .any(|i| i.issue_type == IssueType::DryViolation));
    }

    #[test]
    fn skillify_stub_warns() {
        let dir = scratch("stub");
        write_skill(&dir, "a", "name: a\ntriggers:\n  - t");
        let path = dir.join("a/SKILL.md");
        let mut content = fs::read_to_string(&path).unwrap();
        content.push_str(&format!("\n// {}\n", SKILLIFY_STUB_SENTINEL));
        fs::write(&path, content).unwrap();
        let report = check_resolvable(&dir);
        assert!(report
            .warnings
            .iter()
            .any(|i| i.issue_type == IssueType::SkillifyStubUnreplaced));
    }

    #[test]
    fn check5_routing_eval_surfaces_miss_warning() {
        // Check 5 (1-6-5-6-5): a routing-eval.jsonl fixture whose intent does
        // not contain its expected_skill's trigger must surface as a
        // RoutingMiss warning in check_resolvable's output.
        let dir = scratch("check5");
        write_skill(&dir, "query", "name: query");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| \"what do we know about\" | `skills/query/SKILL.md` |",
        )
        .unwrap();
        let qdir = dir.join("query");
        fs::write(
            qdir.join("routing-eval.jsonl"),
            "{\"intent\":\"totally unrelated nonsense\",\"expected_skill\":\"query\"}\n",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(
            report
                .warnings
                .iter()
                .any(|i| i.issue_type == IssueType::RoutingMiss),
            "expected a RoutingMiss warning; warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn check5_skipped_when_no_fixtures() {
        // When no routing-eval.jsonl exists, Check 5 must stay silent
        // (no Routing* warnings).
        let dir = scratch("check5empty");
        write_skill(&dir, "query", "name: query");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| \"what do we know about\" | `skills/query/SKILL.md` |",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(!report
            .warnings
            .iter()
            .any(|i| matches!(
                i.issue_type,
                IssueType::RoutingMiss
                    | IssueType::RoutingAmbiguous
                    | IssueType::RoutingFalsePositive
                    | IssueType::RoutingFixtureLint
            )));
    }

    #[test]
    fn check6_filing_missing_writes_to_warns() {
        // Check 6 (1-6-5-7-3): a skill with writes_pages:true but no
        // writes_to: must surface as a FilingMissingWritesTo warning.
        let dir = scratch("check6");
        write_skill(&dir, "capture", "name: capture\nwrites_pages: true");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| t | `skills/capture/SKILL.md` |",
        )
        .unwrap();
        fs::write(
            dir.join("_brain-filing-rules.json"),
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"}]}",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(
            report
                .warnings
                .iter()
                .any(|i| i.issue_type == IssueType::FilingMissingWritesTo),
            "expected FilingMissingWritesTo warning; warnings: {:?}",
            report.warnings
        );
    }

    #[test]
    fn check6_skipped_when_no_rules_doc() {
        // When _brain-filing-rules.json is absent, Check 6 must stay silent
        // (no Filing* warnings).
        let dir = scratch("check6empty");
        write_skill(&dir, "capture", "name: capture\nwrites_pages: true");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| t | `skills/capture/SKILL.md` |",
        )
        .unwrap();
        let report = check_resolvable(&dir);
        assert!(!report.warnings.iter().any(|i| matches!(
            i.issue_type,
            IssueType::FilingMissingWritesTo | IssueType::FilingUnknownDirectory
        )));
    }

    #[test]
    fn check6_malformed_rules_doc_warns_unknown_directory() {
        // A malformed rules doc surfaces a single warning reusing the
        // FilingUnknownDirectory type (mirrors the TS catch-block quirk).
        let dir = scratch("check6malformed");
        write_skill(&dir, "capture", "name: capture\nwrites_pages: true");
        fs::write(
            dir.join("RESOLVER.md"),
            "| trigger | skill |\n| --- | --- |\n| t | `skills/capture/SKILL.md` |",
        )
        .unwrap();
        fs::write(dir.join("_brain-filing-rules.json"), "not json at all").unwrap();
        let report = check_resolvable(&dir);
        let hit = report.warnings.iter().find(|i| {
            i.issue_type == IssueType::FilingUnknownDirectory && i.skill == "brain-filing-rules"
        });
        assert!(
            hit.is_some(),
            "expected a malformed-rules warning; warnings: {:?}",
            report.warnings
        );
        assert!(hit.unwrap().message.contains("failed to load"));
    }
}
