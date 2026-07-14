//! Lint handler — runs markdown lint checks.
//! v1 skeleton: lint CLI pipeline not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct LintHandler;

#[async_trait]
impl MinionHandler for LintHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "lint pending CLI migration"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine; use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine; use std::sync::Arc; use tokio_util::sync::CancellationToken;

    #[tokio::test] async fn lint_smoke() {
        let e = Arc::new(InMemoryEngine::new());
        let r = LintHandler.handle(&MinionJobContext::new(
            Arc::clone(&e) as Arc<dyn BrainEngine>, 1, "lint".into(), json!({}), 0,
            "t".into(), CancellationToken::new(), CancellationToken::new(),
        )).await.unwrap();
        assert_eq!(r["status"], "not_implemented");
    }
}
