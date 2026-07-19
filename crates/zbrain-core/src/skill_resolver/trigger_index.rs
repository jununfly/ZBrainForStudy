//! skill-trigger-index — shared loader for the unified skill trigger index.
//!
//! Folds two surfaces into one `ResolverEntry` stream (ported from
//! `src/core/skill-trigger-index.ts`):
//!   1. Per-skill SKILL.md frontmatter `triggers:` (canonical source).
//!   2. Curated RESOLVER.md / AGENTS.md rows from `skillsDir` + parent dir
//!      (preserves the human-readable dispatcher map AND the OpenClaw
//!      workspace-root AGENTS.md merge contract).
//!
//! Merge semantics: UNION, not REPLACE. Dedup keyed on
//! `(skillPath, trigger.trim().toLowerCase())`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::skill_resolver::resolver_filenames::{find_all_resolver_files, has_resolver_file};
use crate::skill_resolver::skill_frontmatter::parse_skill_frontmatter;

/// A parsed resolver entry (from RESOLVER.md / AGENTS.md, or synthesized).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverEntry {
    pub trigger: String,
    /// e.g. `skills/query/SKILL.md`, or a prose label for GStack/external.
    pub skill_path: String,
    pub is_gstack: bool,
    pub section: String,
}

/// Which surface produced a `SkillTriggerEntry`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    Frontmatter,
    ResolverMd,
}

/// A unified trigger entry with its originating surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillTriggerEntry {
    pub trigger: String,
    pub skill_path: String,
    pub is_gstack: bool,
    pub section: String,
    pub source: TriggerSource,
}

/// Section label stamped on every frontmatter-derived entry.
pub const FRONTMATTER_SECTION: &str = "Auto-registered (from skill frontmatter)";

/// Skill subdirectories the loader will not scan for SKILL.md frontmatter.
const FRONTMATTER_SKIP_DIRS: [&str; 2] = ["conventions", "migrations"];

fn heading_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^##\s+(.+)").unwrap())
}
fn list_bold_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^-\s+\*\*([a-z][a-z0-9-]+)\*\*\s*:\s*(.+)$").unwrap())
}
fn list_plain_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^-\s+([a-z][a-z0-9-]+)\s*:\s*(.+)$").unwrap())
}
fn path_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"`(skills/[^`]+/SKILL\.md)`").unwrap())
}
fn suffix_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\s*(?:→|->)\s*`skills/[^`]+`\s*$").unwrap())
}

/// Parse RESOLVER.md / AGENTS.md content into structured entries. Supports
/// two formats that can mix in one file:
///   Format 1 (table): `| trigger phrase | \`skills/<name>/SKILL.md\` |`
///   Format 2 (compact list, v0.41.7.0): `- **skill-name**: t1 | t2`
fn parse_resolver_entries(resolver_content: &str) -> Vec<ResolverEntry> {
    let mut entries: Vec<ResolverEntry> = Vec::new();
    let mut current_section = String::new();

    for line in resolver_content.split('\n') {
        if let Some(m) = heading_re().captures(line) {
            current_section = m.get(1).unwrap().as_str().trim().to_string();
            continue;
        }

        // Format 1: markdown table rows.
        if line.starts_with('|') && !line.contains("---") {
            let cols: Vec<&str> = line
                .split('|')
                .map(|c| c.trim())
                .filter(|c| !c.is_empty())
                .collect();
            if cols.len() < 2 {
                continue;
            }
            let trigger = cols[0];
            let skill_col = cols[1];
            let trigger_lower = trigger.to_lowercase();
            if trigger_lower == "trigger" || trigger_lower == "skill" {
                continue; // header row
            }
            if skill_col.starts_with("GStack:")
                || skill_col.starts_with("Check ")
                || skill_col.starts_with("Read ")
            {
                entries.push(ResolverEntry {
                    trigger: trigger.to_string(),
                    skill_path: skill_col.to_string(),
                    is_gstack: true,
                    section: current_section.clone(),
                });
                continue;
            }
            if let Some(m) = path_re().captures(skill_col) {
                entries.push(ResolverEntry {
                    trigger: trigger.to_string(),
                    skill_path: m.get(1).unwrap().as_str().to_string(),
                    is_gstack: false,
                    section: current_section.clone(),
                });
            }
            continue;
        }

        // Format 2: compact list rows.
        let list_match = list_bold_re().captures(line).or_else(|| list_plain_re().captures(line));
        if let Some(m) = list_match {
            let skill_name = m.get(1).unwrap().as_str().to_string();
            let triggers_raw = m.get(2).unwrap().as_str().trim().to_string();
            let cleaned = suffix_re().replace(&triggers_raw, "").to_string();
            let triggers: Vec<String> = cleaned
                .split('|')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty() && t != "...")
                .collect();
            let skill_path = format!("skills/{}/SKILL.md", skill_name);
            for trigger in triggers {
                entries.push(ResolverEntry {
                    trigger,
                    skill_path: skill_path.clone(),
                    is_gstack: false,
                    section: current_section.clone(),
                });
            }
        }
    }

    entries
}

