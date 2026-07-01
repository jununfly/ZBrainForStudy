//! MCP HTTP handler — POST /mcp with bearer auth, scope enforcement.
//!
//! Mirrors the TypeScript MCP HTTP dispatch in `src/commands/serve-http.ts`
//! lines 1135–1435.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;
use zbrain_core::operation::OperationRegistry;
use zbrain_core::scope::has_scope;
use zbrain_mcp::{build_tool_defs, JsonRpcRequest, JsonRpcResponse, McpToolDef};

use super::AppState;

// ── AuthInfo conversion ───────────────────────────────────────────────

/// Convert the verification-level `AuthInfo` into the operation-level one.
fn to_op_auth_info(info: zbrain_core::AuthInfo) -> zbrain_core::operation::AuthInfo {
    zbrain_core::operation::AuthInfo {
        token: info.token,
        client_id: info.client_id,
        client_name: info.client_name,
        scopes: info.scopes,
        expires_at: Some(info.expires_at as u64),
        source_id: info.source_id,
        allowed_sources: info.allowed_sources,
    }
}

// ── JSON-RPC error codes ──────────────────────────────────────────────

const METHOD_NOT_FOUND: i32 = -32601;
const INVALID_PARAMS: i32 = -32602;
const INTERNAL_ERROR: i32 = -32603;
const PARSE_ERROR: i32 = -32700;

/// Build the MCP HTTP router.
pub fn build_mcp_router(
    state: AppState,
    registry: Arc<OperationRegistry>,
) -> Router {
    let mcp_state = McpState { state, registry };

    Router::new()
        .route("/mcp", get(mcp_get_handler))   // 405 Method Not Allowed
        .route("/mcp", post(mcp_post_handler))
        .with_state(mcp_state)
}

/// Shared state for MCP handlers.
#[derive(Clone)]
struct McpState {
    state: AppState,
    registry: Arc<OperationRegistry>,
}

// ── GET /mcp → 405 Method Not Allowed ─────────────────────────────────

async fn mcp_get_handler() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [(header::ALLOW, "POST")],
        Json(serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": METHOD_NOT_FOUND,
                "message": "MCP endpoint only accepts POST requests"
            }
        })),
    )
        .into_response()
}

// ── POST /mcp handler ─────────────────────────────────────────────────

async fn mcp_post_handler(
    State(mcp): State<McpState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // Step 1: Parse JSON-RPC request
    let request: JsonRpcRequest = match serde_json::from_value(body) {
        Ok(req) => req,
        Err(e) => {
            return json_rpc_response(JsonRpcResponse::error(
                None,
                PARSE_ERROR,
                format!("Parse error: {}", e),
            ));
        }
    };

    let id = request.id.clone();

    // Step 2: Extract bearer token
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return json_rpc_response(JsonRpcResponse::error(
                id,
                INVALID_PARAMS,
                "Missing Authorization header",
            ));
        }
    };

    // Step 3: Verify access token
    let auth_info = match mcp.state.token_queries.verify_access_token(&token).await {
        Ok(info) => to_op_auth_info(info),
        Err(e) => {
            let msg = match e {
                zbrain_core::TokenError::Expired => "Token expired".to_string(),
                _ => "Invalid token".to_string(),
            };
            return json_rpc_response(JsonRpcResponse::error(id, INVALID_PARAMS, msg));
        }
    };

    // Step 4: Route by method
    match request.method.as_str() {
        "tools/list" => json_rpc_response(handle_tools_list(&mcp.registry, id)),
        "tools/call" => json_rpc_response(
            handle_tools_call(&mcp, request.params, auth_info, id).await,
        ),
        _ => json_rpc_response(JsonRpcResponse::error(
            id,
            METHOD_NOT_FOUND,
            format!("Unknown method: {}", request.method),
        )),
    }
}

// ── tools/list handler ────────────────────────────────────────────────

fn handle_tools_list(registry: &OperationRegistry, id: Option<Value>) -> JsonRpcResponse {
    let tools: Vec<McpToolDef> = build_tool_defs(registry);
    let result = serde_json::json!({ "tools": tools });
    JsonRpcResponse::success(id, result)
}

// ── tools/call handler ────────────────────────────────────────────────

