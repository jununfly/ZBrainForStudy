//! Query-intent classification for zbrain hybrid search.
//!
//! Ported from `src/core/search/query-intent.ts` (v0.29.1 merged classifier).
//! Pure module: no DB, no LLM, no async. A single regex pass over the query
//! emits four orthogonal axis suggestions:
//!
//!   * `intent`           — `'entity' | 'temporal' | 'event' | 'general'`
//!   * `suggested_detail` — `'low' | 'medium' | 'high' | None`
//!   * `suggested_salience` — `'off' | 'on'`
//!   * `suggested_recency`  — `'off' | 'on' | 'strong'`
//!   * `suggested_modality` — `'text' | 'image'`
//!
//! Salience and recency are TRULY ORTHOGONAL (per the TS D9 note): both can
//! fire, neither, or just one. The classifier follows "current state → on,
//! canonical truth → off" with a narrow D6 exception: an explicit temporal
//! bound (today / this week / since X / last N days) overrides the canonical
//! pattern.
//!
//! NOTE: TS `weightsForIntent` (per-list RRF-k intent weights + exact-match
//! boost) is intentionally NOT ported. Rust's `fuse_and_boost` runs a single
//! RRF-K over the lexical + vector lists, so per-list weights have no consumer
//! yet — porting them would be dead code. `classify_query` still emits the
//! `intent` so a future per-list-weight path can read it. registered in
//! docs/plans/KNOWN-GAPS.md.

use regex::Regex;
use std::sync::LazyLock;

/// Original v0.29.0 intent type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryIntent {
    Entity,
    Temporal,
    Event,
    General,
}

/// v0.29.0 detail mapping target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchDetail {
    Low,
    Medium,
    High,
}

/// Salience axis suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalienceMode {
    Off,
    On,
}

/// Recency axis suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecencyMode {
    Off,
    On,
    Strong,
}

/// v0.36 cross-modal routing axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalityMode {
    Text,
    Image,
}

/// All four axis suggestions from one classifier pass.
#[derive(Debug, Clone, PartialEq)]
pub struct QuerySuggestions {
    pub intent: QueryIntent,
    pub suggested_detail: Option<SearchDetail>,
    pub suggested_salience: SalienceMode,
    pub suggested_recency: RecencyMode,
    pub suggested_modality: ModalityMode,
}

// ─────────────────────────────────────────────────────────
// Pattern banks (mirrors src/core/search/query-intent.ts)
// ─────────────────────────────────────────────────────────

static TEMPORAL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\bwhen\b",
    r"\blast\s+(met|meeting|call|conversation|chat|talked|spoke|seen|heard|time)\b",
    r"\brecent(ly)?\b",
    r"\bhistory\b",
    r"\btimeline\b",
    r"\bmeeting\s+notes?\b",
    r"\bwhat('s| is| was)\s+new\b",
    r"\blatest\b",
    r"\bupdate(s)?\s+(on|from|about)\b",
    r"\bhow\s+long\s+(ago|since)\b",
    r"\b\d{4}[-/]\d{2}\b",
    r"\blast\s+(week|month|quarter|year)\b",
]));

static EVENT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\bannounce[ds]?(ment)?\b",
    r"\blaunch(ed|es|ing)?\b",
    r"\braised?\s+\$?\d",
    r"\bfund(ing|raise)\b",
    r"\bIPO\b",
    r"\bacquisition\b",
    r"\bmerge[drs]?\b",
    r"\bnews\b",
    r"\bhappened?\b",
]));

static ENTITY_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\bwho\s+is\b",
    r"\bwhat\s+(is|does|are)\b",
    r"\btell\s+me\s+about\b",
    r"\bdescribe\b",
    r"\bsummar(y|ize)\b",
    r"\boverview\b",
    r"\bbackground\b",
    r"\bprofile\b",
    r"\bwhat\s+do\s+(you|we)\s+know\b",
]));

static FULL_CONTEXT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\beverything\b",
    r"\ball\s+(about|info|information|details)\b",
    r"\bfull\s+(history|context|picture|story|details)\b",
    r"\bcomprehensive\b",
    r"\bdeep\s+dive\b",
    r"\bgive\s+me\s+everything\b",
]));

