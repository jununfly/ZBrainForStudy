//! v0.31.2 — `runFactsBackstop`: shared facts pipeline used by every brain
//! write surface that wants real-time hot-memory extraction (Rust port of
//! `backstop.ts`).
//!
//! Encapsulates the v0.31 smart pipeline:
//!
//! ```text
//! extract (extract_facts_from_turn — sanitize + LLM + parser)
//!   ↓
//! resolve (resolve_entity_slug — canonicalize free-form entity refs)
//!   ↓
//! dedup   (embedding cosine @ 0.95 — OMITTED in Rust, see note below)
//!   ↓
//! insert  (fence-first write_facts_to_fence, legacy DB-only fallback)
//! ```
//!
//! Two execution modes:
//!
//!   - `Queue` (default): fire-and-forget via the process `FactsQueue`. The
//!     caller's await is ~zero (just the enqueue). Used by sync, put_page,
//!     file_upload, code_import.
//!   - `Inline`: await the full pipeline; return real `{inserted, duplicate,
//!     superseded, fact_ids}` counts. Used by the explicit `extract_facts`
//!     MCP op so tool-call responses carry truthful numbers.
//!
//! ## Embedding dedup note
//!
//! The TS pipeline dedups against `findCandidateDuplicates` using fact
//! embeddings at cosine ≥ 0.95. The Rust `NewFact` type carries **no**
//! embedding field — the Rust extractor never produces one — so the cosine
//! fast-path would never fire. This matches the Codex Q7 observation that
//! fence rows have no embeddings and DB == fence at write time. The dedup
//! block is therefore intentionally omitted rather than stubbed.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::ai::chat::ChatProvider;
use crate::autopilot::phases::conversation_facts_backfill::extract_facts_from_turn;
use crate::autopilot::phases::resolve::resolve_entity_slug;
use crate::engine::BrainEngine;
use crate::facts::absorb_log::{classify_facts_absorb_error, write_facts_absorb_log, FactsAbsorbReason};
use crate::facts::eligibility::{self, EligibilityResult, ParsedPageLite};
use crate::facts::extract::is_facts_extraction_enabled;
use crate::facts::fence_write::{write_facts_to_fence, lookup_source_local_path, FenceInputFact, FenceTarget};
use crate::facts::queue::{get_facts_queue, reset_facts_queue_for_tests, FactsJob};
use crate::types::{FactInsertStatus, FactVisibility, NewFact};

const DEFAULT_FACTS_MODEL: &str = "anthropic:claude-sonnet-4-6";
const MAX_FACTS: usize = 25;

/// Execution mode (D8). `Queue` is fire-and-forget; `Inline` awaits the
/// full pipeline and returns truthful counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackstopMode {
    Queue,
    Inline,
}

/// Notability filter (D4). `HighOnly` lands HIGH facts now; MEDIUM/LOW wait
/// for the dream cycle / are dropped at the LLM layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotabilityFilter {
    All,
    HighOnly,
}

/// Skip reason surfaced in the result envelope for operator visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsBackstopSkipped {
    ExtractionDisabled,
    QueueOverflow,
    QueueShutdown,
    EligibilityFailed(String),
}

/// Discriminated return shape based on [`BackstopMode`].
#[derive(Debug, Clone)]
pub enum FactsBackstopResult {
    Queue {
        enqueued: bool,
        queue_depth: i64,
        skipped: Option<FactsBackstopSkipped>,
    },
    Inline {
        inserted: usize,
        duplicate: usize,
        superseded: usize,
        fact_ids: Vec<i64>,
        skipped: Option<FactsBackstopSkipped>,
    },
}