/// Walk `skills/<name>/SKILL.md` for each skill, synthesize one
/// `SkillTriggerEntry` per declared `triggers:` string.
fn load_frontmatter_entries(skills_dir: &Path) -> Vec<SkillTriggerEntry> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(skills_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('_') || name.starts_with('.') {
            continue;
        }
        if FRONTMATTER_SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let meta = match entry.path().metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_dir() {
            continue;
        }
        let skill_md = entry.path().join("SKILL.md");
        if !skill_md.exists() {
            continue;
        }
        let content = match fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let parsed = match parse_skill_frontmatter(&content) {
            Some(p) => p,
            None => continue,
        };
        let triggers = match parsed.triggers {
            Some(t) => t,
            None => continue,
        };
        if triggers.is_empty() {
            continue;
        }
        for trigger in triggers {
            let t = trigger.trim().to_string();
            if t.is_empty() {
                continue;
            }
            out.push(SkillTriggerEntry {
                trigger: t,
                skill_path: format!("skills/{}/SKILL.md", name),
                is_gstack: false,
                section: FRONTMATTER_SECTION.to_string(),
                source: TriggerSource::Frontmatter,
            });
        }
    }
    out
}

/// Walk every RESOLVER.md / AGENTS.md across `skillsDir` AND its parent.
fn load_resolver_md_entries(skills_dir: &Path) -> Vec<SkillTriggerEntry> {
    let mut paths: Vec<PathBuf> = find_all_resolver_files(skills_dir);
    let parent = skills_dir.join("..");
    paths.extend(find_all_resolver_files(&parent));
    let mut out = Vec::new();
    for p in paths {
        let content = match fs::read_to_string(&p) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for e in parse_resolver_entries(&content) {
            out.push(SkillTriggerEntry {
                trigger: e.trigger,
                skill_path: e.skill_path,
                is_gstack: e.is_gstack,
                section: e.section,
                source: TriggerSource::ResolverMd,
            });
        }
    }
    out
}

/// Merge frontmatter + resolver_md entries with UNION semantics. Dedup on
/// `(skillPath, normalized trigger)`. First occurrence wins.
fn merge_entries(
    fm: Vec<SkillTriggerEntry>,
    resolver: Vec<SkillTriggerEntry>,
) -> Vec<SkillTriggerEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for e in fm.into_iter().chain(resolver.into_iter()) {
        let key = if e.is_gstack {
            format!("EXT::{}::{}", e.skill_path, e.trigger.trim().to_lowercase())
        } else {
            format!("{}::{}", e.skill_path, e.trigger.trim().to_lowercase())
        };
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(e);
    }
    out
}

/// The shared primitive. Returns the unified entry list for a skills dir.
pub fn load_skill_trigger_index(skills_dir: &Path) -> Vec<SkillTriggerEntry> {
    let fm = load_frontmatter_entries(skills_dir);
    let resolver = load_resolver_md_entries(skills_dir);
    merge_entries(fm, resolver)
}

/// Synthesize a single markdown-table resolver-content string from a unified
/// entry list. Shape-compatible with `parse_resolver_entries`.
pub fn entries_to_resolver_content(entries: &[SkillTriggerEntry]) -> String {
    let mut lines: Vec<String> = vec![
        "## Synthesized trigger index".to_string(),
        String::new(),
        "| trigger | skill |".to_string(),
        "| --- | --- |".to_string(),
    ];
    for e in entries {
        let trigger = escape_pipe(&e.trigger);
        if e.is_gstack {
            lines.push(format!("| {} | {} |", trigger, escape_pipe(&e.skill_path)));
        } else {
            lines.push(format!("| {} | `{}` |", trigger, e.skill_path));
        }
    }
    lines.join("\n")
}

fn escape_pipe(s: &str) -> String {
    s.replace('|', "\\|")
}

