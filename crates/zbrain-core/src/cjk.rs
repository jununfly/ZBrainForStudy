//! CJK (Chinese / Japanese / Korean) detection and word-counting primitives.
//!
//! Ported from `src/core/cjk.ts`.
//!
//! Scope: BMP-only Unicode ranges covering ~99% of real CJK content:
//!   - Han (CJK Unified Ideographs): U+4E00–U+9FFF
//!   - Hiragana: U+3040–U+309F
//!   - Katakana: U+30A0–U+30FF
//!   - Hangul Syllables: U+AC00–U+D7AF
//!
//! Out of scope (v0.32.7 parity): Han extensions A/B/C, halfwidth katakana,
//! compatibility ideographs, compatibility Jamo, iteration marks (々/〇).

/// Sentence-level delimiters for CJK text: 。！？
pub const CJK_SENTENCE_DELIMITERS: [char; 3] = ['\u{3002}', '\u{FF01}', '\u{FF1F}'];

/// Clause-level delimiters for CJK text: ；：，、
pub const CJK_CLAUSE_DELIMITERS: [char; 4] =
    ['\u{FF1B}', '\u{FF1A}', '\u{FF0C}', '\u{3001}'];

/// Density threshold for switching word-count strategy. Below this CJK char
/// density, a doc is treated as Latin-mostly and stays whitespace-tokenized.
/// At or above, it's CJK-mostly and char-counted.
pub const CJK_DENSITY_THRESHOLD: f64 = 0.30;

/// Returns `true` when `c` falls inside any of the four BMP CJK script blocks
/// (Han, Hiragana, Katakana, Hangul Syllables).
#[inline]
pub fn is_cjk_char(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
    )
}

/// Returns `true` when `s` contains at least one BMP CJK character.
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk_char)
}

/// CJK-aware "word" count. CJK languages aren't whitespace-tokenized, so a
/// paragraph of Chinese collapses to 1 word under whitespace-splitting and
/// downstream chunkers never split it.
///
/// Heuristic: switch on CJK character density, not mere presence. Below
/// `CJK_DENSITY_THRESHOLD` (0.30) the doc is Latin-dominant and whitespace
/// tokens are the right unit; at or above it's CJK-dominant and char count
/// is the right unit.
pub fn count_cjk_aware_words(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }

    let cjk_count = s.chars().filter(|&c| is_cjk_char(c)).count();
    let non_whitespace = s.chars().filter(|c| !c.is_whitespace()).count();

    if non_whitespace == 0 {
        return 0;
    }

    let density = cjk_count as f64 / non_whitespace as f64;
    if density >= CJK_DENSITY_THRESHOLD {
        return non_whitespace;
    }

    // Latin-dominant: count whitespace-delimited tokens
    s.split_whitespace().count()
}