/// Context carried through the backstop pipeline. Mirrors TS
/// `FactsBackstopCtx` (minus the LLM `engine`, which Rust threads as a
/// separate `Arc<dyn BrainEngine>` so the queue worker can own it).
#[derive(Debug, Clone)]
pub struct BackstopCtx {
    /// Brain source identifier; default "default".
    pub source_id: String,
    /// source_session for provenance; None if absent.
    pub session_id: Option<String>,
    /// Provenance source string written into facts.source.
    pub source: &'static str,
    /// Execution mode — default `Queue`.
    pub mode: BackstopMode,
    /// Notability filter — default `All`.
    pub notability_filter: NotabilityFilter,
    /// Visibility tier (default [`FactVisibility::Private`]).
    pub visibility: FactVisibility,
    /// Override the chat model (else [`DEFAULT_FACTS_MODEL`]).
    pub model: Option<String>,
    /// Optional entity hints forwarded to the extractor.
    pub entity_hints: Vec<String>,
    /// Mirrors OperationContext.remote for trust-aware logging paths.
    pub remote: bool,
    /// Abort signal for shutdown propagation.
    pub abort: Option<CancellationToken>,
}

impl Default for BackstopCtx {
    fn default() -> Self {
        BackstopCtx {
            source_id: "default".to_string(),
            session_id: None,
            source: "mcp:extract_facts",
            mode: BackstopMode::Queue,
            notability_filter: NotabilityFilter::All,
            visibility: FactVisibility::Private,
            model: None,
            entity_hints: Vec::new(),
            remote: false,
            abort: None,
        }
    }
}

/// Count envelope returned by the inline pipeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PipelineCounts {
    pub inserted: usize,
    pub duplicate: usize,
    pub superseded: usize,
    pub fact_ids: Vec<i64>,
}

/// Run the facts pipeline for one page write. See module docstring for the
/// full lifecycle and mode semantics.
pub async fn run_facts_backstop(
    engine: Arc<dyn BrainEngine>,
    chat: Arc<dyn ChatProvider>,
    slug: &str,
    parsed: Option<ParsedPageLite<'_>>,
    ctx: BackstopCtx,
) -> crate::Result<FactsBackstopResult> {
    let mode = ctx.mode;

    // --- Eligibility + kill-switch gates (run before any LLM cost) ---
    let enabled = is_facts_extraction_enabled(&*engine).await?;
    if !enabled {
        return Ok(skipped_envelope(
            mode,
            FactsBackstopSkipped::ExtractionDisabled,
        ));
    }

    let eligibility = match eligibility::is_facts_backstop_eligible(slug, parsed.as_ref()) {
        EligibilityResult::Eligible => None,
        EligibilityResult::Ineligible { reason } => Some(reason),
    };
    if let Some(reason) = eligibility {
        return Ok(skipped_envelope(
            mode,
            FactsBackstopSkipped::EligibilityFailed(reason),
        ));
    }

    // --- Mode dispatch ---
    if matches!(mode, BackstopMode::Queue) {
        let turn_text: String = parsed
            .map(|p| p.compiled_truth.to_string())
            .unwrap_or_default();
        let slug_owned = slug.to_string();
        let source_id = ctx.source_id.clone();
        let session_id = ctx
            .session_id
            .clone()
            .unwrap_or_else(|| slug.to_string());

        // Clone the engine/chat into the spawned job; keep the originals for
        // the overflow branch below (the `move` closure consumes its captures).
        let engine_for_job = Arc::clone(&engine);
        let chat_for_job = Arc::clone(&chat);
        let slug_for_job = slug_owned.clone();
        let source_id_for_job = source_id.clone();
        let job: FactsJob = Box::new(move |token: CancellationToken| {
            let engine = engine_for_job;
            let chat = chat_for_job;
            let ctx = ctx;
            let turn_text = turn_text.clone();
            let slug = slug_for_job.clone();
            let source_id = source_id_for_job.clone();
            Box::pin(async move {
                let res = run_pipeline_with_body(
                    &*engine,
                    &*chat,
                    PipelineInput {
                        turn_text: &turn_text,
                        is_dream_generated: false,
                    },
                    &ctx,
                    Some(token),
                )
                .await;
                if let Err(err) = res {
                    let reason = classify_facts_absorb_error(&err);
                    let msg = format!("{err}");
                    let _ = write_facts_absorb_log(&*engine, &slug, reason, &msg, &source_id).await;
                }
                Ok(())
            })
        });

        let depth = get_facts_queue(None).enqueue(job, session_id);
        if depth < 0 {
            let _ = write_facts_absorb_log(
                &*engine,
                &slug_owned,
                FactsAbsorbReason::QueueOverflow,
                "queue capacity hit; enqueue dropped",
                &source_id,
            )
            .await;
            return Ok(FactsBackstopResult::Queue {
                enqueued: false,
                queue_depth: 0,
                skipped: Some(FactsBackstopSkipped::QueueOverflow),
            });
        }
        return Ok(FactsBackstopResult::Queue {
            enqueued: true,
            queue_depth: depth,
            skipped: None,
        });
    }

    // Inline mode: caller awaits the full pipeline. Errors bubble to the
    // caller (the explicit-call contract).
    let turn_text: &str = parsed
        .map(|p| p.compiled_truth)
        .unwrap_or("");
    let r = run_pipeline_with_body(
        &*engine,
        &*chat,
        PipelineInput {
            turn_text,
            is_dream_generated: false,
        },
        &ctx,
        ctx.abort.clone(),
    )
    .await?;
    Ok(FactsBackstopResult::Inline {
        inserted: r.inserted,
        duplicate: r.duplicate,
        superseded: r.superseded,
        fact_ids: r.fact_ids,
        skipped: None,
    })
}

