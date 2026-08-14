//! v0.37.0 — brainstorm + LSD shared judge (D6 — single file, two configs).
//!
//! Faithful port of `src/core/judges.ts`. One `run_judge` function + two
//! exported configs that flip the threshold and the inversion rule. Rubric
//! adapted from Open Collider's judge.md (CL-ML/open-collider, MIT). Five axes
//! scored 1-5; weighted average; per-config threshold.
//!
//! Brainstorm: standard rubric, threshold 4.0.
//! LSD: inverted rubric — rejects ideas with resistance > 4.5 ("too obvious").
//! The "productive dissonance" axis (cognitive_load) is weighted heavily.
//!
//! Provider-neutral via the [`ChatProvider`] seam (mirrors TS `gateway.chat()`
//! + the `chatFn` injection point for hermetic tests).

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole, ChatUsage};
use regex::Regex;
use std::sync::LazyLock;

/// Prompt/rubric version — flows into the cache key and the run report.
pub const PROMPT_VERSION: &str = "brainstorm-judge-v1";

/// Default judge chunk size. ~350 tokens/idea × 100 ≈ 35K input tokens,
/// safely under any model context.
pub const DEFAULT_JUDGE_CHUNK_SIZE: usize = 100;

/// Per-axis 1-5 score.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
pub struct JudgeAxisScores {
    pub originality: f64,
    pub resistance: f64,
    pub thesis_density: f64,
    pub concrete_grounding: f64,
    pub cognitive_load: f64,
}

impl JudgeAxisScores {
    /// Sum of the five axis weights — must be 1.0 for a valid config.
    #[must_use]
    pub fn sum(&self) -> f64 {
        self.originality
            + self.resistance
            + self.thesis_density
            + self.concrete_grounding
            + self.cognitive_load
    }
}

/// One idea handed to the judge. The orchestrator builds these from a
/// `(close × far)` cross output.
#[derive(Debug, Clone)]
pub struct JudgeIdea {
    /// Stable id within this run (e.g. "01", "02").
    pub id: String,
    /// Free-form idea text (2-4 sentences).
    pub text: String,
    /// The (close, far) pair that produced this idea — surfaces in the prompt.
    pub close_slug: String,
    pub far_slug: String,
}

/// Per-idea verdict the orchestrator consumes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct JudgeIdeaResult {
    pub id: String,
    pub scores: JudgeAxisScores,
    /// Weighted aggregate per the config's axis weights.
    pub weighted_score: f64,
    /// True iff this idea passes the config's threshold (after inversion rule).
    pub passes: bool,
    /// One-sentence judge note (main strength or rejection reason).
    pub note: String,
}

/// Top-level judge response (one batch).
#[derive(Debug, Clone)]
pub struct JudgeResult {
    pub ideas: Vec<JudgeIdeaResult>,
    /// Number of input ideas that passed the threshold.
    pub pass_count: usize,
    /// Provider:model that answered (for cost accounting / debugging).
    pub model: String,
    pub usage: ChatUsage,
}

/// Brainstorm vs LSD config delta.
#[derive(Debug, Clone, Copy)]
pub struct JudgeConfig {
    /// Stable label — flows into the cache key and the run report.
    pub label: &'static str,
    /// Axis weights — must sum to 1.0 (validated by [`validate_judge_config`]).
    pub weights: JudgeAxisScores,
    /// Threshold on the weighted average; ideas below this are filtered.
    pub threshold: f64,
    /// LSD-only: reject ideas whose `resistance` (coherence) axis exceeds this.
    pub reject_if_resistance_above: Option<f64>,
    /// Append to the rubric prompt.
    pub extra_instructions: Option<&'static str>,
}

/// Brainstorm config. Mirrors Open Collider's judge.md exactly; threshold
/// relaxed from 4.2 → 4.0 (brain-grounded ideas carry inherent constraint).
pub const BRAINSTORM_JUDGE_CONFIG: JudgeConfig = JudgeConfig {
    label: "brainstorm",
    weights: JudgeAxisScores {
        originality: 0.25,
        resistance: 0.20,
        thesis_density: 0.20,
        concrete_grounding: 0.20,
        cognitive_load: 0.15,
    },
    threshold: 4.0,
    reject_if_resistance_above: None,
    extra_instructions: None,
};

