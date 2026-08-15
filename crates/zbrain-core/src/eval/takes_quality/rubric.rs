//! takes-quality-eval/rubric — single source of truth for what "takes
//! quality" means (faithful port of TS `src/core/takes-quality-eval/rubric.ts`).
//!
//! Five dimensions distilled from the cross-modal eval over production takes.
//! Bumping any field here changes [`rubric_sha8`], which the receipt name binds
//! so trend graphs segregate by rubric epoch (a future rubric tweak produces a
//! different key, no silent corruption of trend graphs).

pub const RUBRIC_VERSION: &str = "v1.0";

/// The 5 dimensions a model must score for its result to count toward verdict.
pub const RUBRIC_DIMENSIONS: &[&str] = &[
    "accuracy",
    "attribution",
    "weight_calibration",
    "kind_classification",
    "signal_density",
];

pub const PASS_MEAN_THRESHOLD: f64 = 7.0;
pub const PASS_FLOOR_THRESHOLD: f64 = 5.0;
pub const MIN_SUCCESSES_FOR_VERDICT: usize = 2;

/// Default rubric dimensions for the takes-quality judge (the 5 TS dims).
pub fn default_dimensions() -> Vec<String> {
    RUBRIC_DIMENSIONS.iter().map(|s| s.to_string()).collect()
}

/// Stable 8-char fingerprint over the rubric definition. Receipt-name binds
/// this so two runs with the same rubric produce the same receipt key,
/// while a future rubric tweak segregates trend rows cleanly.
pub fn rubric_sha8() -> String {
    let canonical = serde_json::json!({
        "version": RUBRIC_VERSION,
        "dimensions": RUBRIC_DIMENSIONS,
        "pass_mean": PASS_MEAN_THRESHOLD,
        "pass_floor": PASS_FLOOR_THRESHOLD,
        "min_successes": MIN_SUCCESSES_FOR_VERDICT,
    });
    crate::eval::cross_modal::sha8(&canonical.to_string())
}

/// Faithful analog of TS `renderJudgePrompt`. Builds the 5-dim judge prompt
/// and returns its 8-char sha (binds `prompt_sha8` to corpus + rubric). Two
/// runs over the same corpus + same rubric produce the same sha.
pub fn render_judge_prompt(takes_text: &str) -> (String, String) {
    let dims_block = RUBRIC_DIMENSIONS
        .iter()
        .enumerate()
        .map(|(i, d)| format!("{}. {}", i + 1, d))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = format!(
        "You are evaluating a sample of \"takes\" — typed, weighted, attributed \
         claims pulled from a personal knowledge base. Score the sample on the \
         5 dimensions below.\n\nDimensions:\n{}\n\nTakes sample:\n{}",
        dims_block, takes_text
    );
    let sha = crate::eval::cross_modal::sha8(&prompt);
    (prompt, sha)
}
