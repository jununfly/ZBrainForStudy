//! filing_audit — Check 6 of the skillify checklist (W3) — Rust port (slice 1-6-5-7).
//!
//! For every skill that writes brain pages (`writes_pages: true`), verify:
//!   1. The skill declares a non-empty `writes_to: [dir, ...]` frontmatter.
//!   2. Each directory in `writes_to:` is a valid filing target per
//!      `skills/_brain-filing-rules.json`. `sources/` is explicitly allowed
//!      (bulk data capture is a legitimate filing target).
//!
//! Critical distinction (D-CX-7): `writes_pages: true` is distinct from the
//! pre-existing `mutating: true`. `mutating:true` means "has side effects"
//! (cron/config/report write). `writes_pages:true` means "writes brain pages
//! to a semantic directory." Cron/config/report-writer skills set
//! `mutating:true` but NOT `writes_pages:true`, and so are correctly exempted
//! from filing-audit noise.
//!
//! Scope (grill decision 1-6-5-7): **core + wiring only**. TS `filing-audit`
//! has always had a single consumer (`check-resolvable.ts`); there is no
//! standalone `zbrain filing-audit` CLI — so Rust migrates the core only and
//! does NOT add a new CLI command (unlike routing-eval / Check 5, which had a
//! pre-existing `src/commands/routing-eval.ts`).
//!
//! Slice 1-6-5-7 fully ported (core only; no standalone CLI, per grill
//! decision — TS `filing-audit.ts` is kept until 1-6-5-9):
//!   1-6-5-7-1  types + normalizeDir + loadFilingRules + allowedDirectories  (section A)
//!   1-6-5-7-2  runFilingAudit + FilingIssue/FilingReport scan               (section B)
//!   1-6-5-7-3  Check 6 wired into check_resolvable() as warnings             (check_resolvable.rs)
//!   1-6-5-7-4  typecheck baseline gate + commit

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;

use crate::skill_resolver::skill_frontmatter::parse_skill_frontmatter;

// ---------------------------------------------------------------------------
// Types (mirror src/core/filing-audit.ts)
// ---------------------------------------------------------------------------

/// One rule entry from `_brain-filing-rules.json::rules[]`.
///
/// `kind` is informational (loosely validated — defaults to empty when the
/// key is absent, matching TS which never inspects it). `directory` is the
/// load-bearing field: it must be present or `load_filing_rules` treats the
/// doc as malformed (TS would `normalizeDir(undefined)` and throw at audit
/// time — failing fast at load is strictly safer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingRule {
    #[serde(default)]
    pub kind: String,
    pub directory: String,
    #[serde(default)]
    pub examples: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
}

/// The `sources_dir` entry — a special, always-allowed filing target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcesDirEntry {
    pub directory: String,
    pub purpose: String,
    #[serde(default)]
    pub not_for: Option<Vec<String>>,
}

/// Canonical filing-rules doc from `skills/_brain-filing-rules.json`.
///
/// Only `rules` is load-bearing (TS `loadFilingRules` enforces it is an
/// array); everything else is optional and defaults so a minimal doc
/// (`{"rules":[...]}`) is accepted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingRulesDoc {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub companion: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub rules: Vec<FilingRule>,
    #[serde(default)]
    pub sources_dir: Option<SourcesDirEntry>,
    #[serde(default)]
    pub notes: Option<Vec<String>>,
}

/// The two filing-audit violation kinds (D-CX-7).
///
/// Serializes under the same snake_case strings TS uses so any downstream
/// consumer (e.g. `check_resolvable` JSON output in 1-6-5-7-3) stays aligned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FilingIssueType {
    /// `writes_pages: true` but no `writes_to:` list.
    FilingMissingWritesTo,
    /// `writes_to:` lists a directory not present in the rules doc.
    FilingUnknownDirectory,
}

impl FilingIssueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            FilingIssueType::FilingMissingWritesTo => "filing_missing_writes_to",
            FilingIssueType::FilingUnknownDirectory => "filing_unknown_directory",
        }
    }
}