static CANONICAL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\bwho\s+is\b",
    r"\bwhat\s+(is|are|does|means?)\b",
    r"\bdefin(e|ition|ing)\b",
    r"\bexplain\s+(what|how|why)\b",
    r"\b(history|origin|background)\s+of\b",
    r"\bconcept\s+of\b",
    r"\boverview\s+of\b",
    r"\btell\s+me\s+about\b",
    r"\bcompiled\s+truth\b",
    r"::|->|\.\w+\(",
    r"\b(function|class|method|module)\s+\w+",
    r"\b(graph|traversal|backlinks?|inbound|outbound)\b",
]));

static STRONG_RECENCY_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\btoday\b",
    r"\bright\s+now\b",
    r"\bthis\s+morning\b",
    r"\bjust\s+now\b",
]));

static RECENCY_ON_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\bwhat'?s\s+(going\s+on|happening|new|latest|up)\b",
    r"\b(latest|recent(ly)?|currently)\b",
    r"\b(this|last|past)\s+(week|month|few\s+days|couple\s+days)\b",
    r"\bmeeting\s+(prep|with|for|notes?|brief)\b",
    r"\bbefore\s+(my|the|our)\s+(meeting|call|sync|chat)\b",
    r"\bprep(are)?\s+(for|me)\b",
    r"\bcatch(es|ing)?\b[\s\w]{0,15}\bup\b",
    r"\bremind\s+me\s+(what|about|of)\b",
    r"\b(update|status|progress)\s+(on|with|from)\b",
]));

static EXPLICIT_TEMPORAL_BOUND_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\btoday\b",
    r"\bright\s+now\b",
    r"\bthis\s+morning\b",
    r"\bthis\s+week\b",
    r"\bsince\s+(launch|last|the|\d)",
    r"\blast\s+\d+\s+(day|days|week|weeks|month|months)\b",
]));

static SALIENCE_ON_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\bwhat'?s\s+(going\s+on|happening|been\s+going|been\s+up)\b",
    r"\bcatch(es|ing)?\b[\s\w]{0,15}\bup\b",
    r"\bremind\s+me\s+(what|about|of)\b",
    r"\bprep(are)?\s+(for|me)\b",
    r"\bbefore\s+(my|the|our)\s+(meeting|call|sync|chat)\b",
    r"\bmeeting\s+(prep|with|for|brief)\b",
    r"\b(update|status|progress)\s+(on|with|from)\b",
    r"\bwhat\s+matters\b",
    r"\bwhat'?s\s+important\b",
]));

static CROSS_MODAL_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\b(show|find|get|pull)\s+(me\s+)?(the\s+)?(photos?|images?|pictures?|pics?|screenshots?)\b",
    r"\b(photos?|images?|pictures?|pics?|screenshots?)\s+(of|from|at|with|showing|featuring)\b",
    r"\bwhat\s+does\s+[\w\s']{1,40}?\s+look\s+like\b",
    r"\b(whiteboard|diagram|slide|screenshot|infographic|chart)s?\s+(of|from|about|showing)\b",
    r"\bdiagram\s+(of|for|showing)\b",
    r"\bvisual(s|ly)?\s+(of|from|about|showing|representation)\b",
]));

static AMBIGUOUS_MODALITY_NOUNS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\b(photo|image|picture|pic|screenshot|diagram|whiteboard|slide|chart)s?\b",
    r"\blook(s|ed)?\s+like\b",
    r"\bvisual(s|ly)?\b",
]));

static AMBIGUOUS_REFERENCE_MARKERS: LazyLock<Vec<Regex>> = LazyLock::new(|| compile(&[
    r"\b(any|some|that|those|these|the)\s+(pic|pics|picture|pictures|photo|photos|image|images|screenshot|screenshots|diagram|diagrams|whiteboard|whiteboards|slide|slides|chart|charts)\b",
    r"\bfrom\s+(last|this|the)\s+(week|month|year|offsite|meeting|hackathon|deck)\b",
]));

fn compile(patterns: &[&str]) -> Vec<Regex> {
    patterns
        .iter()
        .map(|p| Regex::new(p).expect("query-intent pattern must compile"))
        .collect()
}

#[inline]
fn matches(bank: &[Regex], q: &str) -> bool {
    bank.iter().any(|re| re.is_match(q))
}

