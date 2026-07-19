//! skill-frontmatter — single content-based parser for SKILL.md YAML
//! frontmatter. Shared by `filing_audit` (writes_pages / writes_to) and
//! later by `dry_fix` (brain_first compliance).
//!
//! Ported from `src/core/skill-frontmatter.ts`. Tolerant on unknown keys;
//! STRICT on the `brain_first:` field (only the canonical
//! `brain_first: exempt` sets the typed field; near-misses populate
//! `brain_first_typo` for surfacing a paste-ready fix hint).

use std::sync::OnceLock;

/// Parsed SKILL.md frontmatter. Every field optional; `raw` is the YAML
/// between the `---` fences.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedFrontmatter {
    pub raw: String,
    pub name: Option<String>,
    pub writes_pages: Option<bool>,
    pub writes_to: Option<Vec<String>>,
    pub mutating: Option<bool>,
    pub tools: Option<Vec<String>>,
    pub triggers: Option<Vec<String>>,
    /// Only the literal canonical `brain_first: exempt` populates this.
    pub brain_first: Option<BrainFirstValue>,
    /// Near-miss declarations for the typo hint.
    pub brain_first_typo: Option<BrainFirstTypo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainFirstValue {
    Exempt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainFirstTypo {
    pub key: String,
    pub value: String,
    pub reason: BrainFirstTypoReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainFirstTypoReason {
    NonCanonicalKey,
    QuotedValue,
    CapitalizedValue,
    UnknownValue,
}

fn fence_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^---\n([\s\S]*?)\n---").unwrap())
}
fn name_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#"(?m)^name:\s*["']?([^"'\n]+?)["']?\s*$"#).unwrap())
}
fn bool_re(field: &'static str) -> &'static regex::Regex {
    static MAP: OnceLock<std::collections::HashMap<&'static str, regex::Regex>> = OnceLock::new();
    let map = MAP.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        for f in ["writes_pages", "mutating"] {
            let re = regex::Regex::new(&format!(r"(?m)^{}:\s*(true|false)\s*$", f)).unwrap();
            m.insert(f, re);
        }
        m
    });
    map.get(field).unwrap()
}
fn inline_array_re(field: &str) -> regex::Regex {
    regex::Regex::new(&format!(r"(?m)^{}:\s*\[([^\]]*)\]\s*$", field)).unwrap()
}
fn block_array_re(field: &str) -> regex::Regex {
    regex::Regex::new(&format!(r"(?m)^{}:\s*\n((?:\s+-\s+[^\n]+\n?)+)", field)).unwrap()
}
fn brain_first_typo_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?im)^(brain[-_]?first)\s*:\s*(.+?)\s*$").unwrap())
}

/// Parse SKILL.md content. Returns None when no YAML frontmatter is found.
/// Content-based (no I/O) so callers control how they load files.
pub fn parse_skill_frontmatter(content: &str) -> Option<ParsedFrontmatter> {
    let caps = fence_re().captures(content)?;
    let raw = caps.get(1)?.as_str().to_string();
    let mut out = ParsedFrontmatter {
        raw: raw.clone(),
        ..Default::default()
    };

    if let Some(m) = name_re().captures(&raw) {
        out.name = Some(m.get(1)?.as_str().trim().to_string());
    }

    for (field, target) in [("writes_pages", &mut out.writes_pages), ("mutating", &mut out.mutating)] {
        if let Some(m) = bool_re(field).captures(&raw) {
            *target = Some(&m[1] == "true");
        }
    }

    out.writes_to = parse_array_field(&raw, "writes_to");
    out.tools = parse_array_field(&raw, "tools");
    out.triggers = parse_array_field(&raw, "triggers");

    parse_brain_first(&raw, &mut out);

    Some(out)
}

