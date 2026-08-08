//! Pure fusion/boost math for zbrain hybrid search.
//!
//! Ported from `src/core/search/hybrid.ts`. Mirrors the TS signatures and
//! semantics exactly; the only deliberate deviations are noted inline:
//!   * `cosine_similarity` iterates `min(a.len(), b.len())` rather than
//!     `a.len()` (TS would read `b[i] === undefined` for short `b` and emit
//!     `NaN`; equal-length inputs — the only real call path — are unchanged).
//!   * `rrf_fusion*` add a `slug` tiebreaker after the primary score sort so
//!     output is deterministic regardless of `HashMap` iteration order. TS
//!     relies on V8's stable `Array.sort`; the tied cases never occur in
//!     practice because RRF scores are normalized floats.
//!
//! These functions are the building blocks the `hybridSearch` orchestrator
//! (later sub-node) wires together with the storage + embedding layers.

use std::collections::HashMap;

/// Backlink boost coefficient. Score is multiplied by
/// `(1 + BACKLINK_BOOST_COEF * ln(1 + count))`. Mirrors `hybrid.ts:56`.
pub const BACKLINK_BOOST_COEF: f64 = 0.05;

/// Compiled-truth chunks get a 2x score bump after RRF normalization when
/// `apply_boost` is on. Mirrors `hybrid.ts:35`.
pub const COMPILED_TRUTH_BOOST: f64 = 2.0;

/// Salience boost strength. `'on'` → k=0.15, `'strong'` → k=0.30. Mirrors
/// `hybrid.ts:159`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SalienceStrength {
    On,
    Strong,
}

impl SalienceStrength {
    pub fn multiplier(self) -> f64 {
        match self {
            SalienceStrength::On => 0.15,
            SalienceStrength::Strong => 0.30,
        }
    }
}

/// Minimal fusion-row shape consumed by the pure boost/fusion functions.
/// Mirrors the fields of TS `SearchResult` that the fusion math reads/writes.
/// The `hybridSearch` orchestrator maps the engine-level `SearchResult`
/// (which carries a `Page`) to/from this struct.
#[derive(Debug, Clone, PartialEq)]
pub struct FusionRow {
    pub slug: String,
    pub chunk_id: Option<u64>,
    /// Used only as the RRF dedup-key fallback when `chunk_id` is absent
    /// (`hybrid.ts` uses `chunk_text.slice(0, 50)`).
    pub chunk_text: String,
    /// `"compiled_truth"` triggers `COMPILED_TRUTH_BOOST` in RRF.
    pub chunk_source: String,
    pub source_id: Option<String>,
    pub score: f64,
    pub backlink_boost: Option<f64>,
    pub salience_boost: Option<f64>,
}

impl FusionRow {
    /// Construct a bare row with sensible fusion defaults.
    pub fn new(slug: &str, score: f64) -> Self {
        FusionRow {
            slug: slug.to_string(),
            chunk_id: None,
            chunk_text: String::new(),
            chunk_source: String::new(),
            source_id: None,
            score,
            backlink_boost: None,
            salience_boost: None,
        }
    }
}

/// RRF dedup key: `${slug}:${chunk_id ?? chunk_text[..50]}`. Mirrors
/// `hybrid.ts:1257` / `hybrid.ts:1217`.
fn fusion_key(r: &FusionRow) -> String {
    match r.chunk_id {
        Some(id) => format!("{}:{}", r.slug, id),
        None => {
            let end = r.chunk_text.len().min(50);
            format!("{}:{}", r.slug, &r.chunk_text[..end])
        }
    }
}

/// Cosine similarity of two equal-length vectors. Returns `0.0` when either
/// magnitude is zero. Mirrors `hybrid.ts:1344`.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    let n = a.len().min(b.len());
    let mut dot = 0.0_f64;
    let mut mag_a = 0.0_f64;
    let mut mag_b = 0.0_f64;
    for i in 0..n {
        let av = a[i] as f64;
        let bv = b[i] as f64;
        dot += av * bv;
        mag_a += av * av;
        mag_b += bv * bv;
    }
    let denom = mag_a.sqrt() * mag_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

