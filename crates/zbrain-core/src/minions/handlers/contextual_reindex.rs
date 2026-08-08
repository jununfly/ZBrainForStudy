//! Contextual-reindex handler — re-embed pages with LLM-generated context
//! prepended to each chunk's text. Maps the TS `contextual_reindex_per_chunk`
//! job (which used a Haiku LLM + rate-lease to synthesize a per-chunk synopsis,
//! two-phase build, and a page-level fallback).
//!
//! ## Status
//!
//! The **embedding portion** is fully wired (NULL-embedding pages are re-embedded
//! from `compiled_truth` via the shared `embed_pages` helper). The **per-chunk
//! LLM context augmentation** is intentionally *deferred*: it requires a
//! `ChatProvider` to be injected into the minion handler context (currently the
//! worker only wires a `ChatProvider` into the `subagent` handler). Until that
//! seam exists, this handler performs the plain re-embed and reports
//! `llm_context_augmentation: "deferred"`. Tracked as G84 in
//! `docs/plans/KNOWN-GAPS.md`.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::engine::BrainEngine;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ContextualReindexHandler;

#[async_trait]
impl MinionHandler for ContextualReindexHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let data = &ctx.data;
        let source_id = data
            .get("source_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let dry_run = data
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let batch = data
            .get("batch")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(50)
            .max(1);

        #[cfg(feature = "embedding")]
        {
            use crate::minions::handlers::embed_util::embed_pages;

            let client = crate::embedding::EmbeddingClient::from_env().ok_or_else(|| {
                crate::Error::new(
                    "ConfigurationError",
                    "contextual-reindex",
                    "embedding provider not configured: set ZEROENTROPY_API_KEY to re-embed pages",
                )
            })?;

            let engine = ctx.engine();
            let mut offset: usize = 0;
            let mut total_scanned = 0usize;
            let mut total_embedded = 0usize;

            loop {
                let stale = engine.list_stale_pages().await?;
                let take: Vec<_> = stale
                    .into_iter()
                    .filter(|p| source_id.as_ref().map_or(true, |s| &p.source_id == s))
                    .skip(offset)
                    .take(batch)
                    .collect();
                if take.is_empty() {
                    break;
                }
                // NOTE: defers the per-chunk LLM context synthesis (G84). The
                // embed loop runs on the raw `compiled_truth` for now.
                let (scanned, embedded) =
                    embed_pages(engine.as_ref(), &client, take, dry_run).await?;
                total_scanned += scanned;
                total_embedded += embedded;
                offset += scanned;
                if dry_run {
                    continue;
                }
            }

            return Ok(json!({
                "status": "ok",
                "dry_run": dry_run,
                "scanned": total_scanned,
                "embedded": total_embedded,
                "llm_context_augmentation": "deferred",
                "note": "per-chunk LLM context synthesis not yet wired (see KNOWN-GAPS G84)",
            }));
        }

        #[cfg(not(feature = "embedding"))]
        {
            Err(crate::Error::new(
                "Unsupported",
                "contextual-reindex",
                "the 'embedding' feature is not enabled in this build; contextual-reindex requires it",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn contextual_reindex_fails_without_embedding_infra() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "contextual_reindex_per_chunk".into(),
            json!({"dry_run": true}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        let r = ContextualReindexHandler.handle(&ctx).await;
        assert!(
            r.is_err(),
            "contextual-reindex must not succeed without embedding infra"
        );
    }
}
