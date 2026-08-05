//! Structured-citations → inline-marker rendering for `zbrain think`.
//!
//! Ported from `src/core/think/cite-render.ts` (v0.28). The model's
//! structured output gives `{ citations: [{page_slug, row_num|null,
//! citation_index}], answer: "…[slug#row]…" }`.
//!
//! Trust contract:
//!   1. ALWAYS prefer the structured citations field — parseable, indexed.
//!   2. If structured is missing/invalid, fall back to a regex scan of the
//!      answer body for `[slug#row]` / `[slug]`. Never fail synthesis because
//!      the model omitted citations — log a warning, persist what we recover.

use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::LazyLock;

/// A resolved citation marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCitation {
    pub page_slug: String,
    /// `None` = page-level citation; `Some` = take citation.
    pub row_num: Option<u32>,
    /// 1-based order in the body / structured list.
    pub citation_index: u32,
}

fn ci(pat: &str) -> Regex {
    Regex::new(pat).expect("think citation rx must compile")
}

// `[slug#3]` → take; `[slug]` → page; `[slug/with/path#7]` → take multi-segment.
// Slugs match the validatePageSlug allowlist (lowercase alnum + hyphen +
// forward-slash). Anything outside that pattern won't match.
static CITATION_RX: LazyLock<Regex> = LazyLock::new(|| {
    ci(r"\[([a-z0-9][a-z0-9\-]*(?:/[a-z0-9][a-z0-9\-]*)*)(?:#(\d+))?\]")
});

/// Extract citation markers from an answer body (fallback path).
///
/// Mirrors `src/core/think/cite-render.ts:parseInlineCitations`.
pub fn parse_inline_citations(body: &str) -> Vec<ParsedCitation> {
    let mut out: Vec<ParsedCitation> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut idx: u32 = 1;
    for cap in CITATION_RX.captures_iter(body) {
        let slug = cap[1].to_lowercase();
        let row_num: Option<u32> = match cap.get(2).map(|m| m.as_str()) {
            Some(s) => match s.parse::<i64>() {
                Ok(n) if n > 0 => Some(n as u32),
                _ => continue, // non-finite or <= 0 → skip entirely (TS `continue`)
            },
            None => None,
        };
        let key = format!(
            "{}#{}",
            slug,
            row_num.map(|n| n.to_string()).unwrap_or_else(|| "_".to_string())
        );
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        out.push(ParsedCitation {
            page_slug: slug,
            row_num,
            citation_index: idx,
        });
        idx += 1;
    }
    out
}

/// Normalized structured citations + any warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedCitations {
    pub citations: Vec<ParsedCitation>,
    pub warnings: Vec<String>,
}

/// Validate a structured citations array (the model's `citations` field).
///
/// Accepts the JSON `Value` of that field. Returns the cleaned list + warnings
/// about dropped/invalid entries. Mirrors
/// `src/core/think/cite-render.ts:normalizeStructuredCitations`.
pub fn normalize_structured_citations(raw: &Value) -> NormalizedCitations {
    let mut citations: Vec<ParsedCitation> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let arr = match raw.as_array() {
        Some(a) => a,
        None => return NormalizedCitations {
            citations,
            warnings: vec!["CITATIONS_NOT_ARRAY".to_string()],
        },
    };
    let mut idx: u32 = 1;
    let mut seen: HashSet<String> = HashSet::new();
    for c in arr {
        let obj = match c.as_object() {
            Some(o) => o,
            None => {
                warnings.push("CITATION_NOT_OBJECT".to_string());
                continue;
            }
        };
        let slug = match obj.get("page_slug").and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => s,
            _ => {
                warnings.push("CITATION_MISSING_SLUG".to_string());
                continue;
            }
        };
        let mut row_num: Option<u32> = None;
        if let Some(rv) = obj.get("row_num") {
            if !rv.is_null() {
                let n: Option<i64> = match rv {
                    Value::Number(n) => n.as_i64(),
                    Value::String(s) => s.parse::<i64>().ok(),
                    _ => None,
                };
                match n {
                    Some(m) if m > 0 => row_num = Some(m as u32),
                    _ => {
                        warnings.push(format!("CITATION_INVALID_ROW({slug}: {rv})"));
                        continue;
                    }
                }
            }
        }
        let key = format!(
            "{}#{}",
            slug.to_lowercase(),
            row_num.map(|n| n.to_string()).unwrap_or_else(|| "_".to_string())
        );
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key);
        citations.push(ParsedCitation {
            page_slug: slug.to_lowercase(),
            row_num,
            citation_index: idx,
        });
        idx += 1;
    }
    NormalizedCitations { citations, warnings }
}

