//! SYNTHESIZE phase + full `run_think` pipeline for `zbrain think`.
//!
//! Ports `src/core/think/index.ts` `runThink`:
//!   GATHER (via [`crate::think::gather::run_gather`])
//!   → render evidence blocks (`<pages>` / `<takes>`)
//!   → build system + user prompt (via [`crate::think::prompt`])
//!   → call the LLM through the provider-neutral [`ChatProvider`] (the Rust
//!     analog of TS `gateway.chat`)
//!   → parse the structured `{answer, citations, gaps}` JSON
//!   → resolve citations (structured first, inline-marker regex fallback)
//!   → return [`ThinkResult`].
//!
//! Model resolution + chat-provider construction mirror the TS
//! `tryBuildGatewayClient` path: resolve the model string to a recipe, build
//! a `ChatProvider` from the recipe + env API key, and on ANY failure (unknown
//! provider, missing key, provider Cargo feature disabled) degrade gracefully
//! to a "no LLM available" result rather than throwing — exactly like the TS
//! gateway adapter.

use crate::ai::chat::{instantiate_chat, ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::ai::resolver::resolve_recipe_strict;
use crate::embedding::EmbeddingClient;
use crate::engine::BrainEngine;
use crate::think::cite_render::{resolve_citations, ParsedCitation};
use crate::think::gather::{
    render_pages_block, run_gather, takes_hit_to_take_for_prompt, ThinkGatherOpts, ThinkGatherResult,
    PAGE_EXCERPT_LEN,
};
use crate::think::prompt::{
    build_think_system_prompt, build_think_user_message, ThinkCalibrationBlockOpts,
    ThinkSystemPromptOpts, ThinkUserMessageOpts,
};
use crate::think::sanitize::render_takes_block;
use crate::think::trajectory::{build_trajectory_block, TrajectoryBuildOpts, TrajectoryBuildResult};
use regex::Regex;
use serde_json::Value;
use std::sync::{Arc, LazyLock};

/// Max output tokens for the think synthesis call (TS `DEFAULT_MAX_OUTPUT_TOKENS`).
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4000;
/// Default think model — mirrors TS `resolveModel(..., {tier:'deep', fallback:'opus'})`.
/// `crate::ai::model_config` maps `ModelTier::Deep` to `anthropic:claude-opus-4-7`.
const DEFAULT_THINK_MODEL: &str = "anthropic:claude-opus-4-7";

static TEMPORAL_RX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(when|history|over time|evolved|since|before|after)\b").expect("think temporal rx")
});
static EVENT_RX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(meeting|event|happened)\b").expect("think event rx"));

/// Detect intent for the system prompt. TS `inferIntent`: anchor ⇒ `entity`;
/// temporal keywords ⇒ `temporal`; event keywords ⇒ `event`; else `general`.
pub fn infer_intent(question: &str, anchor: Option<&str>) -> String {
    if anchor.is_some() {
        return "entity".to_string();
    }
    let q = question.to_lowercase();
    if TEMPORAL_RX.is_match(&q) {
        "temporal".to_string()
    } else if EVENT_RX.is_match(&q) {
        "event".to_string()
    } else {
        "general".to_string()
    }
}

/// Strip ``` fences and extract the first JSON object. Mirrors
/// `src/core/think/index.ts:tryParseJSON`.
pub fn try_parse_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    let stripped = if let Some(rest) = trimmed.strip_prefix("```json") {
        rest
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest
    } else {
        trimmed
    }
    .trim_start();
    let stripped = stripped.strip_suffix("```").map(str::trim_end).unwrap_or(stripped);

    if let Ok(v) = serde_json::from_str::<Value>(stripped) {
        return Some(v);
    }
    // Fallback: first `{...}` block (model emitted prose alongside JSON).
    if let (Some(start), Some(end)) = (stripped.find('{'), stripped.rfind('}')) {
        if end > start {
            if let Ok(v) = serde_json::from_str::<Value>(&stripped[start..=end]) {
                return Some(v);
            }
        }
    }
    None
}

/// Structured LLM response (TS `ThinkResponse`). `citations` is kept as raw
/// JSON so it feeds straight into [`resolve_citations`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThinkResponse {
    pub answer: String,
    pub citations: Value,
    pub gaps: Vec<String>,
}

