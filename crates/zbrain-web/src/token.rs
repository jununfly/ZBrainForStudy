//! POST /token — OAuth 2.1 token endpoint.
//!
//! Supports three grant types:
//! - `client_credentials` (RFC 6749 §4.4)
//! - `authorization_code` (RFC 6749 §4.1.3, confidential clients only)
//! - `refresh_token` (RFC 6749 §6, with rotation)
//!
//! Public clients (PKCE) are handled by the SDK's mcpAuthRouter;
//! this handler only serves confidential client flows.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json, Router,
};
use serde_json::json;

use zbrain_core::oauth_queries::ExchangeTokens;

use crate::AppState;

/// Build the /token route.
pub fn build_token_router(state: AppState) -> Router {
    Router::new()
        .route("/token", axum::routing::post(token_handler))
        .with_state(state)
}

/// POST /token handler.
async fn token_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // Parse the x-www-form-urlencoded body.
    let params: std::collections::HashMap<String, String> =
        form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();

    let grant_type = params.get("grant_type").cloned().unwrap_or_default();

    match grant_type.as_str() {
        "client_credentials" => {
            handle_client_credentials(&state, &params).await
        }
        "authorization_code" => {
            handle_confidential_exchange(
                &state,
                &headers,
                &params,
                "authorization_code",
            )
            .await
        }
        "refresh_token" => {
            handle_confidential_exchange(
                &state,
                &headers,
                &params,
                "refresh_token",
            )
            .await
        }
        _ => {
            // Unknown grant_type — let the next handler deal with it
            // (public client PKCE flows go through the SDK).
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "unsupported_grant_type",
                    "error_description": "Only client_credentials, authorization_code, and refresh_token are supported"
                })),
            )
                .into_response()
        }
    }
}

/// Handle the `client_credentials` grant.
async fn handle_client_credentials(
    state: &AppState,
    params: &std::collections::HashMap<String, String>,
) -> Response {
    let client_id = match params.get("client_id") {
        Some(id) if !id.is_empty() => id.as_str(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "invalid_request", "error_description": "client_id is required"})),
            ).into_response();
        }
    };

    let client_secret = params.get("client_secret").map(|s| s.as_str()).unwrap_or("");
    let scope = params.get("scope").map(|s| s.as_str());

    match state.oauth_queries.exchange_client_credentials(client_id, client_secret, scope).await {
        Ok(tokens) => token_response(tokens),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") || msg.contains("secret") || msg.contains("revoked") {
                (StatusCode::UNAUTHORIZED, Json(json!({
                    "error": "invalid_client",
                }))).into_response()
            } else if msg.contains("not authorized") {
                (StatusCode::BAD_REQUEST, Json(json!({
                    "error": "unauthorized_client",
                }))).into_response()
            } else {
                (StatusCode::BAD_REQUEST, Json(json!({
                    "error": "invalid_grant",
                }))).into_response()
            }
        }
    }
}

/// Handle confidential client exchanges (authorization_code or refresh_token).
async fn handle_confidential_exchange(
    state: &AppState,
    headers: &HeaderMap,
    params: &std::collections::HashMap<String, String>,
    grant_type: &str,
) -> Response {
    // Detect client authentication method: basic vs post.
    let (extracted_client_id, extracted_secret) =
        extract_client_auth(headers, params);

    let client_id = match extracted_client_id {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "invalid_client"})),
            ).into_response();
        }
    };

    let client_secret = match extracted_secret {
        Some(s) => s,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "invalid_client"})),
            ).into_response();
        }
    };

    // Verify the confidential client secret.
    let client = match state
        .oauth_queries
        .verify_confidential_client_secret(&client_id, &client_secret)
        .await
    {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"error": "invalid_client"})),
            ).into_response();
        }
    };

    match grant_type {
        "authorization_code" => {
            let code = match params.get("code") {
                Some(c) if !c.is_empty() => c.as_str(),
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "invalid_request", "error_description": "code is required"})),
                    ).into_response();
                }
            };
            let redirect_uri = params.get("redirect_uri").map(|s| s.as_str());

            match state
                .oauth_queries
                .exchange_authorization_code(&client.client_id, code, redirect_uri)
                .await
            {
                Ok(tokens) => token_response(tokens),
                Err(_) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                ).into_response(),
            }
        }
        "refresh_token" => {
            let refresh_token = match params.get("refresh_token") {
                Some(rt) if !rt.is_empty() => rt.as_str(),
                _ => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "invalid_request", "error_description": "refresh_token is required"})),
                    ).into_response();
                }
            };
            let scopes: Option<Vec<String>> = params
                .get("scope")
                .map(|s| s.split(' ').filter(|x| !x.is_empty()).map(String::from).collect());

            match state
                .oauth_queries
                .exchange_refresh_token(
                    &client.client_id,
                    refresh_token,
                    scopes.as_deref(),
                )
                .await
            {
                Ok(tokens) => token_response(tokens),
                Err(_) => (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": "invalid_grant"})),
                ).into_response(),
            }
        }
        _ => unreachable!(),
    }
}

