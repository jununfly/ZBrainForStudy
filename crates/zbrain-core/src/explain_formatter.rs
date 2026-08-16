//! `zbrain query --explain` per-stage attribution formatter.
//!
//! Renders a `&[QueryResultItem]` as a human-readable, multi-line per-result
//! breakdown of how each final score was formed. It reads the attribution
//! stamps carried on [`crate::operation::QueryResultItem`] (`base_score` plus
//! the migrated boost stamps) and reconstructs the multiplier chain
//! `base → boost → reranker_delta → final` without re-running search.
//!
//! This is a byte-faithful port of the TS renderer
//! `src/core/search/explain-formatter.ts` (`formatResultExplain` /
//! `formatResultsExplain` / `fmt`). The output text is reproduced verbatim,
//! including the two-space alignment padding after `recency` and the
//! trailing-zero-trimming number format — so a diff against the TS CLI output
//! is empty for the migrated stages.
//!
//! Scope: only the three stages that have a Rust data layer today are
//! rendered — salience, recency, and reranker. The five TS boost axes that
//! are NOT migrated (backlink / exact-match / graph adjacency / graph
//! cross-source / session-demote) are intentionally absent: they block on
//! data layers that do not exist in Rust yet, so emitting placeholder lines
//! would be dishonest. registered in docs/plans/MIGRATION.md (G13).

use crate::operation::QueryResultItem;

/// Format a single result with per-stage attribution.
///
/// Returns a multi-line string with NO trailing newline; the caller joins
/// many with `\n\n`. `rank` is 1-based for human display.
///
/// Mirrors TS `formatResultExplain` (`explain-formatter.ts:38`) line for line.
#[must_use]
pub fn format_result_explain(result: &QueryResultItem, rank: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "{rank}. {slug} (score={score})",
        slug = result.page.slug,
        score = fmt(result.score)
    ));

    // base_score is the pre-boost RRF+cosine result. Always present on the
    // Rust side (SearchResult stamps it at fusion entry), so unlike the TS
    // `base_score ?? score` fallback we read it directly.
    lines.push(format!("   base={} (rrf+cosine)", fmt(result.base_score)));

    let mut any_boost = false;

    // Only the migrated stages exist as fields. The TS backlink / exact-match
    // / adjacency / cross_source / session_demote branches have no counterpart
    // here (see module docs + KNOWN-GAPS G13).
    if let Some(b) = result.salience_boost {
        if (b - 1.0).abs() > f64::EPSILON {
            any_boost = true;
            lines.push(format!("   + salience ×{}", fmt(b)));
        }
    }
    if let Some(b) = result.recency_boost {
        if (b - 1.0).abs() > f64::EPSILON {
            any_boost = true;
            // Two trailing spaces after "recency" align the ×-column with the
            // longer "salience"/"backlink" labels — verbatim from TS.
            lines.push(format!("   + recency  ×{}", fmt(b)));
        }
    }
    if let Some(delta) = result.reranker_delta {
        if delta != 0 {
            any_boost = true;
            let arrow = if delta > 0 { '↑' } else { '↓' };
            let sign = if delta > 0 { "+" } else { "" };
            lines.push(format!("   {arrow} reranker rank {sign}{delta}"));
        }
    }

    if !any_boost {
        lines.push("   no boosts applied".to_string());
    }

    lines.push(format!("   = final {}", fmt(result.score)));
    lines.join("\n")
}