/// Reciprocal Rank Fusion over multiple ranked lists, each with its own `k`.
///
/// `score = sum(1 / (k + rank))` across every list a row appears in; the first
/// `FusionRow` seen for a key is retained (its `score` becomes the fused
/// value). After accumulation the scores are normalized to `[0,1]` by the
/// observed max, then `compiled_truth` rows are multiplied by
/// `COMPILED_TRUTH_BOOST` when `apply_boost` is on. Results are sorted by
/// boosted score descending (slug tiebreak for determinism). Mirrors
/// `hybrid.ts:1208` (`rrfFusionWeighted`).
pub fn rrf_fusion_weighted(
    lists: &[(Vec<FusionRow>, usize)],
    apply_boost: bool,
) -> Vec<FusionRow> {
    let mut scores: HashMap<String, (FusionRow, f64)> = HashMap::new();
    for (list, k) in lists {
        for (rank, r) in list.iter().enumerate() {
            let key = fusion_key(r);
            let rrf = 1.0 / (*k as f64 + rank as f64);
            match scores.get_mut(&key) {
                Some(entry) => entry.1 += rrf,
                None => {
                    scores.insert(key, (r.clone(), rrf));
                }
            }
        }
    }
    finalize_rrf(scores, apply_boost)
}

/// Reciprocal Rank Fusion over multiple ranked lists sharing a single `k`.
/// Mirrors `hybrid.ts:1251` (`rrfFusion`).
pub fn rrf_fusion(lists: &[Vec<FusionRow>], k: usize, apply_boost: bool) -> Vec<FusionRow> {
    let wrapped: Vec<(Vec<FusionRow>, usize)> = lists.iter().map(|l| (l.clone(), k)).collect();
    rrf_fusion_weighted(&wrapped, apply_boost)
}

fn finalize_rrf(scores: HashMap<String, (FusionRow, f64)>, apply_boost: bool) -> Vec<FusionRow> {
    let mut entries: Vec<(FusionRow, f64)> = scores.into_values().collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let max_score = entries
        .iter()
        .map(|e| e.1)
        .fold(f64::NEG_INFINITY, f64::max);
    if max_score > 0.0 {
        for (row, score) in entries.iter_mut() {
            let raw = *score;
            *score = raw / max_score;
            let boost = if apply_boost && row.chunk_source == "compiled_truth" {
                COMPILED_TRUTH_BOOST
            } else {
                1.0
            };
            *score *= boost;
            row.score = *score;
        }
    }
    // Primary: boosted score desc. Tiebreak: slug asc (determinism).
    entries.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.slug.cmp(&b.0.slug))
    });
    entries.into_iter().map(|(row, _)| row).collect()
}

/// Absolute score floor below which boost stages skip a result. Returns
/// `-infinity` (no gate) when `floor_ratio` is `None`, non-finite,
/// `< 0`, `> 1`, or no result has a positive finite score. Otherwise
/// `top_score * floor_ratio`, where `top_score` is the largest finite score.
/// Mirrors `hybrid.ts:126` (`computeFloorThreshold`).
pub fn compute_floor_threshold(results: &[FusionRow], floor_ratio: Option<f64>) -> f64 {
    let ratio = match floor_ratio {
        None => return f64::NEG_INFINITY,
        Some(r) if !r.is_finite() || r < 0.0 || r > 1.0 => return f64::NEG_INFINITY,
        Some(r) => r,
    };
    let top = results
        .iter()
        .filter(|r| r.score.is_finite())
        .map(|r| r.score)
        .fold(f64::NEG_INFINITY, f64::max);
    if !top.is_finite() || top <= 0.0 {
        return f64::NEG_INFINITY;
    }
    top * ratio
}