/// Parse an array-shaped YAML field that may appear inline (`field: [a, b]`)
/// or as a block list. Returns None if the field is absent.
fn parse_array_field(raw: &str, field: &str) -> Option<Vec<String>> {
    if let Some(m) = inline_array_re(field).captures(raw) {
        let inner = m.get(1)?.as_str().trim();
        if inner.is_empty() {
            return Some(Vec::new());
        }
        return Some(
            inner
                .split(',')
                .map(|s| s.trim().trim_matches(|c| c == '"' || c == '\u{27}').to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        );
    }
    if let Some(m) = block_array_re(field).captures(raw) {
        let block = m.get(1)?.as_str();
        return Some(
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
                .collect(),
        );
    }
    None
}

fn parse_brain_first(raw: &str, out: &mut ParsedFrontmatter) {
    let m = match brain_first_typo_re().captures(raw) {
        Some(m) => m,
        None => return,
    };
    let key = m.get(1).unwrap().as_str().to_string();
    let value_raw = m.get(2).unwrap().as_str().to_string();

    if key == "brain_first" && value_raw == "exempt" {
        out.brain_first = Some(BrainFirstValue::Exempt);
        return;
    }
    if key != "brain_first" {
        out.brain_first_typo = Some(BrainFirstTypo {
            key,
            value: value_raw,
            reason: BrainFirstTypoReason::NonCanonicalKey,
        });
        return;
    }
    let unquoted: String = value_raw.trim_matches(|c| c == '"' || c == '\u{27}').to_string();
    let was_quoted = unquoted != value_raw;
    if was_quoted && unquoted == "exempt" {
        out.brain_first_typo = Some(BrainFirstTypo {
            key,
            value: value_raw,
            reason: BrainFirstTypoReason::QuotedValue,
        });
        return;
    }
    if unquoted.to_lowercase() == "exempt" {
        out.brain_first_typo = Some(BrainFirstTypo {
            key,
            value: value_raw,
            reason: BrainFirstTypoReason::CapitalizedValue,
        });
        return;
    }
    out.brain_first_typo = Some(BrainFirstTypo {
        key,
        value: value_raw,
        reason: BrainFirstTypoReason::UnknownValue,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_without_fence() {
        assert!(parse_skill_frontmatter("no frontmatter here").is_none());
    }

    #[test]
    fn parses_name() {
        let fm = parse_skill_frontmatter("---\nname: query\n---\nbody").unwrap();
        assert_eq!(fm.name.as_deref(), Some("query"));
    }

    #[test]
    fn parses_triggers_block() {
        let content = "---\nname: q\ntriggers:\n  - harvest this skill\n  - tell me about\n---\nbody";
        let fm = parse_skill_frontmatter(content).unwrap();
        assert_eq!(
            fm.triggers.as_deref(),
            Some(&["harvest this skill".to_string(), "tell me about".to_string()][..])
        );
    }

    #[test]
    fn parses_triggers_inline() {
        let content = "---\nname: q\ntriggers: [\"a\", \"b\"]\n---\nbody";
        let fm = parse_skill_frontmatter(content).unwrap();
        assert_eq!(
            fm.triggers.as_deref(),
            Some(&["a".to_string(), "b".to_string()][..])
        );
    }

    #[test]
    fn parses_writes_pages_bool() {
        let fm = parse_skill_frontmatter("---\nname: q\nwrites_pages: true\n---\n").unwrap();
        assert_eq!(fm.writes_pages, Some(true));
    }

    #[test]
    fn brain_first_canonical() {
        let fm = parse_skill_frontmatter("---\nname: q\nbrain_first: exempt\n---\n").unwrap();
        assert_eq!(fm.brain_first, Some(BrainFirstValue::Exempt));
        assert!(fm.brain_first_typo.is_none());
    }

    #[test]
    fn brain_first_typo_quoted() {
        let fm = parse_skill_frontmatter("---\nname: q\nbrain_first: \"exempt\"\n---\n").unwrap();
        assert!(fm.brain_first.is_none());
        assert_eq!(
            fm.brain_first_typo.as_ref().map(|t| t.reason),
            Some(BrainFirstTypoReason::QuotedValue)
        );
    }

    #[test]
    fn brain_first_typo_key() {
        let fm = parse_skill_frontmatter("---\nname: q\nbrain-first: exempt\n---\n").unwrap();
        assert_eq!(
            fm.brain_first_typo.as_ref().map(|t| t.reason),
            Some(BrainFirstTypoReason::NonCanonicalKey)
        );
    }
}
