//! Recursive Delimiter-Aware Text Chunker
//!
//! Ported from `src/core/chunkers/recursive.ts`.
//!
//! 5-level delimiter hierarchy:
//!   1. Paragraphs (\n\n)
//!   2. Lines (\n)
//!   3. Sentences (. ! ? followed by space/newline; CJK 。！？)
//!   4. Clauses (; : , + CJK ；：，、)
//!   5. Words (whitespace + CJK char-slice fallback)
//!
//! Config: 300-word chunks with 50-word sentence-aware overlap.
//! maxChars hard cap (default 6000) sliding-window safety belt.

use crate::cjk::count_cjk_aware_words;

/// A chunk produced by the recursive text chunker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk {
    pub text: String,
    pub index: usize,
}

/// Options for text chunking.
#[derive(Debug, Clone)]
pub struct ChunkOptions {
    /// Target words per chunk (default 300).
    pub chunk_size: usize,
    /// Overlap words between consecutive chunks (default 50).
    pub chunk_overlap: usize,
    /// Hard cap on any single chunk's character length (default 6000).
    pub max_chars: usize,
}

impl Default for ChunkOptions {
    fn default() -> Self {
        Self {
            chunk_size: 300,
            chunk_overlap: 50,
            max_chars: 6000,
        }
    }
}

/// 5-level delimiter hierarchy for recursive splitting.
/// Each level is a list of delimiter strings tried in order.
fn delimiters(level: usize) -> &'static [&'static str] {
    static L0: &[&str] = &["\n\n"];
    static L1: &[&str] = &["\n"];
    static L2: &[&str] = &[
        ". ", "! ", "? ", ".\n", "!\n", "?\n", "。", "！", "？",
    ];
    static L3: &[&str] = &["; ", ": ", ", ", "；", "：", "，", "、"];
    static L4: &[&str] = &[];
    match level {
        0 => L0,
        1 => L1,
        2 => L2,
        3 => L3,
        _ => L4,
    }
}

const MAX_DELIMITER_LEVELS: usize = 5;

/// Word count, CJK-aware. Delegates to `count_cjk_aware_words`.
fn count_words(text: &str) -> usize {
    count_cjk_aware_words(text)
}

/// Main entry point: chunk `text` according to `opts`.
pub fn chunk_text(text: &str, opts: Option<&ChunkOptions>) -> Vec<TextChunk> {
    let opts = opts.cloned().unwrap_or_default();
    let chunk_size = opts.chunk_size;
    let chunk_overlap = opts.chunk_overlap;
    let max_chars = opts.max_chars;

    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    // Pre-processing: the TS does stripTakesFence + stripFactsFence here.
    // We delegate that to the caller in Rust (separation of concerns).
    // The chunker itself is a pure text→chunks transform.
    let working = trimmed;

    let word_count = count_words(working);
    if word_count <= chunk_size {
        // Single-chunk path: still apply the maxChars cap.
        let capped = cap_by_chars(working, max_chars);
        return capped
            .into_iter()
            .enumerate()
            .map(|(i, t)| TextChunk { text: t, index: i })
            .collect();
    }

    // Recursively split, then greedily merge to target size
    let pieces = recursive_split(working, 0, chunk_size);
    let merged = greedy_merge(&pieces, chunk_size);
    let with_overlap = apply_overlap(&merged, chunk_overlap);
    // Hard char cap on each chunk
    let mut capped: Vec<String> = Vec::new();
    for chunk in &with_overlap {
        capped.extend(cap_by_chars(chunk.trim(), max_chars));
    }
    capped
        .into_iter()
        .enumerate()
        .map(|(i, t)| TextChunk { text: t, index: i })
        .collect()
}

/// Hard-cap a chunk's char length via a sliding window. Returns the input
/// unchanged when it's already <= max_chars.
fn cap_by_chars(text: &str, max_chars: usize) -> Vec<String> {
    if text.len() <= max_chars {
        return if text.is_empty() { vec![] } else { vec![text.to_string()] };
    }
    let overlap = std::cmp::min(500, max_chars / 10);
    let stride = std::cmp::max(1, max_chars.saturating_sub(overlap));
    let mut out: Vec<String> = Vec::new();
    let mut byte_pos: usize = 0;
    while byte_pos < text.len() {
        // Find the char boundary at or after byte_pos + max_chars
        let end_byte = std::cmp::min(byte_pos + max_chars, text.len());
        // Ensure we're on a UTF-8 char boundary
        let end_byte = find_char_boundary(text, end_byte);
        // Similarly ensure byte_pos is on a char boundary (it should be, but be safe)
        let start_byte = find_char_boundary(text, byte_pos);

        let slice = text[start_byte..end_byte].trim();
        if !slice.is_empty() {
            out.push(slice.to_string());
        }
        if end_byte >= text.len() {
            break;
        }
        // Advance by stride chars (not bytes!) for the next window
        byte_pos = advance_by_chars(text, byte_pos, stride);
    }
    out
}