/// Options for [`run_think`].
#[derive(Clone, Default)]
pub struct ThinkSynthesizeOpts {
    pub question: String,
    pub anchor: Option<String>,
    pub rounds: u32,
    pub model: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub will_save: bool,
    /// Calibration mode (v0.36.1.0, D22). When true, the active calibration
    /// profile for `calibration_holder` is retrieved and injected per D22
    /// placement (after retrieval, before question). Off by default
    /// (regression posture); when on but no profile exists, think falls back
    /// to baseline + a `NO_CALIBRATION_PROFILE` warning.
    pub with_calibration: bool,
    /// Holder to read the calibration profile for (default `'garry'`). Only
    /// consulted when `with_calibration` is true.
    pub calibration_holder: Option<String>,
    /// Trajectory injection for temporal / knowledge_update intents
    /// (v0.40.2.0, Eng D1). Default true; a caller may set false to bypass.
    /// The `think.trajectory_enabled` config kill-switch still wins over this.
    pub with_trajectory: bool,
    /// Source scope for the calibration profile read + trajectory queries.
    pub source_id: Option<String>,
    /// Federated source scope for trajectory queries (wins over `source_id`).
    pub allowed_sources: Option<Vec<String>>,
    /// When true, trajectory queries apply `visibility = 'world'` filtering.
    pub remote: bool,
    /// Injected chat provider (test seam). When `None`, resolved from the
    /// model string via [`resolve_think_chat`] (gateway analog).
    pub chat: Option<Arc<dyn ChatProvider>>,
    pub embedding_client: Option<Arc<EmbeddingClient>>,
    pub takes_holders_allow_list: Option<Vec<String>>,
    /// Pre-computed question embedding (enables the vector-takes stream).
    /// **G71**: the vector-takes stream is blocked, so this is accepted but
    /// currently only forwarded to `run_gather` for parity.
    pub question_embedding: Option<Vec<f32>>,
    /// Rerank post-processing settings (G1). When set, the Think gather
    /// pipeline reranks the head of its hybrid-search results through the
    /// same stage the Query operation uses, restoring parity with the TS
    /// `hybridSearch` (which always invokes `applyReranker` for modes that
    /// have reranking enabled). `None` keeps the legacy "no rerank" behavior
    /// (e.g. for tests that don't install a rerank client).
    pub rerank: Option<crate::rerank_client::RerankSettings>,
}

/// Per-call diagnostics for `--explain` / telemetry.
#[derive(Debug, Clone, Default)]
pub struct ThinkDiagnostics {
    pub pages_from_hybrid: usize,
    pub takes_from_keyword: usize,
    pub takes_from_vector: usize,
    pub graph_hits: usize,
}

/// Result of the think pipeline (TS `ThinkResult`).
#[derive(Debug, Clone, Default)]
pub struct ThinkResult {
    pub question: String,
    pub answer: String,
    pub citations: Vec<ParsedCitation>,
    pub gaps: Vec<String>,
    pub pages_gathered: usize,
    pub takes_gathered: usize,
    pub graph_hits: usize,
    pub sources: Vec<String>,
    pub model_used: String,
    pub rounds: u32,
    pub warnings: Vec<String>,
    pub diagnostics: ThinkDiagnostics,
}

/// Build a `ChatProvider` for the given model string, or `None` if it cannot
/// be resolved (unknown provider, missing API key, provider feature
/// disabled). Mirrors TS `tryBuildGatewayClient`'s probe + graceful null.
///
/// The model string may be bare (`claude-opus-4-7`) or `provider:modelId`.
/// Bare ids are treated as Anthropic, matching TS `resolveModel`'s deep-tier
/// default.
pub fn resolve_think_chat(model: &str) -> Option<Arc<dyn ChatProvider>> {
    let model_str = if model.contains(':') {
        model.to_string()
    } else {
        format!("anthropic:{model}")
    };
    let (parsed, recipe) = resolve_recipe_strict(&model_str).ok()?;
    let provider = instantiate_chat(recipe, &parsed.model_id, |env| std::env::var(env).ok()).ok()?;
    Some(Arc::from(provider))
}