/// First RESOLVER.md / AGENTS.md path across `skillsDir` + parent, or None.
pub fn find_primary_resolver_path(skills_dir: &Path) -> Option<PathBuf> {
    let mut paths = find_all_resolver_files(skills_dir);
    let parent = skills_dir.join("..");
    paths.extend(find_all_resolver_files(&parent));
    paths.into_iter().next()
}

/// Test seam — `has_resolver_file` re-exported for callers that need the
/// boolean form without constructing a Path.
pub fn dir_has_resolver_file(dir: &Path) -> bool {
    has_resolver_file(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zb_ti_{}", name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parses_table_rows() {
        let content = "| trigger | skill |\n| --- | --- |\n| harvest this skill | `skills/query/SKILL.md` |";
        let entries = parse_resolver_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].trigger, "harvest this skill");
        assert_eq!(entries[0].skill_path, "skills/query/SKILL.md");
        assert!(!entries[0].is_gstack);
    }

    #[test]
    fn skips_table_header() {
        let content = "| trigger | skill |\n| --- | --- |\n| trigger | skill |";
        let entries = parse_resolver_entries(content);
        assert!(entries.is_empty());
    }

    #[test]
    fn parses_gstack_rows() {
        let content = "| review tweet | GStack: ceo-review |";
        let entries = parse_resolver_entries(content);
        assert_eq!(entries.len(), 1);
        assert!(entries[0].is_gstack);
        assert_eq!(entries[0].skill_path, "GStack: ceo-review");
    }

    #[test]
    fn parses_compact_list_bold() {
        let content = "- **query**: harvest this skill | tell me about";
        let entries = parse_resolver_entries(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].skill_path, "skills/query/SKILL.md");
        assert_eq!(entries[1].trigger, "tell me about");
    }

    #[test]
    fn parses_compact_list_plain() {
        let content = "- query: harvest this skill";
        let entries = parse_resolver_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].skill_path, "skills/query/SKILL.md");
    }

    #[test]
    fn compact_list_strips_path_suffix() {
        let content = "- **query**: harvest this skill -> `skills/query/SKILL.md`";
        let entries = parse_resolver_entries(content);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].trigger, "harvest this skill");
    }

    #[test]
    fn compact_list_ignores_prose_bullets() {
        // `- **Note**: ...` must not match as a skill row.
        let content = "- **Note**: remember to do X";
        let entries = parse_resolver_entries(content);
        assert!(entries.is_empty());
    }

    #[test]
    fn frontmatter_entries_load() {
        let dir = scratch("fm");
        fs::create_dir_all(dir.join("query")).unwrap();
        fs::write(
            dir.join("query/SKILL.md"),
            "---\nname: query\ntriggers:\n  - harvest this skill\n---\nbody",
        )
        .unwrap();
        let idx = load_skill_trigger_index(&dir);
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].source, TriggerSource::Frontmatter);
        assert_eq!(idx[0].skill_path, "skills/query/SKILL.md");
    }

    #[test]
    fn union_dedups_case_insensitive() {
        let dir = scratch("union");
        fs::create_dir_all(dir.join("query")).unwrap();
        fs::write(
            dir.join("query/SKILL.md"),
            "---\nname: query\ntriggers:\n  - Harvest This Skill\n---\nbody",
        )
        .unwrap();
        fs::write(dir.join("RESOLVER.md"), "| harvest this skill | `skills/query/SKILL.md` |").unwrap();
        let idx = load_skill_trigger_index(&dir);
        // Frontmatter (first) wins; resolver_md duplicate collapsed.
        assert_eq!(idx.len(), 1);
        assert_eq!(idx[0].trigger, "Harvest This Skill");
        assert_eq!(idx[0].source, TriggerSource::Frontmatter);
    }

    #[test]
    fn entries_to_resolver_content_roundtrips() {
        let dir = scratch("round");
        fs::create_dir_all(dir.join("query")).unwrap();
        fs::write(
            dir.join("query/SKILL.md"),
            "---\nname: query\ntriggers:\n  - harvest this skill\n---\nbody",
        )
        .unwrap();
        let idx = load_skill_trigger_index(&dir);
        let content = entries_to_resolver_content(&idx);
        let reparsed = parse_resolver_entries(&content);
        assert_eq!(reparsed.len(), 1);
        assert_eq!(reparsed[0].skill_path, "skills/query/SKILL.md");
    }
}
