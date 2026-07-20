//! brain_first — brain-first compliance analyzer (port of
//! `src/core/skill-brain-first.ts`).
//!
//! Pure, no I/O. Consumed by the dry-fix INSERT path (1-6-5-8-3) and the
//! future doctor `skill_brain_first` check (1-6-5-9).
//!
//! Exemption order (top wins):
//!   1. Frontmatter `brain_first: exempt` → exempt_explicit
//!   2. No external-lookup pattern in body → exempt_no_external
//!   3. Otherwise apply the compliance ladder (any one passes):
//!      a. canonical `> **Convention:** ... brain-first ...` callout
//!      b. explicit `## Phase 1` / `## Step 0` brain heading
//!      c. first brain reference appears before first external reference

use std::sync::OnceLock;

use crate::skill_resolver::skill_frontmatter::{
    format_brain_first_typo_hint, parse_skill_frontmatter, BrainFirstTypo, BrainFirstValue,
    ParsedFrontmatter,
};

/// Why the analyzer landed where it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainFirstReason {
    ExemptExplicit,
    ExemptNoExternal,
    CompliantCallout,
    CompliantPhase,
    CompliantPosition,
    MissingBrainFirst,
}

/// OK if any exemption or compliance path matched; Warn otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrainFirstStatus {
    Ok,
    Warn,
}

/// Result of analyzing one SKILL.md.
#[derive(Debug, Clone)]
pub struct BrainFirstAnalysis {
    pub skill: String,
    pub status: BrainFirstStatus,
    pub reason: BrainFirstReason,
    pub external_patterns_matched: Vec<String>,
    pub typo_hint: Option<String>,
    pub formerly_hardcoded_exempt: bool,
}

// ---------------------------------------------------------------------------
// Pattern constants
// ---------------------------------------------------------------------------

fn ext_patterns() -> &'static [(&'static str, &'static regex::Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, &'static regex::Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            ("web_search", web_search_re()),
            ("web_fetch", web_fetch_re()),
            ("exa", exa_re()),
            ("perplexity", perplexity_re()),
            ("happenstance", happenstance_re()),
            ("crustdata", crustdata_re()),
            ("captain_api", captain_api_re()),
            ("firecrawl", firecrawl_re()),
        ]
    })
    .as_slice()
}

fn web_search_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bweb_search\b").unwrap())
}
fn web_fetch_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bweb_fetch\b").unwrap())
}
fn exa_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bexa[\s._-]").unwrap())
}
fn perplexity_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bperplexity\b").unwrap())
}
fn happenstance_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bhappenstance\b").unwrap())
}
fn crustdata_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bcrustdata\b").unwrap())
}
fn captain_api_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bcaptain[\s._-]?api\b").unwrap())
}
fn firecrawl_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bfirecrawl\b").unwrap())
}

fn brain_ref_patterns() -> &'static [&'static regex::Regex] {
    static REFS: OnceLock<Vec<&'static regex::Regex>> = OnceLock::new();
    REFS.get_or_init(|| {
        vec![
            gbrain_search_re(),
            gbrain_query_re(),
            gbrain_get_page_re(),
            gbrain_find_experts_re(),
            gbrain_get_backlinks_re(),
            gbrain_get_timeline_re(),
            gbrain_traverse_graph_re(),
            search_the_brain_re(),
            query_the_brain_re(),
            check_the_brain_re(),
        ]
    })
    .as_slice()
}

fn gbrain_search_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+search\b").unwrap())
}
fn gbrain_query_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+query\b").unwrap())
}
fn gbrain_get_page_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+get[_-]?page\b").unwrap())
}
fn gbrain_find_experts_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+find[_-]?experts\b").unwrap())
}
fn gbrain_get_backlinks_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+get[_-]?backlinks\b").unwrap())
}
fn gbrain_get_timeline_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+get[_-]?timeline\b").unwrap())
}
fn gbrain_traverse_graph_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bgbrain[\s_]+traverse[_-]?graph\b").unwrap())
}
fn search_the_brain_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bsearch\s+the\s+brain\b").unwrap())
}
fn query_the_brain_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bquery\s+the\s+brain\b").unwrap())
}
fn check_the_brain_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?i)\bcheck\s+the\s+brain\b").unwrap())
}

/// Canonical Convention callout regex (start-of-line blockquote containing
/// `**Convention:**` and `brain-first`). Path syntax is intentionally
/// ignored.
pub fn convention_callout_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?im)^>\s*\*\*Convention:\*\*[^\n]*brain-first").unwrap())
}