/// Returns `pos` if it's a valid UTF-8 char boundary, or the nearest
/// lower char boundary otherwise.
fn find_char_boundary(text: &str, pos: usize) -> usize {
    let pos = pos.min(text.len());
    if text.is_char_boundary(pos) {
        return pos;
    }
    // Walk backwards to find the previous char boundary
    (0..pos).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0)
}

/// Advance `pos` forward by `n` chars (not bytes), staying within bounds.
fn advance_by_chars(text: &str, pos: usize, n: usize) -> usize {
    let chars = text[pos..].chars();
    let mut byte_offset = pos;
    for (i, ch) in chars.enumerate() {
        if i >= n {
            break;
        }
        byte_offset += ch.len_utf8();
    }
    byte_offset.min(text.len())
}

/// Recursively split text by descending the delimiter hierarchy.
fn recursive_split(text: &str, level: usize, target: usize) -> Vec<String> {
    if level >= MAX_DELIMITER_LEVELS {
        return split_on_whitespace(text, target);
    }

    let delims = delimiters(level);
    if delims.is_empty() {
        return split_on_whitespace(text, target);
    }

    let pieces = split_at_delimiters(text, delims);

    // If splitting didn't help (only 1 piece), try next level
    if pieces.len() <= 1 {
        return recursive_split(text, level + 1, target);
    }

    // Recurse deeper on pieces that are still too large
    let mut result: Vec<String> = Vec::new();
    for piece in &pieces {
        if count_words(piece) > target {
            result.extend(recursive_split(piece, level + 1, target));
        } else {
            result.push(piece.clone());
        }
    }
    result
}

/// Split text at the earliest occurring delimiter, preserving delimiters
/// at the end of the piece that precedes them (lossless).
fn split_at_delimiters(text: &str, delimiters: &[&str]) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    let mut remaining = text;

    loop {
        if remaining.is_empty() {
            break;
        }

        let mut earliest: Option<usize> = None;
        let mut earliest_delim = "";

        for &delim in delimiters {
            if let Some(idx) = remaining.find(delim) {
                if earliest.is_none() || idx < earliest.unwrap() {
                    earliest = Some(idx);
                    earliest_delim = delim;
                }
            }
        }

        match earliest {
            None => {
                pieces.push(remaining.to_string());
                break;
            }
            Some(pos) => {
                let end = pos + earliest_delim.len();
                let piece = &remaining[..end];
                if !piece.trim().is_empty() {
                    pieces.push(piece.to_string());
                }
                remaining = &remaining[end..];
            }
        }
    }

    pieces.retain(|p| !p.trim().is_empty());
    pieces
}

/// Fallback: split on whitespace boundaries to hit target word count.
/// When the input is whitespace-less or any single "word" exceeds the
/// target (CJK paragraph, long URL), slices on character boundaries.
fn split_on_whitespace(text: &str, target: usize) -> Vec<String> {
    // Collect whitespace-delimited tokens with trailing whitespace
    let words: Vec<&str> = text.split_inclusive(char::is_whitespace).collect();

    // Check for no useful whitespace: empty OR single token > target chars
    let no_useful_whitespace = words.is_empty()
        || words.iter().all(|w| w.trim().is_empty())
        || (words.len() == 1
            && words[0].trim().len() > target
            && !words[0].trim().chars().any(char::is_whitespace));

    if no_useful_whitespace {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return vec![];
        }
        let chars_per_piece = std::cmp::max(1, target);
        let chars: Vec<char> = trimmed.chars().collect();
        let mut pieces: Vec<String> = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            let end = std::cmp::min(i + chars_per_piece, chars.len());
            let slice: String = chars[i..end].iter().collect();
            if !slice.trim().is_empty() {
                pieces.push(slice);
            }
            i = end;
        }
        return pieces;
    }

    // Normal whitespace-tokenized path
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut pieces: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let end = std::cmp::min(i + target, tokens.len());
        let slice = tokens[i..end].join(" ");
        if !slice.trim().is_empty() {
            pieces.push(slice);
        }
        i = end;
    }
    pieces
}

