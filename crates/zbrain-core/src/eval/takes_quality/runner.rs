//! takes-quality-eval/runner — drives the shared cross-modal judge panel and
//! produces a [`TakesQualityReceipt`] (faithful port of TS `runner.ts`).
//!
//! The 4-sha receipt identity (corpus / prompt / models / rubric) is bound so
//! two runs over the same inputs + same rubric produce the same key, while a
//! future rubric or model-set tweak segregates trend rows cleanly. The receipt
//! is written to disk (best-effort artifact) AND persisted to the DB (the
//! durable source of truth via [`write_takes_quality_run`]).

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;

use anyhow::Result;

use crate::engine::BrainEngine;
use crate::eval::cross_modal::{self, ChatRequest, RunEvalOpts, SlotConfig};
use crate::eval::takes_quality::receipt::{
    verdict_to_string, CorpusMeta, DimensionRoll, ReceiptError as TqReceiptError, TakesQualityReceipt,
};
use crate::eval::takes_quality::rubric;
use crate::types::{Take, TakesListOpts};

/// Options for [`run`].
pub struct TakesQualityRunOpts<'a> {
    pub engine: &'a dyn BrainEngine,
    /// Number of takes to sample from the corpus.
    pub sample: usize,
    /// Optional receipt filename slug.
    pub slug: Option<String>,
    /// Override the default 5 takes-quality rubric dimensions.
    pub dimensions: Option<Vec<String>>,
    /// Override the default 3-model judge panel. `None` → `default_slots()`.
    pub slots: Option<Vec<SlotConfig>>,
    /// 1-3 cycles (defaults handled by the judge panel).
    pub cycles: Option<u32>,
    /// Per-call max output tokens for the judge models.
    pub max_tokens: Option<u32>,
    /// Where the judge receipts are written.
    pub receipt_dir: PathBuf,
}

/// Output of a takes-quality run.
#[derive(Debug)]
pub struct TakesQualityRunOutput {
    /// The full takes-quality receipt (also persisted to DB).
    pub receipt: TakesQualityReceipt,
    /// Path of the LAST cross-modal cycle receipt (binds the current sha).
    pub final_receipt_path: String,
    /// How many takes were actually sampled and judged.
    pub n_takes: usize,
}

/// Render sampled takes as the text the judge model sees.
///
/// Faithful to TS `sampleTakesAsText` line format:
/// `- <kind> | holder=<h> | weight=<w> | since=<s> | src=<src>\n  <claim>`
pub fn render_takes(takes: &[Take]) -> String {
    let lines: Vec<String> = takes
        .iter()
        .map(|t| {
            let since = t.since_date.clone().unwrap_or_else(|| "—".to_string());
            let src = t.source.clone().unwrap_or_else(|| "—".to_string());
            format!(
                "- {} | holder={} | weight={} | since={} | src={}\n  {}",
                t.kind, t.holder, t.weight, since, src, t.claim
            )
        })
        .collect();
    lines.join("\n")
}

