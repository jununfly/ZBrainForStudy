//! v0.29 — Recompute emotional weight phase.
//!
//! Faithful port of `src/core/cycle/recompute-emotional-weight.ts`.
//! Deterministic, no LLM calls. Loads page inputs in batch via
//! `engine::batch_load_emotional_inputs`, computes weights with the pure
//! `compute_emotional_weight` (see `emotional_weight.rs`), then writes them
//! back via `engine::set_emotional_weight_batch`.
//!
//! NOTE: TS resolved the high-tags / user-holder overrides from `engine.getConfig`
//! (`emotional_weight.high_tags` / `emotional_weight.user_holder`). Rust has no
//! global config store, so those overrides are passed through `opts` instead
//! (matching the established "config → opts" migration convention, see 1-2 Q2).

use std::collections::HashSet;

use crate::autopilot::phases::emotional_weight::{
    compute_emotional_weight, EmotionalWeightInput, EmotionalWeightOpts,
};
use crate::engine::{BrainEngine, EmotionalWeightWrite};

/// Options for [`run_phase_recompute_emotional_weight`].
#[derive(Debug, Clone, Default)]
pub struct RecomputeEmotionalWeightOpts {
    /// When true, read + compute but skip the UPDATE.
    pub dry_run: bool,
    /// Slugs to recompute. `None` / empty => full-brain recompute.
    pub affected_slugs: Option<Vec<String>>,
    /// Override high-emotion tags (TS config `emotional_weight.high_tags`).
    pub high_emotion_tags: Option<HashSet<String>>,
    /// Override user holder (TS config `emotional_weight.user_holder`).
    pub user_holder: Option<String>,
}

/// Result of [`run_phase_recompute_emotional_weight`].
#[derive(Debug, Clone)]
pub struct RecomputeEmotionalWeightResult {
    pub status: String,
    pub summary: String,
    pub pages_recomputed: u64,
    pub mode: String,
    pub dry_run: bool,
}