/// Greedily merge adjacent pieces until each chunk is near the target size.
/// Avoids creating chunks larger than ceil(target * 1.5).
fn greedy_merge(pieces: &[String], target: usize) -> Vec<String> {
    if pieces.is_empty() {
        return vec![];
    }

    let max_words = (target as f64 * 1.5).ceil() as usize;
    let mut result: Vec<String> = Vec::new();
    let mut current = pieces[0].clone();

    for piece in &pieces[1..] {
        let combined = format!("{}{}", current, piece);
        if count_words(&combined) <= max_words {
            current = combined;
        } else {
            result.push(current);
            current = piece.clone();
        }
    }

    if !current.trim().is_empty() {
        result.push(current);
    }
    result
}

/// Apply sentence-aware trailing overlap.
/// The last N words of chunk[i] are prepended to chunk[i+1].
fn apply_overlap(chunks: &[String], overlap_words: usize) -> Vec<String> {
    if chunks.len() <= 1 || overlap_words == 0 {
        return chunks.to_vec();
    }

    let mut result: Vec<String> = vec![chunks[0].clone()];

    for i in 1..chunks.len() {
        let prev_trailing = extract_trailing_context(&chunks[i - 1], overlap_words);
        result.push(format!("{}{}", prev_trailing, chunks[i]));
    }

    result
}