/// Severity of a filing issue. Only `warning` exists today (declaration-level
/// audit is advisory, not blocking).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FilingSeverity {
    Warning,
}

/// A single filing-audit violation (section B output; defined here so the
/// report type in 1-6-5-7-2 is a thin constructor over these).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilingIssue {
    #[serde(rename = "type")]
    pub issue_type: FilingIssueType,
    pub severity: FilingSeverity,
    pub skill: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    pub message: String,
    pub action: String,
}

/// Aggregate report over a skills-tree scan (section B output).
#[derive(Debug, Clone, Default)]
pub struct FilingReport {
    pub total_scanned: usize,
    pub writes_pages_skills: usize,
    pub issues: Vec<FilingIssue>,
}

// ---------------------------------------------------------------------------
// Section A — loader + directory normalization (slice 1-6-5-7-1)
// ---------------------------------------------------------------------------

/// Normalize a filing directory for consistent comparison.
/// Accepts `people`, `people/`, `/people`, `/people/` and yields `people/`.
/// An empty/whitespace-only/root-only input normalizes to `""` (no directory).
///
/// Mirrors `src/core/filing-audit.ts::normalizeDir`.
pub fn normalize_dir(dir: &str) -> String {
    let trimmed: String = dir
        .trim()
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{}/", trimmed)
    }
}

/// Load canonical filing rules from `skills_dir/_brain-filing-rules.json`.
///
/// Returns `Ok(None)` when the file is absent — filing-audit is a no-op until
/// the rules doc is in place. Returns `Err` on malformed JSON (wrong top-level
/// shape, non-array `rules`, or a rule missing its `directory`) so the caller
/// can surface a loud "rules doc is broken" signal instead of silently
/// degrading.
///
/// Mirrors `src/core/filing-audit.ts::loadFilingRules` (which returns `null`
/// for absence and throws on malformed input).
pub fn load_filing_rules(skills_dir: &Path) -> Result<Option<FilingRulesDoc>, String> {
    let path = skills_dir.join("_brain-filing-rules.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            return Err(format!(
                "_brain-filing-rules.json: cannot read {}: {}",
                path.display(),
                e
            ))
        }
    };
    match serde_json::from_str::<FilingRulesDoc>(&content) {
        Ok(doc) => Ok(Some(doc)),
        Err(e) => Err(format!(
            "_brain-filing-rules.json: malformed ({}): {}",
            path.display(),
            e
        )),
    }
}

/// The canonical set of directories a skill is allowed to list in
/// `writes_to:`. Includes every rule's directory plus the special
/// `sources_dir` entry, all normalized to trailing-slash form.
///
/// Mirrors `src/core/filing-audit.ts::allowedDirectories`.
pub fn allowed_directories(rules: &FilingRulesDoc) -> HashSet<String> {
    let mut set = HashSet::new();
    for r in &rules.rules {
        set.insert(normalize_dir(&r.directory));
    }
    if let Some(sd) = &rules.sources_dir {
        set.insert(normalize_dir(&sd.directory));
    }
    set
}

// ---------------------------------------------------------------------------
// Section B — audit runner (slice 1-6-5-7-2)
// ---------------------------------------------------------------------------
//
// runFilingAudit + the scan loop (writes_pages filter, missing writes_to,
// unknown directory). Consumes load_filing_rules + allowed_directories +
// the existing parse_skill_frontmatter, and emits FilingIssue/FilingReport.

