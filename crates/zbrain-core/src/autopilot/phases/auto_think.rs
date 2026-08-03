//! Auto-think dream phase (v0.28).
//!
//! Port of TS `src/core/cycle/auto-think.ts`. Reads
//! `dream.auto_think.questions[]` from the engine config store, runs the
//! think pipeline on each question, and persists the result as a synthesis
//! page when `auto_commit=true`. Capped by `max_per_cycle` and the
//! [`BudgetMeter`]'s USD cap.
//!
//! Cooldown: `dream.auto_think.last_completion_ts` is written ONLY when at
//! least one synthesis completed (and not in dry-run), so retries after
//! partial failures pick back up.
//!
//! Default-disabled. Operator opts in:
//! ```text
//! zbrain config set dream.auto_think.enabled true
//! zbrain config set dream.auto_think.questions '["What patterns ...","Who ..."]'
//! ```
//!
//! ## Rust deviations (documented so the port stays honest)
//!
//! - **LLM seam**: TS routes each question through `runThink` with a
//!   `ThinkLLMClient` (gateway-backed). The Rust cycle wiring hands phases a
//!   [`ChatProvider`], and `ThinkOperation::execute` needs an
//!   `OperationContext` holding `Arc<dyn BrainEngine>` — an ownership shape a
//!   `&dyn BrainEngine` phase cannot build. Instead this phase inlines the
//!   think pipeline: `search_pages` retrieval → [`ThinkPromptBuilder`] prompt
//!   → `chat()` → `parse_response` → `resolve_citations`. Same prompt, same
//!   parse, same citation contract as `ThinkOperation` — only the transport
//!   differs (mirrors how `synthesize.rs`/`grade_takes.rs` call the provider
//!   directly).
//! - **Model resolution**: TS `resolveModel` is async against the engine.
//!   Rust [`resolve_model`] is sync over a [`ConfigLookup`] snapshot, so we
//!   pre-fetch the relevant `models.*` keys into a `HashMap` first
//!   (`prefetch_model_lookup`). The deprecated key `dream.auto_think.model`
//!   is deliberately dropped (matches the Rust-wide `deprecated_config_key`
//!   removal in `model_config.rs`).
//! - **Budget**: TS `BudgetMeter.check` → Rust `BudgetMeter::check`
//!   (estimate-based gate; unpriced models are warn-once allowed, projected
//!   spend over cap is denied; cumulative cost is reported via `total_spent`).

use std::collections::HashMap;

use chrono::Utc;

use crate::ai::chat::{ChatMessage, ChatOpts, ChatProvider, ChatRole};
use crate::ai::model_config::{resolve_model, ModelTier, ResolveModelOpts};
use crate::autopilot::budget_meter::{BudgetMeter, BudgetMeterOpts, SubmitEstimate};
use crate::engine::{BrainEngine, GetPageOpts, PageInput, SearchOpts, SynthesisEvidenceInput};
use crate::llm::{Citation, ThinkPromptBuilder};
use crate::Result;

/// Options for [`run_phase_auto_think`]. Mirrors TS `AutoThinkPhaseOpts`
/// (the TS `client` injection seam becomes the `chat` parameter).
pub struct AutoThinkPhaseOpts {
    /// Brain directory (accepted for parity with other phases; the Rust port
    /// is DB-canonical and does not reverse-write synthesis pages to disk).
    pub brain_dir: Option<String>,
    /// If true, gate/budget-check each question but call no LLM and write
    /// nothing (mirrors TS `--dry-run`).
    pub dry_run: bool,
    /// Override the budget audit-ledger dir (tests). `None` → temp dir.
    pub audit_dir: Option<std::path::PathBuf>,
    /// CLI `--model` override. Highest precedence in the resolve chain.
    pub model_override: Option<String>,
}

impl Default for AutoThinkPhaseOpts {
    fn default() -> Self {
        Self {
            brain_dir: None,
            dry_run: false,
            audit_dir: None,
            model_override: None,
        }
    }
}

/// Per-question outcome. Mirrors the TS inline `results[]` records.
#[derive(Debug, Clone)]
pub struct QuestionOutcome {
    pub question: String,
    /// `"complete" | "budget_exhausted" | "dry_run" | "failed"`.
    pub status: String,
    /// Saved synthesis slug (auto_commit path only).
    pub slug: Option<String>,
    pub warnings: Vec<String>,
}