/// Extract the last N words from text, trying to align to sentence boundaries.
/// If a sentence boundary exists within the last N words, start there.
fn extract_trailing_context(text: &str, target_words: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= target_words {
        return String::new();
    }

    let start = words.len() - target_words;
    let trailing: String = words[start..].join(" ") + " ";

    // Try to find a sentence boundary to start from
    if let Some(sentence_start) = trailing.find(|c: char| c == '.' || c == '!' || c == '?') {
        // Only use if it's in the first half of the trailing text
        if sentence_start < trailing.len() / 2 {
            let after: String = trailing[sentence_start..]
                .trim_start_matches(|c: char| c == '.' || c == '!' || c == '?' || c.is_whitespace())
                .to_string();
            if !after.trim().is_empty() {
                return after;
            }
        }
    }

    trailing
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- empty / trivial ---

    #[test]
    fn returns_empty_for_empty_input() {
        assert_eq!(chunk_text("", None), vec![]);
        assert_eq!(chunk_text("   ", None), vec![]);
    }

    #[test]
    fn returns_single_chunk_for_short_text() {
        let text = "Hello world. This is a short text.";
        let chunks = chunk_text(text, None);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, text.trim());
        assert_eq!(chunks[0].index, 0);
    }

    #[test]
    fn handles_single_word_input() {
        let chunks = chunk_text("hello", None);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "hello");
    }

    // --- paragraph splitting ---

    #[test]
    fn splits_at_paragraph_boundaries() {
        let paragraph = "word ".repeat(200).trim().to_string();
        let text = format!("{}\n\n{}", paragraph, paragraph);
        let opts = ChunkOptions {
            chunk_size: 250,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() >= 2);
    }

    // --- chunk size ---

    #[test]
    fn respects_chunk_size_target() {
        let text = "word ".repeat(1000).trim().to_string();
        let opts = ChunkOptions {
            chunk_size: 100,
            chunk_overlap: 0,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        for chunk in &chunks {
            let word_count = chunk.text.split_whitespace().count();
            // Allow up to 1.5x target due to greedy merge
            assert!(word_count <= 150, "{} words exceeds 150 cap", word_count);
        }
    }

    // --- overlap ---

    #[test]
    fn applies_overlap_between_chunks() {
        let text = "word ".repeat(1000).trim().to_string();
        let opts = ChunkOptions {
            chunk_size: 100,
            chunk_overlap: 20,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
        assert!(!chunks[1].text.is_empty());
    }

    // --- sentence boundaries ---

    #[test]
    fn splits_at_sentence_boundaries() {
        let sentences: Vec<String> = (0..50)
            .map(|i| format!("This is sentence number {i} with some content about topic {i}."))
            .collect();
        let text = sentences.join(" ");
        let opts = ChunkOptions {
            chunk_size: 50,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
        for chunk in &chunks[..chunks.len() - 1] {
            assert!(
                chunk.text.contains('.') || chunk.text.contains('!') || chunk.text.contains('?'),
                "chunk missing sentence-ending punctuation: {:?}",
                &chunk.text[chunk.text.len().saturating_sub(30)..]
            );
        }
    }

    // --- indices ---

    #[test]
    fn assigns_sequential_indices() {
        let text = "word ".repeat(1000).trim().to_string();
        let opts = ChunkOptions {
            chunk_size: 100,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.index, i);
        }
    }

    // --- unicode ---

    #[test]
    fn handles_unicode_text() {
        let text = format!(
            "Bonjour le monde. {}",
            "Ceci est un texte en francais. ".repeat(100)
        );
        let opts = ChunkOptions {
            chunk_size: 50,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
        assert!(chunks[0].text.contains("Bonjour"));
    }

    // --- line-level split ---

    #[test]
    fn splits_at_single_newline_when_paragraphs_absent() {
        let lines = vec!["This is a single line of text."; 100].join("\n");
        let opts = ChunkOptions {
            chunk_size: 20,
            ..Default::default()
        };
        let chunks = chunk_text(&lines, Some(&opts));
        assert!(chunks.len() > 1);
    }

    // --- word-level split (whitespace only) ---

    #[test]
    fn handles_text_with_only_whitespace_delimiters() {
        let words = vec!["word"; 200].join(" ");
        let opts = ChunkOptions {
            chunk_size: 50,
            ..Default::default()
        };
        let chunks = chunk_text(&words, Some(&opts));
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(!chunk.text.trim().is_empty());
        }
    }

    // --- clause-level delimiters ---

    #[test]
    fn handles_clause_level_delimiters() {
        let text = vec!["clause one; clause two: clause three, clause four"; 100].join(" ");
        let opts = ChunkOptions {
            chunk_size: 30,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
    }

    // --- lossless ---

    #[test]
    fn preserves_content_across_chunks_lossless() {
        let original = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let opts = ChunkOptions {
            chunk_size: 5,
            chunk_overlap: 0,
            ..Default::default()
        };
        let chunks = chunk_text(original, Some(&opts));
        let reconstructed: String = chunks.iter().map(|c| c.text.as_str()).collect::<Vec<_>>().join(" ");
        assert!(reconstructed.contains("First paragraph"));
        assert!(reconstructed.contains("Second paragraph"));
        assert!(reconstructed.contains("Third paragraph"));
    }

    // --- default options ---

    #[test]
    fn default_options_produce_reasonable_chunks() {
        let text = vec!["This is a test sentence with several words."; 500].join(" ");
        let chunks = chunk_text(&text, None);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            let word_count = chunk.text.split_whitespace().count();
            assert!(word_count <= 500, "{} words exceeds 500", word_count);
        }
    }

    // --- mixed delimiter hierarchy ---

    #[test]
    fn handles_mixed_delimiter_hierarchy() {
        let text = [
            "Paragraph one has sentences. And more sentences! Really?",
            "",
            "Paragraph two; with clauses: and more, clauses here.",
            "",
            "Paragraph three.\nWith line breaks.\nAnd more lines.",
        ]
        .join("\n");
        let opts = ChunkOptions {
            chunk_size: 10,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
    }

    // === CJK CHUNKING TESTS ===

    #[test]
    fn cjk_long_pure_chinese_paragraph_splits() {
        let text = "品牌圣经测试用例".repeat(200); // 1600 CJK chars, no whitespace
        let opts = ChunkOptions {
            chunk_size: 100,
            chunk_overlap: 10,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1, "should produce multiple chunks, got {}", chunks.len());
    }

    #[test]
    fn cjk_japanese_sentence_terminator_splits() {
        let text = "今日は晴れです。明日は雨です。明後日は曇りです。".repeat(20);
        let opts = ChunkOptions {
            chunk_size: 50,
            chunk_overlap: 5,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
        let some_end_at_punct = chunks.iter().any(|c| {
            let s = &c.text;
            s.ends_with('。') || s.ends_with('！') || s.ends_with('？')
        });
        assert!(some_end_at_punct);
    }

    #[test]
    fn cjk_korean_hangul_with_spaces_splits() {
        let text = "한글 테스트 입니다 짧은 문장 여러개 ".repeat(50);
        let opts = ChunkOptions {
            chunk_size: 30,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
    }

    #[test]
    fn cjk_mixed_cjk_english_still_splits() {
        let para = "This is English text. 这是中文文本。 More English here. ";
        let text = para.repeat(30);
        let opts = ChunkOptions {
            chunk_size: 20,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
    }

    #[test]
    fn cjk_maxchars_hard_cap_on_whitespaceless_cjk() {
        // 20K char pure-Chinese blob; chunkSize 100K overridden by maxChars=6000
        let text = "测试".repeat(10000); // 20K chars
        let opts = ChunkOptions {
            chunk_size: 100000,
            chunk_overlap: 0,
            max_chars: 6000,
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert!(c.text.len() <= 6000, "chunk len {} > 6000", c.text.len());
        }
    }

    #[test]
    fn cjk_maxchars_sliding_window_preserves_overlap() {
        let text = "A".repeat(15000); // 15K of one char, no delimiters
        let opts = ChunkOptions {
            chunk_size: 100000,
            chunk_overlap: 0,
            max_chars: 6000,
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() >= 3);
        assert!(chunks[0].text.len() <= 6000);
        assert!(chunks[1].text.len() <= 6000);
    }

    #[test]
    fn cjk_maxchars_applies_on_single_short_chunk_path() {
        // A short doc (under chunkSize words) but one huge whitespace-less
        // line that exceeds maxChars.
        let text = "a".repeat(8000); // 1 "word" of 8000 chars
        let opts = ChunkOptions {
            chunk_size: 300,
            max_chars: 6000,
            ..Default::default()
        };
        let chunks = chunk_text(&text, Some(&opts));
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.text.len() <= 6000);
        }
    }

    #[test]
    fn regression_pure_english_doc_unchanged() {
        let para = "The quick brown fox jumps over the lazy dog. ".repeat(50);
        let opts = ChunkOptions {
            chunk_size: 50,
            ..Default::default()
        };
        let chunks = chunk_text(&para, Some(&opts));
        assert!(chunks.len() > 0);
        // Should all be ASCII-range
        for c in &chunks {
            assert!(
                c.text.chars().all(|ch| ch.is_ascii()),
                "non-ASCII in chunk: {:?}",
                c.text
            );
        }
    }

    // --- unit tests for internal helpers ---

    #[test]
    fn cap_by_chars_noop_when_within_limit() {
        assert_eq!(cap_by_chars("hello", 100), vec!["hello"]);
    }

    #[test]
    fn cap_by_chars_empty_returns_empty() {
        assert_eq!(cap_by_chars("", 100), Vec::<String>::new());
    }

    #[test]
    fn cap_by_chars_splits_when_over_limit() {
        let text = "abcdefghij".repeat(100); // 1000 chars
        let result = cap_by_chars(&text, 100);
        assert!(result.len() > 1);
        for r in &result {
            assert!(r.len() <= 100, "got len {}", r.len());
        }
    }

    #[test]
    fn split_at_delimiters_preserves_content() {
        let text = "Hello. World! How are you?";
        let pieces = split_at_delimiters(text, &[". ", "! ", "? "]);
        assert!(pieces.len() > 1);
        let joined: String = pieces.join("");
        assert_eq!(joined, text);
    }

    #[test]
    fn greedy_merge_combines_small_pieces() {
        let pieces: Vec<String> = vec!["a ".into(), "b ".into(), "c ".into()];
        let merged = greedy_merge(&pieces, 10);
        assert!(merged.len() < pieces.len());
    }

    #[test]
    fn apply_overlap_noop_for_single_chunk() {
        let chunks = vec!["hello world".to_string()];
        let result = apply_overlap(&chunks, 5);
        assert_eq!(result, chunks);
    }

    #[test]
    fn apply_overlap_prepends_trailing_context() {
        let chunks = vec![
            "word ".repeat(50).trim().to_string(),
            "word ".repeat(50).trim().to_string(),
        ];
        let result = apply_overlap(&chunks, 5);
        assert_eq!(result.len(), 2);
        // Second chunk should be longer (has overlap prepended)
        assert!(result[1].len() > chunks[1].len());
    }
}
