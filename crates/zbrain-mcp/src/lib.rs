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

use serde::{Deserialize, Serialize};
use serde_json::Value;
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
// JSON-RPC 2.0 Message Types
// ──────────────────────────────────────────────────────────────────────────

/// Incoming JSON-RPC 2.0 request.
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

/// Outgoing JSON-RPC 2.0 response.
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    fn success(id: Option<Value>, result: Value) -> Self {
        JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: impl Into<String>) -> Self {
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
struct JsonRpcError {
    code: i32,
    message: String,
}

// ──────────────────────────────────────────────────────────────────────────
// MCP Protocol Constants (JSON-RPC error codes)
// ──────────────────────────────────────────────────────────────────────────

const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;

// ──────────────────────────────────────────────────────────────────────────
// Stdio MCP Server
// ──────────────────────────────────────────────────────────────────────────

/// Stdio MCP server: reads JSON-RPC 2.0 messages from stdin, writes to stdout.
///
/// Mirrors `startMcpServer()` in TS `src/mcp/server.ts`.
pub struct StdioMcpServer {
    registry: Arc<OperationRegistry>,
    server_name: String,
    server_version: String,
}

impl StdioMcpServer {
    /// Create a new stdio MCP server.
    pub fn new(
        registry: OperationRegistry,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
    ) -> Self {
        StdioMcpServer {
            registry: Arc::new(registry),
            server_name: server_name.into(),
            server_version: server_version.into(),
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

        let tool_params = params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // MCP stdio callers are remote/untrusted by convention (matches TS server.ts)
        // Default source to "default" for stdio (no per-token scope, mirrors TS server.ts)
        let source_id = std::env::var("ZBRAIN_SOURCE").unwrap_or_else(|_| "default".to_string());
        let ctx = OperationContext::remote_mcp(source_id);

        let tool_result = self
            .registry
            .dispatch_tool_call(&name, &ctx, tool_params)
            .await;

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

    #[tokio::test]
    async fn stdio_server_handle_initialize() {
        let registry = make_registry();
        let server = StdioMcpServer::new(registry, "zbrain", "0.0.1");

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
        let registry = make_registry();
        let server = StdioMcpServer::new(registry, "zbrain", "0.0.1");

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
        let registry = make_registry();
        let server = StdioMcpServer::new(registry, "zbrain", "0.0.1");

        let line = r#"{"jsonrpc":"2.0","id":3,"method":"nonexistent/method","params":{}}"#;
        let response = server.handle_line(line).await;

        assert!(response.error.is_some());
        assert_eq!(response.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn stdio_server_tools_call_local_only_rejected() {
        let registry = make_registry();
        let server = StdioMcpServer::new(registry, "zbrain", "0.0.1");

        // put_page is local_only — should return a ToolResult with isError=true
        // (not a JSON-RPC error, but the content body indicates an error)
        let line = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"put_page","arguments":{"slug":"test","compiled_truth":"hello"}}}"#;
        let response = server.handle_line(line).await;

        // The JSON-RPC call itself succeeds (result is Some), but the tool result body says isError
        assert!(response.result.is_some(), "JSON-RPC call should succeed even for error tool results");
        let result = response.result.unwrap();
        assert_eq!(result["isError"], true, "local_only op via MCP should return isError=true");
    }
}