/// LIKE-pattern escape for PGLite/Postgres `ILIKE ... ESCAPE '\'`.
/// Escapes backslash first so introduced backslashes aren't double-escaped.
pub fn escape_like_pattern(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- is_cjk_char ---

    #[test]
    fn is_cjk_char_han() {
        assert!(is_cjk_char('一'));
        assert!(is_cjk_char('品'));
        assert!(is_cjk_char('鿿'));
    }

    #[test]
    fn is_cjk_char_hiragana() {
        assert!(is_cjk_char('あ'));
        assert!(is_cjk_char('ひ'));
    }

    #[test]
    fn is_cjk_char_katakana() {
        assert!(is_cjk_char('カ'));
        assert!(is_cjk_char('タ'));
    }

    #[test]
    fn is_cjk_char_hangul() {
        assert!(is_cjk_char('한'));
        assert!(is_cjk_char('글'));
    }

    #[test]
    fn is_cjk_char_false_ascii() {
        assert!(!is_cjk_char('a'));
        assert!(!is_cjk_char('Z'));
        assert!(!is_cjk_char(' '));
    }

    #[test]
    fn is_cjk_char_false_latin_accents() {
        assert!(!is_cjk_char('é'));
        assert!(!is_cjk_char('ñ'));
    }

    #[test]
    fn is_cjk_char_false_punctuation() {
        assert!(!is_cjk_char('.'));
        assert!(!is_cjk_char('!'));
        assert!(!is_cjk_char('？')); // fullwidth question is NOT in the CJK blocks above
    }

    // --- has_cjk ---

    #[test]
    fn has_cjk_true_han() {
        assert!(has_cjk("品牌圣经"));
    }

    #[test]
    fn has_cjk_true_hiragana() {
        assert!(has_cjk("ひらがな"));
    }

    #[test]
    fn has_cjk_true_katakana() {
        assert!(has_cjk("カタカナ"));
    }

    #[test]
    fn has_cjk_true_hangul() {
        assert!(has_cjk("한글"));
    }

    #[test]
    fn has_cjk_false_ascii() {
        assert!(!has_cjk("hello world"));
    }

    #[test]
    fn has_cjk_false_latin_accents() {
        assert!(!has_cjk("café résumé"));
    }

    #[test]
    fn has_cjk_true_mixed_cjk_ascii() {
        assert!(has_cjk("hello 世界"));
    }

    #[test]
    fn has_cjk_false_empty() {
        assert!(!has_cjk(""));
    }

    // --- count_cjk_aware_words ---

    #[test]
    fn count_cjk_pure_chinese_counts_chars() {
        // "品牌圣经测试用例" = 8 Han characters, all CJK → char count
        assert_eq!(count_cjk_aware_words("品牌圣经测试用例"), 8);
    }

    #[test]
    fn count_cjk_pure_ascii_counts_tokens() {
        assert_eq!(count_cjk_aware_words("this is a normal english sentence"), 6);
    }

    #[test]
    fn count_cjk_mixed_cjk_dominant_char_count() {
        // ~80% CJK by char count → char-count branch
        let s = "品牌圣经品牌圣经 is the brand";
        let expected = s.chars().filter(|c| !c.is_whitespace()).count();
        assert_eq!(count_cjk_aware_words(s), expected);
    }

    #[test]
    fn count_cjk_english_with_one_japanese_term_stays_tokenized() {
        // 1 CJK / ~50 non-ws chars = ~2% → whitespace-tokenized
        let s = "the user wrote a long english blog post about マンガ and other interests";
        let expected = s.split_whitespace().count();
        assert_eq!(count_cjk_aware_words(s), expected);
    }

    #[test]
    fn count_cjk_exactly_at_30_percent_threshold() {
        // 3 CJK chars + 7 ASCII non-ws = 10 total; 3/10 = 0.30 → CJK
        assert_eq!(count_cjk_aware_words("世界世 abcdefg"), 10);
    }

    #[test]
    fn count_cjk_just_below_threshold_whitespace() {
        // 2 CJK + 8 ASCII = 10 non-ws; 2/10 = 0.20 < 0.30 → whitespace
        assert_eq!(count_cjk_aware_words("世界 abcdefgh"), 2);
    }

    #[test]
    fn count_cjk_empty_string() {
        assert_eq!(count_cjk_aware_words(""), 0);
    }

    #[test]
    fn count_cjk_whitespace_only() {
        assert_eq!(count_cjk_aware_words("   \n\t  "), 0);
    }

    // --- constants ---

    #[test]
    fn cjk_density_threshold_is_03() {
        assert!((CJK_DENSITY_THRESHOLD - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn cjk_sentence_delimiters_covers_correct_chars() {
        assert_eq!(CJK_SENTENCE_DELIMITERS, ['\u{3002}', '\u{FF01}', '\u{FF1F}']);
    }

    #[test]
    fn cjk_clause_delimiters_covers_correct_chars() {
        assert_eq!(CJK_CLAUSE_DELIMITERS, ['\u{FF1B}', '\u{FF1A}', '\u{FF0C}', '\u{3001}']);
    }

    // --- escape_like_pattern ---

    #[test]
    fn escape_like_escapes_percent_and_underscore() {
        assert_eq!(escape_like_pattern("100% off_today"), "100\\% off\\_today");
    }

    #[test]
    fn escape_like_escapes_backslash_first() {
        assert_eq!(escape_like_pattern("a\\b"), "a\\\\b");
    }

    #[test]
    fn escape_like_escapes_all_three_metachars() {
        assert_eq!(escape_like_pattern("a\\%b_c"), "a\\\\\\%b\\_c");
    }

    #[test]
    fn escape_like_noop_plain_text() {
        assert_eq!(escape_like_pattern("hello world"), "hello world");
    }

    #[test]
    fn escape_like_noop_cjk() {
        assert_eq!(escape_like_pattern("测试"), "测试");
    }
}