/// Resolved citations + warnings + whether the regex fallback was used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCitations {
    pub citations: Vec<ParsedCitation>,
    pub warnings: Vec<String>,
    pub used_fallback: bool,
}

/// Combine structured citations + body fallback into a single resolved list.
///
/// If structured has any valid entries, use them as the source of truth.
/// Otherwise fall back to the inline-marker scan and emit a warning. Mirrors
/// `src/core/think/cite-render.ts:resolveCitations`.
pub fn resolve_citations(structured_raw: &Value, answer_body: &str) -> ResolvedCitations {
    let structured = normalize_structured_citations(structured_raw);
    if !structured.citations.is_empty() {
        return ResolvedCitations {
            citations: structured.citations,
            warnings: structured.warnings,
            used_fallback: false,
        };
    }
    let fallback = parse_inline_citations(answer_body);
    let mut warnings = structured.warnings;
    warnings.push("CITATIONS_REGEX_FALLBACK".to_string());
    ResolvedCitations {
        citations: fallback,
        warnings,
        used_fallback: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_inline_take_and_page() {
        let body = "See [people/alice#3] and [companies/acme] for details";
        let out = parse_inline_citations(body);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].page_slug, "people/alice");
        assert_eq!(out[0].row_num, Some(3));
        assert_eq!(out[1].page_slug, "companies/acme");
        assert_eq!(out[1].row_num, None);
        assert_eq!(out[0].citation_index, 1);
        assert_eq!(out[1].citation_index, 2);
    }

    #[test]
    fn parse_inline_dedups() {
        let body = "[people/alice#3] again [people/alice#3]";
        let out = parse_inline_citations(body);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn parse_inline_skips_invalid_row() {
        // row "0" → <= 0 → skipped entirely.
        let body = "bad [people/alice#0] good [people/bob#2]";
        let out = parse_inline_citations(body);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].page_slug, "people/bob");
        assert_eq!(out[0].row_num, Some(2));
    }

    #[test]
    fn parse_inline_lowercases_slug() {
        let body = "[PEOPLE/Alice#1]";
        let out = parse_inline_citations(body);
        assert_eq!(out[0].page_slug, "people/alice");
    }

    #[test]
    fn normalize_not_array() {
        let r = normalize_structured_citations(&json!({"foo": 1}));
        assert!(r.citations.is_empty());
        assert_eq!(r.warnings, vec!["CITATIONS_NOT_ARRAY".to_string()]);
    }

    #[test]
    fn normalize_valid_entries() {
        let r = normalize_structured_citations(&json!([
            {"page_slug": "people/alice", "row_num": 3, "citation_index": 1},
            {"page_slug": "companies/acme", "row_num": null, "citation_index": 2}
        ]));
        assert_eq!(r.citations.len(), 2);
        assert_eq!(r.citations[0].row_num, Some(3));
        assert_eq!(r.citations[1].row_num, None);
        assert_eq!(r.citations[1].page_slug, "companies/acme");
    }

    #[test]
    fn normalize_missing_slug_warns() {
        let r = normalize_structured_citations(&json!([{"row_num": 1}]));
        assert!(r.citations.is_empty());
        assert!(r.warnings.contains(&"CITATION_MISSING_SLUG".to_string()));
    }

    #[test]
    fn normalize_invalid_row_warns_and_skips() {
        let r = normalize_structured_citations(&json!([
            {"page_slug": "people/alice", "row_num": -1}
        ]));
        assert!(r.citations.is_empty());
        assert!(r.warnings.iter().any(|w| w.starts_with("CITATION_INVALID_ROW")));
    }

    #[test]
    fn resolve_prefers_structured() {
        let r = resolve_citations(
            &json!([{"page_slug": "people/alice", "row_num": 1}]),
            "stray [people/bob#2]",
        );
        assert!(!r.used_fallback);
        assert_eq!(r.citations.len(), 1);
        assert_eq!(r.citations[0].page_slug, "people/alice");
    }

    #[test]
    fn resolve_falls_back_to_regex() {
        let r = resolve_citations(&json!([]), "see [people/bob#2]");
        assert!(r.used_fallback);
        assert_eq!(r.citations.len(), 1);
        assert!(r.warnings.contains(&"CITATIONS_REGEX_FALLBACK".to_string()));
    }
}
