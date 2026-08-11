//! Rust port of TS `src/core/takes-quality-eval/runner.ts` — MVP.
//!
//! Samples takes from a [`BrainEngine`], renders them as text, and runs the
//! shared cross-modal judge panel (3-model parallel, see
//! [`crate::eval::cross_modal::run_eval`]) to score their quality. The judge
//! panel is intentionally reused (not re-ported) so every eval family shares
//! one verdict/aggregation semantics.
//!
//! Honest degradation: the caller is responsible for supplying a working
//! `chat` closure (typically resolved from API keys at the CLI layer). When
//! the corpus is empty, [`run`] returns `Err` rather than a fake PASS.

use std::path::PathBuf;

use anyhow::Result;

use crate::engine::BrainEngine;
use crate::eval::cross_modal::{self, AggregateResult, ChatRequest, RunEvalOpts};
use crate::types::{Take, TakesListOpts};

/// Default rubric dimensions for the takes-quality judge (MVP).
///
/// Faithful to the TS `takes-quality-eval` rubric's four quality axes, mapped
/// onto the generic 1-10 scoring the cross-modal judge panel understands.
pub fn default_dimensions() -> Vec<String> {
    vec![
        "insight".to_string(),
        "accuracy".to_string(),
        "clarity".to_string(),
        "actionability".to_string(),
    ]
}

/// Options for [`run`].
pub struct TakesQualityOpts<'a> {
    pub engine: &'a dyn BrainEngine,
    /// Number of takes to sample from the corpus.
    pub sample: usize,
    /// Override the default takes-quality rubric dimensions.
    pub dimensions: Option<Vec<String>>,
    /// 1-3 cycles (defaults handled by the judge panel).
    pub cycles: Option<u32>,
    /// Per-call max output tokens for the judge models.
    pub max_tokens: Option<u32>,
    /// Where the judge receipts are written.
    pub receipt_dir: PathBuf,
    /// Optional slug for receipt naming.
    pub slug: Option<String>,
}

/// Result of a takes-quality run.
#[derive(Debug)]
pub struct TakesQualityResult {
    pub final_aggregate: AggregateResult,
    pub final_receipt_path: String,
    /// How many takes were actually sampled and judged.
    pub n_takes: usize,
}

/// Render sampled takes as the text the judge model sees.
///
/// Faithful to TS `sampleTakesAsText` line format:
/// `- <kind> | holder=<h> | weight=<w> | since=<s> | src=<src>\n  <claim>`
fn render_takes(takes: &[Take]) -> String {
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
/// Samples `opts.sample` takes from `opts.engine`, renders them, and runs the
/// shared cross-modal judge panel. Honest degradation: returns `Err` if the
/// corpus is empty.
pub async fn run<F, Fut>(opts: &TakesQualityOpts<'_>, chat: &F) -> Result<TakesQualityResult>
where
    F: Fn(ChatRequest) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
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
    let task = "Evaluate the quality of these extracted knowledge-base 'takes' \
        (key insights / bets). Each take is a claim carrying metadata \
        (kind, holder, weight, since-date, source)."
        .to_string();

    let eval_opts = RunEvalOpts {
        task,
        output,
        slug: opts.slug.clone(),
        dimensions: opts.dimensions.clone(),
        slots: None, // default 3-model panel from cross_modal
        cycles: opts.cycles,
        receipt_dir: opts.receipt_dir.clone(),
        max_tokens: opts.max_tokens,
        on_progress: None,
    };

    let res = cross_modal::run_eval(&eval_opts, chat).await?;
    Ok(TakesQualityResult {
        final_aggregate: res.final_aggregate,
        final_receipt_path: res.final_receipt_path,
        n_takes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;
    use crate::types::Take;

    /// A fake judge returning valid 1-10 scores on the MVP rubric, so the
    /// full runner can be exercised without an API key.
    async fn fake_chat(_req: ChatRequest) -> Result<String> {
        Ok(
            r#"{"scores":{"insight":{"score":9,"feedback":"good"},"accuracy":{"score":8,"feedback":"ok"},"clarity":{"score":9,"feedback":"clear"},"actionability":{"score":7,"feedback":"ok"}},"overall":8.25,"improvements":["tighten claim 3"]}"#
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

        let opts = TakesQualityOpts {
            engine: &engine,
            sample: 5,
            dimensions: None,
            cycles: Some(1),
            max_tokens: Some(2000),
            receipt_dir,
            slug: Some("test-tq".to_string()),
        };

        let res = run(&opts, &fake_chat).await.unwrap();
        assert_eq!(res.n_takes, 5);
        // fake_chat returns all four dimensions >= 7 → verdict must be Pass.
        assert_eq!(res.final_aggregate.verdict, cross_modal::Verdict::Pass);
    }

    #[tokio::test]
    async fn run_errors_on_empty_corpus() {
        let engine = InMemoryEngine::new();
        let receipt_dir = std::env::temp_dir().join(format!("tq_test_empty_{}", std::process::id()));
        std::fs::create_dir_all(&receipt_dir).ok();

        let opts = TakesQualityOpts {
            engine: &engine,
            sample: 10,
            dimensions: None,
            cycles: Some(1),
            max_tokens: Some(2000),
            receipt_dir,
            slug: None,
        };

        let err = run(&opts, &fake_chat).await.unwrap_err();
        assert!(err.to_string().contains("no takes to evaluate"));
    }
}