/// Format a full result list. Handles enumeration internally.
///
/// Returns a single string WITH a trailing newline so the caller can write it
/// straight to stdout. Mirrors TS `formatResultsExplain`
/// (`explain-formatter.ts:103`): empty list → `"No results.\n"`; otherwise
/// each result joined with a blank line, plus one trailing newline.
#[must_use]
pub fn format_results_explain(results: &[QueryResultItem]) -> String {
    if results.is_empty() {
        return "No results.\n".to_string();
    }
    let body = results
        .iter()
        .enumerate()
        .map(|(i, r)| format_result_explain(r, i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("{body}\n")
}

/// Compact number formatter. 4 decimal places, then trailing zeros (and an
/// optional trailing dot) trimmed. Non-finite values stringify as-is.
///
/// Mirrors TS `fmt` (`explain-formatter.ts:113`):
/// `n.toFixed(4).replace(/\.?0+$/, '')`.
fn fmt(n: f64) -> String {
    if !n.is_finite() {
        // Match JS String(n): "NaN", "Infinity", "-Infinity".
        return if n.is_nan() {
            "NaN".to_string()
        } else if n > 0.0 {
            "Infinity".to_string()
        } else {
            "-Infinity".to_string()
        };
    }
    let s = format!("{n:.4}");
    // Trim trailing zeros, then a dangling decimal point. `toFixed(4)` always
    // emits a '.', so this only strips the fractional tail — never the integer
    // digits.
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Page;
    use crate::types::PageKind;
    use serde_json::{Map, Value};

    /// Minimal `QueryResultItem` builder for formatter assertions. Only the
    /// fields the formatter reads matter; the rest of `Page` is filled with
    /// inert defaults.
    fn item(
        slug: &str,
        score: f64,
        base_score: f64,
        salience_boost: Option<f64>,
        recency_boost: Option<f64>,
        reranker_delta: Option<i64>,
    ) -> QueryResultItem {
        QueryResultItem {
            page: Page {
                id: 1,
                slug: slug.to_string(),
                page_type: "note".to_string(),
                page_kind: PageKind::Markdown,
                title: "T".to_string(),
                compiled_truth: String::new(),
                timeline: String::new(),
                frontmatter: Value::Object(Map::default()),
                content_hash: None,
                emotional_weight: None,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                updated_at: "2026-01-01T00:00:00Z".to_string(),
                deleted_at: None,
                last_retrieved_at: None,
                effective_date: None,
                effective_date_source: None,
                import_filename: None,
                salience_touched_at: None,
                salience_score: None,
                generation: 1,
                embedding: None,
                chunker_version: 1,
                source_path: None,
                source_id: "default".to_string(),
                source_kind: None,
                source_uri: None,
                ingested_via: None,
                ingested_at: None,
                contextual_retrieval_mode: None,
                corpus_generation: None,
            },
            score,
            snippet: None,
            base_score,
            salience_boost,
            recency_boost,
            reranker_delta,
        }
    }

    #[test]
    fn fmt_trims_trailing_zeros_and_dot() {
        assert_eq!(fmt(12.4), "12.4");
        assert_eq!(fmt(10.2), "10.2");
        assert_eq!(fmt(1.0), "1");
        assert_eq!(fmt(1.05), "1.05");
        assert_eq!(fmt(1.5000), "1.5");
        // 4-decimal precision, then trim.
        assert_eq!(fmt(0.0123456), "0.0123");
        assert_eq!(fmt(0.0), "0");
    }

    #[test]
    fn fmt_handles_non_finite() {
        assert_eq!(fmt(f64::NAN), "NaN");
        assert_eq!(fmt(f64::INFINITY), "Infinity");
        assert_eq!(fmt(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn single_result_no_boosts() {
        let r = item("people/alice", 10.2, 10.2, None, None, None);
        let out = format_result_explain(&r, 1);
        assert_eq!(
            out,
            "1. people/alice (score=10.2)\n   base=10.2 (rrf+cosine)\n   no boosts applied\n   = final 10.2"
        );
    }

    #[test]
    fn boost_at_exactly_one_is_not_rendered() {
        // A stamped-but-neutral (×1.0) boost must not print, matching the TS
        // `!== 1.0` guard — otherwise it'd claim a boost that did nothing.
        let r = item("x/y", 5.0, 5.0, Some(1.0), Some(1.0), Some(0));
        let out = format_result_explain(&r, 1);
        assert_eq!(
            out,
            "1. x/y (score=5)\n   base=5 (rrf+cosine)\n   no boosts applied\n   = final 5"
        );
    }

    #[test]
    fn salience_and_recency_boosts_render_with_alignment() {
        let r = item("proj/notes", 12.4, 10.2, Some(1.05), Some(1.0034), None);
        let out = format_result_explain(&r, 2);
        assert_eq!(
            out,
            "2. proj/notes (score=12.4)\n   base=10.2 (rrf+cosine)\n   + salience ×1.05\n   + recency  ×1.0034\n   = final 12.4"
        );
    }

    #[test]
    fn reranker_delta_positive_and_negative() {
        let up = item("a/up", 9.0, 8.0, None, None, Some(2));
        assert_eq!(
            format_result_explain(&up, 1),
            "1. a/up (score=9)\n   base=8 (rrf+cosine)\n   ↑ reranker rank +2\n   = final 9"
        );
        let down = item("a/down", 7.0, 8.0, None, None, Some(-1));
        assert_eq!(
            format_result_explain(&down, 1),
            "1. a/down (score=7)\n   base=8 (rrf+cosine)\n   ↓ reranker rank -1\n   = final 7"
        );
    }

    #[test]
    fn empty_list_prints_no_results() {
        assert_eq!(format_results_explain(&[]), "No results.\n");
    }

    #[test]
    fn multiple_results_joined_with_blank_line_and_trailing_newline() {
        let a = item("a", 2.0, 2.0, None, None, None);
        let b = item("b", 1.0, 1.0, Some(1.1), None, None);
        let out = format_results_explain(&[a, b]);
        assert_eq!(
            out,
            "1. a (score=2)\n   base=2 (rrf+cosine)\n   no boosts applied\n   = final 2\n\n\
2. b (score=1)\n   base=1 (rrf+cosine)\n   + salience ×1.1\n   = final 1\n"
        );
    }
}