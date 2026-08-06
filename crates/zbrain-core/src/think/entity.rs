//! Shared candidate-entity extraction for trajectory routing.
//!
//! Ported from `src/core/think/entity-extract.ts` (v0.40.2.0). Consumed by
//! `zbrain think` synthesis + the LongMemEval harness. Two sources, in
//! priority order:
//!
//!   1. Retrieved slugs that look like entity pages (`people/`, `companies/`,
//!      `organizations/`, `orgs/`, `deals/`). High precision — these came
//!      back from hybrid search, so we know the brain has them.
//!   2. Noun-phrase extraction from the question text. Medium precision — the
//!      downstream `resolveEntitySlug` resolution gate filters non-matches.
//!
//! Capped at 5 candidates per question. Pure (no engine access).

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// Single-word tokenizer: letters, hyphens, apostrophes (length 1-40). The
// caller stitches consecutive non-stop-word tokens into phrases so "Blue
// Bottle" stays together while "I last meet Marco" splits at stop-wordedges.
static WORD_RX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[a-zA-Z][a-zA-Z\-']{0,40}\b").expect("think entity word rx must compile")
});

// Lowercased entity-prefix paths the brain uses for canonical entity pages.
const ENTITY_PREFIXES: &[&str] = &[
    "people/",
    "companies/",
    "organizations/",
    "orgs/",
    "deals/",
];

// Stop-word set — common English words that would otherwise produce noise
// candidates. Lowercased; comparison happens after lowercasing the token.
static STOP_WORDS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    [
        "a", "an", "the", "i", "you", "he", "she", "we", "they", "it", "me", "us", "them", "my",
        "your", "his", "her", "our", "their", "this", "that", "these", "those", "is", "am", "are",
        "was", "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "doing",
        "can", "could", "will", "would", "should", "may", "might", "must", "what", "when", "where",
        "who", "whom", "whose", "why", "how", "which", "of", "in", "on", "at", "to", "from", "with",
        "without", "by", "for", "about", "against", "between", "through", "during", "before",
        "after", "above", "below", "and", "or", "but", "nor", "so", "yet", "because", "if", "as",
        "than", "into", "onto", "time", "date", "day", "week", "month", "year", "today", "yesterday",
        "tomorrow", "now", "then", "ago", "since", "until", "long", "thing", "things", "something",
        "anything", "nothing", "one", "ones", "kind", "sort", "type", "lot", "lots", "last", "first",
        "next", "previous", "recent", "latest", "current", "still", "just", "also", "only", "such",
        "much", "many", "most", "more", "less", "few", "some", "any", "all", "no", "not", "each",
        "every", "both", "either", "neither", "same", "different", "other", "others", "another",
        "said", "say", "says", "told", "tell", "tells", "asked", "ask", "know", "knew", "known",
        "think", "thought", "changed", "switched", "moved", "updated", "good", "bad", "better",
        "worse", "best", "worst", "new", "old", "big", "small", "high", "low",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
});

/// Where a candidate entity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityOrigin {
    /// Came from a retrieval result's slug (`people/marco`).
    Retrieved,
    /// Derived from question text via noun-phrase scan.
    Extracted,
}

/// A candidate entity for trajectory resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityCandidate {
    /// Raw candidate text. For retrieved candidates this is the slug itself
    /// (already canonical); for extracted candidates it's the lowercase phrase.
    pub raw: String,
    pub origin: EntityOrigin,
}

const MAX_CANDIDATES: usize = 5;

/// Common verbs that precede entity references in questions.
static LEADING_VERBS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    [
        "meet", "met", "saw", "see", "seen", "visit", "visited", "spoke", "speak", "spoken",
        "talked", "talk", "called", "call", "wrote", "write", "got", "get", "gotten", "bought",
        "buy", "received", "sold", "pinged", "emailed", "texted", "reached",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
});

