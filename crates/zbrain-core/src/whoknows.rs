//! Expert-routing query engine (ported from `src/commands/whoknows.ts`).
//!
//! "Who should I talk to about X?" — returns ranked person/company candidates
//! from the brain that know about a given topic.
//!
//! Scope of this module:
//!   * [`rank_candidates`] — the **pure** ranking function (locked by ENG-D1).
//!     No engine, no disk, no clock; takes pre-fetched signals, returns ranked
//!     results. Directly unit-tested.
//!   * [`find_experts`] — the search-driven entrypoint: hybrid search with the
//!     person/company type filter + salience/recency boosts DISABLED (so the
//!     raw fused relevance score is preserved), batch-fetch salience +
//!     effective-date signals, then rank.
//!
//! Ranking spec (locked by ENG-D1, verbatim from whoknows.ts):
//!
//! ```text
//!   score(page) = expertise × max(0.1, recency_decay) × (0.5 + 0.5 × salience)
//!
//!   expertise     = ln(1 + raw_match)        // sub-linear; raw_match is the
//!                                            // hybrid-search fused score.
//!   recency_decay = exp(-days_since_effective / 180)   // ~6mo half-life
//!                   floored at 0.1           // cold-start defense
//!   salience      = pages.salience_score normalized to 0..1, centered 0.5
//! ```
//!
//! Boost disablement rationale: the shared `fuse_and_boost` pipeline applies
//! salience + recency boosts by default (G13, pinned 'on'). whoknows applies
//! its OWN salience/recency formula on the raw relevance score, so it sets
//! `SearchOpts.disable_salience_boost` + `disable_recency_boost` to avoid
//! double-boosting. Mirrors TS `hybridSearch(..., { salience:'off', recency:'off' })`.
//!
//! Type filter: TS threads `expertTypesFromPack(activePack)` to honor
//! user-defined `expert_routing:` declarations. The schema-pack subsystem is
//! not migrated yet (Part10 Phase12), so Rust uses `DEFAULT_TYPES`
//! (person/company). Pack-aware type derivation is registered in
//! docs/plans/KNOWN-GAPS.md and will be wired when schema-pack lands.

use crate::engine::{BrainEngine, SearchOpts};
use crate::types::PageRef;
use serde::{Deserialize, Serialize};

/// Default person/company filter when no active pack overrides it. Mirrors TS
/// `DEFAULT_TYPES`.
pub const DEFAULT_TYPES: &[&str] = &["person", "company"];
/// Default max results. Mirrors TS `DEFAULT_LIMIT`.
pub const DEFAULT_LIMIT: usize = 5;
const RECENCY_HALF_LIFE_DAYS: f64 = 180.0; // 6 months
const RECENCY_FLOOR: f64 = 0.1;
const SALIENCE_CENTER: f64 = 0.5; // missing salience = neutral
const MS_PER_DAY: f64 = 86_400_000.0;

/// Per-candidate ranking-function input. Mirrors the TS `inputs` shape built in
/// `findExperts` before calling `rankCandidates`.
#[derive(Debug, Clone)]
pub struct CandidateInput {
    pub slug: String,
    pub source_id: String,
    pub title: String,
    /// Page type (`person` / `company` / …).
    pub page_type: String,
    /// Raw hybrid-search fused relevance score (whoknows' expertise proxy).
    pub raw_match: f64,
    /// Days since the effective date, or `None` for cold-start (no date).
    pub days_since_effective: Option<f64>,
    /// Salience already normalized to 0..1 (`raw / (1 + raw)`), or `None` when
    /// missing / invalid so the ranker falls back to the 0.5 neutral center.
    pub salience_normalized: Option<f64>,
}

/// Per-result factor breakdown (for `--explain`). Mirrors TS
/// `WhoknowsResult.factors`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhoknowsFactors {
    pub expertise: f64,
    pub recency_decay: f64,
    pub recency_factor: f64,
    pub salience: f64,
    pub salience_factor: f64,
    pub days_since_effective: Option<f64>,
    pub raw_match: f64,
}

/// A ranked expert candidate. Mirrors TS `WhoknowsResult`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WhoknowsResult {
    pub slug: String,
    pub source_id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub page_type: String,
    pub score: f64,
    pub factors: WhoknowsFactors,
}