/// Full think pipeline. Ports `src/core/think/index.ts:runThink`.
///
/// GATHER → render → prompt → chat → parse → resolve citations → result.
/// On any LLM-unavailable condition, degrades gracefully (mirrors TS: returns
/// a result with `rounds: 0` and a sentinel answer, with gather counts
/// preserved so the caller still sees what was retrieved).
pub async fn run_think(engine: &dyn BrainEngine, opts: &ThinkSynthesizeOpts) -> ThinkResult {
    let rounds = opts.rounds.max(1);
    let mut warnings = Vec::new();

    // Model resolution: CLI override → engine config `models.think` → deep default.
    let model_used = match &opts.model {
        Some(m) => m.clone(),
        None => match engine.get_config("models.think").await {
            Ok(Some(s)) if !s.trim().is_empty() => s,
            _ => DEFAULT_THINK_MODEL.to_string(),
        },
    };

    // GATHER
    let gather: ThinkGatherResult = run_gather(
        engine,
        &ThinkGatherOpts {
            question: opts.question.clone(),
            anchor: opts.anchor.clone(),
            gather_limit: None,
            takes_limit: None,
            graph_depth: None,
            question_embedding: opts.question_embedding.clone(),
            embedding_client: opts.embedding_client.clone(),
            takes_holders_allow_list: opts.takes_holders_allow_list.clone(),
            rerank: opts.rerank.clone(),
        },
    )
    .await;

    let pages_block = render_pages_block(&gather.pages, PAGE_EXCERPT_LEN);
    let takes_for_prompt: Vec<_> = gather.takes.iter().map(takes_hit_to_take_for_prompt).collect();
    let rendered = render_takes_block(&takes_for_prompt);
    if rendered.sanitized_count > 0 {
        warnings.push(format!("SANITIZED_{}_TAKE_CLAIMS", rendered.sanitized_count));
    }
    let graph_block = if !gather.graph_slugs.is_empty() {
        Some(format!(
            "<anchor>{}</anchor>\nReachable: {}",
            opts.anchor.clone().unwrap_or_default(),
            gather
                .graph_slugs
                .iter()
                .take(30)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ))
    } else {
        None
    };

    // v0.36.1.0 (E1) — optional calibration profile retrieval. When enabled
    // and a profile exists, inject it per D22 (after retrieval, before
    // question). When enabled and no profile, fall back to baseline + warn.
    let mut calibration_block_opts: Option<ThinkCalibrationBlockOpts> = None;
    if opts.with_calibration {
        let holder = opts
            .calibration_holder
            .clone()
            .unwrap_or_else(|| "garry".to_string());
        match engine
            .get_calibration_profile(
                &holder,
                opts.source_id.as_deref(),
                opts.allowed_sources.as_deref(),
            )
            .await
        {
            Ok(Some(profile)) => {
                calibration_block_opts = Some(ThinkCalibrationBlockOpts {
                    holder: profile.holder,
                    pattern_statements: profile.pattern_statements,
                    active_bias_tags: profile.active_bias_tags,
                    brier: profile.brier,
                });
            }
            Ok(None) => warnings.push("NO_CALIBRATION_PROFILE".to_string()),
            Err(e) => warnings.push(format!("CALIBRATION_FETCH_FAILED: {e}")),
        }
    }

    // v0.40.2.0 — trajectory injection for temporal / knowledge_update intents.
    // Default ON; the `think.trajectory_enabled` config kill-switch is honored
    // inside `build_trajectory_block`. Best-effort: any error degrades to an
    // empty block + a warning, never failing the think call.
    let retrieved_slugs: Vec<String> = gather.pages.iter().map(|p| p.page.slug.clone()).collect();
    let trajectory: TrajectoryBuildResult = if opts.with_trajectory {
        build_trajectory_block(
            engine,
            &opts.question,
            &retrieved_slugs,
            &TrajectoryBuildOpts {
                source_id: opts.source_id.clone(),
                allowed_sources: opts.allowed_sources.clone(),
                remote: opts.remote,
            },
        )
        .await
    } else {
        TrajectoryBuildResult::default()
    };
    for w in &trajectory.warnings {
        warnings.push(w.clone());
    }

    // SYNTHESIZE
    let intent = infer_intent(&opts.question, opts.anchor.as_deref());
    let system_prompt = build_think_system_prompt(&ThinkSystemPromptOpts {
        intent: Some(intent),
        anchor: opts.anchor.clone(),
        since: opts.since.clone(),
        until: opts.until.clone(),
        will_save: opts.will_save,
        with_calibration: calibration_block_opts.is_some(),
    });
    let user_message = build_think_user_message(&ThinkUserMessageOpts {
        question: &opts.question,
        pages_block: &pages_block,
        takes_block: &rendered.rendered,
        graph_block: graph_block.as_deref(),
        calibration: calibration_block_opts,
        trajectory_block: if trajectory.rendered.is_empty() {
            None
        } else {
            Some(trajectory.rendered.as_str())
        },
    });

    // Resolve the chat provider (injected for tests, or via the gateway analog).
    let chat: Arc<dyn ChatProvider> = match &opts.chat {
        Some(c) => c.clone(),
        None => match resolve_think_chat(&model_used) {
            Some(c) => c,
            None => {
                warnings.push("NO_ANTHROPIC_API_KEY".to_string());
                return graceful_after_gather(
                    opts,
                    model_used,
                    rounds,
                    warnings,
                    &gather,
                    "(no LLM available — set anthropic_api_key via zbrain config or ANTHROPIC_API_KEY env)",
                    "no LLM available; gather succeeded but synthesis skipped",
                );
            }
        },
    };

    let chat_opts = ChatOpts {
        model: Some(model_used.clone()),
        system: Some(system_prompt),
        messages: vec![ChatMessage::text(ChatRole::User, user_message)],
        max_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS),
        cache_system: true,
        ..Default::default()
    };

    let chat_result = match chat.chat(chat_opts).await {
        Ok(r) => r,
        Err(e) => {
            warnings.push(format!("THINK_LLM_FAILED: {e}"));
            return graceful_after_gather(
                opts,
                model_used,
                rounds,
                warnings,
                &gather,
                "(LLM request failed; gather succeeded but synthesis skipped)",
                &format!("LLM request failed: {e}"),
            );
        }
    };

    let parsed = try_parse_json(&chat_result.text);
    let response: ThinkResponse = match parsed {
        Some(v) if v.is_object() => {
            let answer = v.get("answer").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let citations = v.get("citations").cloned().unwrap_or(Value::Null);
            let gaps = v
                .get("gaps")
                .and_then(|g| g.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            ThinkResponse { answer, citations, gaps }
        }
        _ => {
            warnings.push("LLM_OUTPUT_NOT_JSON".to_string());
            ThinkResponse {
                answer: chat_result.text.clone(),
                citations: Value::Null,
                gaps: Vec::new(),
            }
        }
    };

    let resolved = resolve_citations(&response.citations, &response.answer);
    for w in &resolved.warnings {
        warnings.push(w.clone());
    }

    // Round-loop scaffolding (TS rounds>1 currently re-runs without gap-driven
    // retrieval — the v0.29 gap-fill heuristic is a follow-up).
    if rounds > 1 {
        warnings.push("ROUNDS_GT_1_NOT_GAP_DRIVEN".to_string());
    }

    ThinkResult {
        question: opts.question.clone(),
        answer: response.answer,
        citations: resolved.citations,
        gaps: response.gaps,
        pages_gathered: gather.pages.len(),
        takes_gathered: gather.takes.len(),
        graph_hits: gather.graph_slugs.len(),
        sources: gather.pages.iter().map(|p| p.page.slug.clone()).collect(),
        model_used,
        rounds: 1,
        warnings,
        diagnostics: ThinkDiagnostics {
            pages_from_hybrid: gather.diagnostics.pages_from_hybrid,
            takes_from_keyword: gather.diagnostics.takes_from_keyword,
            takes_from_vector: gather.diagnostics.takes_from_vector,
            graph_hits: gather.diagnostics.graph_hits,
        },
    }
}

