//! Admin OAuth client management endpoints: register, update-ttl, revoke.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};

use super::super::AppState;
use zbrain_core::oauth_queries::RegisterClientRequest;
use zbrain_core::normalize_scopes_input;

/// Build the OAuth client management router.
pub fn build_oauth_router() -> Router<AppState> {
    Router::new()
        .route("/register-client", post(register_client_handler))
        .route("/update-client-ttl", post(update_client_ttl_handler))
        .route("/revoke-client", post(revoke_client_handler))
}

// ── Handlers ──────────────────────────────────────────────────────────

async fn register_client_handler(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let name = match body.get("name").and_then(|v| v.as_str()) {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "Name required"})))
            .into_response(),
    };

    // Normalize scopes: accept both "scopes" and "scope" keys, validate against ALLOWED_SCOPES
    let scope_string = match normalize_scopes_input(body.get("scopes").or_else(|| body.get("scope"))) {
        Ok(s) => s,
        Err(e) => return (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": format!("Invalid scopes: {}", e)})))
            .into_response(),
    };

    let grant_types: Vec<String> = body
        .get("grantTypes")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_else(|| vec!["client_credentials".to_string()]);

    let redirect_uris: Vec<String> = body
        .get("redirectUris")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let token_endpoint_auth_method = body
        .get("tokenEndpointAuthMethod")
        .and_then(|v| v.as_str())
        .map(String::from);

    let token_ttl = body
        .get("tokenTtl")
        .and_then(|v| v.as_i64())
        .filter(|&t| t > 0);

    // Extract optional source_id / federated_read from request body.
    let source_id = body
        .get("sourceId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "default".to_string());

    let federated_read: Vec<String> = body
        .get("federatedRead")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();

    let req = RegisterClientRequest {
        name,
        scope: scope_string,
        grant_types,
        redirect_uris,
        token_endpoint_auth_method,
        token_ttl,
        source_id,
        federated_read,
    };

    match state.oauth_queries.register_client(req).await {
        Ok(resp) => Json(serde_json::json!({
            "ok": true,
            "clientId": resp.client_id,
            "clientSecret": resp.client_secret,
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn update_client_ttl_handler(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let client_id = match body.get("clientId").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "clientId required"})))
            .into_response(),
    };

    let ttl = body.get("tokenTtl").and_then(|v| v.as_i64());

    match state.oauth_queries.update_client_ttl(&client_id, ttl).await {
        Ok(resp) => {
            let mut json = serde_json::json!({
                "ok": true,
                "updated": resp.updated,
            });
            if let Some(ttl_val) = resp.token_ttl {
                json["tokenTtl"] = serde_json::json!(ttl_val);
            } else {
                json["tokenTtl"] = serde_json::Value::Null;
            }
            Json(json).into_response()
        }
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn revoke_client_handler(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    let client_id = match body.get("clientId").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => return (axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"ok": false, "error": "clientId required"})))
            .into_response(),
    };

    match state.oauth_queries.revoke_client(&client_id).await {
        Ok(resp) => Json(serde_json::json!({
            "ok": true,
            "revoked": resp.revoked,
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;

    use zbrain_core::InMemoryEngine;

    use super::super::super::build_router;
    use super::super::super::auth::AdminAuth;
    use crate::MagicLinkAuth;

    fn make_spa_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("index.html"), b"<html></html>").unwrap();
        (dir, path)
    }

    /// Start admin server with a random bootstrap token.
    /// Returns (port, token).
    async fn start_admin_server() -> (u16, String) {
        let auth = AdminAuth::new(None);
        let token = auth.bootstrap_token().to_string();
        let (_dir, spa_path) = make_spa_dir();

        let engine = std::sync::Arc::new(InMemoryEngine::default());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let state = super::super::super::AppState {
            admin_auth: auth.clone(),
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tx,
            spa_dir: spa_path,
            operation_registry: Arc::new(zbrain_core::operation::OperationRegistry::new()),
            engine: engine as std::sync::Arc<dyn zbrain_core::BrainEngine>,
        };

        let app = build_router(state);
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, token)
    }

    /// Login and extract the `zbrain_admin` session cookie.
    async fn login_admin(port: u16, token: &str) -> String {
        let client = reqwest::Client::new();
        let login_resp = client
            .post(format!("http://127.0.0.1:{port}/admin/login"))
            .json(&serde_json::json!({"token": token}))
            .send()
            .await
            .unwrap();
        assert!(login_resp.status().is_success(), "login must succeed");
        login_resp
            .headers()
            .get("set-cookie")
            .expect("login must set cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn register_client_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/register-client"))
            .json(&serde_json::json!({"name": "test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn register_client_returns_client_id_and_secret() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/register-client"))
            .header("Cookie", &cookie)
            .json(&serde_json::json!({
                "name": "my-agent",
                "scopes": ["read", "write"],
                "grantTypes": ["client_credentials"],
                "tokenTtl": 3600,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert!(body.get("clientId").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty()));
        assert!(body.get("clientSecret").and_then(|v| v.as_str()).map_or(false, |s| !s.is_empty()));
    }

    #[tokio::test]
    async fn register_client_rejects_missing_name() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/register-client"))
            .header("Cookie", &cookie)
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_client_ttl_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/update-client-ttl"))
            .json(&serde_json::json!({"clientId": "c1", "tokenTtl": 3600}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_client_ttl_returns_ok() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/update-client-ttl"))
            .header("Cookie", &cookie)
            .json(&serde_json::json!({"clientId": "c1", "tokenTtl": 7200}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["updated"], true);
        assert_eq!(body["tokenTtl"], 7200);
    }

    #[tokio::test]
    async fn revoke_client_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/revoke-client"))
            .json(&serde_json::json!({"clientId": "c1"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn revoke_client_returns_ok() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("http://127.0.0.1:{port}/revoke-client"))
            .header("Cookie", &cookie)
            .json(&serde_json::json!({"clientId": "c1"}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["revoked"], true);
    }
}