/// Pure ranking function (locked by ENG-D1). Exported for tests; the
/// CLI/MCP path calls [`find_experts`] which adds the search step.
///
/// Sorts by score DESC, tie-broken by slug ascending for determinism, then
/// truncates to `max(1, limit)`. Mirrors TS `rankCandidates`.
#[must_use]
pub fn rank_candidates(candidates: &[CandidateInput], limit: usize) -> Vec<WhoknowsResult> {
    let mut ranked: Vec<WhoknowsResult> = candidates
        .iter()
        .map(|c| {
            // expertise: sub-linear via ln(1 + raw_match). Clamp raw to >= 0 to
            // defend against negative-score producers; ln(1+0) = 0.
            let safe_raw = if c.raw_match.is_finite() {
                c.raw_match.max(0.0)
            } else {
                0.0
            };
            let expertise = safe_raw.ln_1p();

            // recency_decay: exp(-days/180). Floor at 0.1 so cold-start (no
            // effective_date) people don't multiplicative-zero out.
            let recency_decay = match c.days_since_effective {
                Some(d) if d.is_finite() => {
                    let days = d.max(0.0);
                    (-days / RECENCY_HALF_LIFE_DAYS).exp()
                }
                _ => RECENCY_FLOOR,
            };
            let recency_factor = recency_decay.max(RECENCY_FLOOR);

            // salience: linear, centered at 0.5. NaN / out-of-range → 0.5.
            let mut salience = c.salience_normalized.unwrap_or(SALIENCE_CENTER);
            if !salience.is_finite() {
                salience = SALIENCE_CENTER;
            }
            salience = salience.clamp(0.0, 1.0);
            let salience_factor = 0.5 + 0.5 * salience;

            let raw_score = expertise * recency_factor * salience_factor;
            let score = if raw_score.is_finite() { raw_score } else { 0.0 };

            WhoknowsResult {
                slug: c.slug.clone(),
                source_id: c.source_id.clone(),
                title: c.title.clone(),
                page_type: c.page_type.clone(),
                score,
                factors: WhoknowsFactors {
                    expertise,
                    recency_decay,
                    recency_factor,
                    salience,
                    salience_factor,
                    days_since_effective: c.days_since_effective,
                    raw_match: c.raw_match,
                },
            }
        })
        .collect();

    // Sort by score DESC; tie-break by slug ascending for determinism.
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.slug.cmp(&b.slug))
    });

    ranked.truncate(limit.max(1));
    ranked
}

/// Options for [`find_experts`]. Mirrors the subset of TS `WhoknowsOpts` the
/// local CLI path uses (thin-client remote routing lives in the CLI layer).
#[derive(Debug, Clone, Default)]
pub struct FindExpertsOpts {
    pub topic: String,
    /// `None` → `DEFAULT_LIMIT`.
    pub limit: Option<usize>,
    /// Page-type whitelist. `None`/empty → `DEFAULT_TYPES` (person/company).
    pub types: Option<Vec<String>>,
    /// Single-source scope (P0 leak seal). `None` = all accessible sources.
    pub source_id: Option<String>,
}

