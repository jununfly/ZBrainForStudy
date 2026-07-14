//! Extract-conversation-facts handler — extract facts from conversations.
//!
//! ## TS reference
//!
//! `src/commands/jobs.ts` — `extract-conversation-facts` handler (1115 lines).
//! Uses Haiku LLM + BudgetTracker for multi-turn fact extraction. v1 skeleton:
//! both dependencies not yet ported.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ExtractConversationFactsHandler;

#[async_trait]
impl MinionHandler for ExtractConversationFactsHandler {
    async fn handle(&self, _ctx: &MinionJobContext) -> Result<Value> {
        // v1: Haiku LLM + BudgetTracker not yet ported.
        Ok(json!({"status": "not_implemented", "detail": "extract-conversation-facts pending Haiku LLM + BudgetTracker port"}))
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
    async fn extract_conversation_facts_smoke_does_not_panic() {
        let eng = engine();
        let context = MinionJobContext::new(
            Arc::clone(&eng) as Arc<dyn BrainEngine>,
            1, "extract-conversation-facts".into(), json!({}), 0,
            "tok".into(), CancellationToken::new(), CancellationToken::new(),
        );
        let handler = ExtractConversationFactsHandler;
        let result = handler.handle(&context).await.expect("should not panic");
        assert_eq!(result["status"], "not_implemented");
    }
}