/// Public pipeline entry-point — extract → resolve → insert. Used by the
/// `extract_facts` MCP op with a raw turn text. No eligibility/kill-switch
/// gates (those run upstream in [`run_facts_backstop`]).
pub async fn run_facts_pipeline(
    turn_text: &str,
    engine: Arc<dyn BrainEngine>,
    chat: Arc<dyn ChatProvider>,
    ctx: BackstopCtx,
) -> crate::Result<PipelineCounts> {
    run_pipeline_with_body(
        &*engine,
        &*chat,
        PipelineInput {
            turn_text,
            is_dream_generated: false,
        },
        &ctx,
        ctx.abort.clone(),
    )
    .await
}

struct PipelineInput<'a> {
    turn_text: &'a str,
    is_dream_generated: bool,
}

#[derive(Clone)]
struct Survived<'a> {
    f: &'a NewFact,
    resolved_slug: Option<String>,
}

/// Inner pipeline body. Eligibility + kill-switch are upstream; we just
/// extract → resolve → insert (fence-first, legacy DB-only fallback).
async fn run_pipeline_with_body(
    engine: &dyn BrainEngine,
    chat: &dyn ChatProvider,
    input: PipelineInput<'_>,
    ctx: &BackstopCtx,
    abort: Option<CancellationToken>,
) -> crate::Result<PipelineCounts> {
    if abort.as_ref().map(|t| t.is_cancelled()).unwrap_or(false) {
        return Ok(PipelineCounts::default());
    }

    let model = ctx.model.as_deref().unwrap_or(DEFAULT_FACTS_MODEL);
    let (facts, _usage) = extract_facts_from_turn(
        chat,
        model,
        input.turn_text,
        &ctx.source,
        ctx.session_id.as_deref(),
        MAX_FACTS,
    )
    .await?;

    let filter_high_only = matches!(ctx.notability_filter, NotabilityFilter::HighOnly);
    let visibility = ctx.visibility.clone();

    let mut inserted = 0usize;
    let mut duplicate = 0usize;
    let mut superseded = 0usize;
    let mut fact_ids: Vec<i64> = Vec::new();

    // Phase 1: filter + resolve entity slug. (Embedding dedup omitted — see
    // module note.)
    let mut survived: Vec<Survived<'_>> = Vec::with_capacity(facts.len());
    for f in &facts {
        if abort.as_ref().map(|t| t.is_cancelled()).unwrap_or(false) {
            break;
        }
        if filter_high_only && f.notability.as_deref() != Some("high") {
            continue;
        }
        let resolved_slug = match &f.entity_slug {
            Some(es) => resolve_entity_slug(engine, &ctx.source_id, es).await,
            None => None,
        };
        survived.push(Survived {
            f,
            resolved_slug,
        });
    }

    if survived.is_empty() {
        return Ok(PipelineCounts::default());
    }

    // Phase 2: group survived facts by resolved entity slug.
    let mut by_entity: HashMap<String, Vec<Survived<'_>>> = HashMap::new();
    let mut unparented: Vec<Survived<'_>> = Vec::new();
    for s in &survived {
        match &s.resolved_slug {
            Some(slug) => by_entity.entry(slug.clone()).or_default().push(s.clone()),
            None => unparented.push(s.clone()),
        }
    }

    // Phase 3: look up source.local_path once for the fence path.
    let local_path = lookup_source_local_path(engine, &ctx.source_id).await?;

    // Phase 4: legacy DB-only fallback for unparented + thin-client.
    let mut legacy_bucket: Vec<Survived<'_>> = Vec::new();
    match &local_path {
        None => {
            for s in &survived {
                legacy_bucket.push(s.clone());
            }
        }
        Some(_) => {
            for s in &unparented {
                legacy_bucket.push(s.clone());
            }
        }
    }

    for s in &legacy_bucket {
        let new_fact = legacy_new_fact(s.f, s.resolved_slug.as_deref(), visibility.clone());
        match engine
            .insert_fact(
                &ctx.source_id,
                s.resolved_slug.as_deref().unwrap_or(""),
                &new_fact,
            )
            .await?
        {
            FactInsertStatus::Inserted => inserted += 1,
            FactInsertStatus::Duplicate => duplicate += 1,
            FactInsertStatus::Superseded => superseded += 1,
        }
    }

    let local_path = match local_path {
        Some(p) => p,
        None => {
            return Ok(PipelineCounts {
                inserted,
                duplicate,
                superseded,
                fact_ids,
            });
        }
    };

    // Phase 5: fence-write per entity.
    for (slug, group) in &by_entity {
        if abort.as_ref().map(|t| t.is_cancelled()).unwrap_or(false) {
            break;
        }

        let input_facts: Vec<FenceInputFact> = group
            .iter()
            .map(|s| FenceInputFact {
                fact: s.f.fact.clone(),
                kind: s.f.kind.clone(),
                notability: s.f.notability.clone(),
                source: s.f.source.clone(),
                context: None,
                visibility: visibility.clone(),
                confidence: s.f.confidence,
                valid_from: s.f.valid_from.clone(),
                session_id: s.f.source_session.clone(),
            })
            .collect();

        let target = FenceTarget {
            source_id: ctx.source_id.clone(),
            local_path: Some(local_path.clone()),
            slug: slug.clone(),
        };
        let result = write_facts_to_fence(engine, &target, &input_facts).await?;

        if result.fence_write_failed {
            // .tmp quarantined; rows NOT inserted; do not fall through.
            continue;
        }
        if result.stub_guard_blocked {
            // Route these facts to the legacy DB-only path so they aren't
            // dropped (the slug stays attached but no markdown file is
            // created).
            for s in group {
                let new_fact = legacy_new_fact(s.f, Some(slug), visibility.clone());
                match engine.insert_fact(&ctx.source_id, slug, &new_fact).await? {
                    FactInsertStatus::Inserted => inserted += 1,
                    FactInsertStatus::Duplicate => duplicate += 1,
                    FactInsertStatus::Superseded => superseded += 1,
                }
            }
            continue;
        }
        if result.legacy_fallback {
            tracing::warn!(
                "[facts-backstop] writeFactsToFence returned legacyFallback for slug={slug} \
                 despite local_path being set — investigation needed."
            );
            continue;
        }

        inserted += result.inserted;
        fact_ids.extend(result.ids);
    }

    Ok(PipelineCounts {
        inserted,
        duplicate,
        superseded,
        fact_ids,
    })
}

