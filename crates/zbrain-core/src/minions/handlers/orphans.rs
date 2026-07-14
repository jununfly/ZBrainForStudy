//! Orphans handler — finds orphan pages (pages with no incoming links).
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts:1481` — `engine.findOrphanPages()`.
//!
//! ## Job data shape
//!
//! No required fields. Returns `{ "orphans": [...] }` with each entry having
//! `slug`, `title`, `domain`.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

/// Finds orphan pages (pages with no incoming links).
pub struct OrphansHandler;

#[async_trait]
impl MinionHandler for OrphansHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let engine = ctx.engine();
        let orphans = engine.find_orphan_pages().await?;
        Ok(serde_json::to_value(&orphans).unwrap_or(Value::Null))
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
            "orphans".to_string(),
            data,
            0,
            "test-token".to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn orphans_handler_returns_empty_when_no_pages() {
        let eng = engine();
        let handler = OrphansHandler;
        let context = ctx(&eng, json!({}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        let arr = result.as_array().expect("result should be an array");
        assert!(arr.is_empty());
    }

    #[tokio::test]
    async fn orphans_handler_finds_pages_without_backlinks() {
        let eng = engine();

        // Create a page with no incoming links
        eng.put_page(
            "orphan-page",
            Some("test-source"),
            &crate::PageInput {
                page_type: "page".to_string(),
                title: "Orphan Page".to_string(),
                compiled_truth: "no one links here".to_string(),
                ..Default::default()
            },
        )
        .await
        .expect("put_page");

        let handler = OrphansHandler;
        let context = ctx(&eng, json!({}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        let arr = result.as_array().expect("result should be an array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["slug"], "orphan-page");
        assert_eq!(arr[0]["title"], "Orphan Page");
    }
}