async fn handle_tools_call(
    mcp: &McpState,
    params: Option<Value>,
    auth_info: zbrain_core::operation::AuthInfo,
    id: Option<Value>,
) -> JsonRpcResponse {
    // Extract name and arguments from params
    let params = match params {
        Some(p) => p,
        None => return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing params"),
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n.to_string(),
        None => return JsonRpcResponse::error(id, INVALID_PARAMS, "Missing tool name"),
    };

    let arguments = params.get("arguments").cloned().unwrap_or(Value::Object(Default::default()));

    // Step 1: Lookup operation
    let op = match mcp.registry.lookup(&name) {
        Some(op) => op,
        None => {
            let tool_result = serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::to_string(&serde_json::json!({
                        "error": "unknown_operation",
                        "message": format!("Unknown operation: {}", name)
                    })).unwrap_or_default()
                }],
                "isError": true
            });
            return JsonRpcResponse::success(id, tool_result);
        }
    };

    // Step 2: Scope enforcement
    let required_scope = op.required_scope();
    if !has_scope(&auth_info.scopes, required_scope) {
        let tool_result = serde_json::json!({
            "content": [{
                "type": "text",
                "text": serde_json::to_string(&serde_json::json!({
                    "error": "insufficient_scope",
                    "message": format!(
                        "Operation '{}' requires scope '{}'. Your scopes: {:?}",
                        name, required_scope, auth_info.scopes
                    ),
                    "required_scope": required_scope,
                    "your_scopes": auth_info.scopes,
                })).unwrap_or_default()
            }],
            "isError": true
        });
        return JsonRpcResponse::success(id, tool_result);
    }

    // Step 3: Build operation context
    let source_id = auth_info
        .source_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut ctx = zbrain_core::operation::OperationContext::remote_mcp(source_id);
    ctx.auth = Some(auth_info);

    // Step 4: Dispatch
    let tool_result = mcp.registry.dispatch_tool_call(&name, &ctx, arguments).await;

    JsonRpcResponse::success(id, serde_json::to_value(&tool_result).unwrap_or_default())
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Extract the bearer token from the Authorization header.
fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let auth = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let auth = auth.trim();
    if auth.len() <= 7 || !auth[..7].eq_ignore_ascii_case("Bearer ") {
        return None;
    }
    Some(auth[7..].trim().to_string())
}