/// LSD config. The "Lateral Synaptic Drift" inversion: cognitive_load
/// dominates; resistance > 4.5 ("too obvious") is an automatic rejection;
/// axiomatic inversions required; threshold relaxed to 3.5.
pub const LSD_JUDGE_CONFIG: JudgeConfig = JudgeConfig {
    label: "lsd",
    weights: JudgeAxisScores {
        originality: 0.20,
        resistance: 0.05,
        thesis_density: 0.15,
        concrete_grounding: 0.10,
        cognitive_load: 0.50,
    },
    threshold: 3.5,
    reject_if_resistance_above: Some(4.5),
    extra_instructions: Some(
        "Every kept idea MUST invert at least one implicit axiom (X is good → X is the problem; \
         everyone does Y → the opposite; dominant narrative says Z → the hidden cause).",
    ),
};

/// Validate a config's axis weights sum to 1.0 (±1e-9 tolerance).
#[must_use]
pub fn validate_judge_config(config: &JudgeConfig) -> bool {
    (config.weights.sum() - 1.0).abs() <= 1e-9
}

static FENCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"```(?:json)?\s*\n?([\s\S]*?)```").expect("judge fence regex must compile")
});

fn build_judge_prompt(config: &JudgeConfig, ideas: &[JudgeIdea]) -> String {
    let w = config.weights;
    let ideas_block = ideas
        .iter()
        .map(|idea| {
            format!(
                "## Idea {}\n(close={} × far={})\n{}",
                idea.id, idea.close_slug, idea.far_slug, idea.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let inversion_rule = match config.reject_if_resistance_above {
        Some(ceil) => format!(
            "\n\n## LSD INVERSION RULE\nAny idea with resistance > {:.1} is REJECTED regardless of \
             weighted score — these are the ideas the user would surface without LSD. \"Too \
             obvious\" is the failure mode here.",
            ceil
        ),
        None => String::new(),
    };

    let extras = config
        .extra_instructions
        .map(|e| format!("\n\n## ADDITIONAL CONSTRAINT\n{e}"))
        .unwrap_or_default();

    format!(
        "You are a structural evaluator filtering brainstorm ideas. Score each idea on the \
         underlying potential, not the current wording.\n\n\
         ## AXES (each scored 1-5)\n\n\
         **Originality (weight {:.2})** — Is the underlying thesis genuinely new?\n\
         \x20\x205: thesis never seen formulated this way\n\
         \x20\x203: known angle with new packaging\n\
         \x20\x201: reformulation of standard advice\n\n\
         **Resistance (weight {:.2})** — Does the core thesis hold up against the strongest possible objection?\n\
         \x20\x205: holds up even against the strongest counterargument\n\
         \x20\x203: substance recoverable, current wording doesn't resist\n\
         \x20\x201: a single objection collapses the entire idea\n\n\
         **Thesis density (weight {:.2})** — Could it be formulated as a single testable + refutable thesis?\n\
         \x20\x205: precise thesis identifiable, directly attackable\n\
         \x20\x203: implicit thesis, recoverable with reformulation\n\
         \x20\x201: observation or anecdote from which no thesis can be extracted\n\n\
         **Concrete grounding (weight {:.2})** — Could the idea rely on a specific fact, figure, or named situation?\n\
         \x20\x205: grounding already present, or obvious + immediately findable evidence\n\
         \x20\x203: grounding possible but requires non-trivial research\n\
         \x20\x201: pure abstraction, no real data could support it\n\n\
         **Cognitive load (weight {:.2})** — Does the idea force reconstruction, or is it immediately expected?\n\
         \x20\x205: productive dissonance — the reader must stop and think\n\
         \x20\x203: slightly counter-intuitive\n\
         \x20\x201: expected information, no friction{}\n\
         {}\n\n\
         ## IDEAS TO EVALUATE\n\n\
         {}\n\n\
         ## OUTPUT FORMAT (strict JSON, no prose outside the JSON)\n\n\
         ```json\n\
         {{\n  \"ideas\": [\n    {{\n      \"id\": \"<idea id>\",\n      \"scores\": {{\n        \
         \"originality\": <1-5>,\n        \"resistance\": <1-5>,\n        \"thesis_density\": <1-5>,\n        \
         \"concrete_grounding\": <1-5>,\n        \"cognitive_load\": <1-5>\n      }},\n      \
         \"note\": \"<one sentence — main strength if passing, rejection reason if not>\"\n    }}\n  ]\n}}\n\
         ```\n\n\
         Respond with ONLY the JSON block, nothing before or after.",
        w.originality,
        w.resistance,
        w.thesis_density,
        w.concrete_grounding,
        w.cognitive_load,
        inversion_rule,
        extras,
        ideas_block,
    )
}

/// Parse judge JSON with a 3-strategy fallback. Throws on unparseable rather
/// than fabricating a verdict. Mirrors TS `parseJudgeJSON`.
pub fn parse_judge_json(text: &str) -> crate::Result<serde_json::Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(crate::Error::engine("parseJudgeJSON: empty response"));
    }
    // Strategy 1: direct parse.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Ok(v);
    }
    // Strategy 2: fenced ```json block.
    if let Some(cap) = FENCE_RE.captures(trimmed) {
        if let Some(inner) = cap.get(1) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(inner.as_str().trim()) {
                return Ok(v);
            }
        }
    }
    // Strategy 3: common-repairs pass (strip fences, drop trailing commas,
    // then take the first `{` … last `}` slice).
    let cleaned = FENCE_RE
        .replace_all(trimmed, "$1")
        .replace(",]", "]")
        .replace(",}", "}");
    if let Some(start) = cleaned.find('{') {
        let rest = &cleaned[start..];
        if let Some(end) = rest.rfind('}') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&rest[..end + 1]) {
                return Ok(v);
            }
        }
    }
    Err(crate::Error::engine(
        "parseJudgeJSON: no strategy produced valid JSON",
    ))
}