/// Extract client_id and client_secret from either:
/// - `client_secret_basic`: `Authorization: Basic base64(client_id:client_secret)`
/// - `client_secret_post`: `client_id` + `client_secret` in the request body
fn extract_client_auth(
    headers: &HeaderMap,
    params: &std::collections::HashMap<String, String>,
) -> (Option<String>, Option<String>) {
    // Try Basic auth header first.
    if let Some(auth_header) = headers.get("authorization") {
        if let Ok(auth_str) = auth_header.to_str() {
            if auth_str.to_lowercase().starts_with("basic ") {
                let encoded = &auth_str[6..];
                if let Ok(decoded) = base64_decode(encoded) {
                    if let Some((id, secret)) = decoded.split_once(':') {
                        return (
                            Some(url_decode(id)),
                            Some(url_decode(secret)),
                        );
                    }
                }
            }
        }
    }

    // Fall back to body params (client_secret_post).
    let client_id = params.get("client_id").cloned();
    let client_secret = params.get("client_secret").cloned();

    (client_id, client_secret)
}

/// Base64-decode a string (no padding tolerant).
fn base64_decode(input: &str) -> Result<String, ()> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine.decode(input.trim()).map_err(|_| ())?;
    String::from_utf8(bytes).map_err(|_| ())
}

/// URL-decode a string.
fn url_decode(input: &str) -> String {
    form_urlencoded::parse(input.as_bytes())
        .map(|(k, _)| k.into_owned())
        .collect::<Vec<_>>()
        .join("")
}

/// Build the JSON response for a successful token exchange.
fn token_response(tokens: ExchangeTokens) -> Response {
    let mut body = json!({
        "access_token": tokens.access_token,
        "token_type": tokens.token_type,
        "expires_in": tokens.expires_in,
        "scope": tokens.scope,
    });

    if let Some(rt) = &tokens.refresh_token {
        body["refresh_token"] = json!(rt);
    }

    (StatusCode::OK, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use crate::MagicLinkAuth;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::sync::broadcast;
    use zbrain_core::InMemoryEngine;

    use crate::auth::AdminAuth;

    fn make_spa_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("index.html"), b"<html></html>").unwrap();
        (dir, path)
    }

    fn test_state() -> AppState {
        let auth = AdminAuth::new(None);
        let (_dir, spa_path) = make_spa_dir();
        let engine = Arc::new(InMemoryEngine::default());
        let (tx, _rx) = broadcast::channel(64);
        AppState {
            admin_auth: auth,
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone() as Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone() as Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone() as Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine.clone() as Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tx,
            spa_dir: spa_path,
            operation_registry: Arc::new(zbrain_core::operation::OperationRegistry::new()),
            engine: engine as Arc<dyn zbrain_core::BrainEngine>,
        }
    }

    async fn start_server(state: AppState) -> u16 {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let app = crate::build_router(state);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        port
    }

    async fn start_token_server() -> (u16, AppState) {
        let state = test_state();
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let app = build_token_router(state.clone());

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, state)
    }

    // ── client_credentials grant tests ──────────────────────────────────

    #[tokio::test]
    async fn client_credentials_success() {
        let (port, _state) = start_token_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials&client_id=test&client_secret=secret&scope=read")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.get("access_token").and_then(|v| v.as_str()).is_some());
        assert_eq!(body["token_type"], "bearer");
        assert!(body["expires_in"].as_i64().is_some());
        // No refresh_token for client_credentials
        assert!(body.get("refresh_token").is_none());
    }

    #[tokio::test]
    async fn client_credentials_missing_client_id_returns_400() {
        let (port, _state) = start_token_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=client_credentials&client_secret=secret")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "invalid_request");
    }

    #[tokio::test]
    async fn unsupported_grant_type_returns_400() {
        let (port, _state) = start_token_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=password")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "unsupported_grant_type");
    }

    // ── authorization_code grant tests ──────────────────────────────────

    #[tokio::test]
    async fn authorization_code_missing_code_returns_400() {
        let (port, _state) = start_token_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=authorization_code&client_id=test&client_secret=secret")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "invalid_request");
    }

    #[tokio::test]
    async fn authorization_code_missing_client_auth_returns_401() {
        let (port, _state) = start_token_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=authorization_code&code=abc123")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "invalid_client");
    }

    // ── refresh_token grant tests ───────────────────────────────────────

    #[tokio::test]
    async fn refresh_token_missing_refresh_token_returns_400() {
        let (port, _state) = start_token_server().await;
        let client = reqwest::Client::new();

        let resp = client
            .post(format!("http://127.0.0.1:{port}/token"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("grant_type=refresh_token&client_id=test&client_secret=secret")
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["error"], "invalid_request");
    }

    // ── Basic auth extraction tests ─────────────────────────────────────

    #[test]
    fn extract_client_auth_from_basic_header() {
        let mut headers = HeaderMap::new();
        let encoded = base64_encode("test-client:test-secret");
        headers.insert(
            "authorization",
            format!("Basic {encoded}").parse().unwrap(),
        );

        let params = std::collections::HashMap::new();
        let (id, secret) = extract_client_auth(&headers, &params);
        assert_eq!(id.unwrap(), "test-client");
        assert_eq!(secret.unwrap(), "test-secret");
    }

    #[test]
    fn extract_client_auth_from_body_params() {
        let headers = HeaderMap::new();
        let mut params = std::collections::HashMap::new();
        params.insert("client_id".to_string(), "body-client".to_string());
        params.insert("client_secret".to_string(), "body-secret".to_string());

        let (id, secret) = extract_client_auth(&headers, &params);
        assert_eq!(id.unwrap(), "body-client");
        assert_eq!(secret.unwrap(), "body-secret");
    }

    fn base64_encode(input: &str) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(input.as_bytes())
    }
}
