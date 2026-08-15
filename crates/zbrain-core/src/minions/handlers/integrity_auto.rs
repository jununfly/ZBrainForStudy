//! Integrity-auto handler — UNSUPPORTED / wontfix.
//!
//! The TS `integrity-auto` command was deleted under option C and has no Rust
//! verb. Ad-hoc integrity is available via the wired `integrity` handler. Tracked
//! as G82 in `docs/plans/KNOWN-GAPS.md`.

use async_trait::async_trait;
use serde_json::Value;

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct IntegrityAutoHandler;

#[async_trait]
impl MinionHandler for IntegrityAutoHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Err(crate::minions::handlers::util::unsupported_job(
            "integrity-auto",
            "G82",
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
    async fn integrity_auto_is_unsupported() {
        let ctx = MinionJobContext::new(
            Arc::new(InMemoryEngine::new()) as Arc<dyn BrainEngine>,
            1,
            "integrity-auto".into(),
            serde_json::json!({}),
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        assert!(IntegrityAutoHandler.handle(&ctx).await.is_err());
    }
}