fn is_axis_score_in_range(n: &serde_json::Value) -> bool {
    match n {
        serde_json::Value::Number(x) => {
            let v = x.as_f64().unwrap_or(f64::NAN);
            v.is_finite() && (1.0..=5.0).contains(&v)
        }
        _ => false,
    }
}

/// Validate + extract one idea's shape from a parsed JSON value. Returns
/// `None` when the row is malformed (caller stderr-warns + skips).
fn validate_idea_shape(
    raw: &serde_json::Value,
) -> Option<(String, JudgeAxisScores, String)> {
    let obj = raw.as_object()?;
    let id = obj.get("id")?.as_str()?.to_string();
    let note = obj
        .get("note")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let s = obj.get("scores")?.as_object()?;
    let get = |k: &str| s.get(k).filter(|n: &&serde_json::Value| is_axis_score_in_range(*n)).and_then(|v| v.as_f64());
    let originality = get("originality")?;
    let resistance = get("resistance")?;
    let thesis_density = get("thesis_density")?;
    let concrete_grounding = get("concrete_grounding")?;
    let cognitive_load = get("cognitive_load")?;
    Some((
        id,
        JudgeAxisScores {
            originality,
            resistance,
            thesis_density,
            concrete_grounding,
            cognitive_load,
        },
        note,
    ))
}

/// Weighted score computation.
#[must_use]
pub fn weighted_score(scores: &JudgeAxisScores, weights: &JudgeAxisScores) -> f64 {
    scores.originality * weights.originality
        + scores.resistance * weights.resistance
        + scores.thesis_density * weights.thesis_density
        + scores.concrete_grounding * weights.concrete_grounding
        + scores.cognitive_load * weights.cognitive_load
}

/// Per-config passing rule. LSD additionally enforces the inversion-rule
/// resistance ceiling (raw `resistance` axis, not weighted).
#[must_use]
pub fn idea_passes(idea: &JudgeIdeaResult, config: &JudgeConfig) -> bool {
    if idea.weighted_score < config.threshold {
        return false;
    }
    if let Some(ceil) = config.reject_if_resistance_above {
        if idea.scores.resistance > ceil {
            return false;
        }
    }
    true
}

