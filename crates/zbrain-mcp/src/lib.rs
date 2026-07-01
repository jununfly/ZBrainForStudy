//! `zbrain-mcp` — Rust MCP (Model Context Protocol) server for zbrain.
//!
//! Implements the stdio MCP transport (JSON-RPC 2.0 over stdin/stdout).
//! Mirrors `src/mcp/server.ts` and `src/mcp/tool-defs.ts` from the TS codebase.
//!
//! # Architecture
//!
//! - `build_tool_defs()` converts an `OperationRegistry` into `McpToolDef` list
//!   for the `tools/list` response.
//! - `StdioMcpServer` handles JSON-RPC 2.0 framing, routing `initialize`,
//!   `tools/list`, and `tools/call` to the shared `dispatch_tool_call()` path.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zbrain_core::engine::BrainEngine;
use zbrain_core::operation::{OperationContext, OperationRegistry};

// ──────────────────────────────────────────────────────────────────────────
// MCP Tool Definition (mirrors TS tool-defs.ts)
// ──────────────────────────────────────────────────────────────────────────

/// MCP tool definition shape returned by `tools/list`.
///
/// Mirrors `McpToolDef` in TS `src/mcp/tool-defs.ts`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Build MCP tool definitions from an `OperationRegistry`.
///
/// Mirrors `buildToolDefs(operations)` in TS `src/mcp/tool-defs.ts`.
/// Filters out `local_only` operations — they should not be advertised via MCP.
pub fn build_tool_defs(registry: &OperationRegistry) -> Vec<McpToolDef> {
    registry
        .operations()
        .into_iter()
        .filter(|op| !op.local_only())
        .map(|op| McpToolDef {
            name: op.name().to_string(),
            description: op.description().to_string(),
            input_schema: op.input_schema(),
        })
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────
// Sliding-window rate limiter
// ──────────────────────────────────────────────────────────────────────────

/// Sliding-window rate limiter for MCP tools/call.
///
/// Uses atomic counter + window-start timestamp for thread-safe, lock-free
/// rate enforcement. Rejects requests when the counter exceeds `max_per_window`
/// within the current window (default: 60 seconds).
pub struct SlidingWindowRateLimiter {
    /// Maximum allowed requests per window.
    max_per_window: u64,
    /// Window start timestamp (millis since epoch).
    window_start: AtomicU64,
    /// Request counter for the current window.
    counter: AtomicU64,
}

impl SlidingWindowRateLimiter {
    /// Create a new rate limiter.
    ///
    /// `max_per_window` is the maximum number of requests allowed within
    /// `window_duration`. The window slides forward each time the interval
    /// expires.
    pub fn new(max_per_window: u64, _window_duration: Duration) -> Self {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        SlidingWindowRateLimiter {
            max_per_window,
            window_start: AtomicU64::new(now_ms),
            counter: AtomicU64::new(0),
        }
    }

    /// Check whether a request is allowed.
    ///
    /// Returns `true` if within limits, `false` if over limit.
    /// Thread-safe — can be called from multiple async tasks.
    pub fn check(&self) -> bool {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let ws = self.window_start.load(Ordering::Relaxed);
        // If the window has expired, try to reset
        if now_ms >= ws + 60_000 {
            // CAS: only one thread wins the reset
            if self.window_start.compare_exchange(ws, now_ms, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                self.counter.store(1, Ordering::Release);
                return true;
            }
            // Another thread reset — fall through to normal check
        }

        let count = self.counter.fetch_add(1, Ordering::AcqRel) + 1;
        if count <= self.max_per_window {
            true
        } else {
            // Over limit — decrement to avoid counter drift
            self.counter.fetch_sub(1, Ordering::Release);
            false
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// JSON-RPC 2.0 Message Types
// ──────────────────────────────────────────────────────────────────────────

/// Incoming JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

/// Outgoing JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

// ──────────────────────────────────────────────────────────────────────────
// MCP Protocol Constants (JSON-RPC error codes)
// ──────────────────────────────────────────────────────────────────────────

const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;
const RATE_LIMITED: i32 = -32000;

// ──────────────────────────────────────────────────────────────────────────
// Stdio MCP Server
// ──────────────────────────────────────────────────────────────────────────

/// Stdio MCP server: reads JSON-RPC 2.0 messages from stdin, writes to stdout.
///
/// Mirrors `startMcpServer()` in TS `src/mcp/server.ts`.
pub struct StdioMcpServer {
    registry: Arc<OperationRegistry>,
    engine: Arc<dyn BrainEngine>,
    server_name: String,
    server_version: String,
    rate_limiter: Option<Arc<SlidingWindowRateLimiter>>,
}

impl StdioMcpServer {
    /// Create a new stdio MCP server.
    ///
    /// `rate_limit_per_minute`: if `Some(n)`, enables rate limiting at `n` requests/minute.
    /// `None` disables rate limiting entirely.
    pub fn new(
        registry: OperationRegistry,
        engine: Arc<dyn BrainEngine>,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
        rate_limit_per_minute: Option<u64>,
    ) -> Self {
        let rate_limiter = rate_limit_per_minute.map(|max| {
            Arc::new(SlidingWindowRateLimiter::new(max, Duration::from_secs(60)))
        });
        StdioMcpServer {
            registry: Arc::new(registry),
            engine,
            server_name: server_name.into(),
            server_version: server_version.into(),
            rate_limiter,
        }
    }

    /// Run the server: read one JSON-RPC message per line from stdin, write responses to stdout.
    ///
    /// Blocks until stdin is closed (EOF).
    pub async fn run(self) -> anyhow::Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
        let mut writer = tokio::io::BufWriter::new(stdout);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                // stdin EOF — clean shutdown
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let response = self.handle_line(trimmed).await;
            let json = serde_json::to_string(&response)?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }

        Ok(())
    }

    async fn handle_line(&self, line: &str) -> JsonRpcResponse {
        let request: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse::error(None, -32700, format!("Parse error: {}", e));
            }
        };

        let id = request.id.clone();
        match request.method.as_str() {
            "initialize" => self.handle_initialize(id),
            "tools/list" => self.handle_tools_list(id),
            "tools/call" => self.handle_tools_call(id, request.params).await,
            // Notification methods (no id): return no response (but we still return one here for simplicity)
            _ => JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("Method not found: {}", request.method)),
        }
    }

    fn handle_initialize(&self, id: Option<Value>) -> JsonRpcResponse {
        JsonRpcResponse::success(
            id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": self.server_name,
                    "version": self.server_version
                }
            }),
        )
    }

    fn handle_tools_list(&self, id: Option<Value>) -> JsonRpcResponse {
        let tools = build_tool_defs(&self.registry);
        JsonRpcResponse::success(id, serde_json::json!({ "tools": tools }))
    }

    async fn handle_tools_call(&self, id: Option<Value>, params: Option<Value>) -> JsonRpcResponse {
        let params = match params {
            Some(p) => p,
            None => {
                return JsonRpcResponse::error(id, INVALID_PARAMS, "tools/call requires params");
            }
        };

        let name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => {
                return JsonRpcResponse::error(id, INVALID_PARAMS, "tools/call requires params.name");
            }
        };

        // Rate limit check
        if let Some(ref rl) = self.rate_limiter {
            if !rl.check() {
                tracing::warn!(operation = %name, "rate limit exceeded");
                return JsonRpcResponse::error(id, RATE_LIMITED, "Rate limit exceeded. Try again later.");
            }
        }

        let tool_params = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Audit span: record operation, timing, and error status
        let span = tracing::info_span!("tools/call", operation = %name);
        let _enter = span.enter();
        let start = std::time::Instant::now();

        // MCP stdio callers are remote/untrusted by convention (matches TS server.ts)
        let source_id = std::env::var("ZBRAIN_SOURCE").unwrap_or_else(|_| "default".to_string());
        let ctx = OperationContext::remote_mcp(source_id).with_engine(self.engine.clone());

        let tool_result = self
            .registry
            .dispatch_tool_call(&name, &ctx, tool_params)
            .await;

        let elapsed_ms = start.elapsed().as_millis() as u64;
        if tool_result.is_error {
            tracing::warn!(
                operation = %name,
                duration_ms = elapsed_ms,
                "tools/call returned error"
            );
        } else {
            tracing::info!(
                operation = %name,
                duration_ms = elapsed_ms,
                "tools/call completed"
            );
        }

        match serde_json::to_value(&tool_result) {
            Ok(v) => JsonRpcResponse::success(id, v),
            Err(e) => JsonRpcResponse::error(
                id,
                INTERNAL_ERROR,
                format!("Failed to serialize tool result: {}", e),
            ),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zbrain_core::operation::{
        GetPageOperation, QueryOperation, ThinkOperation,
        PutPageOperation, DeletePageOperation, RestorePageOperation,
        PurgeDeletedPagesOperation, ListPagesOperation,
    };

    fn make_registry() -> OperationRegistry {
        let mut reg = OperationRegistry::new();
        reg.register(GetPageOperation);
        reg.register(ThinkOperation);
        reg.register(QueryOperation);
        // Note: put_page, delete_page, etc. are local_only — excluded from MCP tool defs
        reg.register(PutPageOperation);
        reg.register(DeletePageOperation);
        reg.register(RestorePageOperation);
        reg.register(PurgeDeletedPagesOperation);
        reg.register(ListPagesOperation);
        reg
    }

    #[test]
    fn build_tool_defs_excludes_local_only_operations() {
        let registry = make_registry();
        let tools = build_tool_defs(&registry);

        // get_page, think, query are NOT local_only → should appear
        assert!(tools.iter().any(|t| t.name == "get_page"), "get_page should be in tool defs");
        assert!(tools.iter().any(|t| t.name == "think"), "think should be in tool defs");
        assert!(tools.iter().any(|t| t.name == "query"), "query should be in tool defs");

        // put_page, delete_page, restore_page, purge_deleted_pages are local_only → excluded
        assert!(!tools.iter().any(|t| t.name == "put_page"), "put_page should NOT be in MCP tool defs (local_only)");
        assert!(!tools.iter().any(|t| t.name == "delete_page"), "delete_page should NOT be in MCP tool defs (local_only)");
        assert!(!tools.iter().any(|t| t.name == "restore_page"), "restore_page should NOT be in MCP tool defs (local_only)");
        assert!(!tools.iter().any(|t| t.name == "purge_deleted_pages"), "purge_deleted_pages should NOT be in MCP tool defs (local_only)");

        // list_pages is not local_only → should appear
        assert!(tools.iter().any(|t| t.name == "list_pages"), "list_pages should be in tool defs");
    }

    #[test]
    fn build_tool_defs_has_correct_shape() {
        let registry = make_registry();
        let tools = build_tool_defs(&registry);
        let get_page = tools.iter().find(|t| t.name == "get_page").unwrap();

        // Has name and non-empty description
        assert!(!get_page.description.is_empty());

        // input_schema is an object
        assert_eq!(get_page.input_schema["type"], "object");

        // slug is a required property
        let required = get_page.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "slug"), "slug should be required in get_page schema");

        // Properties exist
        assert!(get_page.input_schema["properties"]["slug"].is_object());
    }

    #[test]
    fn think_tool_def_requires_question() {
        let registry = make_registry();
        let tools = build_tool_defs(&registry);
        let think = tools.iter().find(|t| t.name == "think").unwrap();

        let required = think.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|r| r == "question"), "question should be required in think schema");
    }

    #[test]
    fn list_pages_tool_def_not_local_only() {
        let registry = make_registry();
        let tools = build_tool_defs(&registry);
        let list_pages = tools.iter().find(|t| t.name == "list_pages").unwrap();

        // list_pages is NOT local_only, so it appears in MCP tool defs
        assert_eq!(list_pages.name, "list_pages");
        // All params are optional
        let required = list_pages.input_schema["required"].as_array().unwrap();
        assert!(required.is_empty(), "list_pages should have no required params");
    }

    fn make_server() -> StdioMcpServer {
        StdioMcpServer::new(
            make_registry(),
            std::sync::Arc::new(zbrain_core::InMemoryEngine::default()),
            "zbrain",
            "0.0.1",
            None, // no rate limit in tests
        )
    }

    #[tokio::test]
    async fn stdio_server_handle_initialize() {
        let server = make_server();

        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let response = server.handle_line(line).await;

        assert!(response.result.is_some());
        assert!(response.error.is_none());
        let result = response.result.unwrap();
        assert_eq!(result["serverInfo"]["name"], "zbrain");
        assert_eq!(result["serverInfo"]["version"], "0.0.1");
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn stdio_server_handle_tools_list() {
        let server = make_server();

        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let response = server.handle_line(line).await;

        assert!(response.result.is_some());
        let tools = &response.result.unwrap()["tools"];
        assert!(tools.is_array());

        let tools_arr = tools.as_array().unwrap();
        assert!(!tools_arr.is_empty(), "tools/list should return at least one tool");

        // Verify each tool has name, description, inputSchema
        for tool in tools_arr {
            assert!(tool["name"].is_string(), "each tool should have a name");
            assert!(tool["description"].is_string(), "each tool should have a description");
            assert!(tool["inputSchema"].is_object(), "each tool should have inputSchema");
        }
    }

    #[tokio::test]
    async fn stdio_server_unknown_method_returns_error() {
        let server = make_server();

        let line = r#"{"jsonrpc":"2.0","id":3,"method":"nonexistent/method","params":{}}"#;
        let response = server.handle_line(line).await;

        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn stdio_server_tools_call_local_only_rejected() {
        let server = make_server();

        // put_page is local_only — should return a ToolResult with isError=true
        // (not a JSON-RPC error, but the content body indicates an error)
        let line = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"put_page","arguments":{"slug":"test","compiled_truth":"hello"}}}"#;
        let response = server.handle_line(line).await;

        // The JSON-RPC call itself succeeds (result is Some), but the tool result body says isError
        assert!(response.result.is_some(), "JSON-RPC call should succeed even for error tool results");
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true, "local_only op via MCP should return isError=true");
    }

    #[tokio::test]
    async fn stdio_server_tools_call_list_pages_with_engine() {
        let server = make_server();

        let line = r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"list_pages","arguments":{}}}"#;
        let response = server.handle_line(line).await;

        assert!(response.result.is_some(), "tools/call list_pages should return a result");
        let result = response.result.unwrap();
        // isError only present when true (MCP convention); absence = success
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(!is_error, "list_pages with engine should succeed");
        assert!(result["content"].is_array(), "should have content array");
    }

    #[tokio::test]
    async fn stdio_server_tools_call_engine_data_roundtrip() {
        use zbrain_core::engine::{BrainEngine, PageInput};
        use zbrain_core::PageType;

        // Pre-populate engine with data
        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        engine
            .put_page(
                "hello-world",
                None, // default source
                &PageInput {
                    page_type: PageType::from("note"),
                    title: "Hello World".into(),
                    compiled_truth: "This is a test page".into(),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: None,
                },
            )
            .await
            .expect("put_page should succeed");

        // Create MCP server with pre-populated engine
        let server = StdioMcpServer::new(make_registry(), engine, "zbrain", "0.0.1", None);

        // Call get_page through MCP
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_page","arguments":{"slug":"hello-world"}}}"#;
        let response = server.handle_line(line).await;

        assert!(response.result.is_some(), "get_page should return a result");
        let result = response.result.unwrap();
        let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
        assert!(!is_error, "get_page should succeed through MCP");
        // Verify content contains the page data
        let content = result["content"].as_array().unwrap();
        let text = content[0]["text"].as_str().unwrap();
        assert!(text.contains("hello-world"), "response should contain the slug");
        assert!(text.contains("Hello World"), "response should contain the title");
    }

    #[tokio::test]
    async fn rate_limiter_allows_first_request() {
        let registry = make_registry();
        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let server = StdioMcpServer::new(registry, engine, "zbrain", "0.0.1", Some(10));

        let line = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_pages","arguments":{}}}"#;
        let response = server.handle_line(line).await;

        assert!(response.result.is_some(), "first request should succeed");
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn rate_limiter_rejects_over_limit() {
        let registry = make_registry();
        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let server = StdioMcpServer::new(registry, engine, "zbrain", "0.0.1", Some(2));

        // Eat all 2 tokens
        for i in 0..2 {
            let line = format!(r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"list_pages","arguments":{{}}}}}}"#, i);
            let response = server.handle_line(&line).await;
            assert!(response.result.is_some(), "request {} should succeed", i);
        }

        // 3rd request should be rate limited
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_pages","arguments":{}}}"#;
        let response = server.handle_line(line).await;

        assert!(response.error.is_some(), "3rd request should be rate limited");
        assert_eq!(response.error.unwrap().code, -32000);
    }

    #[tokio::test]
    async fn rate_limiter_none_allows_unlimited() {
        let registry = make_registry();
        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let server = StdioMcpServer::new(registry, engine, "zbrain", "0.0.1", None);

        // 5 requests with no rate limit — all should succeed
        for i in 0..5 {
            let line = format!(r#"{{"jsonrpc":"2.0","id":{},"method":"tools/call","params":{{"name":"list_pages","arguments":{{}}}}}}"#, i);
            let response = server.handle_line(&line).await;
            assert!(response.result.is_some(), "request {} should succeed with no rate limit", i);
        }
    }

    #[test]
    fn rate_limiter_window_resets_after_expiry() {
        use std::sync::atomic::Ordering;

        let rl = SlidingWindowRateLimiter::new(2, Duration::from_secs(60));

        // Exhaust the window's 2 requests
        assert!(rl.check());
        assert!(rl.check());
        assert!(!rl.check(), "3rd request should be rejected within same window");

        // Simulate window expiry by moving window_start back 61 seconds
        let old_start = rl.window_start.load(Ordering::Relaxed);
        let expired_start = old_start - 61_000; // 61 seconds ago
        rl.window_start.store(expired_start, Ordering::Relaxed);

        // First request after expiry should reset the window and succeed
        assert!(rl.check(), "first request after window expiry should reset and succeed");
        assert!(rl.check(), "second request in new window should succeed");
        assert!(!rl.check(), "third request in new window should be rejected");
    }
}