/// Run the takes-quality evaluation.
///
/// Samples `opts.sample` takes from `opts.engine`, renders them, builds the
/// 5-dimension judge prompt, and runs the shared cross-modal judge panel. The
/// resulting [`cross_modal::AggregateResult`] is mapped into a
/// [`TakesQualityReceipt`], which is written to disk and persisted to the DB.
///
/// Honest degradation: returns `Err` if the corpus is empty (no fake PASS),
/// and propagates judge-provider failures from the injected `chat` closure.
pub async fn run<F, Fut>(opts: &TakesQualityRunOpts<'_>, chat: &F) -> Result<TakesQualityRunOutput>
where
    F: Fn(ChatRequest) -> Fut,
    Fut: Future<Output = Result<String>>,
{
    let takes = opts
        .engine
        .list_takes(&TakesListOpts {
            limit: Some(opts.sample as u32),
            ..Default::default()
        })
        .await?;
    let n_takes = takes.len();
    if n_takes == 0 {
        anyhow::bail!(
            "eval-takes-quality: no takes to evaluate (empty corpus). Seed takes first (e.g. `zbrain extract takes`)."
        );
    }

    let output = render_takes(&takes);

    // 4-sha receipt identity components.
    let corpus_sha8 = crate::eval::takes_quality::receipt::corpus_sha8(&output);
    let (prompt, prompt_sha8) = rubric::render_judge_prompt(&output);
    let slots = opts.slots.clone().unwrap_or_else(cross_modal::default_slots);
    let models: Vec<String> = slots.iter().map(|s| s.model.clone()).collect();
    let models_sha8 = crate::eval::takes_quality::receipt::model_set_sha8(&models);
    let rubric_sha8 = rubric::rubric_sha8();

    let dimensions = opts
        .dimensions
        .clone()
        .unwrap_or_else(rubric::default_dimensions);
    let cycles = opts.cycles.unwrap_or(1);
    let max_tokens = opts.max_tokens.unwrap_or(4000);

    let eval_opts = RunEvalOpts {
        task: prompt,
        output,
        slug: opts.slug.clone(),
        dimensions: Some(dimensions),
        slots: Some(slots.clone()),
        cycles: opts.cycles,
        receipt_dir: opts.receipt_dir.clone(),
        max_tokens: opts.max_tokens,
        on_progress: None,
    };

    let res = cross_modal::run_eval(&eval_opts, chat).await?;
    let agg = &res.final_aggregate;

    // Map the cross-modal aggregate into the takes-quality receipt shape.
    let mut scores: BTreeMap<String, DimensionRoll> = BTreeMap::new();
    for (dim, roll) in &agg.dimensions {
        scores.insert(dim.clone(), DimensionRoll::from_cross(roll));
    }

    let verdict = verdict_to_string(&agg.verdict);
    let improvements = if agg.top_improvements.is_empty() {
        None
    } else {
        Some(agg.top_improvements.clone())
    };
    let errors = if agg.errors.is_empty() {
        None
    } else {
        Some(
            agg.errors
                .iter()
                .map(|e| TqReceiptError {
                    model_id: e.model_id.clone(),
                    error: e.error.clone(),
                })
                .collect(),
        )
    };
    let successes_per_cycle: Vec<usize> = res
        .cycles
        .iter()
        .map(|c| c.slots.iter().filter(|s| s.ok).count())
        .collect();
    let cycles_run = res.cycles.len();

    let cost = cross_modal::estimate_cost(&slots, cycles, max_tokens);
    let cost_usd = cost.per_run_max_usd;

    let ts = chrono::Utc::now().to_rfc3339();
    let corpus = CorpusMeta {
        source: opts
            .slug
            .clone()
            .unwrap_or_else(|| "takes-corpus".to_string()),
        n_takes,
        slug_prefix: opts.slug.clone(),
        corpus_sha8: corpus_sha8.clone(),
    };

    let receipt = TakesQualityReceipt {
        schema_version: crate::eval::takes_quality::receipt::RECEIPT_SCHEMA_VERSION,
        ts,
        rubric_version: rubric::RUBRIC_VERSION.to_string(),
        rubric_sha8,
        corpus,
        prompt_sha8,
        models_sha8,
        models,
        cycles_run,
        successes_per_cycle,
        verdict,
        scores,
        overall_score: agg.overall,
        cost_usd,
        improvements,
        errors,
        verdict_message: Some(agg.verdict_message.clone()),
    };

    // Best-effort disk artifact; the DB row is authoritative.
    crate::eval::takes_quality::receipt::write_receipt_artifact(&receipt, &opts.receipt_dir);
    // Persist to DB (idempotent on the 4-sha unique key).
    crate::eval::takes_quality::receipt::write_takes_quality_run(opts.engine, &receipt).await?;

    Ok(TakesQualityRunOutput {
        receipt,
        final_receipt_path: res.final_receipt_path,
        n_takes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;
    use crate::types::Take;

    /// A fake judge returning valid 1-10 scores on the 5 takes-quality rubric
    /// dims, so the full runner can be exercised without an API key.
    async fn fake_chat(_req: ChatRequest) -> Result<String> {
        Ok(
            r#"{"scores":{"accuracy":{"score":9,"feedback":"good"},"attribution":{"score":8,"feedback":"ok"},"weight_calibration":{"score":9,"feedback":"clear"},"kind_classification":{"score":8,"feedback":"ok"},"signal_density":{"score":7,"feedback":"ok"}},"overall":8.2,"improvements":["tighten claim 3"]}"#
                .to_string(),
        )
    }

    fn mk_take(id: u64, claim: &str) -> Take {
        Take {
            id,
            page_id: 1,
            row_num: id as i32,
            claim: claim.to_string(),
            kind: "bet".to_string(),
            holder: "alice".to_string(),
            weight: 0.7,
            since_date: None,
            until_date: None,
            source: Some("note".to_string()),
            superseded_by: None,
            active: true,
            resolved_at: None,
            resolved_quality: None,
            resolved_outcome: None,
            resolved_evidence: None,
            resolved_value: None,
            resolved_unit: None,
            resolved_by: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[tokio::test]
    async fn run_scores_sampled_takes_without_api_key() {
        let engine = InMemoryEngine::new();
        for i in 0..5u64 {
            engine.add_take(mk_take(
                i,
                &format!("take {i}: markets are efficient on average over long horizons"),
            ));
        }
        let receipt_dir = std::env::temp_dir().join(format!("tq_test_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = TakesQualityRunOpts {
            engine: &engine,
            sample: 5,
            slug: Some("test-tq".to_string()),
            dimensions: None,
            slots: None,
            cycles: Some(1),
            max_tokens: Some(2000),
            receipt_dir,
        };

        let res = run(&opts, &fake_chat).await.unwrap();
        assert_eq!(res.n_takes, 5);
        // The 5 rubric dims all scored >= 7 → verdict must be pass.
        assert_eq!(res.receipt.verdict, "pass");
        assert_eq!(res.receipt.scores.len(), 5);
        assert!(res.receipt.overall_score.unwrap() > 7.0);
    }

    #[tokio::test]
    async fn run_errors_on_empty_corpus() {
        let engine = InMemoryEngine::new();
        let receipt_dir = std::env::temp_dir().join(format!("tq_test_empty_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = TakesQualityRunOpts {
            engine: &engine,
            sample: 10,
            slug: None,
            dimensions: None,
            slots: None,
            cycles: Some(1),
            max_tokens: Some(2000),
            receipt_dir,
        };

        let err = run(&opts, &fake_chat).await.unwrap_err();
        assert!(err.to_string().contains("no takes to evaluate"));
    }
}
