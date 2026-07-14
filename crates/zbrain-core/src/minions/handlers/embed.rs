//! Embed handler — computes vector embeddings for stale pages.
//! v1 skeleton: embedding API not yet wired in Rust engine.

use async_trait::async_trait;
use serde_json::{json, Value};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct EmbedHandler;
#[async_trait]
impl MinionHandler for EmbedHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        Ok(json!({"status": "not_implemented", "detail": "embed pending embedding API port"}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine; use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine; use std::sync::Arc; use tokio_util::sync::CancellationToken;
    #[tokio::test] async fn embed_smoke() {
        let e = Arc::new(InMemoryEngine::new());
        let r = EmbedHandler.handle(&MinionJobContext::new(Arc::clone(&e) as Arc<dyn BrainEngine>,1,"embed".into(),json!({}),0,"t".into(),CancellationToken::new(),CancellationToken::new())).await.unwrap();
        assert_eq!(r["status"],"not_implemented");
    }
}