/// Build a graceful-degradation [`ThinkResult`] after gather has run. Mirrors
/// the TS `runThink` branch that returns the gather without synthesis.
fn graceful_after_gather(
    opts: &ThinkSynthesizeOpts,
    model_used: String,
    _rounds: u32,
    warnings: Vec<String>,
    gather: &ThinkGatherResult,
    answer: &str,
    gap: &str,
) -> ThinkResult {
    ThinkResult {
        question: opts.question.clone(),
        answer: answer.to_string(),
        citations: Vec::new(),
        gaps: vec![gap.to_string()],
        pages_gathered: gather.pages.len(),
        takes_gathered: gather.takes.len(),
        graph_hits: gather.graph_slugs.len(),
        sources: gather.pages.iter().map(|p| p.page.slug.clone()).collect(),
        model_used,
        rounds: 0,
        warnings,
        diagnostics: ThinkDiagnostics {
            pages_from_hybrid: gather.diagnostics.pages_from_hybrid,
            takes_from_keyword: gather.diagnostics.takes_from_keyword,
            takes_from_vector: gather.diagnostics.takes_from_vector,
            graph_hits: gather.diagnostics.graph_hits,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::{ChatResult, ChatUsage, MockChatProvider, StopReason};
    use crate::engine::InMemoryEngine;
    use serde_json::json;

    #[test]
    fn infer_intent_anchor_is_entity() {
        assert_eq!(infer_intent("anything", Some("people/alice")), "entity");
    }
    #[test]
    fn infer_intent_temporal() {
        assert_eq!(infer_intent("when did this happen", None), "temporal");
        assert_eq!(infer_intent("how has it evolved over time", None), "temporal");
    }
    #[test]
    fn infer_intent_event() {
        assert_eq!(infer_intent("which meeting happened last", None), "event");
    }
    #[test]
    fn infer_intent_general() {
        assert_eq!(infer_intent("what is ZBrain", None), "general");
    }

    #[test]
    fn parse_json_strips_fences_and_recovers_block() {
        assert!(try_parse_json("```json\n{\"a\":1}\n```").is_some());
        assert!(try_parse_json("prose before {\"a\":2} trailing").is_some());
        assert!(try_parse_json("no json here at all").is_none());
    }

    #[tokio::test]
    async fn run_think_uses_injected_chat_and_resolves_citations() {
        let answer = json!({
            "answer": "Alice founded the lab [people/alice#1].",
            "citations": [{"page_slug": "people/alice", "row_num": 1, "citation_index": 1}],
            "gaps": ["missing revenue data"]
        })
        .to_string();
        let chat = Arc::new(MockChatProvider::new(answer));
        let engine = InMemoryEngine::default();
        let result = run_think(
            &engine,
            &ThinkSynthesizeOpts {
                question: "who founded the lab?".into(),
                chat: Some(chat),
                model: Some("anthropic:claude-opus-4-7".into()),
                ..Default::default()
            },
        )
        .await;
        assert!(result.answer.contains("Alice founded the lab"));
        assert_eq!(result.citations.len(), 1);
        assert_eq!(result.citations[0].page_slug, "people/alice");
        assert_eq!(result.citations[0].row_num, Some(1));
        assert_eq!(result.gaps, vec!["missing revenue data"]);
        assert_eq!(result.rounds, 1);
        assert!(!result.warnings.iter().any(|w| w == "NO_ANTHROPIC_API_KEY"));
    }

    #[tokio::test]
    async fn run_think_graceful_when_no_chat_and_no_key() {
        // No injected chat + (in this build) no API key / provider feature →
        // resolve_think_chat returns None → graceful degradation.
        let engine = InMemoryEngine::default();
        let result = run_think(
            &engine,
            &ThinkSynthesizeOpts {
                question: "q".into(),
                model: Some("anthropic:claude-opus-4-7".into()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(result.rounds, 0);
        assert!(result.warnings.iter().any(|w| w == "NO_ANTHROPIC_API_KEY"));
        assert!(result.answer.contains("no LLM available"));
    }

    #[tokio::test]
    async fn run_think_falls_back_to_regex_citations_on_missing_structured() {
        // Model returned no `citations` field → resolve_citations falls back to
        // the inline `[slug#row]` markers in the answer body.
        let answer = json!({
            "answer": "Bob left in 2020 [people/bob#2].",
            "gaps": []
        })
        .to_string();
        let chat = Arc::new(MockChatProvider::new(answer));
        let engine = InMemoryEngine::default();
        let result = run_think(
            &engine,
            &ThinkSynthesizeOpts {
                question: "when did Bob leave?".into(),
                chat: Some(chat),
                model: Some("anthropic:claude-opus-4-7".into()),
                ..Default::default()
            },
        )
        .await;
        assert!(result.warnings.iter().any(|w| w == "CITATIONS_REGEX_FALLBACK"));
        assert_eq!(result.citations.len(), 1);
        assert_eq!(result.citations[0].page_slug, "people/bob");
        assert_eq!(result.citations[0].row_num, Some(2));
    }
}