/// Classify a query and return all four axis suggestions.
///
/// Resolution rules mirror TS `classifyQuery`:
///   * `intent`: full-context > temporal > event > entity > general
///   * `suggested_detail`: entity → low, temporal/event → high, general → None
///   * `suggested_recency`: STRONG > ON; CANONICAL wins unless an explicit
///     temporal bound also matches; default `'off'`
///   * `suggested_salience`: ON when SALIENCE_ON matches and not canonical-
///     without-bound; default `'off'`
///   * `suggested_modality`: `'image'` only on an explicit cross-modal regex,
///     else `'text'`
pub fn classify_query(query: &str) -> QuerySuggestions {
    let intent = classify_query_intent(query);
    let suggested_detail = intent_to_detail(intent);

    let has_canonical = matches(&CANONICAL_PATTERNS, query);
    let has_temporal_bound = matches(&EXPLICIT_TEMPORAL_BOUND_PATTERNS, query);
    let has_strong_recency = matches(&STRONG_RECENCY_PATTERNS, query);
    let has_recency_on = matches(&RECENCY_ON_PATTERNS, query);
    let has_salience_on = matches(&SALIENCE_ON_PATTERNS, query);

    let suggested_recency = if has_canonical && !has_temporal_bound {
        RecencyMode::Off
    } else if has_strong_recency {
        RecencyMode::Strong
    } else if has_recency_on {
        RecencyMode::On
    } else {
        RecencyMode::Off
    };

    let suggested_salience = if has_canonical && !has_temporal_bound {
        SalienceMode::Off
    } else if has_salience_on {
        SalienceMode::On
    } else {
        SalienceMode::Off
    };

    let suggested_modality =
        if matches(&CROSS_MODAL_PATTERNS, query) { ModalityMode::Image } else { ModalityMode::Text };

    QuerySuggestions {
        intent,
        suggested_detail,
        suggested_salience,
        suggested_recency,
        suggested_modality,
    }
}

/// v0.36 — heuristic gate for the optional LLM intent escalation (Commit 4).
/// Pure; no LLM call. Fires only for the narrow band where a tie-break earns
/// its cost (visual noun + ambiguous reference marker, but not already a
/// confident cross-modal match). Mirrors TS `isAmbiguousModalityQuery`.
pub fn is_ambiguous_modality_query(query: &str) -> bool {
    if matches(&CROSS_MODAL_PATTERNS, query) {
        return false;
    }
    if !matches(&AMBIGUOUS_MODALITY_NOUNS, query) {
        return false;
    }
    matches(&AMBIGUOUS_REFERENCE_MARKERS, query)
}

/// v0.29.0 intent classifier. Priority: full-context > temporal > event >
/// entity > general.
pub fn classify_query_intent(query: &str) -> QueryIntent {
    if matches(&FULL_CONTEXT_PATTERNS, query) {
        return QueryIntent::Temporal;
    }
    if matches(&TEMPORAL_PATTERNS, query) {
        return QueryIntent::Temporal;
    }
    if matches(&EVENT_PATTERNS, query) {
        return QueryIntent::Event;
    }
    if matches(&ENTITY_PATTERNS, query) {
        return QueryIntent::Entity;
    }
    QueryIntent::General
}

/// v0.29.0 intent → detail mapping.
pub fn intent_to_detail(intent: QueryIntent) -> Option<SearchDetail> {
    match intent {
        QueryIntent::Entity => Some(SearchDetail::Low),
        QueryIntent::Temporal => Some(SearchDetail::High),
        QueryIntent::Event => Some(SearchDetail::High),
        QueryIntent::General => None,
    }
}