/// Run the recompute-emotional-weight phase.
///
/// Mirrors `runPhaseRecomputeEmotionalWeight`: incremental empty set => zero-work
/// ok; full/incremental => batch-load, compute, (optionally) write. Errors
/// propagate as `Err` and are mapped to `PhaseStatus::Fail` by the cycle arm.
pub async fn run_phase_recompute_emotional_weight(
    engine: &dyn BrainEngine,
    opts: &RecomputeEmotionalWeightOpts,
) -> crate::Result<RecomputeEmotionalWeightResult> {
    // Incremental path: empty array means "no changes touched" — zero-work ok.
    if let Some(slugs) = &opts.affected_slugs {
        if slugs.is_empty() {
            return Ok(RecomputeEmotionalWeightResult {
                status: "ok".into(),
                summary: "recompute_emotional_weight (incremental, 0 slugs)".into(),
                pages_recomputed: 0,
                mode: "incremental".into(),
                dry_run: opts.dry_run,
            });
        }
    }

    let ew_opts = EmotionalWeightOpts {
        high_emotion_tags: opts.high_emotion_tags.clone(),
        user_holder: opts.user_holder.clone(),
    };

    let inputs = engine
        .batch_load_emotional_inputs(opts.affected_slugs.as_deref())
        .await?;

    let writes: Vec<EmotionalWeightWrite> = inputs
        .iter()
        .map(|row| {
            let weight = compute_emotional_weight(
                &EmotionalWeightInput {
                    tags: row.tags.clone(),
                    takes: row.takes.clone(),
                },
                &ew_opts,
            );
            EmotionalWeightWrite {
                slug: row.slug.clone(),
                source_id: row.source_id.clone(),
                emotional_weight: weight,
            }
        })
        .collect();

    let mode = if opts.affected_slugs.is_some() {
        "incremental"
    } else {
        "full"
    };

    if opts.dry_run {
        return Ok(RecomputeEmotionalWeightResult {
            status: "ok".into(),
            summary: format!("recompute_emotional_weight (dry-run, {} pages)", writes.len()),
            pages_recomputed: writes.len() as u64,
            mode: mode.into(),
            dry_run: true,
        });
    }

    let updated = engine.set_emotional_weight_batch(&writes).await?;

    Ok(RecomputeEmotionalWeightResult {
        status: "ok".into(),
        summary: format!("recompute_emotional_weight ({} pages)", updated),
        pages_recomputed: updated,
        mode: mode.into(),
        dry_run: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineConfig, GetPageOpts, InMemoryEngine, PageInput};

    async fn setup() -> InMemoryEngine {
        let e = InMemoryEngine::new();
        e.connect(&EngineConfig::default()).await.unwrap();
        e
    }

    async fn put_page_with_tags_and_takes(e: &InMemoryEngine, slug: &str, tags: &[&str]) {
        use crate::types::TakeInput;
        let mut fm = serde_json::json!({});
        fm["tags"] = serde_json::json!(tags);
        let page = e
            .put_page(slug, Some("default"), &PageInput {
                page_type: "page".to_string(),
                title: slug.to_string(),
                frontmatter: Some(fm),
                ..Default::default()
            })
            .await
            .unwrap();
        // Give the page an active take so density contributes.
        e.add_takes_batch(
            page.id,
            &[TakeInput {
                page_id: page.id,
                row_num: None,
                claim: "holder has a take".to_string(),
                kind: "take".to_string(),
                holder: "garry".to_string(),
                weight: 1.0,
                since_date: None,
                until_date: None,
                source: None,
                superseded_by: None,
                active: Some(true),
            }],
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn empty_inputs_produce_zero_recomputed() {
        let e = setup().await;
        let res = run_phase_recompute_emotional_weight(
            &e,
            &RecomputeEmotionalWeightOpts::default(),
        )
        .await
        .unwrap();
        assert_eq!(res.status, "ok");
        assert_eq!(res.pages_recomputed, 0);
        assert_eq!(res.mode, "full");
    }

    #[tokio::test]
    async fn incremental_empty_slugs_is_zero_work() {
        let e = setup().await;
        let res = run_phase_recompute_emotional_weight(
            &e,
            &RecomputeEmotionalWeightOpts {
                affected_slugs: Some(vec![]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(res.pages_recomputed, 0);
        assert_eq!(res.mode, "incremental");
    }

    #[tokio::test]
    async fn computes_and_writes_weight_for_tagged_page() {
        let e = setup().await;
        put_page_with_tags_and_takes(&e, "concepts/wedding", &["wedding"]).await;

        let res = run_phase_recompute_emotional_weight(
            &e,
            &RecomputeEmotionalWeightOpts::default(),
        )
        .await
        .unwrap();
        assert_eq!(res.status, "ok");
        assert_eq!(res.pages_recomputed, 1, "one page should be recomputed");

        // The page should now carry emotional_weight ≈ 0.5 (tag boost) + density.
        let page = e
            .get_page("concepts/wedding", &GetPageOpts {
                source_id: Some("default".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        let w = page.emotional_weight.expect("emotional_weight should be set");
        assert!(w > 0.5, "tagged page should have weight > 0.5, got {w}");
        assert!(page.salience_touched_at.is_some(), "salience_touched_at bumped");
    }

    #[tokio::test]
    async fn dry_run_does_not_write() {
        let e = setup().await;
        put_page_with_tags_and_takes(&e, "concepts/wedding", &["wedding"]).await;

        let res = run_phase_recompute_emotional_weight(
            &e,
            &RecomputeEmotionalWeightOpts {
                dry_run: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert!(res.dry_run);
        assert_eq!(res.pages_recomputed, 1);

        // No weight written in dry-run.
        let page = e
            .get_page("concepts/wedding", &GetPageOpts {
                source_id: Some("default".to_string()),
                ..Default::default()
            })
            .await
            .unwrap()
            .unwrap();
        assert!(page.emotional_weight.is_none());
    }

    #[tokio::test]
    async fn affected_slugs_filters_pages() {
        let e = setup().await;
        put_page_with_tags_and_takes(&e, "concepts/wedding", &["wedding"]).await;
        put_page_with_tags_and_takes(&e, "concepts/other", &[]).await;

        let res = run_phase_recompute_emotional_weight(
            &e,
            &RecomputeEmotionalWeightOpts {
                affected_slugs: Some(vec!["concepts/wedding".to_string()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(res.mode, "incremental");
        assert_eq!(res.pages_recomputed, 1, "only the affected slug recomputed");
    }
}
