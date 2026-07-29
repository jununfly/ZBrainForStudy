//! v0.29 — Emotional weight: deterministic 0..1 score for each page, computed
//! from tags + active takes. Pure function, no DB.
//!
//! Faithful port of `src/core/cycle/emotional-weight.ts` (`computeEmotionalWeight`).
//! The cycle phase (`recompute_emotional_weight`, see `recompute_emotional_weight.rs`)
//! loads inputs in batch via `engine.batch_load_emotional_inputs` and writes results
//! in batch via `engine.set_emotional_weight_batch`; this module only holds the math.
//!
//! Tunable: `HIGH_EMOTION_TAGS` is the default seed list. Override via
//! `EmotionalWeightOpts.high_emotion_tags`. The user holder (used in the
//! Garry-as-holder ratio) defaults to `DEFAULT_USER_HOLDER` and is overridable via
//! `EmotionalWeightOpts.user_holder`.
//!
//! NOTE: TS resolved the high-tags override from `engine.getConfig('emotional_weight.high_tags')`.
//! Rust has no global config store (see 1-1-6 note), so the override is passed through
//! `opts` instead — matching the established "config → opts" migration convention.

use std::collections::HashSet;

/// Re-export the shared take shape so callers (and `EmotionalWeightInput`) can
/// use a single definition anchored on the engine contract.
pub use crate::engine::EmotionalWeightTake;

/// Default high-emotion tag seed list. Pages with any (case-insensitive) matching
/// tag get the tag-emotion boost (0.5) in the formula.
///
/// Anglocentric and personal-life-biased on purpose: v1 default for a personal brain.
/// Override unconditionally at install time if a brain is mostly work-life.
pub const HIGH_EMOTION_TAGS: &[&str] = &[
    "family",
    "marriage",
    "wedding",
    "loss",
    "death",
    "grief",
    "relationship",
    "love",
    "mental-health",
    "health",
    "illness",
    "birth",
    "children",
    "kids",
    "parents",
];

/// Holder name treated as "the user" for the Garry-as-holder ratio.
pub const DEFAULT_USER_HOLDER: &str = "garry";

/// Inputs to [`compute_emotional_weight`].
#[derive(Debug, Clone, Default)]
pub struct EmotionalWeightInput {
    pub tags: Vec<String>,
    pub takes: Vec<EmotionalWeightTake>,
}

/// Overrides for [`compute_emotional_weight`].
#[derive(Debug, Clone, Default)]
pub struct EmotionalWeightOpts {
    /// Override the default `HIGH_EMOTION_TAGS` set. Tag matching is case-insensitive.
    pub high_emotion_tags: Option<HashSet<String>>,
    /// Override the default user holder name (used in the Garry-as-holder ratio).
    pub user_holder: Option<String>,
}

/// The default high-emotion tag set (lowercased).
pub fn high_emotion_tags_default() -> HashSet<String> {
    HIGH_EMOTION_TAGS.iter().map(|s| s.to_string()).collect()
}

fn clamp01(n: f64) -> f64 {
    if !n.is_finite() {
        return 0.0;
    }
    if n < 0.0 {
        return 0.0;
    }
    if n > 1.0 {
        return 1.0;
    }
    n
}

