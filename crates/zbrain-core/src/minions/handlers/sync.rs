//! Sync handler — synchronizes a git repository into brain pages.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts:1103` — `performSync` from `src/commands/sync.ts`.
//! v1 skeleton: the full sync pipeline (git clone, markdown parse, chunk,
//! page upsert) is pending CLI migration.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct SyncHandler;

#[async_trait]
impl MinionHandler for SyncHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        // v1: full sync pipeline not yet ported from TS.
        Ok(json!({"status": "not_implemented", "detail": "sync pipeline pending CLI migration"}))
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

    #[tokio::test]
    async fn sync_handler_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "sync".into(), json!({"source_id": "test"}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = SyncHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
