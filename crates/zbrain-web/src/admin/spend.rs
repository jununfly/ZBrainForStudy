//! Admin spend endpoint: per-agent-client spending summary.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use super::super::AppState;

/// Build the spend router.
pub fn build_spend_router() -> Router<AppState> {
    Router::new()
        .route("/agents/spend", get(get_spend_handler))
}

async fn get_spend_handler(
    State(state): State<AppState>,
) -> Response {
    match state.admin_queries.list_agent_client_spend().await {
        Ok(items) => Json(serde_json::json!({
            "ok": true,
            "data": items,
        })).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use crate::MagicLinkAuth;

    fn make_spa_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("index.html"), b"<html></html>").unwrap();
        (dir, path)
    }

    async fn start_admin_server() -> (u16, String) {
        let auth = super::super::super::auth::AdminAuth::new(None);
        let token = auth.bootstrap_token().to_string();
        let (_dir, spa_path) = make_spa_dir();

        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let state = super::super::super::AppState {
            admin_auth: auth.clone(),
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tx,
            spa_dir: spa_path,
            operation_registry: Arc::new(zbrain_core::operation::OperationRegistry::new()),
            engine: engine as std::sync::Arc<dyn zbrain_core::BrainEngine>,
            zbrain_home: std::env::temp_dir().join("zbrain-test"),
        };

        let app = super::super::super::build_router(state);
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, token)
    }

    #[tokio::test]
    async fn spend_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;
        let url = format!("http://127.0.0.1:{port}/agents/spend");

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED,
            "spend endpoint must require admin auth");
    }

    #[tokio::test]
    async fn spend_returns_empty_with_admin_auth() {
        let (port, token) = start_admin_server().await;

        // Login to get a session cookie
        let client = reqwest::Client::new();
        let login_resp = client
            .post(format!("http://127.0.0.1:{port}/admin/login"))
            .json(&serde_json::json!({"token": token}))
            .send()
            .await
            .unwrap();
        assert!(login_resp.status().is_success(), "login must succeed");

        let cookie = login_resp
            .headers()
            .get("set-cookie")
            .expect("login must set cookie")
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();

        // Hit the spend endpoint with admin session
        let resp = client
            .get(format!("http://127.0.0.1:{port}/agents/spend"))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK, "admin session must be accepted");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"], serde_json::json!([]));
    }
}