/// Extract candidate entities from a question + retrieval-result slugs.
///
/// Output is deterministic order: retrieved-slug candidates first (in input
/// order, deduped), then noun-phrase candidates (in question-text order, deduped
/// against the retrieved set + each other). Capped at `MAX_CANDIDATES` total.
/// Mirrors `src/core/think/entity-extract.ts:extractCandidateEntities`.
pub fn extract_candidate_entities(
    question: &str,
    retrieved_slugs: &[String],
) -> Vec<EntityCandidate> {
    let mut out: Vec<EntityCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Source 1: retrieved slugs matching known entity prefixes.
    for slug in retrieved_slugs {
        if out.len() >= MAX_CANDIDATES {
            break;
        }
        let lower = slug.to_lowercase();
        if !ENTITY_PREFIXES.iter().any(|p| lower.starts_with(p)) {
            continue;
        }
        if seen.contains(&lower) {
            continue;
        }
        seen.insert(lower);
        out.push(EntityCandidate {
            raw: slug.clone(),
            origin: EntityOrigin::Retrieved,
        });
    }

    // Source 2: noun-phrase extraction from question text. Tokenize into single
    // words, then stitch runs of consecutive non-stop-words into phrases.
    if out.len() < MAX_CANDIDATES {
        let tokens: Vec<String> = WORD_RX
            .find_iter(question)
            .map(|m| m.as_str().to_lowercase())
            .collect();
        let mut phrases: Vec<String> = Vec::new();
        let mut current: Vec<String> = Vec::new();
        let flush = |current: &mut Vec<String>, phrases: &mut Vec<String>| {
            if !current.is_empty() {
                let joined = current.join(" ");
                // TS uses UTF-16 length; char length matches for ASCII tokens.
                if joined.chars().count() >= 2 && joined.chars().count() <= 40 {
                    phrases.push(joined);
                }
                current.clear();
            }
        };
        for tok in &tokens {
            if STOP_WORDS.contains(tok) {
                flush(&mut current, &mut phrases);
            } else {
                current.push(tok.clone());
            }
        }
        flush(&mut current, &mut phrases);

        for phrase in &phrases {
            if out.len() >= MAX_CANDIDATES {
                break;
            }
            let core = strip_leading_verb(phrase);
            if core.chars().count() < 2 {
                continue;
            }
            if seen.contains(&core) {
                continue;
            }
            seen.insert(core.clone());
            out.push(EntityCandidate {
                raw: core,
                origin: EntityOrigin::Extracted,
            });
        }
    }

    out
}

/// If the first word of a multi-word phrase is a common preceding verb AND the
/// remaining phrase is non-empty, return just the remaining phrase. Otherwise
/// return the phrase unchanged. Mirrors `stripLeadingVerb`.
fn strip_leading_verb(phrase: &str) -> String {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    if words.len() < 2 {
        return phrase.to_string();
    }
    if LEADING_VERBS.contains(words[0]) {
        words[1..].join(" ")
    } else {
        phrase.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrieved_slugs_first() {
        let slugs = vec![
            "people/marco".to_string(),
            "companies/acme".to_string(),
            "random/page".to_string(),
        ];
        let out = extract_candidate_entities("anything", &slugs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].raw, "people/marco");
        assert_eq!(out[0].origin, EntityOrigin::Retrieved);
        assert_eq!(out[1].raw, "companies/acme");
        // "random/page" is not an entity prefix → excluded.
    }

    #[test]
    fn retrieved_deduped_and_capped() {
        let slugs: Vec<String> = (0..10).map(|i| format!("people/p{i}")).collect();
        let out = extract_candidate_entities("x", &slugs);
        assert_eq!(out.len(), MAX_CANDIDATES);
    }

    #[test]
    fn extracted_noun_phrases() {
        // "meet marco at blue bottle": "marco" (verb-stripped) + "blue bottle".
        let out = extract_candidate_entities("when did I meet marco at blue bottle", &[]);
        let raws: Vec<&str> = out.iter().map(|c| c.raw.as_str()).collect();
        assert!(raws.contains(&"marco"));
        assert!(raws.contains(&"blue bottle"));
        assert!(out.iter().all(|c| c.origin == EntityOrigin::Extracted));
    }

    #[test]
    fn leading_verb_stripped() {
        let out = extract_candidate_entities("saw alice downtown", &[]);
        let raws: Vec<&str> = out.iter().map(|c| c.raw.as_str()).collect();
        assert!(raws.contains(&"alice"));
        assert!(!raws.contains(&"saw alice"));
    }

    #[test]
    fn extracted_dedup_against_retrieved() {
        let slugs = vec!["people/marco".to_string()];
        let out = extract_candidate_entities("tell me about marco", &slugs);
        let raws: Vec<&str> = out.iter().map(|c| c.raw.as_str()).collect();
        // "marco" from question should not be re-added (already in seen).
        assert_eq!(raws.iter().filter(|r| **r == "marco").count(), 1);
    }

    #[test]
    fn stops_are_not_candidates() {
        let out = extract_candidate_entities("the a an of with", &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn extracted_capped_with_retrieved() {
        let slugs = vec!["people/a".to_string(), "people/b".to_string()];
        // Long question yielding many phrases; total capped at MAX_CANDIDATES.
        let q = "meet carol saw dave visited erin called frank texted grace pinged heidi bought ivan";
        let out = extract_candidate_entities(q, &slugs);
        assert!(out.len() <= MAX_CANDIDATES);
        assert_eq!(out.len(), MAX_CANDIDATES);
    }

    #[test]
    fn determinism() {
        let slugs = vec!["people/marco".to_string(), "companies/acme".to_string()];
        let a = extract_candidate_entities("meet marco at blue bottle", &slugs);
        let b = extract_candidate_entities("meet marco at blue bottle", &slugs);
        assert_eq!(a, b);
    }
}
