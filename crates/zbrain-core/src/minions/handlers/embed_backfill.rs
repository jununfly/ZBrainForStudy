//! Embed-backfill handler — backfill embeddings for existing pages that have no
//! vector yet (NULL `embedding`). Maps the TS `embed-backfill` job.
//!
//! Like `embed`, the target set is the NULL-embedding pages
//! (`BrainEngine::list_stale_pages`); the distinction in TS was that
//! `embed-backfill` was the explicit "fill the gaps" entry point. Both share
//! the `embed_pages` helper. See `reindex.rs` for the feature-gate rationale.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::engine::BrainEngine;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct EmbedBackfillHandler;

#[async_trait]
impl MinionHandler for EmbedBackfillHandler {
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
                    "embed-backfill",
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
            }));
        }

        #[cfg(not(feature = "embedding"))]
        {
            Err(crate::Error::new(
                "Unsupported",
                "embed-backfill",
                "the 'embedding' feature is not enabled in this build; embed-backfill requires it",
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
    async fn embed_backfill_fails_without_embedding_infra() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "embed-backfill".into(),
            json!({"dry_run": true}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        let r = EmbedBackfillHandler.handle(&ctx).await;
        assert!(
            r.is_err(),
            "embed-backfill must not succeed without embedding infra"
        );
    }
}
