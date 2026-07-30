//! Model context budget helpers for the synthesize phase (1-3-4-3).
//!
//! Faithful Rust port of the three pure functions from the deleted
//! `src/core/cycle/synthesize.ts`:
//!   - `computeChunkCharBudget` (was `computeChunkCharBudget`)
//!   - `splitTranscriptByBudget` (was `splitTranscriptByBudget`)
//!   - `rewriteChunkedSlug`     (was `rewriteChunkedSlug`)
//!
//! All three are pure (`&str`/`&str`/`usize` in, `Vec<String>`/`String`/`u32`
//! out) with no engine dependency, so they can land independently of the
//! orchestration work in 1-3-4-4..1-3-4-6.
//!
//! Porting notes vs the original TypeScript:
//!   - The TS `computeChunkCharBudget` emitted a once-per-process stderr
//!     warning for unknown models via a module-level `Set`. That is a side
//!     effect; we drop it to keep the function pure. The orchestrator
//!     (1-3-4-4) can log the fallback choice if it cares.
//!   - `MIN_PROMPT_TOKENS` floor is applied by the caller (config loader),
//!     exactly as in TS — this function trusts `config_max_prompt_tokens`
//!     is already floored when `Some`.
//!   - `splitTranscriptByBudget` counts **Unicode scalar values** (chars),
//!     not UTF-16 code units (TS `string.length`) nor UTF-8 bytes. Rust has
//!     no UTF-16 surrogates, so the original `safeSplitIndex` surrogate-pair
//!     guard collapses to "hard-split at the `max_chars` char boundary",
//!     which is by construction a valid char boundary.

/// Anthropic model id → input context window (tokens).
/// Unknown id (non-Anthropic alias, custom string) → `UNKNOWN_MODEL_BUDGET_TOKENS`
/// fallback via `compute_chunk_char_budget`. Mirrors the TS `MODEL_CONTEXT_TOKENS`
/// map; keyed on the exact strings the model resolver returns for known aliases.
const MODEL_CONTEXT_TOKENS: &[(&str, u32)] = &[
    ("claude-opus-4-7", 1_000_000),
    ("claude-opus-4-6", 1_000_000),
    ("claude-sonnet-4-6", 200_000),
    ("claude-sonnet-4-5", 200_000),
    ("claude-haiku-4-5-20251001", 200_000),
];

/// Token-to-char ratio. 3.5 matches PR #748; conservative for English text.
const CHARS_PER_TOKEN: f64 = 3.5;
/// Reserve 10% of context window for system prompt + tool defs + output.
const HEADROOM_RATIO: f64 = 0.9;
/// Floor on user-overridable max_prompt_tokens (matches PR #748 minimum).
/// Applied by the caller (config loader), not inside `compute_chunk_char_budget`.
pub const MIN_PROMPT_TOKENS: u32 = 100_000;
/// Default chunk-count cap; operator-configurable via
/// `dream.synthesize.max_chunks_per_transcript`.
pub const DEFAULT_MAX_CHUNKS: usize = 24;
/// Conservative default budget when model is unknown (200K × HEADROOM_RATIO).
const UNKNOWN_MODEL_BUDGET_TOKENS: u32 = 180_000;

/// Compute per-chunk character budget for the resolved model + config override.
///
/// Resolution (identical to TS):
///   - `config_max_prompt_tokens` (already floored at `MIN_PROMPT_TOKENS` by the
///     caller) wins when `Some`.
///   - Else the model's `MODEL_CONTEXT_TOKENS` entry × `HEADROOM_RATIO`.
///   - Else (non-Anthropic alias / custom id) `UNKNOWN_MODEL_BUDGET_TOKENS`.
///
/// Returns a character count (floored). Pure: no I/O, no global state.
pub fn compute_chunk_char_budget(model: &str, config_max_prompt_tokens: Option<u32>) -> u32 {
    if let Some(cfg) = config_max_prompt_tokens {
        return (cfg as f64 * CHARS_PER_TOKEN).floor() as u32;
    }
    match MODEL_CONTEXT_TOKENS.iter().find(|(m, _)| *m == model) {
        Some((_, ctx)) => (*ctx as f64 * HEADROOM_RATIO * CHARS_PER_TOKEN).floor() as u32,
        None => (UNKNOWN_MODEL_BUDGET_TOKENS as f64 * CHARS_PER_TOKEN).floor() as u32,
    }
}

