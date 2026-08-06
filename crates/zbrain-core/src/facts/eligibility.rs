//! v0.31.2 — facts backstop eligibility predicate (Rust port of
//! `src/core/facts/eligibility.ts`).
//!
//! Single source of truth for "should this page write fire the facts
//! extraction backstop?" Used by put_page, sync post-import, file_upload,
//! code_import, and the `extract_facts` MCP op negative path.

use serde_json::Value;

/// Path prefixes that rescue a page even when frontmatter type is not
/// eligible (directory shape wins over legacy frontmatter type).
pub const RESCUE_SLUG_PREFIXES: &[&str] = &["meetings/", "personal/", "daily/"];

/// Page types that are eligible for extraction by default.
pub const ELIGIBLE_TYPES: &[&str] = &[
    "note", "meeting", "slack", "email", "calendar-event", "source", "writing",
];

const MIN_BODY_CHARS: usize = 80;

/// Result of the eligibility check. `reason` is a stable string consumed by
/// tests and the `facts_extraction_health` doctor check (grouped by reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EligibilityResult {
    Eligible,
    Ineligible { reason: String },
}

/// A minimal view of a parsed page, sufficient for the eligibility check.
#[derive(Debug, Clone)]
pub struct ParsedPageLite<'a> {
    pub page_type: &'a str,
    pub compiled_truth: &'a str,
    /// Owned frontmatter value (cheap; avoids lifetime-escape when callers
    /// build the view around a transient `serde_json::json!` literal).
    pub frontmatter: Value,
}

/// Should this page write fire the facts extraction backstop?
#[must_use]
pub fn is_facts_backstop_eligible(slug: &str, parsed: Option<&ParsedPageLite<'_>>) -> EligibilityResult {
    let Some(parsed) = parsed else {
        return EligibilityResult::Ineligible { reason: "no_parsed_page".into() };
    };

    if slug.starts_with("wiki/agents/") {
        return EligibilityResult::Ineligible { reason: "subagent_namespace".into() };
    }

    if parsed.frontmatter.get("dream_generated") == Some(&Value::Bool(true)) {
        return EligibilityResult::Ineligible { reason: "dream_generated".into() };
    }

    let body = parsed.compiled_truth.trim();
    if body.len() < MIN_BODY_CHARS {
        return EligibilityResult::Ineligible { reason: "too_short".into() };
    }

    let type_ok = ELIGIBLE_TYPES.contains(&parsed.page_type);
    let slug_ok = RESCUE_SLUG_PREFIXES.iter().any(|p| slug.starts_with(p));
    if !type_ok && !slug_ok {
        return EligibilityResult::Ineligible {
            reason: format!("kind:{}", parsed.page_type),
        };
    }

    EligibilityResult::Eligible
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // A body well over the 80-char minimum.
    const LONG_BODY: &str = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua ut enim ad minim veniam quis nostrud";

    fn page(page_type: &'static str) -> ParsedPageLite<'static> {
        ParsedPageLite {
            page_type,
            compiled_truth: LONG_BODY,
            frontmatter: json!({}),
        }
    }

    #[test]
    fn none_parsed_is_ineligible() {
        assert_eq!(
            is_facts_backstop_eligible("notes/foo", None),
            EligibilityResult::Ineligible { reason: "no_parsed_page".into() }
        );
    }

    #[test]
    fn subagent_namespace_rejected() {
        assert_eq!(
            is_facts_backstop_eligible("wiki/agents/scratch", Some(&page("note"))),
            EligibilityResult::Ineligible { reason: "subagent_namespace".into() }
        );
    }

    #[test]
    fn dream_generated_rejected() {
        let fm = json!({"dream_generated": true});
        let p = ParsedPageLite { page_type: "note", compiled_truth: LONG_BODY, frontmatter: fm };
        assert_eq!(
            is_facts_backstop_eligible("notes/foo", Some(&p)),
            EligibilityResult::Ineligible { reason: "dream_generated".into() }
        );
    }

    #[test]
    fn too_short_rejected() {
        let p = ParsedPageLite { page_type: "note", compiled_truth: "short", frontmatter: json!({}) };
        assert_eq!(
            is_facts_backstop_eligible("notes/foo", Some(&p)),
            EligibilityResult::Ineligible { reason: "too_short".into() }
        );
    }

    #[test]
    fn eligible_note() {
        assert_eq!(is_facts_backstop_eligible("notes/foo", Some(&page("note"))), EligibilityResult::Eligible);
    }

    #[test]
    fn ineligible_unknown_kind() {
        assert_eq!(
            is_facts_backstop_eligible("notes/foo", Some(&page("recipe"))),
            EligibilityResult::Ineligible { reason: "kind:recipe".into() }
        );
    }

    #[test]
    fn rescue_prefix_overrides_kind() {
        // meetings/ slug typed as 'note' (legacy default) → still eligible.
        assert_eq!(
            is_facts_backstop_eligible("meetings/2026-05-09-foo", Some(&page("note"))),
            EligibilityResult::Eligible
        );
    }
}