/// Search-driven entrypoint. Hybrid-searches with the type filter + boosts
/// disabled, batch-fetches salience + effective-date signals, then ranks.
/// Mirrors TS `findExperts`.
///
/// # Errors
/// Propagates engine errors from `search_pages`. Salience / effective-date
/// fetches fail-soft to empty maps (mirrors the TS `.catch(() => new Map())`),
/// so a missing-signal backend degrades to neutral ranking rather than erroring.
pub async fn find_experts(
    engine: &dyn BrainEngine,
    opts: &FindExpertsOpts,
) -> crate::Result<Vec<WhoknowsResult>> {
    let limit = opts.limit.unwrap_or(DEFAULT_LIMIT);
    // Over-fetch: cast a wide net so the type-filtered + re-ranked head is
    // stable. Mirrors TS `innerLimit = max(limit * 10, 50)`.
    let inner_limit = (limit * 10).max(50);

    let types: Vec<String> = match &opts.types {
        Some(t) if !t.is_empty() => t.clone(),
        _ => DEFAULT_TYPES.iter().map(|s| (*s).to_string()).collect(),
    };

    // 1. Hybrid search with SQL-level type filter + boosts OFF (we apply our
    //    own formula on the raw relevance score).
    let search_opts = SearchOpts {
        keywords: opts.topic.split_whitespace().map(str::to_string).collect(),
        limit: Some(inner_limit),
        source_id: opts.source_id.clone(),
        types: Some(types),
        disable_salience_boost: true,
        disable_recency_boost: true,
        ..Default::default()
    };
    let results = engine.search_pages(&search_opts).await?;
    if results.is_empty() {
        return Ok(Vec::new());
    }

    // 2. Dedup to one row per (source_id, slug), keeping the max raw_match.
    //    (search_pages returns page-grain rows, but defend against cross-source
    //    fan-out duplicates.)
    use std::collections::HashMap;
    let mut by_key: HashMap<String, &crate::engine::SearchResult> = HashMap::new();
    for r in &results {
        let key = format!("{}::{}", r.page.source_id, r.page.slug);
        match by_key.get(&key) {
            Some(prev) if prev.base_score >= r.base_score => {}
            _ => {
                by_key.insert(key, r);
            }
        }
    }
    let candidates: Vec<&crate::engine::SearchResult> = by_key.into_values().collect();

    // 3. Batch-fetch salience + effective_date per (slug, source_id) ref.
    //    Fail-soft to empty maps so a missing-signal backend degrades to
    //    neutral ranking (mirrors TS `.catch(() => new Map())`).
    let refs: Vec<PageRef> = candidates
        .iter()
        .map(|c| PageRef {
            slug: c.page.slug.clone(),
            source_id: c.page.source_id.clone(),
        })
        .collect();
    let salience_map = engine
        .get_salience_scores(&refs)
        .await
        .unwrap_or_default();
    let date_map = engine.get_effective_dates(&refs).await.unwrap_or_default();

    // 4. Build the ranking-function input shape.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0i64, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let inputs: Vec<CandidateInput> = candidates
        .iter()
        .map(|c| {
            let key = format!("{}::{}", c.page.source_id, c.page.slug);
            // Salience scores are unbounded (emotional_weight×5 + ln(1+takes));
            // squash to 0..1 via ratio = s / (1 + s). Negative / non-finite →
            // None so the ranker uses the 0.5 neutral center.
            let salience_normalized = salience_map.get(&key).and_then(|&s| {
                if s.is_finite() && s >= 0.0 {
                    Some(s / (1.0 + s))
                } else {
                    None
                }
            });
            // days_since_effective: (now - effective) / ms_per_day, floored at 0.
            let days_since_effective = date_map.get(&key).and_then(|iso| {
                crate::engine::iso8601_to_unix_ms(iso).map(|ms| {
                    let days = (now_ms - ms) as f64 / MS_PER_DAY;
                    days.max(0.0)
                })
            });
            CandidateInput {
                slug: c.page.slug.clone(),
                source_id: c.page.source_id.clone(),
                title: c.page.title.clone(),
                page_type: c.page.page_type.clone(),
                raw_match: c.base_score,
                days_since_effective,
                salience_normalized,
            }
        })
        .collect();

    // 5. Rank.
    Ok(rank_candidates(&inputs, limit))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(slug: &str, raw: f64, days: Option<f64>, sal: Option<f64>) -> CandidateInput {
        CandidateInput {
            slug: slug.to_string(),
            source_id: "default".to_string(),
            title: format!("Title {slug}"),
            page_type: "person".to_string(),
            raw_match: raw,
            days_since_effective: days,
            salience_normalized: sal,
        }
    }

    #[test]
    fn ranks_higher_raw_match_first() {
        // Same recency + salience → higher raw_match wins via ln(1+raw).
        let out = rank_candidates(
            &[
                input("low", 1.0, Some(0.0), Some(0.5)),
                input("high", 10.0, Some(0.0), Some(0.5)),
            ],
            5,
        );
        assert_eq!(out[0].slug, "high");
        assert_eq!(out[1].slug, "low");
    }

    #[test]
    fn cold_start_uses_recency_floor_not_zero() {
        // No effective date → recency_factor = 0.1, NOT multiplicative-zero.
        let out = rank_candidates(&[input("cold", 5.0, None, Some(0.5))], 5);
        assert!((out[0].factors.recency_factor - RECENCY_FLOOR).abs() < 1e-12);
        assert!(out[0].score > 0.0, "cold-start must stay visible");
    }

    #[test]
    fn recency_decay_is_exponential_half_life() {
        // At exactly one half-life window (180d), decay = exp(-1) ≈ 0.3679.
        let out = rank_candidates(&[input("aged", 5.0, Some(180.0), Some(0.5))], 5);
        let expected = (-1.0f64).exp();
        assert!((out[0].factors.recency_decay - expected).abs() < 1e-9);
    }

    #[test]
    fn missing_salience_is_neutral_center() {
        // salience None → 0.5 center → salience_factor = 0.75.
        let out = rank_candidates(&[input("nosal", 5.0, Some(0.0), None)], 5);
        assert!((out[0].factors.salience - SALIENCE_CENTER).abs() < 1e-12);
        assert!((out[0].factors.salience_factor - 0.75).abs() < 1e-12);
    }

    #[test]
    fn salience_boosts_score_linearly() {
        // Higher salience → higher salience_factor → higher score, all else equal.
        let out = rank_candidates(
            &[
                input("lo", 5.0, Some(0.0), Some(0.0)),
                input("hi", 5.0, Some(0.0), Some(1.0)),
            ],
            5,
        );
        assert_eq!(out[0].slug, "hi");
        // salience_factor: lo=0.5, hi=1.0.
        let hi = out.iter().find(|r| r.slug == "hi").unwrap();
        let lo = out.iter().find(|r| r.slug == "lo").unwrap();
        assert!((hi.factors.salience_factor - 1.0).abs() < 1e-12);
        assert!((lo.factors.salience_factor - 0.5).abs() < 1e-12);
    }

    #[test]
    fn zero_raw_match_yields_zero_expertise_and_score() {
        // ln(1+0) = 0 → whole product = 0.
        let out = rank_candidates(&[input("empty", 0.0, Some(0.0), Some(1.0))], 5);
        assert_eq!(out[0].factors.expertise, 0.0);
        assert_eq!(out[0].score, 0.0);
    }

    #[test]
    fn negative_raw_match_clamped_to_zero() {
        let out = rank_candidates(&[input("neg", -3.0, Some(0.0), Some(0.5))], 5);
        assert_eq!(out[0].factors.expertise, 0.0);
        assert_eq!(out[0].score, 0.0);
    }

    #[test]
    fn tie_break_by_slug_ascending() {
        // Identical signals → deterministic slug-ascending order.
        let out = rank_candidates(
            &[
                input("zebra", 5.0, Some(0.0), Some(0.5)),
                input("alpha", 5.0, Some(0.0), Some(0.5)),
                input("mango", 5.0, Some(0.0), Some(0.5)),
            ],
            5,
        );
        assert_eq!(out[0].slug, "alpha");
        assert_eq!(out[1].slug, "mango");
        assert_eq!(out[2].slug, "zebra");
    }

    #[test]
    fn respects_limit_and_floors_at_one() {
        let all = [
            input("a", 5.0, Some(0.0), Some(0.5)),
            input("b", 4.0, Some(0.0), Some(0.5)),
            input("c", 3.0, Some(0.0), Some(0.5)),
        ];
        assert_eq!(rank_candidates(&all, 2).len(), 2);
        // limit 0 floors to 1 (never returns an empty slice for non-empty input).
        assert_eq!(rank_candidates(&all, 0).len(), 1);
    }

    #[test]
    fn future_effective_date_clamped_to_zero_days() {
        // days_since_effective < 0 → clamped to 0 → decay = exp(0) = 1.0.
        // (Callers already clamp, but the ranker must be defensive too.)
        let out = rank_candidates(&[input("future", 5.0, Some(-30.0), Some(0.5))], 5);
        assert!((out[0].factors.recency_decay - 1.0).abs() < 1e-12);
    }
}