/// Explicit phase-heading regex (`## Phase 1` / `## Step 0` naming a brain
/// phase).
fn phase_heading_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"(?im)^##+\s*(?:Phase\s*1|Step\s*0)\b[^\n]*brain").unwrap())
}

/// Frontmatter fence regex used by body extraction.
fn frontmatter_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^---\n[\s\S]*?\n---\n?").unwrap())
}

/// Skills that were in PR #1206's hardcoded EXEMPT_SKILLS allowlist.
/// Informational only — guides the doctor `--fix` opt-in, NOT an exemption
/// rule. Mirrors `FORMERLY_HARDCODED_EXEMPT`.
pub static FORMERLY_HARDCODED_EXEMPT: &[&str] = &[
    "brain-ops", "brain-commit", "brain-enrichment-pipeline", "brain-export",
    "brain-ingest-gate", "brain-librarian", "brain-link-refs", "brain-link-report",
    "brain-pdf", "brain-pdf-auto", "brain-plan", "brain-publish", "brain-storage",
    "brain-storage-links", "brain-taxonomist", "zbrain", "zbrain-pr", "zbrain-upgrade",
    "benchmark-zbrain", "exa", "happenstance", "crustdata", "captain-api", "healthcheck",
    "backblaze", "browser", "browser-use", "binary-deps", "captcha-solver",
    "container-restart", "durable-service", "data-loss-gate", "channel-discovery",
    "clawvisor", "clawvisor-shield", "cron-scheduler", "cronify", "correction-pipeline",
    "acknowledge", "ask-user", "backoff", "acp-coding", "code-pr", "skill-creator",
    "ingest", "freshness-monitor",
];

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Strip the leading YAML frontmatter fence (body-only by construction, so a
/// `tools: [web_search]` declaration in frontmatter never counts as the
/// first external reference).
pub fn strip_frontmatter(content: &str) -> String {
    frontmatter_re().replace(content, "").to_string()
}

/// 0-indexed byte offset of the first brain-reference match in `body`, or -1.
pub fn find_first_brain_ref_offset(body: &str) -> i64 {
    let mut min: i64 = -1;
    for re in brain_ref_patterns() {
        if let Some(m) = re.find(body) {
            let idx = m.start() as i64;
            if min == -1 || idx < min {
                min = idx;
            }
        }
    }
    min
}

/// 0-indexed byte offset of the first external-reference match in `body`, or -1.
pub fn find_first_external_ref_offset(body: &str) -> i64 {
    let mut min: i64 = -1;
    for (_, re) in ext_patterns() {
        if let Some(m) = re.find(body) {
            let idx = m.start() as i64;
            if min == -1 || idx < min {
                min = idx;
            }
        }
    }
    min
}

// ---------------------------------------------------------------------------
// Analyzer
// ---------------------------------------------------------------------------

/// Analyze a single SKILL.md for brain-first compliance. Pure function:
/// no I/O, no side effects. Drives the doctor check, the skillify-check gate,
/// and the dry-fix MISSING_RULE detector.
pub fn analyze_skill_brain_first(
    content: &str,
    skill_name: &str,
    frontmatter: Option<&ParsedFrontmatter>,
) -> BrainFirstAnalysis {
    let formerly = FORMERLY_HARDCODED_EXEMPT.contains(&skill_name);
    let typo_hint = frontmatter
        .and_then(|f| f.brain_first_typo.as_ref())
        .and_then(format_brain_first_typo_hint);

    // Exemption 1: explicit declarative opt-out.
    if let Some(fm) = frontmatter {
        if fm.brain_first == Some(BrainFirstValue::Exempt) {
            return BrainFirstAnalysis {
                skill: skill_name.to_string(),
                status: BrainFirstStatus::Ok,
                reason: BrainFirstReason::ExemptExplicit,
                external_patterns_matched: Vec::new(),
                typo_hint,
                formerly_hardcoded_exempt: formerly,
            };
        }
    }

    // Body extraction — strip frontmatter so a `tools: [web_search]`
    // declaration in YAML doesn't false-flag the skill.
    let body = strip_frontmatter(content);

    let external_patterns_matched: Vec<String> = ext_patterns()
        .iter()
        .filter(|(_, re)| re.is_match(&body))
        .map(|(name, _)| (*name).to_string())
        .collect();

    // Exemption 2: no external pattern present anywhere in body.
    if external_patterns_matched.is_empty() {
        return BrainFirstAnalysis {
            skill: skill_name.to_string(),
            status: BrainFirstStatus::Ok,
            reason: BrainFirstReason::ExemptNoExternal,
            external_patterns_matched: Vec::new(),
            typo_hint,
            formerly_hardcoded_exempt: formerly,
        };
    }

    // Compliance a: canonical Convention callout referencing brain-first.
    if convention_callout_re().is_match(&body) {
        return BrainFirstAnalysis {
            skill: skill_name.to_string(),
            status: BrainFirstStatus::Ok,
            reason: BrainFirstReason::CompliantCallout,
            external_patterns_matched,
            typo_hint,
            formerly_hardcoded_exempt: formerly,
        };
    }

    // Compliance b: explicit Phase 1 / Step 0 brain heading.
    if phase_heading_re().is_match(&body) {
        return BrainFirstAnalysis {
            skill: skill_name.to_string(),
            status: BrainFirstStatus::Ok,
            reason: BrainFirstReason::CompliantPhase,
            external_patterns_matched,
            typo_hint,
            formerly_hardcoded_exempt: formerly,
        };
    }

    // Compliance c: first brain reference appears BEFORE first external
    // reference in body (position-relative, body-only).
    let first_brain = find_first_brain_ref_offset(&body);
    let first_external = find_first_external_ref_offset(&body);
    if first_brain != -1 && first_external != -1 && first_brain < first_external {
        return BrainFirstAnalysis {
            skill: skill_name.to_string(),
            status: BrainFirstStatus::Ok,
            reason: BrainFirstReason::CompliantPosition,
            external_patterns_matched,
            typo_hint,
            formerly_hardcoded_exempt: formerly,
        };
    }

    // Otherwise: external pattern present, no compliance signal. Warn.
    BrainFirstAnalysis {
        skill: skill_name.to_string(),
        status: BrainFirstStatus::Warn,
        reason: BrainFirstReason::MissingBrainFirst,
        external_patterns_matched,
        typo_hint,
        formerly_hardcoded_exempt: formerly,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exempt_explicit() {
        let c = "---\nname: a\nbrain_first: exempt\n---\n\nUse web_search here.\n";
        let fm = parse_skill_frontmatter(c);
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert_eq!(a.status, BrainFirstStatus::Ok);
        assert_eq!(a.reason, BrainFirstReason::ExemptExplicit);
    }

    #[test]
    fn exempt_no_external() {
        let c = "---\nname: a\n---\n\nJust does local stuff.\n";
        let fm = parse_skill_frontmatter(c);
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert_eq!(a.status, BrainFirstStatus::Ok);
        assert_eq!(a.reason, BrainFirstReason::ExemptNoExternal);
    }

    #[test]
    fn compliant_callout() {
        let c = "---\nname: a\n---\n\n> **Convention:** see conventions/brain-first.md for the lookup chain.\n\nUse web_search here.\n";
        let fm = parse_skill_frontmatter(c);
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert_eq!(a.status, BrainFirstStatus::Ok);
        assert_eq!(a.reason, BrainFirstReason::CompliantCallout);
    }

    #[test]
    fn compliant_phase() {
        let c = "---\nname: a\n---\n\n## Phase 1: Brain-First Lookup\n\nUse web_search here.\n";
        let fm = parse_skill_frontmatter(c);
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert_eq!(a.status, BrainFirstStatus::Ok);
        assert_eq!(a.reason, BrainFirstReason::CompliantPhase);
    }

    #[test]
    fn compliant_position() {
        // brain reference before external reference in body.
        let c = "---\nname: a\n---\n\nQuery the brain first, then use web_search if needed.\n";
        let fm = parse_skill_frontmatter(c);
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert_eq!(a.status, BrainFirstStatus::Ok);
        assert_eq!(a.reason, BrainFirstReason::CompliantPosition);
    }

    #[test]
    fn missing_brain_first_warns() {
        let c = "---\nname: a\n---\n\nUse web_search to look things up.\n";
        let fm = parse_skill_frontmatter(c);
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert_eq!(a.status, BrainFirstStatus::Warn);
        assert_eq!(a.reason, BrainFirstReason::MissingBrainFirst);
        assert!(a.external_patterns_matched.contains(&"web_search".to_string()));
    }

    #[test]
    fn typo_hint_surfaces() {
        let c = "---\nname: a\nbrain_first: \"exempt\"\n---\n\nUse web_search here.\n";
        let fm = parse_skill_frontmatter(c);
        assert!(fm.as_ref().unwrap().brain_first.is_none());
        assert!(fm.as_ref().unwrap().brain_first_typo.is_some());
        let a = analyze_skill_brain_first(c, "a", fm.as_ref());
        assert!(a.typo_hint.is_some());
        assert!(a.typo_hint.unwrap().contains("drop the quotes"));
    }
}