/// Apply the log-compressed backlink boost in place. `factor =
/// 1 + BACKLINK_BOOST_COEF * ln(1 + count)`; stamped on `backlink_boost`.
/// Rows with non-finite score, below `floor`, or zero backlink count are
/// skipped. Keyed by `slug` (NOT `source_id::slug`). Mirrors `hybrid.ts:76`.
pub fn apply_backlink_boost(
    results: &mut [FusionRow],
    counts: &HashMap<String, usize>,
    floor: Option<f64>,
) {
    for r in results.iter_mut() {
        if !r.score.is_finite() {
            continue;
        }
        if let Some(f) = floor {
            if r.score < f {
                continue;
            }
        }
        let count = counts.get(&r.slug).copied().unwrap_or(0);
        if count > 0 {
            let factor = 1.0 + BACKLINK_BOOST_COEF * (1.0_f64 + count as f64).ln();
            r.score *= factor;
            r.backlink_boost = Some(factor);
        }
    }
}

/// Apply the log-compressed salience boost in place. `factor =
/// 1 + k * ln(1 + score)` with `k` from `strength`; stamped on
/// `salience_boost`. Keyed by `${source_id ?? 'default'}::${slug}`. Rows with
/// non-finite score, below `floor`, or zero/negative salience are skipped.
/// Mirrors `hybrid.ts:153` (`applySalienceBoost`).
pub fn apply_salience_boost(
    results: &mut [FusionRow],
    scores: &HashMap<String, f64>,
    strength: SalienceStrength,
    floor: Option<f64>,
) {
    let k = strength.multiplier();
    for r in results.iter_mut() {
        if !r.score.is_finite() {
            continue;
        }
        if let Some(f) = floor {
            if r.score < f {
                continue;
            }
        }
        let key = format!(
            "{}::{}",
            r.source_id.clone().unwrap_or_else(|| "default".to_string()),
            r.slug
        );
        let score = match scores.get(&key) {
            Some(s) if *s > 0.0 => *s,
            _ => continue,
        };
        let factor = 1.0 + k * (1.0_f64 + score).ln();
        r.score *= factor;
        r.salience_boost = Some(factor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(slug: &str, score: f64) -> FusionRow {
        FusionRow::new(slug, score)
    }

    // ── cosine_similarity ────────────────────────────────────────────────
    #[test]
    fn cosine_identical_is_one() {
        let a: Vec<f32> = vec![1.0, 0.0, 0.0];
        let b: Vec<f32> = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a: Vec<f32> = vec![1.0, 0.0];
        let b: Vec<f32> = vec![0.0, 1.0];
        assert!((cosine_similarity(&a, &b) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn cosine_zero_magnitude_is_zero() {
        let a: Vec<f32> = vec![0.0, 0.0];
        let b: Vec<f32> = vec![1.0, 1.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    // ── rrf_fusion ──────────────────────────────────────────────────────
    #[test]
    fn rrf_normalizes_top_to_one() {
        let lists = vec![vec![row("a", 0.0), row("b", 0.0)], vec![row("b", 0.0)]];
        let out = rrf_fusion(&lists, 60, false);
        // 'b' appears in both lists (rank 1 + rank 0) → highest fused score.
        assert_eq!(out.len(), 2);
        let top = out.iter().find(|r| r.slug == "b").unwrap();
        assert!((top.score - 1.0).abs() < 1e-9, "top normalized to 1.0, got {}", top.score);
    }

    #[test]
    fn rrf_accumulates_across_lists() {
        // k=60 → 1/(60+0)=0.0166667 for rank0. 'a' only in list0 rank0;
        // 'b' in list0 rank1 (1/61) + list1 rank0 (1/60).
        let lists = vec![vec![row("a", 0.0), row("b", 0.0)], vec![row("b", 0.0)]];
        let out = rrf_fusion(&lists, 60, false);
        let a = out.iter().find(|r| r.slug == "a").unwrap();
        let b = out.iter().find(|r| r.slug == "b").unwrap();
        assert!(b.score > a.score, "b fused from two lists must beat a");
    }

    #[test]
    fn rrf_empty_input() {
        let lists: Vec<Vec<FusionRow>> = vec![];
        assert!(rrf_fusion(&lists, 60, false).is_empty());
    }

    #[test]
    fn rrf_compiled_truth_boosted() {
        // Single compiled_truth row: normalized to 1.0, then *COMPILED_TRUTH_BOOST.
        let mut ct = row("truth", 0.0);
        ct.chunk_source = "compiled_truth".to_string();
        let lists = vec![vec![ct]];
        let out = rrf_fusion(&lists, 60, true);
        let truth = &out[0];
        assert!((truth.score - COMPILED_TRUTH_BOOST).abs() < 1e-9, "got {}", truth.score);
    }

    #[test]
    fn rrf_no_boost_when_disabled() {
        let mut ct = row("truth", 0.0);
        ct.chunk_source = "compiled_truth".to_string();
        let lists = vec![vec![ct]];
        let out = rrf_fusion(&lists, 60, false);
        let truth = &out[0];
        assert!((truth.score - 1.0).abs() < 1e-9, "no boost when apply_boost=false");
    }

    // ── rrf_fusion_weighted ─────────────────────────────────────────────
    #[test]
    fn rrf_weighted_per_list_k() {
        // list0 k=10 (rank0 → 1/10), list1 k=60 (rank0 → 1/60). The raw
        // accumulated rrf is 1/10 + 1/60 = 0.1166..., but `finalize_rrf`
        // normalizes by the observed max (which IS this sum since "a" is
        // the only key) → 1.0. The Rust port mirrors `hybrid.ts`
        // `rrfFusionWeighted` exactly. (A port-only test that asserted
        // 0.1166 was incorrect.)
        let lists = vec![(vec![row("a", 0.0)], 10), (vec![row("a", 0.0)], 60)];
        let out = rrf_fusion_weighted(&lists, false);
        let a = out.iter().find(|r| r.slug == "a").unwrap();
        assert!((a.score - 1.0).abs() < 1e-9, "normalized to max=1, got {}", a.score);
    }

    #[test]
    fn rrf_weighted_per_list_k_different_ranks() {
        // Two distinct slugs across the lists: 'a' rank0 in k=10 (1/10),
        // 'b' rank0 in k=60 (1/60). After accumulation: a=0.1, b≈0.0166.
        // Normalize by max=0.1 → a=1.0, b≈0.1666.
        let lists = vec![(vec![row("a", 0.0), row("b", 0.0)], 10), (vec![], 60)];
        let out = rrf_fusion_weighted(&lists, false);
        let a = out.iter().find(|r| r.slug == "a").unwrap();
        let b = out.iter().find(|r| r.slug == "b").unwrap();
        assert!((a.score - 1.0).abs() < 1e-9, "a: {}", a.score);
        // b accumulated = 1/(10+1) = 1/11 ≈ 0.0909. Normalize: 0.0909/0.1 = 0.909.
        assert!((b.score - 1.0 / 11.0 / 0.1).abs() < 1e-9, "b: {}", b.score);
    }

    // ── compute_floor_threshold ─────────────────────────────────────────
    #[test]
    fn floor_none_is_neg_inf() {
        assert_eq!(compute_floor_threshold(&[row("a", 5.0)], None), f64::NEG_INFINITY);
    }

    #[test]
    fn floor_out_of_range_is_neg_inf() {
        assert_eq!(compute_floor_threshold(&[row("a", 5.0)], Some(2.0)), f64::NEG_INFINITY);
        assert_eq!(compute_floor_threshold(&[row("a", 5.0)], Some(-0.1)), f64::NEG_INFINITY);
        // NaN ratio is non-finite → gate disabled.
        assert_eq!(
            compute_floor_threshold(&[row("a", 5.0)], Some(f64::NAN)),
            f64::NEG_INFINITY
        );
    }

    #[test]
    fn floor_normal_is_top_times_ratio() {
        let rs = [row("a", 4.0), row("b", 8.0)];
        assert_eq!(compute_floor_threshold(&rs, Some(0.5)), 4.0);
    }

    #[test]
    fn floor_no_positive_score_is_neg_inf() {
        let rs = [row("a", f64::NAN), row("b", -3.0)];
        assert_eq!(compute_floor_threshold(&rs, Some(0.5)), f64::NEG_INFINITY);
    }

    // ── apply_backlink_boost ────────────────────────────────────────────
    #[test]
    fn backlink_factor_formula() {
        let mut rs = [row("a", 1.0)];
        let mut counts = HashMap::new();
        counts.insert("a".to_string(), 9);
        apply_backlink_boost(&mut rs, &counts, None);
        // 1 + 0.05 * ln(1+9) = 1 + 0.05*ln(10) = 1.11513
        let expected = 1.0 + 0.05 * (10.0_f64).ln();
        assert!((rs[0].score - expected).abs() < 1e-9, "got {}", rs[0].score);
        assert!((rs[0].backlink_boost.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn backlink_zero_count_skipped() {
        let mut rs = [row("a", 1.0)];
        let counts = HashMap::new();
        apply_backlink_boost(&mut rs, &counts, None);
        assert_eq!(rs[0].score, 1.0);
        assert!(rs[0].backlink_boost.is_none());
    }

    #[test]
    fn backlink_floor_gate_skips() {
        let mut rs = [row("a", 0.5)];
        let mut counts = HashMap::new();
        counts.insert("a".to_string(), 9);
        apply_backlink_boost(&mut rs, &counts, Some(1.0));
        assert_eq!(rs[0].score, 0.5);
        assert!(rs[0].backlink_boost.is_none());
    }

    // ── apply_salience_boost ────────────────────────────────────────────
    #[test]
    fn salience_on_factor() {
        let mut rs = [row("a", 1.0)];
        rs[0].source_id = Some("s1".to_string());
        let mut scores = HashMap::new();
        scores.insert("s1::a".to_string(), 9.0);
        apply_salience_boost(&mut rs, &scores, SalienceStrength::On, None);
        // 1 + 0.15 * ln(1+9) = 1 + 0.15*ln(10) = 1.34539
        let expected = 1.0 + 0.15 * (10.0_f64).ln();
        assert!((rs[0].score - expected).abs() < 1e-9, "got {}", rs[0].score);
        assert!((rs[0].salience_boost.unwrap() - expected).abs() < 1e-9);
    }

    #[test]
    fn salience_strong_double_k() {
        let mut rs = [row("a", 1.0)];
        rs[0].source_id = Some("s1".to_string());
        let mut scores = HashMap::new();
        scores.insert("s1::a".to_string(), 9.0);
        apply_salience_boost(&mut rs, &scores, SalienceStrength::Strong, None);
        // 1 + 0.30 * ln(10) = 1.65916
        let expected = 1.0 + 0.30 * (10.0_f64).ln();
        assert!((rs[0].score - expected).abs() < 1e-9, "got {}", rs[0].score);
    }

    #[test]
    fn salience_default_source_key() {
        let mut rs = [row("a", 1.0)];
        // source_id unset → key "default::a"
        let mut scores = HashMap::new();
        scores.insert("default::a".to_string(), 4.0);
        apply_salience_boost(&mut rs, &scores, SalienceStrength::On, None);
        assert!(rs[0].salience_boost.is_some());
        assert!(rs[0].score > 1.0);
    }

    #[test]
    fn salience_zero_skipped() {
        let mut rs = [row("a", 1.0)];
        rs[0].source_id = Some("s1".to_string());
        let mut scores = HashMap::new();
        scores.insert("s1::a".to_string(), 0.0);
        apply_salience_boost(&mut rs, &scores, SalienceStrength::On, None);
        assert_eq!(rs[0].score, 1.0);
        assert!(rs[0].salience_boost.is_none());
    }
}