/// Compute emotional weight in [0..1] from a page's tags + active takes.
///
/// Formula (sum capped at 1.0):
/// 1) Tag emotion boost   max 0.5  (any matching high-emotion tag)
/// 2) Take density        max 0.3  (0.1 per active take, capped)
/// 3) Take avg weight     max 0.1  (avg of take.weight, scaled)
/// 4) User-holder ratio   max 0.1  (active takes by user / total active)
///
/// Returns exactly 0.0 for empty inputs (no tags, no takes).
pub fn compute_emotional_weight(input: &EmotionalWeightInput, opts: &EmotionalWeightOpts) -> f64 {
    let tag_set: HashSet<String> = opts
        .high_emotion_tags
        .clone()
        .unwrap_or_else(high_emotion_tags_default);
    let user_holder = opts
        .user_holder
        .clone()
        .unwrap_or_else(|| DEFAULT_USER_HOLDER.to_string())
        .to_lowercase();

    let all_takes = &input.takes;
    let takes: Vec<&EmotionalWeightTake> = all_takes.iter().filter(|t| t.active).collect();

    // 1) Tag emotion boost — case-insensitive match.
    let mut tag_boost = 0.0;
    for t in &input.tags {
        if tag_set.iter().any(|ht| ht.eq_ignore_ascii_case(t)) {
            tag_boost = 0.5;
            break;
        }
    }

    // 2) Take density: 0.1 per active take, capped at 0.3.
    let take_density = (takes.len() as f64 * 0.1).min(0.3);

    // 3) Take avg weight, scaled into 0..0.1.
    let take_avg_weight = if takes.is_empty() {
        0.0
    } else {
        let sum: f64 = takes.iter().map(|t| clamp01(t.weight)).sum();
        (sum / takes.len() as f64) * 0.1
    };

    // 4) User-holder ratio over active takes, scaled into 0..0.1.
    let user_holder_ratio = if takes.is_empty() {
        0.0
    } else {
        let user_takes = takes
            .iter()
            .filter(|t| t.holder.to_lowercase() == user_holder)
            .count();
        (user_takes as f64 / takes.len() as f64) * 0.1
    };

    let total = tag_boost + take_density + take_avg_weight + user_holder_ratio;
    total.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(holder: &str, weight: f64, active: bool) -> EmotionalWeightTake {
        EmotionalWeightTake {
            holder: holder.to_string(),
            weight,
            kind: "take".to_string(),
            active,
        }
    }

    #[test]
    fn empty_input_is_zero() {
        let input = EmotionalWeightInput::default();
        let opts = EmotionalWeightOpts::default();
        assert_eq!(compute_emotional_weight(&input, &opts), 0.0);
    }

    #[test]
    fn tag_boost_caps_at_half() {
        let input = EmotionalWeightInput {
            tags: vec!["wedding".to_string()],
            takes: vec![],
        };
        let opts = EmotionalWeightOpts::default();
        // Tag emotion boost alone = 0.5.
        assert!((compute_emotional_weight(&input, &opts) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn tag_match_is_case_insensitive() {
        let input = EmotionalWeightInput {
            tags: vec!["Health".to_string()],
            takes: vec![],
        };
        let opts = EmotionalWeightOpts::default();
        assert!((compute_emotional_weight(&input, &opts) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn take_density_caps_at_three_tenths() {
        // 4 active takes => density 0.4 capped to 0.3. No high tag, no user ratio.
        let input = EmotionalWeightInput {
            tags: vec![],
            takes: vec![
                take("alice", 0.0, true),
                take("bob", 0.0, true),
                take("carol", 0.0, true),
                take("dave", 0.0, true),
            ],
        };
        let opts = EmotionalWeightOpts {
            user_holder: Some("garry".to_string()),
            ..Default::default()
        };
        let w = compute_emotional_weight(&input, &opts);
        assert!((w - 0.3).abs() < 1e-9, "got {w}");
    }

    #[test]
    fn inactive_takes_excluded_from_density() {
        let input = EmotionalWeightInput {
            tags: vec![],
            takes: vec![take("alice", 0.0, true), take("bob", 0.0, false)],
        };
        let opts = EmotionalWeightOpts::default();
        // 1 active take => density 0.1.
        assert!((compute_emotional_weight(&input, &opts) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn avg_weight_contributes() {
        let input = EmotionalWeightInput {
            tags: vec![],
            takes: vec![take("garry", 1.0, true), take("garry", 0.0, true)],
        };
        let opts = EmotionalWeightOpts {
            user_holder: Some("garry".to_string()),
            ..Default::default()
        };
        // density 0.2 + avg_weight (avg 0.5 * 0.1 = 0.05) + user_ratio (2/2 * 0.1 = 0.1) = 0.35.
        let w = compute_emotional_weight(&input, &opts);
        assert!((w - 0.35).abs() < 1e-9, "got {w}");
    }

    #[test]
    fn high_tags_override_works() {
        let input = EmotionalWeightInput {
            tags: vec!["work".to_string()],
            takes: vec![],
        };
        // Default set does not contain "work" => no boost.
        assert_eq!(compute_emotional_weight(&input, &EmotionalWeightOpts::default()), 0.0);
        // Override set contains "work" => boost 0.5.
        let mut override_set = HashSet::new();
        override_set.insert("work".to_string());
        let opts = EmotionalWeightOpts {
            high_emotion_tags: Some(override_set),
            ..Default::default()
        };
        assert!((compute_emotional_weight(&input, &opts) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn user_holder_override_works() {
        let input = EmotionalWeightInput {
            tags: vec![],
            takes: vec![take("alice", 0.0, true)],
        };
        // Default user holder 'garry' => alice is not the user => no user ratio.
        assert!((compute_emotional_weight(&input, &EmotionalWeightOpts::default()) - 0.1).abs() < 1e-9);
        // Override user holder to 'alice' => ratio 0.1.
        let opts = EmotionalWeightOpts {
            user_holder: Some("alice".to_string()),
            ..Default::default()
        };
        assert!((compute_emotional_weight(&input, &opts) - 0.2).abs() < 1e-9, "user ratio should add 0.1 on top of density 0.1");
    }

    #[test]
    fn total_clamps_to_one() {
        // Many high signals should clamp at 1.0, never exceed.
        let input = EmotionalWeightInput {
            tags: vec!["grief".to_string()],
            takes: (0..20)
                .map(|i| take("garry", 1.0, true))
                .collect(),
        };
        let opts = EmotionalWeightOpts {
            user_holder: Some("garry".to_string()),
            ..Default::default()
        };
        let w = compute_emotional_weight(&input, &opts);
        assert!((w - 1.0).abs() < 1e-9, "got {w}");
        assert!(w <= 1.0);
    }
}