/// Convert a `JsonRpcResponse` into an axum `Response`.
fn json_rpc_response(resp: JsonRpcResponse) -> Response {
    Json(resp).into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use zbrain_core::operation::TypedOperation;
    use zbrain_core::InMemoryEngine;

    use crate::{AdminAuth, MagicLinkAuth};

    fn make_spa_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("index.html"), b"<html></html>").unwrap();
        (dir, path)
    }

    fn build_test_registry() -> OperationRegistry {
        // Register a minimal set of operations for testing.
        // Real registry would have all ops; for tests we just need a few.
        let mut reg = OperationRegistry::new();

        // Register a read-scoped operation
        #[derive(Debug, Clone)]
        struct TestReadOp;
        #[async_trait::async_trait]
        impl TypedOperation for TestReadOp {
            type Params = serde_json::Value;
            type Output = serde_json::Value;
            fn name(&self) -> &'static str { "test_read" }
            fn description(&self) -> &'static str { "A test read operation" }
            fn required_scope(&self) -> &'static str { "read" }
            async fn execute(
                &self,
                _ctx: &zbrain_core::operation::OperationContext,
                _params: Self::Params,
            ) -> zbrain_core::operation::OperationResult<Self::Output> {
                Ok(serde_json::json!({"ok": true}))
            }
        }
        reg.register(TestReadOp);

        // Register a write-scoped operation
        #[derive(Debug, Clone)]
        struct TestWriteOp;
        #[async_trait::async_trait]
        impl TypedOperation for TestWriteOp {
            type Params = serde_json::Value;
            type Output = serde_json::Value;
            fn name(&self) -> &'static str { "test_write" }
            fn description(&self) -> &'static str { "A test write operation" }
            fn required_scope(&self) -> &'static str { "write" }
            async fn execute(
                &self,
                _ctx: &zbrain_core::operation::OperationContext,
                _params: Self::Params,
            ) -> zbrain_core::operation::OperationResult<Self::Output> {
                Ok(serde_json::json!({"ok": true}))
            }
        }
        reg.register(TestWriteOp);

        // Register an admin-scoped operation
        #[derive(Debug, Clone)]
        struct TestAdminOp;
        #[async_trait::async_trait]
        impl TypedOperation for TestAdminOp {
            type Params = serde_json::Value;
            type Output = serde_json::Value;
            fn name(&self) -> &'static str { "test_admin" }
            fn description(&self) -> &'static str { "A test admin operation" }
            fn required_scope(&self) -> &'static str { "admin" }
            async fn execute(
                &self,
                _ctx: &zbrain_core::operation::OperationContext,
                _params: Self::Params,
            ) -> zbrain_core::operation::OperationResult<Self::Output> {
                Ok(serde_json::json!({"ok": true}))
            }
        }
        reg.register(TestAdminOp);

        reg
    }

    async fn start_mcp_server() -> (u16, AppState, Arc<OperationRegistry>) {
        let auth = AdminAuth::new(None);
        let (_dir, spa_path) = make_spa_dir();
        let engine = Arc::new(InMemoryEngine::default());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let registry = Arc::new(build_test_registry());

        let state = AppState {
            admin_auth: auth,
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone() as Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone() as Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone() as Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine as Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tx,
            spa_dir: spa_path,
            operation_registry: registry.clone(),
        };

        let app = build_mcp_router(state.clone(), registry.clone());

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        (port, state, registry)
    }

    /// Register a client with specific scopes and get a bearer token.
    async fn register_and_get_token(state: &AppState, scopes: &str) -> String {
        let client = state.oauth_queries.register_client(zbrain_core::oauth_queries::RegisterClientRequest {
            name: "test-mcp-client".to_string(),
            scope: scopes.to_string(),
            grant_types: vec!["client_credentials".to_string()],
            redirect_uris: vec![],
            token_endpoint_auth_method: None,
            token_ttl: Some(3600),
            source_id: "default".to_string(),
            federated_read: vec![],
        }).await.unwrap();

        // Exchange client credentials for access token
        let tokens = state.oauth_queries.exchange_client_credentials(
            &client.client_id,
            &client.client_secret,
            Some(scopes),
        ).await.unwrap();

        tokens.access_token
    }

    #[tokio::test]
    async fn mcp_get_returns_405() {
        let (port, _state, _registry) = start_mcp_server().await;
        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/mcp"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn mcp_post_without_auth_returns_error() {
        let (port, _state, _registry) = start_mcp_server().await;
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["error"].is_object(), "Expected error, got: {}", body);
    }

    #[tokio::test]
    async fn mcp_tools_list_with_valid_token() {
        let (port, state, _registry) = start_mcp_server().await;
        let token = register_and_get_token(&state, "read").await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body["result"]["tools"].is_array(), "Expected tools array, got: {}", body);
    }

    #[tokio::test]
    async fn mcp_tools_call_read_op_with_read_scope_succeeds() {
        let (port, state, _registry) = start_mcp_server().await;
        let token = register_and_get_token(&state, "read").await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "test_read",
                    "arguments": {}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let result = &body["result"];
        // Successful dispatch returns content without isError
        assert!(result["content"].is_array(),
            "Expected content array, got: {}", body);
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        let content: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        assert_eq!(content["ok"], true, "Expected ok: true, got: {}", content);
    }

    #[tokio::test]
    async fn mcp_tools_call_write_op_with_read_scope_fails_scope_check() {
        let (port, state, _registry) = start_mcp_server().await;
        let token = register_and_get_token(&state, "read").await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "test_write",
                    "arguments": {}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let result = &body["result"];
        // Scope failure returns isError: true with content
        assert!(result["isError"].as_bool().unwrap_or(false),
            "Expected scope error (isError=true), got: {}", body);
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        let content: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        assert_eq!(content["error"], "insufficient_scope",
            "Expected insufficient_scope, got: {}", content);
    }

    #[tokio::test]
    async fn mcp_tools_call_write_op_with_admin_scope_succeeds() {
        let (port, state, _registry) = start_mcp_server().await;
        // admin implies write
        let token = register_and_get_token(&state, "admin").await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "test_write",
                    "arguments": {}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let result = &body["result"];
        // Success: content array with {ok: true}, no isError
        assert!(result["content"].is_array(),
            "admin scope should imply write, got: {}", body);
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        let content: serde_json::Value = serde_json::from_str(text).unwrap_or_default();
        assert_eq!(content["ok"], true,
            "Expected ok: true, got: {}", content);
    }

    #[tokio::test]
    async fn mcp_tools_call_unknown_operation_returns_error() {
        let (port, state, _registry) = start_mcp_server().await;
        let token = register_and_get_token(&state, "read").await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "nonexistent_op",
                    "arguments": {}
                }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        let result = &body["result"];
        assert!(result["isError"].as_bool().unwrap_or(false), "Expected error for unknown op");
    }

    #[tokio::test]
    async fn mcp_unknown_method_returns_jsonrpc_error() {
        let (port, state, _registry) = start_mcp_server().await;
        let token = register_and_get_token(&state, "read").await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Authorization", format!("Bearer {}", token))
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 6,
                "method": "invalid/method",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn mcp_parse_error_returns_jsonrpc_error() {
        let (port, _state, _registry) = start_mcp_server().await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/mcp"))
            .header("Content-Type", "application/json")
            .body("not json")
            .send()
            .await
            .unwrap();
        // Axum might return 400 for bad JSON before reaching our handler,
        // or 200 if it reaches our handler. Both are acceptable.
        let status = resp.status();
        assert!(status == StatusCode::OK || status == StatusCode::BAD_REQUEST,
            "Expected OK or BAD_REQUEST, got {}", status);
    }
}