/// Result of an `auto_think` run. Mirrors TS `DreamPhaseResult`.
#[derive(Debug, Clone, Default)]
pub struct AutoThinkPhaseResult {
    /// `"complete" | "partial" | "failed" | "skipped"`.
    pub status: String,
    pub detail: String,
    pub reason: Option<String>,
    pub questions_run: u64,
    pub synthesized: u64,
    pub dry_run: bool,
    pub outcomes: Vec<QuestionOutcome>,
    pub duration_ms: u64,
}

impl AutoThinkPhaseResult {
    fn skipped(reason: &str, detail: &str) -> Self {
        Self {
            status: "skipped".into(),
            detail: detail.into(),
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

/// In-process auto-think config. Defaults mirror TS `loadConfig`.
struct AutoThinkConfig {
    enabled: bool,
    questions: Vec<String>,
    max_per_cycle: usize,
    budget_usd: f64,
    cooldown_days: i64,
    auto_commit: bool,
}

/// Load config from the engine config store. Mirrors TS `loadConfig`
/// (auto-think.ts:55): 6 keys under `dream.auto_think.*`, malformed values
/// fall back to defaults.
async fn load_config(engine: &dyn BrainEngine) -> Result<AutoThinkConfig> {
    let enabled_raw = engine.get_config("dream.auto_think.enabled").await?;
    let questions_raw = engine.get_config("dream.auto_think.questions").await?;
    let max_per_raw = engine.get_config("dream.auto_think.max_per_cycle").await?;
    let budget_raw = engine.get_config("dream.auto_think.budget").await?;
    let cooldown_raw = engine.get_config("dream.auto_think.cooldown_days").await?;
    let auto_commit_raw = engine.get_config("dream.auto_think.auto_commit").await?;

    // Questions: JSON string array; non-arrays and non-string members are
    // silently dropped (mirrors TS `parsed.filter(q => typeof q === 'string')`).
    let questions = questions_raw
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| match v {
            serde_json::Value::Array(items) => Some(
                items
                    .into_iter()
                    .filter_map(|x| match x {
                        serde_json::Value::String(q) => Some(q),
                        _ => None,
                    })
                    .collect::<Vec<String>>(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    // TS: `maxPerStr ? Math.max(1, parseInt(maxPerStr,10) || 5) : 5`.
    let max_per_cycle = max_per_raw
        .map(|s| {
            let n = s.trim().parse::<i64>().unwrap_or(0);
            let n = if n == 0 { 5 } else { n };
            n.max(1) as usize
        })
        .unwrap_or(5);

    // TS: `budgetStr ? Math.max(0, parseFloat(budgetStr) || 2.0) : 2.0`.
    let budget_usd = budget_raw
        .map(|s| {
            let f = s.trim().parse::<f64>().unwrap_or(0.0);
            let f = if f == 0.0 { 2.0 } else { f };
            f.max(0.0)
        })
        .unwrap_or(2.0);

    // TS: `cooldownStr ? Math.max(0, parseInt(cooldownStr,10) || 30) : 30`.
    let cooldown_days = cooldown_raw
        .map(|s| {
            let n = s.trim().parse::<i64>().unwrap_or(0);
            let n = if n == 0 { 30 } else { n };
            n.max(0)
        })
        .unwrap_or(30);

    Ok(AutoThinkConfig {
        enabled: enabled_raw.as_deref() == Some("true"),
        questions,
        max_per_cycle,
        budget_usd,
        cooldown_days,
        auto_commit: auto_commit_raw.as_deref() == Some("true"),
    })
}

/// Cooldown gate. Mirrors TS `isCoolingDown` (auto-think.ts:81): reads
/// `dream.auto_think.last_completion_ts`; unparseable timestamps → not
/// cooling down.
async fn is_cooling_down(engine: &dyn BrainEngine, days: i64) -> Result<bool> {
    if days <= 0 {
        return Ok(false);
    }
    let Some(last) = engine
        .get_config("dream.auto_think.last_completion_ts")
        .await?
    else {
        return Ok(false);
    };
    let Ok(last_ts) = chrono::DateTime::parse_from_rfc3339(last.trim()) else {
        return Ok(false);
    };
    let elapsed_ms = Utc::now().timestamp_millis() - last_ts.timestamp_millis();
    Ok(elapsed_ms < days * 86_400_000)
}

/// Pre-fetch the config keys [`resolve_model`] walks for the auto-think
/// chain into a sync [`ConfigLookup`] snapshot. Covers: the phase key,
/// `models.default`, the deep-tier override, and one level of user aliases
/// for whichever raw values we saw (alias chains deeper than the prefetched
/// keys fall back to built-in aliases, which need no config).
///
/// `pub` so the CLI `auto-think` subcommand can resolve the same model the
/// phase will use, then build a matching [`ChatProvider`] (the phase itself
/// stays transport-agnostic and re-resolves internally).
pub async fn prefetch_model_lookup(engine: &dyn BrainEngine) -> Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    let mut raw_values: Vec<String> = Vec::new();
    for key in ["models.auto_think", "models.default", "models.tier.deep"] {
        if let Some(v) = engine.get_config(key).await? {
            raw_values.push(v.trim().to_string());
            map.insert(key.to_string(), v);
        }
    }
    // One level of user-defined alias indirection for observed values plus
    // the well-known fallback names.
    let mut alias_names: Vec<String> = raw_values;
    alias_names.extend(["opus", "sonnet", "haiku"].map(String::from));
    for name in alias_names {
        let key = format!("models.aliases.{name}");
        if map.contains_key(&key) {
            continue;
        }
        if let Some(v) = engine.get_config(&key).await? {
            map.insert(key, v);
        }
    }
    Ok(map)
}

/// One think round for a single question: retrieval → prompt → chat →
/// parse → citations. Inlines the `ThinkOperation::execute` pipeline over a
/// [`ChatProvider`] transport (see module docs).
struct ThinkRound {
    answer: String,
    warnings: Vec<String>,
    citations: Vec<Citation>,
    usage_input: u64,
    usage_output: u64,
    pages_gathered: u64,
}

async fn run_think_round(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    question: &str,
    model_id: &str,
) -> std::result::Result<ThinkRound, String> {
    let mut warnings = Vec::new();

    // Retrieval (mirrors ThinkOperation::execute phase 2).
    let keywords = crate::operation::extract_keywords(question);
    let mut builder = ThinkPromptBuilder::new(question);
    let mut pages_gathered = 0u64;
    if !keywords.is_empty() {
        let results = engine
            .search_pages(&SearchOpts {
                keywords,
                limit: Some(5),
                min_score: Some(0.1),
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        pages_gathered = results.len() as u64;
        for r in &results {
            let snippet = r.snippet.clone().unwrap_or_else(|| r.page.title.clone());
            builder.add_context(snippet, r.page.slug.clone());
        }
    }

    // LLM call over the ChatProvider transport.
    let result = chat
        .chat(ChatOpts {
            model: Some(model_id.to_string()),
            system: Some(builder.build_system_prompt()),
            messages: vec![ChatMessage::text(
                ChatRole::User,
                builder.build_user_prompt(),
            )],
            max_tokens: Some(4_000),
            cache_system: false,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;

    // Parse + citation resolution (mirrors ThinkOperation::execute phase 3).
    let answer = match ThinkPromptBuilder::parse_response(&result.text) {
        Ok(parsed) => {
            warnings.extend(parsed.warnings);
            let resolved = crate::llm::resolve_citations(&parsed.citations, &parsed.answer);
            warnings.extend(resolved.warnings);
            return Ok(ThinkRound {
                answer: parsed.answer,
                warnings,
                citations: resolved.citations,
                usage_input: result.usage.input_tokens,
                usage_output: result.usage.output_tokens,
                pages_gathered,
            });
        }
        Err(e) => {
            warnings.push(format!("Failed to parse LLM response: {e}"));
            format!(
                "LLM response could not be parsed. Raw content: {}",
                result.text
            )
        }
    };
    Ok(ThinkRound {
        answer,
        warnings,
        citations: Vec::new(),
        usage_input: result.usage.input_tokens,
        usage_output: result.usage.output_tokens,
        pages_gathered,
    })
}

/// Persist citations into `synthesis_evidence`. Port of TS
/// `persistCitations` (think/index.ts:169): page-level citations
/// (`row_num == None`) are NOT persisted; slugs missing from the brain are
/// skipped with a `CITATION_PAGE_NOT_IN_BRAIN` warning.
async fn persist_citations(
    engine: &dyn BrainEngine,
    synthesis_page_id: i64,
    citations: &[Citation],
) -> Result<(u64, Vec<String>)> {
    let mut warnings = Vec::new();
    let mut slug_to_page_id: HashMap<String, i64> = HashMap::new();
    for c in citations {
        if c.row_num.is_none() || slug_to_page_id.contains_key(&c.page_slug) {
            continue;
        }
        if let Some(page) = engine
            .get_page(&c.page_slug, &GetPageOpts::default())
            .await?
        {
            slug_to_page_id.insert(c.page_slug.clone(), page.id as i64);
        }
    }
    let mut inputs: Vec<SynthesisEvidenceInput> = Vec::new();
    for c in citations {
        let Some(row_num) = c.row_num else { continue };
        let Some(&page_id) = slug_to_page_id.get(&c.page_slug) else {
            warnings.push(format!(
                "CITATION_PAGE_NOT_IN_BRAIN: {}#{row_num}",
                c.page_slug
            ));
            continue;
        };
        inputs.push(SynthesisEvidenceInput {
            synthesis_page_id,
            take_page_id: page_id,
            take_row_num: Some(row_num),
            citation_index: c.citation_index,
        });
    }
    if inputs.is_empty() {
        return Ok((0, warnings));
    }
    let inserted = engine.add_synthesis_evidence(&inputs).await?;
    Ok((inserted, warnings))
}

/// Slugify a question for the synthesis page slug. Mirrors TS
/// `persistSynthesis` (think/index.ts:491): lowercase, strip
/// non-`[a-z0-9\s]`, collapse whitespace to `-`, cap 60 chars,
/// `"untitled"` when empty.
fn slug_safe(question: &str) -> String {
    let lowered = question.to_lowercase();
    let stripped: String = lowered
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c.is_whitespace())
        .collect();
    let joined = stripped.split_whitespace().collect::<Vec<_>>().join("-");
    let capped: String = joined.chars().take(60).collect();
    // TS slices BEFORE the empty check (`.slice(0,60) || 'untitled'`), and a
    // 60-char slice of a non-empty string is non-empty, so order matches.
    if capped.is_empty() {
        "untitled".to_string()
    } else {
        capped
    }
}

/// Persist a synthesis page + its evidence. Returns the saved slug. Port of
/// TS `persistSynthesis` (think/index.ts:486).
async fn persist_synthesis(
    engine: &dyn BrainEngine,
    question: &str,
    round: &ThinkRound,
    model_used: &str,
) -> Result<(String, u64, Vec<String>)> {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let slug = format!("synthesis/{}-{}", slug_safe(question), today);

    // Markdown body. The TS body includes a `## Gaps` section from
    // `result.gaps`; the Rust think pipeline has no gaps field (llm.rs
    // ThinkOutput carries warnings instead), so the body is title + answer.
    let body = format!("# {question}\n\n{}", round.answer);

    let title: String = question.chars().take(200).collect();
    let page = engine
        .put_page(
            &slug,
            None,
            &PageInput {
                page_type: "synthesis".to_string(),
                title,
                compiled_truth: body,
                frontmatter: Some(serde_json::json!({
                    "type": "synthesis",
                    "question": question,
                    "model": model_used,
                    "date": today,
                    "pages_gathered": round.pages_gathered,
                })),
                ..Default::default()
            },
        )
        .await?;

    let (inserted, warnings) = persist_citations(engine, page.id as i64, &round.citations).await?;
    Ok((slug, inserted, warnings))
}

/// Run the auto-think phase. Mirrors TS `runPhaseAutoThink`
/// (auto-think.ts:94).
pub async fn run_phase_auto_think(
    engine: &dyn BrainEngine,
    chat: Option<&dyn ChatProvider>,
    opts: &AutoThinkPhaseOpts,
) -> Result<AutoThinkPhaseResult> {
    let start = std::time::Instant::now();
    let config = load_config(engine).await?;

    if !config.enabled {
        return Ok(AutoThinkPhaseResult::skipped(
            "not_configured",
            "dream.auto_think.enabled is false",
        ));
    }
    if config.questions.is_empty() {
        return Ok(AutoThinkPhaseResult::skipped(
            "no_questions",
            "dream.auto_think.questions is empty",
        ));
    }
    if is_cooling_down(engine, config.cooldown_days).await? {
        return Ok(AutoThinkPhaseResult::skipped(
            "cooldown_active",
            &format!(
                "auto_think cooled down ({}d cooldown)",
                config.cooldown_days
            ),
        ));
    }

    let meter = BudgetMeter::new(BudgetMeterOpts {
        budget_usd: config.budget_usd,
        phase: "auto_think".to_string(),
        audit_dir: opts.audit_dir.clone().unwrap_or_else(std::env::temp_dir),
        audit_path: None,
    });

    // Model resolution over a pre-fetched config snapshot (see module docs).
    let lookup = prefetch_model_lookup(engine).await?;
    let model_id = resolve_model(
        &lookup,
        &ResolveModelOpts {
            cli_flag: opts.model_override.clone(),
            config_key: Some("models.auto_think".to_string()),
            tier: Some(ModelTier::Deep),
            fallback: "opus".to_string(),
            ..Default::default()
        },
    );

    let limit = config.questions.len().min(config.max_per_cycle);
    let mut outcomes: Vec<QuestionOutcome> = Vec::new();

    for q in config.questions.iter().take(limit) {
        // Pre-check budget for the planned synthesize call. Estimate ~5K
        // input tokens (system + ~30 takes + 20 page chunks), 4K output cap.
        let label: String = q.chars().take(40).collect();
        // Budget gate via BudgetMeter (mirrors TS `BudgetMeter.check`):
        // unpriced model → warped-once allow; projected > cap → deny.
        let check = meter.check(&SubmitEstimate {
            model_id: model_id.clone(),
            estimated_input_tokens: 5_000,
            max_output_tokens: 4_000,
            label: Some(format!("auto_think:{label}")),
        });
        if !check.allowed {
            outcomes.push(QuestionOutcome {
                question: q.clone(),
                status: "budget_exhausted".into(),
                slug: None,
                warnings: Vec::new(),
            });
            break;
        }

        if opts.dry_run {
            outcomes.push(QuestionOutcome {
                question: q.clone(),
                status: "dry_run".into(),
                slug: None,
                warnings: Vec::new(),
            });
            continue;
        }

        let Some(chat) = chat else {
            outcomes.push(QuestionOutcome {
                question: q.clone(),
                status: "failed".into(),
                slug: None,
                warnings: vec!["no chat provider wired".into()],
            });
            continue;
        };

        match run_think_round(engine, chat, q, &model_id).await {
            Ok(round) => {
                // BudgetMeter tracks cumulative spend via `check` (estimate-based,
                // mirroring TS where actual usage is recorded by gateway hooks);
                // no separate `record` call is needed here.
                let mut warnings = round.warnings.clone();
                let mut slug = None;
                let mut failed = false;
                if config.auto_commit {
                    match persist_synthesis(engine, q, &round, &model_id).await {
                        Ok((s, _inserted, persist_warnings)) => {
                            warnings.extend(persist_warnings);
                            slug = Some(s);
                        }
                        Err(e) => {
                            failed = true;
                            warnings.push(e.to_string());
                        }
                    }
                }
                outcomes.push(QuestionOutcome {
                    question: q.clone(),
                    status: if failed { "failed" } else { "complete" }.into(),
                    slug,
                    warnings,
                });
            }
            Err(e) => {
                outcomes.push(QuestionOutcome {
                    question: q.clone(),
                    status: "failed".into(),
                    slug: None,
                    warnings: vec![e],
                });
            }
        }
    }

    // Update cooldown timestamp ONLY when at least one synthesis completed.
    let any_complete = outcomes.iter().any(|r| r.status == "complete");
    if any_complete && !opts.dry_run {
        engine
            .set_config(
                "dream.auto_think.last_completion_ts",
                &Utc::now().to_rfc3339(),
            )
            .await?;
    }

    let synthesized = outcomes.iter().filter(|r| r.status == "complete").count() as u64;
    let budget_skipped = outcomes
        .iter()
        .filter(|r| r.status == "budget_exhausted")
        .count();
    let failed = outcomes.iter().filter(|r| r.status == "failed").count();
    let detail = format!(
        "{synthesized} synthesized, {budget_skipped} skipped (budget), {failed} failed. \
         Cumulative cost: ${:.4} / ${:.2}",
        meter.total_spent(),
        config.budget_usd
    );

    let status = if any_complete {
        "complete"
    } else if outcomes.is_empty() {
        "skipped"
    } else {
        "partial"
    };

    Ok(AutoThinkPhaseResult {
        status: status.into(),
        detail,
        reason: None,
        questions_run: outcomes.len() as u64,
        synthesized,
        dry_run: opts.dry_run,
        outcomes,
        duration_ms: u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use crate::engine::InMemoryEngine;

    fn think_json(answer: &str) -> String {
        serde_json::json!({
            "answer": answer,
            "warnings": [],
            "evidence_used": 0,
            "sources": [],
            "citations": [],
        })
        .to_string()
    }

    async fn enable(engine: &InMemoryEngine, questions: &str) {
        engine
            .set_config("dream.auto_think.enabled", "true")
            .await
            .unwrap();
        engine
            .set_config("dream.auto_think.questions", questions)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn skips_when_not_enabled() {
        let engine = InMemoryEngine::new();
        let r = run_phase_auto_think(&engine, None, &AutoThinkPhaseOpts::default())
            .await
            .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("not_configured"));
    }

    #[tokio::test]
    async fn skips_when_no_questions() {
        let engine = InMemoryEngine::new();
        engine
            .set_config("dream.auto_think.enabled", "true")
            .await
            .unwrap();
        let r = run_phase_auto_think(&engine, None, &AutoThinkPhaseOpts::default())
            .await
            .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("no_questions"));
    }

    #[tokio::test]
    async fn skips_when_cooling_down() {
        let engine = InMemoryEngine::new();
        enable(&engine, r#"["q1"]"#).await;
        engine
            .set_config(
                "dream.auto_think.last_completion_ts",
                &Utc::now().to_rfc3339(),
            )
            .await
            .unwrap();
        let r = run_phase_auto_think(&engine, None, &AutoThinkPhaseOpts::default())
            .await
            .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("cooldown_active"));
    }

    #[tokio::test]
    async fn dry_run_calls_no_llm_and_writes_no_cooldown() {
        let engine = InMemoryEngine::new();
        enable(&engine, r#"["q1","q2"]"#).await;
        let opts = AutoThinkPhaseOpts {
            dry_run: true,
            ..Default::default()
        };
        let r = run_phase_auto_think(&engine, None, &opts).await.unwrap();
        assert_eq!(r.status, "partial"); // no completes, non-empty results
        assert_eq!(r.questions_run, 2);
        assert!(r.outcomes.iter().all(|o| o.status == "dry_run"));
        let ts = engine
            .get_config("dream.auto_think.last_completion_ts")
            .await
            .unwrap();
        assert!(ts.is_none());
    }

    #[tokio::test]
    async fn complete_run_sets_cooldown_and_persists_when_auto_commit() {
        let engine = InMemoryEngine::new();
        enable(&engine, r#"["What patterns recur?"]"#).await;
        engine
            .set_config("dream.auto_think.auto_commit", "true")
            .await
            .unwrap();
        let chat = MockChatProvider::new(think_json("The pattern is X."));
        let r = run_phase_auto_think(&engine, Some(&chat), &AutoThinkPhaseOpts::default())
            .await
            .unwrap();
        assert_eq!(r.status, "complete", "detail: {}", r.detail);
        assert_eq!(r.synthesized, 1);
        let slug = r.outcomes[0].slug.clone().expect("slug persisted");
        assert!(slug.starts_with("synthesis/what-patterns-recur-"));
        // Page landed in the engine.
        let page = engine
            .get_page(&slug, &GetPageOpts::default())
            .await
            .unwrap()
            .expect("synthesis page saved");
        assert!(page.compiled_truth.contains("The pattern is X."));
        // Cooldown timestamp written.
        let ts = engine
            .get_config("dream.auto_think.last_completion_ts")
            .await
            .unwrap();
        assert!(ts.is_some());
    }

    #[tokio::test]
    async fn max_per_cycle_truncates() {
        let engine = InMemoryEngine::new();
        enable(&engine, r#"["q1","q2","q3"]"#).await;
        engine
            .set_config("dream.auto_think.max_per_cycle", "2")
            .await
            .unwrap();
        let chat = MockChatProvider::new(think_json("a"));
        let r = run_phase_auto_think(&engine, Some(&chat), &AutoThinkPhaseOpts::default())
            .await
            .unwrap();
        assert_eq!(r.questions_run, 2);
    }

    #[test]
    fn slug_safe_mirrors_ts() {
        assert_eq!(slug_safe("What patterns recur?"), "what-patterns-recur");
        assert_eq!(slug_safe("你好 世界"), "untitled"); // non-ascii stripped
        assert_eq!(slug_safe("  A  B  "), "a-b");
        let long = "word ".repeat(30);
        assert!(slug_safe(&long).chars().count() <= 60);
    }
}
