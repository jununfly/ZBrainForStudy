//! Repair-jsonb handler — UNSUPPORTED / wontfix (pglite dead tech).
//!
//! The TS `repair-jsonb` command repaired PGLite-specific `jsonb` corruption.
//! ZBrain now runs on libsql/Postgres, not PGLite, so the command has no Rust
//! target. Tracked as G78 in `docs/plans/KNOWN-GAPS.md` (the command-level gap;
//! this minion job type shares that disposition).

use async_trait::async_trait;
use serde_json::Value;

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct RepairJsonbHandler;

#[async_trait]
impl MinionHandler for RepairJsonbHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Err(crate::minions::handlers::util::unsupported_job(
            "repair-jsonb",
            "G78",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn repair_jsonb_is_unsupported() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "repair-jsonb".into(),
            serde_json::json!({}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        assert!(RepairJsonbHandler.handle(&ctx).await.is_err());
    }
}
