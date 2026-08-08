//! Reindex handler — re-embed all live pages from their `compiled_truth` into
//! the page-level vector column.
//!
//! Faithful Rust port of `zbrain-cli::run_reindex_pages`: it reads the
//! embedding provider from the environment, pages the live pages, embeds their
//! `compiled_truth`, and writes the vectors back via `put_page_embedding`.
//!
//! ## Feature gate
//!
//! `EmbeddingClient::from_env()` is only built with the `embedding` feature
//! (the same gate `zbrain-cli` enables). Without it this handler reports a
//! clear "feature not enabled" error instead of compiling dead code. At
//! runtime the minion worker uses a `LibsqlEngine` (the only engine that
//! implements `list_pages` / `put_page_embedding` with real SQL).

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::engine::{BrainEngine, PageFilters};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ReindexHandler;

#[async_trait]
impl MinionHandler for ReindexHandler {
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
                    "reindex",
                    "embedding provider not configured: set ZEROENTROPY_API_KEY to re-embed pages",
                )
            })?;

            let engine = ctx.engine();
            let mut offset: usize = 0;
            let mut total_scanned = 0usize;
            let mut total_embedded = 0usize;

            loop {
                let filters = PageFilters {
                    page_type: None,
                    tag: None,
                    limit: Some(batch),
                    offset: Some(offset),
                    updated_after: None,
                    slug_prefix: None,
                    include_deleted: false,
                    sort: None,
                    source_id: source_id.clone(),
                    source_ids: None,
                };
                let pages = engine.list_pages(&filters).await?;
                if pages.is_empty() {
                    break;
                }
                let (scanned, embedded) = embed_pages(engine.as_ref(), &client, pages, dry_run).await?;
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
                "reindex",
                "the 'embedding' feature is not enabled in this build; reindex requires it",
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

    fn ctx(data: Value) -> MinionJobContext {
        MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "reindex".into(),
            data,
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    // Without the `embedding` feature (the default test config) the handler must
    // fail loudly rather than silently pretend. With the feature + no provider
    // it fails on config. Either way a unit test (InMemoryEngine, no provider)
    // must not succeed — that path requires LibsqlEngine + a real provider.
    #[tokio::test]
    async fn reindex_fails_without_embedding_infra() {
        let r = ReindexHandler.handle(&ctx(json!({"dry_run": true}))).await;
        assert!(r.is_err(), "reindex must not succeed without embedding infra");
    }
}