fn legacy_new_fact(f: &NewFact, entity_slug: Option<&str>, visibility: FactVisibility) -> NewFact {
    NewFact {
        fact: f.fact.clone(),
        kind: f.kind.clone(),
        entity_slug: entity_slug.map(|s| s.to_string()),
        visibility: Some(visibility),
        context: None,
        valid_from: f.valid_from.clone(),
        valid_until: None,
        source: f.source.clone(),
        source_session: f.source_session.clone(),
        confidence: f.confidence,
        notability: f.notability.clone(),
        claim_metric: None,
        claim_value: None,
        claim_unit: None,
        claim_period: None,
        event_type: None,
        row_num: None,
        source_markdown_slug: None,
    }
}

fn skipped_envelope(mode: BackstopMode, skipped: FactsBackstopSkipped) -> FactsBackstopResult {
    match mode {
        BackstopMode::Queue => FactsBackstopResult::Queue {
            enqueued: false,
            queue_depth: 0,
            skipped: Some(skipped),
        },
        BackstopMode::Inline => FactsBackstopResult::Inline {
            inserted: 0,
            duplicate: 0,
            superseded: 0,
            fact_ids: Vec::new(),
            skipped: Some(skipped),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use crate::engine::InMemoryEngine;
    use crate::facts::queue::FactsQueueOpts;
    use crate::types::FactKind;

    fn inline_ctx(source_id: &str) -> BackstopCtx {
        BackstopCtx {
            source_id: source_id.to_string(),
            session_id: None,
            source: "mcp:extract_facts",
            mode: BackstopMode::Inline,
            notability_filter: NotabilityFilter::All,
            visibility: FactVisibility::Private,
            model: None,
            entity_hints: Vec::new(),
            remote: false,
            abort: None,
        }
    }

    #[tokio::test]
    async fn eligibility_failed_for_subagent_slug() {
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let chat = Arc::new(MockChatProvider::new("[]"));
        let ctx = inline_ctx("default");
        let result = run_facts_backstop(
            engine,
            chat,
            "wiki/agents/foo",
            None,
            ctx,
        )
        .await
        .unwrap();
        match result {
            FactsBackstopResult::Inline { skipped, .. } => {
                assert_eq!(
                    skipped,
                    Some(FactsBackstopSkipped::EligibilityFailed(
                        "subagent_namespace".to_string()
                    ))
                );
            }
            _ => panic!("expected inline result"),
        }
    }

    #[tokio::test]
    async fn queue_mode_enqueues_when_eligible() {
        reset_facts_queue_for_tests();
        let _ = get_facts_queue(Some(FactsQueueOpts {
            cap: 100,
            per_session_inflight_cap: 1,
            shutdown_grace_ms: 5000,
        }));
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let chat = Arc::new(MockChatProvider::new("[]"));
        let ctx = BackstopCtx {
            mode: BackstopMode::Queue,
            ..inline_ctx("default")
        };
        let parsed = ParsedPageLite {
            page_type: "note",
            compiled_truth: "Jared likes oolong tea. He also enjoys hiking and reading science \
fiction novels on rainy afternoons with a cup of tea by the window.",
            frontmatter: json!({}),
        };
        let result = run_facts_backstop(engine, chat, "notes/jared", Some(parsed), ctx)
            .await
            .unwrap();
        match result {
            FactsBackstopResult::Queue { enqueued, .. } => assert!(enqueued),
            _ => panic!("expected queue result"),
        }
    }

    #[tokio::test]
    async fn inline_happy_path_inserts_fact() {
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let chat = Arc::new(MockChatProvider::new("[]"));
        chat.queue_text(
            r#"{"facts":[{"fact":"likes oolong tea","kind":"preference","notability":"high","entity":"jared"}]}"#,
        );
        let ctx = inline_ctx("default");
        let counts = run_facts_pipeline(
            "Jared likes oolong tea.",
            Arc::clone(&engine),
            chat,
            ctx,
        )
        .await
        .unwrap();
        assert_eq!(counts.inserted, 1);
        assert_eq!(counts.duplicate, 0);

        let rows = engine
            .list_facts_by_entity("default", "jared", &Default::default())
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].fact, "likes oolong tea");
        assert_eq!(rows[0].kind, FactKind::Preference);
    }

    #[tokio::test]
    async fn notability_high_only_filters_medium() {
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let chat = Arc::new(MockChatProvider::new("[]"));
        chat.queue_text(
            r#"{"facts":[{"fact":"minor detail","kind":"fact","notability":"medium"}]}"#,
        );
        let ctx = BackstopCtx {
            notability_filter: NotabilityFilter::HighOnly,
            ..inline_ctx("default")
        };
        let counts = run_facts_pipeline(
            "minor detail",
            Arc::clone(&engine),
            chat,
            ctx,
        )
        .await
        .unwrap();
        assert_eq!(counts.inserted, 0);
    }
}