/// Split `content` into chunks each at most `max_chars` (Unicode scalar values)
/// long, picking boundaries via a 3-tier ladder lifted from PR #748:
///   1. `\n## Topic:` separators (matches the daily-aggregated transcript shape)
///   2. `\n---\n` markdown HR markers
///   3. nearest `\n` newline
///
/// Deterministic chunk identity (D9): the back-half-of-budget search window is
/// seeded with a deterministic offset derived from `content_hash` so the same
/// `(content, content_hash, max_chars)` triple always produces identical chunks.
/// The hash-derived offset jitters the search start within
/// `[0.5×budget, 0.6×budget]` so the back-half rule still holds.
///
/// If no boundary fits, hard-split at `max_chars` (also deterministic in the
/// inputs, and always lands on a char boundary in Rust).
///
/// # Panics
/// Panics if `max_chars == 0` (the budget must be positive).
pub fn split_transcript_by_budget(content: &str, content_hash: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        panic!("split_transcript_by_budget: max_chars must be > 0, got 0");
    }
    let total = content.chars().count();
    if total <= max_chars {
        return vec![content.to_string()];
    }
    // Byte offset for each char index, plus an end sentinel.
    let byte_offsets: Vec<usize> = content.char_indices().map(|(b, _)| b).collect();
    let byte_at = |ci: usize| -> usize {
        if ci >= total {
            content.len()
        } else {
            byte_offsets[ci]
        }
    };

    let hash_int = parse_hash_offset(content_hash);
    // Jitter window is the next 10% of budget after the 50% midpoint.
    let jitter_range = ((max_chars as f64) * 0.1).max(1.0) as usize;
    let search_start = (((max_chars as f64) * 0.5) as usize + (hash_int as usize % jitter_range)).min(max_chars);

    let mut out: Vec<String> = Vec::new();
    let mut rs = 0usize; // remaining start char index
    loop {
        let remaining_len = total - rs;
        if remaining_len <= max_chars {
            out.push(content[byte_at(rs)..].to_string());
            break;
        }
        let win_start = rs + search_start;
        let win_end = (rs + max_chars).min(total);
        let window = &content[byte_at(win_start)..byte_at(win_end)];
        // Tier 1: "\n## Topic:" — last occurrence inside the search window.
        // Tier 2: "\n---\n" markdown HR.
        // Tier 3: any newline.
        // `rel` is a byte offset within `window`; convert to a char index within
        // `window` so we can add `win_start` to get an absolute char index.
        let rel = window
            .rfind("\n## Topic:")
            .or_else(|| window.rfind("\n---\n"))
            .or_else(|| window.rfind('\n'));
        let split_ci = match rel {
            Some(b) => win_start + window[..b].chars().count(),
            // No boundary fits; hard-split at the `max_chars` char boundary
            // (always a valid char boundary in Rust).
            None => win_end,
        };
        out.push(content[byte_at(rs)..byte_at(split_ci)].to_string());
        rs = split_ci;
    }
    out
}

/// First 8 hex chars of `content_hash` as a `u32`, used to jitter the splitter
/// search window. Mirrors the TS `parseHashOffset` (returns 0 on a non-hex or
/// empty input). Real `content_hash` values are sha256 hex, so this is exact.
fn parse_hash_offset(content_hash: &str) -> u32 {
    let hex: String = content_hash.chars().take(8).collect();
    u32::from_str_radix(&hex, 16).unwrap_or(0)
}

/// D6: orchestrator-side deterministic slug rewrite. Zero model trust.
///
/// Expected shape from the synthesis prompt builder for a chunked child is
/// already `<base>-<hash6>-c<idx>`, but if the model drops the chunk suffix
/// this rewrite enforces uniqueness post-hoc. Same hash AND same chunk idx →
/// idempotent.
///
/// Cases (identical to TS):
///   - already correctly suffixed (`...-<hash6>-c<idx>`) → return unchanged.
///   - bare hash suffix (`...-<hash6>`) → append `-c<idx>`.
///   - some other shape → pass through (orchestrator can't safely guess where
///     to inject the chunk index).
pub fn rewrite_chunked_slug(slug: &str, hash6: &str, idx: usize) -> String {
    if slug.is_empty() {
        return slug.to_string();
    }
    let expected = format!("{}-c{}", hash6, idx);
    // Already correctly chunk-suffixed.
    if slug == expected {
        return slug.to_string();
    }
    if slug.ends_with(&format!("-{}", expected)) || slug.ends_with(&format!("/{}", expected)) {
        return slug.to_string();
    }
    // Bare hash6 at end of last path segment (preceded by start / '-' / '/'):
    // rewrite. (Manual check instead of a regex so a `hash6` containing regex
    // metacharacters can't break the match — the TS original built a `RegExp`
    // from the raw hash.)
    if let Some(prefix) = strip_trailing_hash6(slug, hash6) {
        return format!("{}{}-c{}", prefix, hash6, idx);
    }
    // Unknown shape — pass through; collision risk is bounded by the model's
    // per-chunk-prompt guidance and the existing slug-prefix allow-list.
    slug.to_string()
}

