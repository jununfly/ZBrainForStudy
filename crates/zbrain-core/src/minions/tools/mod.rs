//! Brain tools for subagent handlers (roadmap 1-4-1).
//!
//! Defines the [`ToolDef`] trait — a provider-neutral tool contract that
//! individual tool implementations (search, read page, write page, …) satisfy.
//! The adapter [`to_gateway_tools`] bridges this world into the AI
//! [`tool_loop`](crate::ai::tool_loop) types ([`ChatToolDef`] + [`ToolHandler`])
//! so a subagent handler can pass tools to the LLM loop without zbrain-core
//! depending on any gateway crate.
//!
//! ## Registration
//!
//! [`build_brain_tools`] is the single entry-point: it takes an optional
//! allowlist of op names and returns `Vec<Arc<dyn ToolDef>>`. The static
//! [`BRAIN_TOOL_ALLOWLIST`] is the global safe set — no tool outside it can ever
//! be registered for a subagent.
//!
//! ## TS reference
//!
//! - `ToolDef` interface — `src/core/minions/types.ts` L528-538
//! - `buildBrainTools` — `src/core/minions/tools/brain-allowlist.ts` L233
//! - `BRAIN_TOOL_ALLOWLIST` — `brain-allowlist.ts` L48-67
//! - Tool adapter: `src/core/ai/gateway.ts` (TS does inline mapping, Rust makes
//!   it explicit via [`to_gateway_tools`])

pub mod brain;

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::ai::chat::ChatToolDef;
use crate::ai::tool_loop::ToolHandler;
use crate::engine::BrainEngine;
use crate::Result;

// ─── ToolDef trait ───────────────────────────────────────────────────────────

/// A brain tool the subagent may invoke. Mirrors the TS `ToolDef` interface.
///
/// Object-safe by design: tools are stored as `Arc<dyn ToolDef>` and the
/// adapter bridges them to `Box<dyn ToolHandler>` for the LLM loop.
///
/// ## Idempotency
///
/// Defaults to `true` (most brain tools are read-only). A non-idempotent tool
/// (e.g. `put_page`) MUST override this to `false` so the crash-replay logic in
/// `tool_loop` knows it cannot safely re-execute a pending row.
#[async_trait]
pub trait ToolDef: Send + Sync {
    /// Tool name as exposed to the LLM, e.g. `"brain_resolve_slugs"`.
    fn name(&self) -> &str;

    /// Human-readable description injected into the system prompt.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's input parameters.
    fn input_schema(&self) -> Value;

    /// Whether this tool is safe to re-run after a crash-mid-execute. Defaults
    /// to `true` (read-only tools). Write tools MUST return `false`.
    fn idempotent(&self) -> bool {
        true
    }

    /// Optional one-line hint for the system prompt about when to use this
    /// tool. Mirrors TS `BRAIN_TOOL_USAGE_HINTS`.
    fn usage_hint(&self) -> Option<&str> {
        None
    }

    /// Execute the tool. `input` is the LLM-supplied JSON arguments.
    /// `engine` provides access to the brain's data layer.
    /// `signal` lets cooperative tools abort mid-flight on timeout/cancel.
    async fn execute(
        &self,
        input: Value,
        engine: Arc<dyn BrainEngine>,
        signal: CancellationToken,
    ) -> Result<Value>;
}

// ─── Allowlist ───────────────────────────────────────────────────────────────

/// Global safe-set of op names that may be exposed as brain tools.
///
/// Mirrors TS `BRAIN_TOOL_ALLOWLIST` (`brain-allowlist.ts` L48-67). Any tool
/// whose op name is NOT in this set will be silently excluded by
/// [`build_brain_tools`] — even if an individual `ToolDef` impl exists for it.
///
/// Currently registered (10 of 13 TS originals; 3 deferred → KNOWN-GAPS):
/// - resolve_slugs, get_backlinks, get_recent_salience, list_pages
/// - get_page, search, traverse_graph, put_page
/// - query (search_pages hybrid), traverse_paths
pub static BRAIN_TOOL_ALLOWLIST: &[&str] = &[
    "resolve_slugs",
    "get_backlinks",
    "get_recent_salience",
    "list_pages",
    "get_page",
    "search",
    "traverse_graph",
    "put_page",
];

// ─── build_brain_tools ───────────────────────────────────────────────────────

/// Build the list of `ToolDef` instances for a subagent invocation.
///
/// `allowed_names` is an optional second filter applied on top of
/// [`BRAIN_TOOL_ALLOWLIST`]. When `None`, all allowlisted tools are included.
/// When `Some(names)`, only the intersection of allowlist ∩ names is returned.
///
/// Mirrors TS `buildBrainTools` (`brain-allowlist.ts` L233).
pub fn build_brain_tools(
    allowed_names: Option<&[String]>,
) -> Vec<Arc<dyn ToolDef>> {
    let mut tools: Vec<Arc<dyn ToolDef>> = Vec::new();

    // Register all tool implementations.
    brain::register_all(&mut tools);

    // Apply allowlist filter.
    if let Some(ref names) = allowed_names {
        tools.retain(|t| {
            let op_name = t.name().strip_prefix("brain_").unwrap_or(t.name());
            BRAIN_TOOL_ALLOWLIST.contains(&op_name) && names.iter().any(|n| n == op_name)
        });
    } else {
        tools.retain(|t| {
            let op_name = t.name().strip_prefix("brain_").unwrap_or(t.name());
            BRAIN_TOOL_ALLOWLIST.contains(&op_name)
        });
    }

    tools
}

// ─── to_gateway_tools adapter ────────────────────────────────────────────────

