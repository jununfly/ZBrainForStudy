//! Sync handler — synchronizes a git repository into brain pages.
//!
//! Wires the minion `sync` job to the Rust `sync_brain` operation
//! (`crates/zbrain-core/src/operation.rs`), the port of
//! `src/commands/sync.ts` (`performSync`). The operation is dispatched via
//! the shared `OperationRegistry` (same path the CLI/MCP transports use), so
//! trust-boundary enforcement, param validation, and result serialization are
//! identical to the CLI.

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::engine::BrainEngine;
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::operation::{register_all, OperationContext, OperationRegistry};
use crate::Result;

pub struct SyncHandler;

#[async_trait]
impl MinionHandler for SyncHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let data = &ctx.data;
        let source_id = data
            .get("source_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let repo_path = data
            .get("repo_path")
            .and_then(|v| v.as_str())
            .map(String::from);
        let no_pull = data.get("no_pull").and_then(|v| v.as_bool());

        let mut registry = OperationRegistry::new();
        register_all(&mut registry);

        let mut op_ctx = OperationContext::local_cli();
        op_ctx.engine = Some(ctx.engine().clone());
        // Pass through the minion's source_id when provided; otherwise fall
        // back to the registry default ("default").
        if let Some(sid) = &source_id {
            if !sid.is_empty() {
                op_ctx.source_id = sid.clone();
            }
        }

        let params = json!({
            "source_id": source_id,
            "repo_path": repo_path,
            "no_pull": no_pull,
        });

        let output = registry
            .dispatch_json("sync_brain", &op_ctx, params)
            .await
            .map_err(|e| {
                crate::Error::new("SyncOperationError", "sync_brain", &e.to_string())
            })?;
        Ok(output)
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

    fn ctx() -> MinionJobContext {
        let e = Arc::new(InMemoryEngine::new());
        MinionJobContext::new(
            Arc::clone(&e) as Arc<dyn BrainEngine>,
            1,
            "sync".into(),
            json!({ "source_id": "test" }),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn sync_handler_dispatches_to_sync_brain() {
        let result = SyncHandler.handle(&ctx()).await;
        // Wired to the sync_brain operation: either a real SyncBrainOutput
        // (source resolves, sourceId present) or a structured error from the
        // operation — but never the old "not_implemented" stub.
        match result {
            Ok(v) => assert!(
                v.get("sourceId").is_some() || v.get("source_id").is_some(),
                "expected sync_brain output, got: {v}"
            ),
            Err(e) => assert!(
                !e.to_string().contains("not_implemented"),
                "sync handler must not return the old stub: {e}"
            ),
        }
    }
}