/// Scan every skill under `skills_dir` for filing-audit violations.
///
/// Returns `Err` when `_brain-filing-rules.json` is present but malformed —
/// matching TS `runFilingAudit`, which lets `loadFilingRules` throw on bad
/// input instead of silently degrading. Returns an empty report when the
/// rules doc is absent (filing-audit is a no-op) or `skills_dir` itself is
/// unreadable.
///
/// Mirrors `src/core/filing-audit.ts::runFilingAudit`:
///   - skip `.`/`_`-prefixed entries and non-directories;
///   - skip skills without a `SKILL.md`;
///   - only `writes_pages === true` skills are audited (false/undefined and
///     `mutating: true` skills are correctly exempted — D-CX-7);
///   - missing `writes_to:` → `FilingMissingWritesTo` (no `directory`);
///   - each dir in `writes_to:` not in `allowedDirectories` →
///     `FilingUnknownDirectory` (with the raw dir string in `directory`).
pub fn run_filing_audit(skills_dir: &Path) -> Result<FilingReport, String> {
    let rules = match load_filing_rules(skills_dir)? {
        None => return Ok(FilingReport::default()),
        Some(r) => r,
    };
    let allowed = allowed_directories(&rules);

    let mut report = FilingReport::default();

    let entries = match std::fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return Ok(report), // unreadable dir → empty scan (TS parity)
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name.starts_with('_') {
            continue;
        }
        let path = entry.path();
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        report.total_scanned += 1;

        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let fm = match parse_skill_frontmatter(&content) {
            Some(f) => f,
            None => continue,
        };
        // D-CX-7: only the literal true opt-in path is audited.
        if fm.writes_pages != Some(true) {
            continue;
        }
        report.writes_pages_skills += 1;

        let skill_name = fm.name.clone().unwrap_or_else(|| name.clone());
        let writes_to = fm.writes_to.clone().unwrap_or_default();

        if writes_to.is_empty() {
            report.issues.push(FilingIssue {
                issue_type: FilingIssueType::FilingMissingWritesTo,
                severity: FilingSeverity::Warning,
                skill: skill_name.clone(),
                directory: None,
                message: format!(
                    "Skill '{}' has writes_pages: true but no writes_to: list",
                    skill_name
                ),
                action: format!(
                    "Add a writes_to: [dir, ...] list to skills/{}/SKILL.md frontmatter \
                     (see skills/_brain-filing-rules.json for valid directories)",
                    name
                ),
            });
            continue;
        }

        for raw_dir in &writes_to {
            let normalized = normalize_dir(raw_dir);
            if !allowed.contains(&normalized) {
                report.issues.push(FilingIssue {
                    issue_type: FilingIssueType::FilingUnknownDirectory,
                    severity: FilingSeverity::Warning,
                    skill: skill_name.clone(),
                    directory: Some(raw_dir.clone()),
                    message: format!(
                        "Skill '{}' declares writes_to: '{}' which is not listed in \
                         _brain-filing-rules.json",
                        skill_name, raw_dir
                    ),
                    action: format!(
                        "Fix the writes_to: entry in skills/{}/SKILL.md or add '{}' to \
                         skills/_brain-filing-rules.json rules[]",
                        name, normalized
                    ),
                });
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Section A — normalize_dir / load / allowedDirectories (1-6-5-7-1)
    // -----------------------------------------------------------------------

    #[test]
    fn normalize_dir_adds_trailing_slash() {
        assert_eq!(normalize_dir("people"), "people/");
        assert_eq!(normalize_dir("people/"), "people/");
        assert_eq!(normalize_dir("/people"), "people/");
        assert_eq!(normalize_dir("/people/"), "people/");
    }

    #[test]
    fn normalize_dir_collapses_inner_and_trims() {
        assert_eq!(normalize_dir("  people  "), "people/");
        assert_eq!(normalize_dir("//people//"), "people/");
        assert_eq!(normalize_dir("people//nested/"), "people//nested/");
    }

    #[test]
    fn normalize_dir_empty_or_root_yields_empty() {
        assert_eq!(normalize_dir(""), "");
        assert_eq!(normalize_dir("   "), "");
        assert_eq!(normalize_dir("/"), "");
        assert_eq!(normalize_dir("///"), "");
    }

    #[test]
    fn load_returns_none_when_absent() {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_none_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No _brain-filing-rules.json present.
        let got = load_filing_rules(&dir).unwrap();
        assert!(got.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_returns_none_when_skills_dir_missing() {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_missing_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let got = load_filing_rules(&dir).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn load_errors_on_non_object() {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_arr_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("_brain-filing-rules.json"), "[1, 2, 3]").unwrap();
        let got = load_filing_rules(&dir);
        assert!(got.is_err());
        assert!(got.unwrap_err().contains("malformed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_errors_on_rules_not_array() {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_rules_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("_brain-filing-rules.json"),
            "{\"version\":\"1\",\"rules\":\"not-an-array\"}",
        )
        .unwrap();
        let got = load_filing_rules(&dir);
        assert!(got.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_errors_on_bad_json() {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_bad_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("_brain-filing-rules.json"), "{ this is not json ").unwrap();
        assert!(load_filing_rules(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_parses_rules_and_sources_dir() {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_ok_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = "\
{
  \"version\": \"1\",
  \"rules\": [
    {\"kind\": \"person\", \"directory\": \"people\", \"description\": \"person pages\"},
    {\"kind\": \"company\", \"directory\": \"companies/\"}
  ],
  \"sources_dir\": {\"directory\": \"/sources\", \"purpose\": \"bulk capture\"}
}
";
        std::fs::write(dir.join("_brain-filing-rules.json"), doc).unwrap();
        let loaded = load_filing_rules(&dir).unwrap().expect("should parse");
        assert_eq!(loaded.rules.len(), 2);
        assert_eq!(loaded.rules[0].directory, "people");
        assert_eq!(loaded.rules[1].directory, "companies/");
        assert_eq!(loaded.sources_dir.as_ref().unwrap().directory, "/sources");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn allowed_directories_merges_rules_and_sources_normalized() {
        let rules = FilingRulesDoc {
            version: Some("1".to_string()),
            companion: None,
            description: None,
            rules: vec![
                FilingRule {
                    kind: "person".to_string(),
                    directory: "people".to_string(),
                    examples: None,
                    description: None,
                },
                FilingRule {
                    kind: "company".to_string(),
                    directory: "/companies/".to_string(),
                    examples: None,
                    description: None,
                },
            ],
            sources_dir: Some(SourcesDirEntry {
                directory: "sources/".to_string(),
                purpose: "bulk".to_string(),
                not_for: None,
            }),
            notes: None,
        };
        let allowed = allowed_directories(&rules);
        let mut expected = HashSet::new();
        expected.insert("people/".to_string());
        expected.insert("companies/".to_string());
        expected.insert("sources/".to_string());
        assert_eq!(allowed, expected);
    }

    // -----------------------------------------------------------------------
    // Section B — run_filing_audit (1-6-5-7-2)
    // -----------------------------------------------------------------------

    fn scratch(sub: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zbrain_filing_run_{}_{}",
            std::process::id(),
            sub
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_skill(dir: &Path, slug: &str, frontmatter: &str) {
        let sd = dir.join(slug);
        std::fs::create_dir_all(&sd).unwrap();
        std::fs::write(sd.join("SKILL.md"), format!("---\n{}\n---\nbody\n", frontmatter)).unwrap();
    }

    fn write_rules(dir: &Path, body: &str) {
        std::fs::write(dir.join("_brain-filing-rules.json"), body).unwrap();
    }

    #[test]
    fn run_skips_when_rules_absent() {
        // No _brain-filing-rules.json → no-op empty report (TS: loadFilingRules null).
        let dir = scratch("norecs");
        write_skill(&dir, "a", "name: a");
        let rep = run_filing_audit(&dir).unwrap();
        assert_eq!(rep.total_scanned, 0);
        assert_eq!(rep.writes_pages_skills, 0);
        assert!(rep.issues.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_reports_missing_writes_to() {
        let dir = scratch("missing");
        write_rules(
            &dir,
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"}]}",
        );
        write_skill(&dir, "capture", "name: capture\nwrites_pages: true");
        let rep = run_filing_audit(&dir).unwrap();
        assert_eq!(rep.total_scanned, 1);
        assert_eq!(rep.writes_pages_skills, 1);
        assert_eq!(rep.issues.len(), 1);
        let iss = &rep.issues[0];
        assert_eq!(iss.issue_type, FilingIssueType::FilingMissingWritesTo);
        assert_eq!(iss.skill, "capture");
        assert_eq!(iss.directory, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_reports_unknown_directory() {
        let dir = scratch("unknown");
        write_rules(
            &dir,
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"}]}",
        );
        write_skill(
            &dir,
            "capture",
            "name: capture\nwrites_pages: true\nwrites_to: [bogus]",
        );
        let rep = run_filing_audit(&dir).unwrap();
        assert_eq!(rep.issues.len(), 1);
        let iss = &rep.issues[0];
        assert_eq!(iss.issue_type, FilingIssueType::FilingUnknownDirectory);
        assert_eq!(iss.directory.as_deref(), Some("bogus"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_clean_when_writes_to_allowed() {
        let dir = scratch("clean");
        write_rules(
            &dir,
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"},{\"directory\":\"sources\"}]}",
        );
        write_skill(
            &dir,
            "capture",
            "name: capture\nwrites_pages: true\nwrites_to: [people]",
        );
        let rep = run_filing_audit(&dir).unwrap();
        assert_eq!(rep.writes_pages_skills, 1);
        assert!(
            rep.issues.is_empty(),
            "expected no issues, got {:?}",
            rep.issues
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_skips_non_writes_pages_skills() {
        // D-CX-7: mutating:true alone must NOT trigger filing-audit noise.
        let dir = scratch("nonwp");
        write_rules(
            &dir,
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"}]}",
        );
        write_skill(&dir, "reporter", "name: reporter\nmutating: true");
        write_skill(
            &dir,
            "cron",
            "name: cron\nmutating: true\nwrites_pages: false",
        );
        let rep = run_filing_audit(&dir).unwrap();
        assert_eq!(rep.total_scanned, 2);
        assert_eq!(rep.writes_pages_skills, 0);
        assert!(rep.issues.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_skips_dot_and_underscore_dirs() {
        let dir = scratch("dots");
        write_rules(
            &dir,
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"}]}",
        );
        write_skill(&dir, "real", "name: real\nwrites_pages: true");
        let hidden = dir.join(".hidden");
        std::fs::create_dir_all(&hidden).unwrap();
        std::fs::write(
            hidden.join("SKILL.md"),
            "---\nname: hidden\nwrites_pages: true\n---\n",
        )
        .unwrap();
        let privdir = dir.join("_private");
        std::fs::create_dir_all(&privdir).unwrap();
        std::fs::write(
            privdir.join("SKILL.md"),
            "---\nname: priv\nwrites_pages: true\n---\n",
        )
        .unwrap();
        let rep = run_filing_audit(&dir).unwrap();
        assert_eq!(rep.total_scanned, 1, "dot/underscore skill dirs must be skipped");
        assert_eq!(rep.writes_pages_skills, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_counts_multiple_unknown_dirs() {
        let dir = scratch("multi");
        write_rules(
            &dir,
            "{\"version\":\"1\",\"rules\":[{\"directory\":\"people\"}]}",
        );
        write_skill(
            &dir,
            "capture",
            "name: capture\nwrites_pages: true\nwrites_to: [bogus, people, other]",
        );
        let rep = run_filing_audit(&dir).unwrap();
        // bogus + other are unknown (2); people is allowed.
        assert_eq!(rep.issues.len(), 2);
        assert!(rep
            .issues
            .iter()
            .all(|i| i.issue_type == FilingIssueType::FilingUnknownDirectory));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_propagates_malformed_rules_as_err() {
        let dir = scratch("malformed");
        write_rules(&dir, "this is not json");
        write_skill(&dir, "capture", "name: capture\nwrites_pages: true");
        let got = run_filing_audit(&dir);
        assert!(got.is_err(), "malformed rules doc must surface as Err (TS throws)");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
