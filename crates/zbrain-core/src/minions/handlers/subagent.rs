//! Subagent handler — delegates a job to the AI gateway tool-loop.
//!
//! ## TS reference
//!
//! `src/core/minions/handlers/subagent.ts` — the TS subagent handler constructs
//! a gateway `SubagentSession` with brain tools + shell tool, then delegates to
//! the `toolLoop`. The Rust v1 keeps the same shape: build brain tools from the
//! engine, wire them via `to_gateway_tools`, and pass to `tool_loop`.
//!
//! ## v1 scope (grill Q4)
//!
//! - Gateway path only: the handler owns an `Arc<dyn ChatProvider>` injected at
//!   construction and passes it to `tool_loop`.
//! - No crash-resumability (deferred to 1-4-1-1): `NoopHooks` for persistence.
//! - No self-fix (deferred to 1-4-1-2): tool errors propagate as job failures.
//! - No shell tool (deferred to 1-4-5).
//!
//! ## Job data shape
//!
//! The job's `data` field is expected to have:
//! - `prompt` (required): the user-facing task description.
//! - `system` (optional): override the default subagent system prompt.
//!
//! Missing `prompt` → the handler returns an error (no implicit empty-task).

use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::ai::{
    tool_loop, ChatMessage, ChatProvider, ChatRole, NoopHooks, ToolLoopOpts,
};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::minions::tools::{build_brain_tools, to_gateway_tools};
use crate::{Error, Result};

const DEFAULT_SUBAGENT_SYSTEM: &str = "\
You are a helpful AI assistant with access to a knowledge base (brain). \
Use the available tools to look up information when needed, then compose \
a thorough, well-structured answer.";

/// Subagent handler v1 — processes one minion job by running the AI gateway
/// tool-loop with brain tools.
pub struct SubagentHandler {
    chat_provider: Arc<dyn ChatProvider>,
}

impl SubagentHandler {
    /// Create a subagent handler wired to the given chat provider.
    #[must_use]
    pub fn new(chat_provider: Arc<dyn ChatProvider>) -> Self {
        Self { chat_provider }
    }
}

#[async_trait]
impl MinionHandler for SubagentHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        // Extract prompt (required) and optional system override from job data.
        let prompt = ctx
            .data
            .get("prompt")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::new("InvalidJobData", "missing_prompt", "subagent job data must contain a non-empty \"prompt\" field")
            })?;

        let system = ctx
            .data
            .get("system")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SUBAGENT_SYSTEM);

        // Build brain tools from the engine backing this job.
        let engine = Arc::clone(ctx.engine());
        let brain_tools = build_brain_tools(None);
        let (chat_tool_defs, handlers_map) =
            to_gateway_tools(&brain_tools, Arc::clone(&engine), ctx.signal.clone());

        // Run the gateway tool-loop. The per-job cancellation token is wired as
        // the abort closure so a timeout/cancel/pause/lock-loss stops the loop.
        let signal = ctx.signal.clone();
        let abort = move || signal.is_cancelled();

        let opts = ToolLoopOpts {
            system: Some(system.to_string()),
            initial_messages: vec![ChatMessage::text(ChatRole::User, prompt)],
            tools: chat_tool_defs,
            ..Default::default()
        };

        let result = tool_loop(
            self.chat_provider.as_ref(),
            opts,
            &handlers_map,
            &NoopHooks,
            &abort,
        )
        .await
        .map_err(|e| {
            Error::engine(format!("subagent tool_loop failed: {e}"))
        })?;

        Ok(json!({
            "result": result.final_text,
            "total_turns": result.total_turns,
            "stop_reason": format!("{:?}", result.stop_reason),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::MockChatProvider;
    use crate::minions::handler::MinionJobContext;
    use crate::InMemoryEngine;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<InMemoryEngine> {
        Arc::new(InMemoryEngine::new())
    }

    fn ctx(engine: &Arc<InMemoryEngine>, data: Value) -> MinionJobContext {
        let engine: Arc<dyn crate::engine::BrainEngine> = Arc::clone(engine) as Arc<_>;
        MinionJobContext::new(
            engine,
            1,
            "subagent".to_string(),
            data,
            0,
            "test-token".to_string(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn subagent_handler_delegates_to_chat_provider() {
        let engine = engine();
        let mock = Arc::new(MockChatProvider::new("the answer is 42"));
        let handler = SubagentHandler::new(mock);
        let context = ctx(&engine, json!({"prompt": "what is the answer?"}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["result"], "the answer is 42");
        assert_eq!(result["total_turns"], 0); // single turn, no tool calls
    }

    #[tokio::test]
    async fn subagent_handler_with_custom_system_prompt() {
        let engine = engine();
        let mock = Arc::new(MockChatProvider::new("ok"));
        let handler = SubagentHandler::new(mock);
        let context = ctx(
            &engine,
            json!({"prompt": "hi", "system": "You are a pirate."}),
        );

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["result"], "ok");
    }

    #[tokio::test]
    async fn subagent_handler_errors_on_missing_prompt() {
        let engine = engine();
        let mock = Arc::new(MockChatProvider::new("unused"));
        let handler = SubagentHandler::new(mock);

        // Empty prompt
        let context = ctx(&engine, json!({"prompt": ""}));
        let err = handler.handle(&context).await.unwrap_err();
        assert!(err.to_string().contains("prompt"), "should mention prompt: {err}");

        // No prompt field at all
        let context = ctx(&engine, json!({"system": "be helpful"}));
        let err = handler.handle(&context).await.unwrap_err();
        assert!(err.to_string().contains("prompt"), "should mention prompt: {err}");
    }

    #[tokio::test]
    async fn subagent_handler_respects_cancellation() {
        let engine = engine();
        let mock = Arc::new(MockChatProvider::new("unused"));
        let handler = SubagentHandler::new(mock);

        let signal = CancellationToken::new();
        signal.cancel(); // pre-cancel before handle

        let engine_arc: Arc<dyn crate::engine::BrainEngine> = Arc::clone(&engine) as Arc<_>;
        let context = MinionJobContext::new(
            engine_arc,
            1,
            "subagent".to_string(),
            json!({"prompt": "do something long"}),
            0,
            "test-token".to_string(),
            signal,
            CancellationToken::new(),
        );

        let result = handler.handle(&context).await;
        // tool_loop catches the abort and returns Aborted, which is not an error
        // — it's a normal ToolLoopResult with stop_reason Aborted.
        match result {
            Ok(val) => {
                assert!(
                    val["stop_reason"].as_str().unwrap_or("").contains("Aborted"),
                    "expected Aborted stop_reason, got: {val}"
                );
            }
            Err(e) => {
                // Also acceptable: ChatError propagates as an engine error
                assert!(e.to_string().contains("tool_loop") || e.to_string().contains("abort"),
                    "unexpected error: {e}");
            }
        }
    }

    #[tokio::test]
    async fn subagent_handler_is_object_safe() {
        // Verify SubagentHandler can be stored as Arc<dyn MinionHandler>.
        let engine = engine();
        let mock = Arc::new(MockChatProvider::new("object safe"));
        let handler: Arc<dyn MinionHandler> = Arc::new(SubagentHandler::new(mock));
        let context = ctx(&engine, json!({"prompt": "test"}));

        let result = handler.handle(&context).await.expect("handle should succeed");
        assert_eq!(result["result"], "object safe");
    }
}
