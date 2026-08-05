//! Think trajectory-routing intent classifier.
//!
//! Ported from `src/core/think/intent.ts` (v0.40.2.0). Pure: no DB, no LLM,
//! no async. Regex-first so the common `'other'` intent adds zero latency on
//! the hot path. Three buckets:
//!
//!   * `Temporal`        — "when did I last…", "how long ago…", date markers
//!   * `KnowledgeUpdate` — "X changed/switched/moved/no longer…"
//!   * `Other`           — everything else (no trajectory injection)
//!
//! The classifier deliberately errs toward `Other` — false positives waste
//! prompt tokens on irrelevant trajectory blocks; false negatives just miss a
//! few boosts. Recall over precision is NOT the right tradeoff here.

use regex::{Regex, RegexBuilder};
use std::sync::LazyLock;

/// Trajectory-routing intent for `zbrain think`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkIntent {
    Temporal,
    KnowledgeUpdate,
    Other,
}

fn ci(pat: &str) -> Regex {
    RegexBuilder::new(pat)
        .case_insensitive(true)
        .build()
        .expect("think intent regex must compile")
}

// Combined alternation mirrors the TS `TEMPORAL_RX` (compiled once at module
// load, case-insensitive). `\b`/`\s` semantics match the JS regex engine.
static TEMPORAL_RX: LazyLock<Regex> = LazyLock::new(|| {
    ci(&[
        r"\bwhen\b",
        r"\bhow\s+long\s+ago\b",
        r"\bhow\s+long\s+(have|has|did|do)\b",
        r"\blast\s+(time|met|saw|spoke|visited)\b",
        r"\b(is\s+)?still\b",
        r"\bcurrent(?:ly)?\b",
        r"\bnow\b",
        r"\bbefore\s+(I|we|the|that)\b",
        r"\bafter\s+(I|we|the|that)\b",
        r"\bsince\s+(when|I|we|the|last|\d{4})\b",
        r"\b(20\d{2}|jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:tember)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\b",
    ].join("|"))
});

// `KNOWLEDGE_UPDATE_RX` — supersession verbs win over temporal when both match
// (every supersession question is also temporal, but trajectory's
// `(superseded prior)` annotation is the knowledge_update differentiator).
static KNOWLEDGE_UPDATE_RX: LazyLock<Regex> = LazyLock::new(|| {
    ci(&[
        r"\b(?:chang|switch|mov|updat)(?:e[ds]?|ed|es|ing)?\b",
        r"\bno\s+longer\b",
        r"\binstead\s+of\b",
        r"\bused\s+to\b",
        r"\b(?:they|he|she|we|I)\s+stopped\b",
        r"\b(current|latest|new|most\s+recent)\s+\w+",
        r"\bwhat(?:'s|\s+is)\s+(?:the\s+)?(?:current|latest|new)\b",
    ].join("|"))
});

/// Classify a question into one of the three trajectory intents.
///
/// Knowledge-update patterns win over temporal when both match. Empty input
/// (and non-string in the TS original) maps to `Other`. Faithful to
/// `src/core/think/intent.ts:classifyIntent`.
pub fn classify_intent(question: &str) -> ThinkIntent {
    if question.is_empty() {
        return ThinkIntent::Other;
    }
    if KNOWLEDGE_UPDATE_RX.is_match(question) {
        return ThinkIntent::KnowledgeUpdate;
    }
    if TEMPORAL_RX.is_match(question) {
        return ThinkIntent::Temporal;
    }
    ThinkIntent::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_other() {
        assert_eq!(classify_intent(""), ThinkIntent::Other);
    }

    #[test]
    fn temporal_when() {
        assert_eq!(classify_intent("when did I last meet Marco"), ThinkIntent::Temporal);
    }

    #[test]
    fn temporal_date_marker() {
        assert_eq!(classify_intent("What happened in March 2024"), ThinkIntent::Temporal);
    }

    #[test]
    fn temporal_case_insensitive() {
        assert_eq!(classify_intent("WHEN did we speak"), ThinkIntent::Temporal);
    }

    #[test]
    fn knowledge_update_wins_over_temporal() {
        // "changed" is a supersession verb AND would trip temporal "still"? No,
        // but it is the more specific signal and is checked first.
        assert_eq!(classify_intent("Alice changed her role"), ThinkIntent::KnowledgeUpdate);
    }

    #[test]
    fn knowledge_update_switched() {
        assert_eq!(classify_intent("they switched vendors last quarter"), ThinkIntent::KnowledgeUpdate);
    }

    #[test]
    fn knowledge_update_no_longer() {
        assert_eq!(classify_intent("we no longer use that tool"), ThinkIntent::KnowledgeUpdate);
    }

    #[test]
    fn other_default() {
        assert_eq!(classify_intent("tell me about the launch plan"), ThinkIntent::Other);
    }

    #[test]
    fn other_greeting() {
        assert_eq!(classify_intent("how are you"), ThinkIntent::Other);
    }
}