/// If `slug` ends with `hash6` and the char immediately before it (if any) is
/// the start of the string, `-`, or `/`, return the slug with `hash6` removed
/// (so the caller can re-append `-c<idx>`). Otherwise `None` (pass through).
fn strip_trailing_hash6(slug: &str, hash6: &str) -> Option<String> {
    if !slug.ends_with(hash6) {
        return None;
    }
    let without = &slug[..slug.len() - hash6.len()];
    match without.chars().next_back() {
        // slug was exactly `hash6` → `^hash6$` matched.
        None => Some(String::new()),
        Some(c) if c == '-' || c == '/' => Some(without.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── compute_chunk_char_budget ───────────────────────────────────────

    #[test]
    fn budget_known_opus_uses_context_times_headroom() {
        // 1_000_000 × 0.9 × 3.5 = 3_150_000
        assert_eq!(compute_chunk_char_budget("claude-opus-4-7", None), 3_150_000);
        assert_eq!(compute_chunk_char_budget("claude-opus-4-6", None), 3_150_000);
    }

    #[test]
    fn budget_known_sonnet_uses_context_times_headroom() {
        // 200_000 × 0.9 × 3.5 = 630_000
        assert_eq!(compute_chunk_char_budget("claude-sonnet-4-6", None), 630_000);
        assert_eq!(compute_chunk_char_budget("claude-sonnet-4-5", None), 630_000);
        assert_eq!(compute_chunk_char_budget("claude-haiku-4-5-20251001", None), 630_000);
    }

    #[test]
    fn budget_unknown_model_falls_back_to_conservative() {
        // 180_000 × 3.5 = 630_000
        assert_eq!(compute_chunk_char_budget("gpt-4o", None), 630_000);
        assert_eq!(compute_chunk_char_budget("some-custom-id", None), 630_000);
    }

    #[test]
    fn budget_config_override_wins_and_is_floored() {
        // config wins regardless of model; floored to char count.
        assert_eq!(compute_chunk_char_budget("claude-opus-4-7", Some(120_000)), 420_000);
        // Non-integer result is floored.
        assert_eq!(compute_chunk_char_budget("claude-opus-4-7", Some(100_001)), 350_003);
        // Unknown model + config override → config still wins.
        assert_eq!(compute_chunk_char_budget("gpt-4o", Some(50_000)), 175_000);
    }

    // ── split_transcript_by_budget ──────────────────────────────────────

    #[test]
    #[should_panic(expected = "max_chars must be > 0")]
    fn split_panics_on_zero_budget() {
        split_transcript_by_budget("hello", "abcdef01", 0);
    }

    #[test]
    fn split_short_content_is_single_chunk() {
        let out = split_transcript_by_budget("short text", "abcdef01", 100);
        assert_eq!(out, vec!["short text".to_string()]);
    }

    #[test]
    fn split_exact_budget_is_single_chunk() {
        let content = "a".repeat(100);
        let out = split_transcript_by_budget(&content, "abcdef01", 100);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], content);
    }

    #[test]
    fn split_one_over_budget_breaks_at_topic() {
        // 3 topic sections, budget smaller than the whole thing so it must split.
        let content = [
            "intro line",
            "## Topic: alpha",
            "body alpha",
            "## Topic: beta",
            "body beta",
            "## Topic: gamma",
            "body gamma",
        ]
        .join("\n");
        let out = split_transcript_by_budget(&content, "abcdef01", 40);
        // Each chunk should start at a topic boundary (the leading "\n" is kept
        // with the previous chunk, so the next chunk begins with "## Topic:").
        assert!(out.len() >= 2, "expected multiple chunks, got {}", out.len());
        assert!(out[1].starts_with("\n## Topic:"));
        // Reassembling yields the original content (splitting is lossless).
        assert_eq!(out.concat(), content);
    }

    #[test]
    fn split_breaks_at_hr_when_no_topic() {
        let content = [
            "first block of text here",
            "---",
            "second block of text here",
            "---",
            "third block of text here",
        ]
        .join("\n");
        let out = split_transcript_by_budget(&content, "abcdef01", 30);
        assert!(out.len() >= 2);
        // At least one chunk boundary is an HR marker.
        assert!(out.iter().any(|c| c.contains("---")) || out.concat() == content);
        assert_eq!(out.concat(), content);
    }

    #[test]
    fn split_falls_back_to_newline() {
        let content = "word word word word word word word word word word word word word word word";
        let out = split_transcript_by_budget(&content, "abcdef01", 20);
        assert!(out.len() >= 2);
        // No internal topic/HR markers exist, so every boundary is a space/newline.
        for c in &out {
            assert!(!c.is_empty());
        }
        assert_eq!(out.concat(), content);
    }

    #[test]
    fn split_is_deterministic_for_same_inputs() {
        let content = "a".repeat(500);
        let a = split_transcript_by_budget(&content, "deadbeef", 100);
        let b = split_transcript_by_budget(&content, "deadbeef", 100);
        assert_eq!(a, b);
        // And reassembly is exact.
        assert_eq!(a.concat(), content);
    }

    #[test]
    fn split_differs_by_hash_jitter_but_still_deterministic() {
        let content = "x".repeat(1000);
        let h1 = split_transcript_by_budget(&content, "aaaaaaaa", 200);
        let h2 = split_transcript_by_budget(&content, "bbbbbbbb", 200);
        // Each is individually deterministic.
        assert_eq!(h1, split_transcript_by_budget(&content, "aaaaaaaa", 200));
        assert_eq!(h2, split_transcript_by_budget(&content, "bbbbbbbb", 200));
        // Both reassemble to the original.
        assert_eq!(h1.concat(), content);
        assert_eq!(h2.concat(), content);
    }

    #[test]
    fn split_hard_splits_when_no_boundary() {
        // One giant "word" with no newline → must hard-split at max_chars.
        let content = "z".repeat(250);
        let out = split_transcript_by_budget(&content, "abcdef01", 100);
        assert!(out.len() >= 2);
        for c in &out {
            assert!(c.chars().count() <= 100);
        }
        assert_eq!(out.concat(), content);
    }

    // ── rewrite_chunked_slug ────────────────────────────────────────────

    #[test]
    fn slug_empty_passthrough() {
        assert_eq!(rewrite_chunked_slug("", "abc123", 0), "");
    }

    #[test]
    fn slug_already_correct_unchanged() {
        let slug = "abc123-c0";
        assert_eq!(rewrite_chunked_slug(slug, "abc123", 0), slug);
        let slug2 = "abc123-c3";
        assert_eq!(rewrite_chunked_slug(slug2, "abc123", 3), slug2);
    }

    #[test]
    fn slug_ends_with_full_suffix_unchanged() {
        assert_eq!(rewrite_chunked_slug("base-abc123-c0", "abc123", 0), "base-abc123-c0");
        assert_eq!(rewrite_chunked_slug("dir/base/abc123-c2", "abc123", 2), "dir/base/abc123-c2");
    }

    #[test]
    fn slug_bare_hash_appends_chunk_index() {
        assert_eq!(rewrite_chunked_slug("abc123", "abc123", 0), "abc123-c0");
        assert_eq!(rewrite_chunked_slug("base-abc123", "abc123", 1), "base-abc123-c1");
        assert_eq!(rewrite_chunked_slug("dir/base/abc123", "abc123", 2), "dir/base/abc123-c2");
    }

    #[test]
    fn slug_unknown_shape_passthrough() {
        // "base-other" ends in "other", not the hash → can't safely inject.
        assert_eq!(rewrite_chunked_slug("base-other", "abc123", 0), "base-other");
        // Trailing hash6 not preceded by separator (no '-' or '/' before it).
        assert_eq!(rewrite_chunked_slug("xyzabc123", "abc123", 0), "xyzabc123");
    }

    #[test]
    fn slug_chunk_index_is_used() {
        assert_eq!(rewrite_chunked_slug("base-abc123", "abc123", 5), "base-abc123-c5");
    }
}