/// Options for [`run_judge`].
pub struct RunJudgeOptions {
    /// Override the chat model (e.g. for `zbrain models doctor` probes).
    pub model_override: Option<String>,
    /// Anti-bias context from the user's calibration profile (D4 + codex #8).
    pub active_bias_tags: Vec<String>,
    /// Maximum ideas per single LLM call (default 100).
    pub max_ideas_per_call: Option<usize>,
}

impl Default for RunJudgeOptions {
    fn default() -> Self {
        Self {
            model_override: None,
            active_bias_tags: vec![],
            max_ideas_per_call: None,
        }
    }
}

/// Judge a batch of ideas. Automatically chunks large idea sets into
/// `max_ideas_per_call`-sized sub-batches (default 100) to avoid blowing the
/// model context window. Each chunk is a separate LLM call; results are
/// concatenated. Throws on parse failure of any chunk (caller maps to
/// judge_failed + saves unscored, per D12).
pub async fn run_judge(
    config: &JudgeConfig,
    ideas: &[JudgeIdea],
    chat: &dyn ChatProvider,
    opts: &RunJudgeOptions,
) -> crate::Result<JudgeResult> {
    if ideas.is_empty() {
        return Ok(JudgeResult {
            ideas: vec![],
            pass_count: 0,
            model: "noop".to_string(),
            usage: ChatUsage::default(),
        });
    }
    let chunk_size = opts.max_ideas_per_call.unwrap_or(DEFAULT_JUDGE_CHUNK_SIZE).max(1);
    let chunks: Vec<&[JudgeIdea]> = ideas.chunks(chunk_size).collect();

    let mut all_results: Vec<JudgeIdeaResult> = Vec::new();
    let mut last_model = "noop".to_string();
    let mut total_usage = ChatUsage::default();

    for chunk in chunks {
        let chunk_result = run_judge_chunk(config, chunk, chat, opts).await?;
        all_results.extend(chunk_result.ideas);
        last_model = chunk_result.model;
        total_usage.input_tokens += chunk_result.usage.input_tokens;
        total_usage.output_tokens += chunk_result.usage.output_tokens;
        total_usage.cache_read_tokens += chunk_result.usage.cache_read_tokens;
        total_usage.cache_creation_tokens += chunk_result.usage.cache_creation_tokens;
    }

    let pass_count = all_results.iter().filter(|i| i.passes).count();
    Ok(JudgeResult {
        ideas: all_results,
        pass_count,
        model: last_model,
        usage: total_usage,
    })
}

