//! Purge handler — hard-deletes soft-deleted pages older than N hours.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts:1496` — `engine.purgeDeletedPages` + `purgeExpiredSources` + `purgeStaleCheckpoints`.
//! v1 only calls `engine.purge_deleted_pages`; expired sources and stale
//! checkpoints are separate engine methods to be added when the CLI migration
//! reaches them.
//!
//! ## Job data shape
//!
//! - `older_than_hours` (optional, default 72): pages deleted longer than
//!   this many hours ago are permanently removed.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

/// Hard-deletes soft-deleted pages.
pub struct PurgeHandler;

#[async_trait]
impl MinionHandler for PurgeHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let older_than_hours = ctx
            .data
            .get("older_than_hours")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(72);

        let engine = ctx.engine();
        let result = engine.purge_deleted_pages(older_than_hours).await?;
        Ok(serde_json::to_value(&result).unwrap_or(Value::Null))
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
            "purge".to_string(),
            data,
            0,
            "test-token".to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn purge_handler_returns_empty_when_nothing_to_purge() {
        let eng = engine();
        let handler = PurgeHandler;
        let context = ctx(&eng, json!({"older_than_hours": 72}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["count"], 0);
    }

    #[tokio::test]
    async fn purge_handler_respects_default_older_than_hours() {
        let eng = engine();
        let handler = PurgeHandler;
        // No older_than_hours in data — uses default 72.
        let context = ctx(&eng, json!({}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["count"], 0);
    }
}
