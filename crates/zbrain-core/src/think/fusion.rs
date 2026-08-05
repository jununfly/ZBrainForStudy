//! Reciprocal-rank fusion over two ranked lists with a caller-supplied key.
//!
//! Ported from `src/core/think/gather.ts:fusionRanked` (v0.28). This is the
//! generic RRF helper the think gather uses to fuse its keyword + vector take
//! streams (keyed by `(slug, row)`), distinct from `crate::search::fusion::
//! rrf_fusion` which fuses `FusionRow`s. Keeping a generic helper here is the
//! faithful port: the think streams are heterogeneous and don't carry the
//! `FusionRow` shape.

use std::collections::HashMap;

/// RRF constant shared with `src/core/search/hybrid.ts` and
/// `crate::search::fusion`.
pub const RRF_K: usize = 60;

/// Reciprocal-rank score: `1 / (k + rank)`, where `rank` is 1-based.
pub fn rrf_score(rank: usize) -> f64 {
    1.0 / (RRF_K as f64 + rank as f64)
}

/// Fuse two ranked lists by a `(slug, row_num?)`-style key.
///
/// Returns the merged list sorted by fused score descending. When an item
/// appears in both lists, its scores accumulate. The first-seen item for a key
/// is retained (matching TS `fuseRanked`). Mirrors
/// `src/core/think/gather.ts:fuseRanked`.
pub fn fuse_ranked<T: Clone>(
    a: &[T],
    b: &[T],
    key_fn: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut scores: HashMap<String, (T, f64)> = HashMap::new();
    for (i, item) in a.iter().enumerate() {
        let k = key_fn(item);
        scores.insert(k, (item.clone(), rrf_score(i + 1)));
    }
    for (i, item) in b.iter().enumerate() {
        let k = key_fn(item);
        match scores.get_mut(&k) {
            Some(entry) => entry.1 += rrf_score(i + 1),
            None => {
                scores.insert(k, (item.clone(), rrf_score(i + 1)));
            }
        }
    }
    let mut entries: Vec<(T, f64)> = scores.into_values().collect();
    entries.sort_by(|x, y| {
        y.1.partial_cmp(&x.1)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    entries.into_iter().map(|(item, _)| item).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rrf_score_formula() {
        // rank 1 → 1/(60+1)
        assert!((rrf_score(1) - 1.0 / 61.0).abs() < 1e-12);
    }

    #[test]
    fn overlap_accumulates() {
        // 'a' only in a (rank0); 'b' in both a(rank1) + b(rank0) → higher.
        let a = vec!["a", "b"];
        let b = vec!["b"];
        let out = fuse_ranked(&a, &b, |s: & &str| s.to_string());
        assert_eq!(out.len(), 2);
        let b_pos = out.iter().position(|x| x == &"b").unwrap();
        let a_pos = out.iter().position(|x| x == &"a").unwrap();
        assert!(b_pos < a_pos, "b fused from two lists must rank above a");
    }

    #[test]
    fn first_seen_item_retained() {
        #[derive(Debug, Clone, PartialEq)]
        struct Item {
            key: &'static str,
            tag: &'static str,
        }
        let a = vec![Item { key: "x", tag: "from_a" }];
        let b = vec![Item { key: "x", tag: "from_b" }];
        let out = fuse_ranked(&a, &b, |i| i.key.to_string());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].tag, "from_a");
    }

    #[test]
    fn empty_inputs() {
        let a: Vec<&str> = vec![];
        let b: Vec<&str> = vec![];
        assert!(fuse_ranked(&a, &b, |s: & &str| s.to_string()).is_empty());
    }

    #[test]
    fn takes_key_shape() {
        // Emulates the think gather's take key: `${slug}#${row}`.
        let a = vec![("people/alice", 3), ("companies/acme", 1)];
        let b = vec![("companies/acme", 1), ("people/bob", 2)];
        let out = fuse_ranked(&a, &b, |(s, r)| format!("{s}#{r}"));
        // companies/acme appears in both → ranked first.
        assert_eq!(out[0], ("companies/acme", 1));
        assert_eq!(out.len(), 3);
    }
}