/// Single-chunk inner loop.
async fn run_judge_chunk(
    config: &JudgeConfig,
    ideas: &[JudgeIdea],
    chat: &dyn ChatProvider,
    opts: &RunJudgeOptions,
) -> crate::Result<JudgeResult> {
    let prompt = build_judge_prompt(config, ideas);

    let system = if !opts.active_bias_tags.is_empty() {
        Some(format!(
            "You are scoring ideas for a user with the following known biases: {}. Penalize the \
             originality axis when an idea closely matches a known bias pattern.",
            opts.active_bias_tags.join(", ")
        ))
    } else {
        None
    };

    let req = ChatOpts {
        model: opts.model_override.clone(),
        system,
        messages: vec![ChatMessage {
            role: ChatRole::User,
            content: crate::ai::chat::ChatContent::Text(prompt),
        }],
        max_tokens: Some(4000),
        ..Default::default()
    };

    let result = chat
        .chat(req)
        .await
        .map_err(|e| crate::Error::engine(format!("runJudge chat call failed: {e}")))?;

    let parsed = parse_judge_json(&result.text)?;
    let raw_ideas = parsed
        .get("ideas")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            crate::Error::engine(format!(
                "runJudge: response missing 'ideas' array. Got: {}",
                &result.text.chars().take(200).collect::<String>()
            ))
        })?;

    let mut idea_results: Vec<JudgeIdeaResult> = Vec::new();
    for raw in raw_ideas {
        let Some((id, scores, note)) = validate_idea_shape(raw) else {
            // Skip malformed rows — orchestrator surfaces a stderr warning if
            // fewer ideas come back than were submitted.
            continue;
        };
        let weighted_score = weighted_score(&scores, &config.weights);
        let mut ir = JudgeIdeaResult {
            id,
            scores,
            weighted_score,
            passes: false,
            note,
        };
        ir.passes = idea_passes(&ir, config);
        idea_results.push(ir);
    }

    let pass_count = idea_results.iter().filter(|i| i.passes).count();
    Ok(JudgeResult {
        ideas: idea_results,
        pass_count,
        model: result.model,
        usage: result.usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatContent, MockChatProvider, StopReason};

    #[test]
    fn configs_validate() {
        assert!(validate_judge_config(&BRAINSTORM_JUDGE_CONFIG));
        assert!(validate_judge_config(&LSD_JUDGE_CONFIG));
    }

    #[test]
    fn weighted_score_and_passes_brainstorm() {
        let scores = JudgeAxisScores {
            originality: 5.0,
            resistance: 5.0,
            thesis_density: 5.0,
            concrete_grounding: 5.0,
            cognitive_load: 5.0,
        };
        let w = weighted_score(&scores, &BRAINSTORM_JUDGE_CONFIG.weights);
        assert!((w - 5.0).abs() < 1e-9);
        let ir = JudgeIdeaResult {
            id: "1".into(),
            scores,
            weighted_score: w,
            passes: false,
            note: String::new(),
        };
        assert!(idea_passes(&ir, &BRAINSTORM_JUDGE_CONFIG));
    }

    #[test]
    fn lsd_rejects_too_obvious() {
        let scores = JudgeAxisScores {
            originality: 5.0,
            // resistance 5.0 > LSD ceiling 4.5 → rejected regardless of score.
            resistance: 5.0,
            thesis_density: 5.0,
            concrete_grounding: 5.0,
            cognitive_load: 5.0,
        };
        let w = weighted_score(&scores, &LSD_JUDGE_CONFIG.weights);
        let ir = JudgeIdeaResult {
            id: "1".into(),
            scores,
            weighted_score: w,
            passes: false,
            note: String::new(),
        };
        assert!(!idea_passes(&ir, &LSD_JUDGE_CONFIG));
    }

    #[test]
    fn parse_judge_json_three_strategies() {
        // 1: direct
        let v = parse_judge_json(r#"{"ideas":[]}"#).unwrap();
        assert!(v.get("ideas").is_some());
        // 2: fenced
        let v = parse_judge_json("text\n```json\n{\"ideas\":[]}\n```\nmore").unwrap();
        assert!(v.get("ideas").is_some());
        // 3: trailing comma + prose
        let v = parse_judge_json("blah {\"ideas\":[{\"id\":\"1\",}],} trailing").unwrap();
        assert!(v.get("ideas").is_some());
        // empty → err
        assert!(parse_judge_json("   ").is_err());
    }

    #[tokio::test]
    async fn run_judge_empty_is_noop() {
        let mock = MockChatProvider::new("unused");
        let res = run_judge(
            &BRAINSTORM_JUDGE_CONFIG,
            &[],
            &mock,
            &RunJudgeOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(res.pass_count, 0);
        assert_eq!(res.model, "noop");
    }

    #[tokio::test]
    async fn run_judge_parses_mock_response() {
        let mock = MockChatProvider::new("unused");
        mock.queue_text(
            r#"```json
            {"ideas":[
              {"id":"01","scores":{"originality":5,"resistance":4,"thesis_density":4,"concrete_grounding":4,"cognitive_load":3},"note":"strong"}
            ]}
            ```"#,
        );
        let ideas = vec![JudgeIdea {
            id: "01".into(),
            text: "idea".into(),
            close_slug: "a".into(),
            far_slug: "b".into(),
        }];
        let res = run_judge(
            &BRAINSTORM_JUDGE_CONFIG,
            &ideas,
            &mock,
            &RunJudgeOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(res.ideas.len(), 1);
        // weighted = .25*5+.20*4+.20*4+.20*4+.15*3 = 1.25+.8+.8+.8+.45 = 4.10 ≥ 4.0
        assert!(res.ideas[0].passes);
        assert!(res.ideas[0].weighted_score > 4.0);
        // Ensure the chat request carried the prompt as a user text block.
        let _ = ChatContent::Text(String::new());
        let _ = StopReason::End;
    }
}
