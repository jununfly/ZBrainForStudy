//! Backlinks handler — validates and rebuilds backlink integrity.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts:1296` — `runBacklinksCore`.
//! v1 runs a single-page backlink lookup as the core engine call.
//!
//! ## Job data shape
//!
//! - `slug` (required): the page slug to check backlinks for.
//! - `source_id` (optional): scope to a specific source.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::{Error, Result};

/// Validates and returns backlinks for a given page.
pub struct BacklinksHandler;

#[async_trait]
impl MinionHandler for BacklinksHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let slug = ctx
            .data
            .get("slug")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::new(
                    "InvalidJobData",
                    "missing_slug",
                    "backlinks job data must contain a non-empty \"slug\" field",
                )
            })?;

        let source_id = ctx.data.get("source_id").and_then(|v| v.as_str());

        let engine = ctx.engine();
        let backlinks = engine.get_backlinks(slug, source_id).await?;
        Ok(serde_json::to_value(&backlinks).unwrap_or(Value::Null))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    fn ctx(engine: &Arc<dyn BrainEngine>, data: Value) -> MinionJobContext {
        MinionJobContext::new(
            Arc::clone(engine),
            1,
            "backlinks".to_string(),
            data,
            0,
            "test-token".to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn backlinks_handler_errors_on_missing_slug() {
        let eng = engine();
        let handler = BacklinksHandler;
        let context = ctx(&eng, json!({}));

        let err = handler.handle(&context).await.unwrap_err();
        assert!(err.to_string().contains("slug"));
    }

    #[tokio::test]
    async fn backlinks_handler_returns_empty_for_nonexistent_page() {
        let eng = engine();
        let handler = BacklinksHandler;
        let context = ctx(&eng, json!({"slug": "no-such-page"}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        let arr = result.as_array().expect("result should be an array");
        assert!(arr.is_empty());
    }
}