/// v0.29.0 helper. Routes through `classify_query` internally.
pub fn auto_detect_detail(query: &str) -> Option<SearchDetail> {
    classify_query(query).suggested_detail
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_query_intent ──────────────────────────────────────────
    #[test]
    fn intent_full_context_is_temporal() {
        assert_eq!(classify_query_intent("give me everything about the Q3 launch"), QueryIntent::Temporal);
    }
    #[test]
    fn intent_temporal_pattern() {
        assert_eq!(classify_query_intent("when did we last meet"), QueryIntent::Temporal);
    }
    #[test]
    fn intent_event_pattern() {
        assert_eq!(classify_query_intent("the company raised $10M"), QueryIntent::Event);
    }
    #[test]
    fn intent_entity_pattern() {
        assert_eq!(classify_query_intent("who is Jane Doe"), QueryIntent::Entity);
    }
    #[test]
    fn intent_general_default() {
        assert_eq!(classify_query_intent("some random thought"), QueryIntent::General);
    }

    // ── intent_to_detail ───────────────────────────────────────────────
    #[test]
    fn detail_entity_low() {
        assert_eq!(intent_to_detail(QueryIntent::Entity), Some(SearchDetail::Low));
    }
    #[test]
    fn detail_temporal_high() {
        assert_eq!(intent_to_detail(QueryIntent::Temporal), Some(SearchDetail::High));
    }
    #[test]
    fn detail_general_none() {
        assert_eq!(intent_to_detail(QueryIntent::General), None);
    }

    // ── classify_query axes ────────────────────────────────────────────
    #[test]
    fn classify_canonical_recency_off() {
        // "who is X" is canonical → recency off even though it's an entity.
        let s = classify_query("who is the CTO");
        assert_eq!(s.suggested_recency, RecencyMode::Off);
        assert_eq!(s.suggested_salience, SalienceMode::Off);
        assert_eq!(s.intent, QueryIntent::Entity);
        assert_eq!(s.suggested_detail, Some(SearchDetail::Low));
    }

    #[test]
    fn classify_recency_on_via_pattern() {
        let s = classify_query("what's the latest on the merger");
        assert_eq!(s.suggested_recency, RecencyMode::On);
    }

    #[test]
    fn classify_strong_recency_today() {
        let s = classify_query("what happened today");
        assert_eq!(s.suggested_recency, RecencyMode::Strong);
    }

    #[test]
    fn classify_temporal_bound_overrides_canonical() {
        // "who is X today" → canonical (\bwho is\b) + temporal bound
        // (\btoday\b) → recency STRONG (today is a STRONG_RECENCY
        // pattern, beats both canonical-suppression and RECENCY_ON).
        // The Rust port mirrors `query-intent.ts classifyQuery` exactly
        // (TS test "who is widget-ceo today" → recency=strong at
        // b8e0a0ea test/query-intent.test.ts:122). A port-only test that
        // asserted `On` was incorrect.
        let s = classify_query("who is the CEO today");
        assert_eq!(s.suggested_recency, RecencyMode::Strong);
    }

    #[test]
    fn classify_temporal_bound_weak_recency() {
        // Counterpart: a non-strong temporal bound on a canonical query
        // (e.g. "who is X this week" — not in STRONG_RECENCY_PATTERNS
        // but in EXPLICIT_TEMPORAL_BOUND) suppresses the canonical-off
        // override and lands on RECENCY_ON, not Strong.
        let s = classify_query("who is the CEO this week");
        assert_eq!(s.suggested_recency, RecencyMode::On);
    }

    #[test]
    fn classify_salience_on_for_catch_up() {
        let s = classify_query("catch me up on the project");
        assert_eq!(s.suggested_salience, SalienceMode::On);
    }

    #[test]
    fn classify_modality_image_explicit() {
        let s = classify_query("show me photos from the offsite");
        assert_eq!(s.suggested_modality, ModalityMode::Image);
    }

    #[test]
    fn classify_modality_text_default() {
        let s = classify_query("who is Jane Doe");
        assert_eq!(s.suggested_modality, ModalityMode::Text);
    }

    // ── is_ambiguous_modality_query ────────────────────────────────────
    #[test]
    fn ambiguous_false_when_confident() {
        assert!(!is_ambiguous_modality_query("show me photos of the launch"));
    }
    #[test]
    fn ambiguous_false_without_noun() {
        assert!(!is_ambiguous_modality_query("tell me about the meeting"));
    }
    #[test]
    fn ambiguous_true_noun_plus_marker() {
        // A truly ambiguous case: visual noun + a demonstrative/determiner
        // marker, but NOT already a confident cross-modal match. The
        // earlier test "any pics from last week's offsite" failed because
        // the CROSS_MODAL_PATTERNS `\b(pics|photos|...)\s+(of|from|...)`
        // matched `pics from` first, sending the query into the confident
        // branch (`is_ambiguous_modality_query` returns false). The TS
        // algorithm intentionally suppresses ambiguity for that case.
        assert!(is_ambiguous_modality_query("any pics I saw recently"));
    }

    // ── auto_detect_detail ─────────────────────────────────────────────
    #[test]
    fn auto_detect_detail_entity_low() {
        assert_eq!(auto_detect_detail("who is Jane Doe"), Some(SearchDetail::Low));
    }
    #[test]
    fn auto_detect_detail_general_none() {
        assert_eq!(auto_detect_detail("random note"), None);
    }
}