/// Bridge from [`ToolDef`] to the AI module's tool types.
///
/// Returns the pair `(Vec<ChatToolDef>, HashMap<String, Box<dyn ToolHandler>>)`
/// expected by [`tool_loop`](crate::ai::tool_loop::tool_loop). The returned
/// handlers capture `engine` + `signal` by value, so they are self-contained
/// and can be moved into the loop without lifetime entanglement.
///
/// Mirrors the TS inline mapping in `buildBrainTools` → `gateway.toolLoop`.
pub fn to_gateway_tools(
    tools: &[Arc<dyn ToolDef>],
    engine: Arc<dyn BrainEngine>,
    signal: CancellationToken,
) -> (Vec<ChatToolDef>, HashMap<String, Box<dyn ToolHandler>>) {
    let mut defs = Vec::with_capacity(tools.len());
    let mut handlers: HashMap<String, Box<dyn ToolHandler>> =
        HashMap::with_capacity(tools.len());

    for tool in tools {
        defs.push(ChatToolDef {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            input_schema: tool.input_schema(),
        });

        let bridge = ToolHandlerBridge {
            tool: Arc::clone(tool),
            engine: Arc::clone(&engine),
            signal: signal.clone(),
        };
        handlers.insert(tool.name().to_string(), Box::new(bridge));
    }

    (defs, handlers)
}

/// Private bridge: a [`ToolHandler`] that delegates to a [`ToolDef`].
struct ToolHandlerBridge {
    tool: Arc<dyn ToolDef>,
    engine: Arc<dyn BrainEngine>,
    signal: CancellationToken,
}

#[async_trait]
impl ToolHandler for ToolHandlerBridge {
    fn idempotent(&self) -> bool {
        self.tool.idempotent()
    }

    async fn execute(
        &self,
        input: Value,
    ) -> std::result::Result<Value, String> {
        self.tool
            .execute(input, Arc::clone(&self.engine), self.signal.clone())
            .await
            .map_err(|e| e.to_string())
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InMemoryEngine;
    use serde_json::json;

    // A concrete tool for adapter tests.
    struct EchoTool;
    #[async_trait]
    impl ToolDef for EchoTool {
        fn name(&self) -> &str {
            "test_echo"
        }
        fn description(&self) -> &str {
            "echoes input"
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object", "properties": {}})
        }
        async fn execute(
            &self,
            input: Value,
            _engine: Arc<dyn BrainEngine>,
            _signal: CancellationToken,
        ) -> Result<Value> {
            Ok(input)
        }
    }

    // ── build_brain_tools with full registry ─────────────────────────────

    #[test]
    fn build_brain_tools_returns_all_eight_tools_when_no_filter() {
        let tools = build_brain_tools(None);
        assert_eq!(tools.len(), 8, "all 8 allowlisted tools should be registered");
    }

    #[test]
    fn build_brain_tools_filters_by_allowed_names() {
        let allowed = vec!["search".to_string()];
        let tools = build_brain_tools(Some(&allowed));
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "brain_search");
    }

    #[test]
    fn build_brain_tools_returns_empty_when_no_allowed_names_match() {
        let allowed = vec!["nonexistent".to_string()];
        let tools = build_brain_tools(Some(&allowed));
        assert!(tools.is_empty());
    }

    // ── to_gateway_tools adapter ─────────────────────────────────────────

    #[tokio::test]
    async fn adapter_produces_chat_tool_def_with_correct_name_and_schema() {
        let tool: Arc<dyn ToolDef> = Arc::new(EchoTool);
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let signal = CancellationToken::new();

        let (defs, _handlers) =
            to_gateway_tools(&[tool], engine, signal);

        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "test_echo");
        assert_eq!(defs[0].description, "echoes input");
        assert_eq!(
            defs[0].input_schema,
            json!({"type": "object", "properties": {}})
        );
    }

    #[tokio::test]
    async fn adapter_handler_delegates_execute_to_tool() {
        let tool: Arc<dyn ToolDef> = Arc::new(EchoTool);
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let signal = CancellationToken::new();

        let (_, handlers) =
            to_gateway_tools(&[tool], engine, signal);

        let handler = handlers.get("test_echo").expect("handler registered");
        let output = handler.execute(json!({"msg": "hello"})).await.unwrap();
        assert_eq!(output, json!({"msg": "hello"}));
    }

    #[tokio::test]
    async fn adapter_handler_reports_idempotent() {
        let tool: Arc<dyn ToolDef> = Arc::new(EchoTool);
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let signal = CancellationToken::new();

        let (_, handlers) =
            to_gateway_tools(&[tool], engine, signal);

        let handler = handlers.get("test_echo").expect("handler registered");
        assert!(handler.idempotent());
    }

    #[tokio::test]
    async fn adapter_handles_multiple_tools() {
        struct GreetTool;
        #[async_trait]
        impl ToolDef for GreetTool {
            fn name(&self) -> &str {
                "test_greet"
            }
            fn description(&self) -> &str {
                "greets"
            }
            fn input_schema(&self) -> Value {
                json!({})
            }
            async fn execute(
                &self,
                _input: Value,
                _engine: Arc<dyn BrainEngine>,
                _signal: CancellationToken,
            ) -> Result<Value> {
                Ok(json!("hello"))
            }
        }

        let tools: Vec<Arc<dyn ToolDef>> =
            vec![Arc::new(EchoTool), Arc::new(GreetTool)];
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        let signal = CancellationToken::new();

        let (defs, handlers) =
            to_gateway_tools(&tools, engine, signal);

        assert_eq!(defs.len(), 2);
        assert_eq!(handlers.len(), 2);
        assert!(handlers.contains_key("test_echo"));
        assert!(handlers.contains_key("test_greet"));
    }
}
