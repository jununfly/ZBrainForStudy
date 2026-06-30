//! `zbrain-web` — axum-based HTTP API for zbrain.

mod admin;
mod auth;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::Path,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;

pub use auth::AdminAuth;

/// Application state shared across all request handlers.
#[derive(Clone)]
pub struct AppState {
    /// Admin authentication and session management.
    pub admin_auth: AdminAuth,
    /// Admin dashboard data-access layer.
    pub admin_queries: Arc<dyn zbrain_core::AdminQueries>,
    /// Calibration data-access layer.
    pub calibration_queries: Arc<dyn zbrain_core::CalibrationQueries>,
    /// Path to the admin SPA static files directory.
    pub spa_dir: PathBuf,
}

/// Health-check response body.
#[derive(Serialize)]
struct HealthResponse {
    status: String,
}

/// `GET /health` handler — liveness probe.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
    })
}

/// Serve the admin SPA: try to serve the requested file, fall back to
/// index.html for client-side routing.
async fn admin_spa_handler(
    Path(path): Path<String>,
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    match serve_spa_file(&state.spa_dir, &path).await {
        Some(response) => response,
        None => serve_spa_index(&state.spa_dir).await,
    }
}

/// Try to serve a specific file from the SPA directory.
/// Returns `Some(response)` if the file exists, or `None` if not found.
async fn serve_spa_file(spa_dir: &std::path::Path, path: &str) -> Option<Response> {
    // Prevent directory traversal.
    let sanitized = path.trim_start_matches('/');
    if sanitized.contains("..") {
        return None;
    }

    let file_path = if sanitized.is_empty() {
        spa_dir.join("index.html")
    } else {
        spa_dir.join(sanitized)
    };

    let data = tokio::fs::read(&file_path).await.ok()?;

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let content_type = match ext {
        "html" => "text/html; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    };

    Some(
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type)],
            data,
        )
            .into_response(),
    )
}

/// Serve index.html (SPA fallback).
async fn serve_spa_index(spa_dir: &std::path::Path) -> Response {
    let index_path = spa_dir.join("index.html");
    match tokio::fs::read_to_string(&index_path).await {
        Ok(html) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

/// Build the axum router with all routes registered.
pub fn build_router(state: AppState) -> Router {
    let spa_dir = state.spa_dir.clone();
    let index_path = spa_dir.join("index.html");

    let main = Router::new()
        .route("/health", get(health))
        .route("/admin/{*path}", get(admin_spa_handler))
        .fallback(move |uri: Uri| {
            let index = index_path.clone();
            async move {
                if uri.path().starts_with("/admin") {
                    match tokio::fs::read_to_string(&index).await {
                        Ok(html) => (
                            StatusCode::OK,
                            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                            html,
                        )
                            .into_response(),
                        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
                    }
                } else {
                    (StatusCode::NOT_FOUND, "Not Found").into_response()
                }
            }
        })
        .with_state(state.clone());

    main.merge(auth::admin_auth_routes(state.clone()))
        .merge(admin::build_admin_router(state))
}

/// Start the HTTP server and block until shutdown signal.
///
/// The server binds to `addr` and serves the router built by [`build_router`].
/// Returns an error if binding fails.
pub async fn run(addr: SocketAddr, state: AppState) -> anyhow::Result<()> {
    let bootstrap_token = state.admin_auth.bootstrap_token().to_string();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    eprintln!("Admin bootstrap token: {bootstrap_token}");
    axum::serve(listener, build_router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::io::Write;

    /// Start the server on an OS-assigned port, return the port number.
    async fn start_test_server(state: AppState) -> u16 {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, build_router(state)).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        port
    }

    fn make_spa_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let spa_path = dir.path().to_path_buf();
        let index_path = spa_path.join("index.html");
        let mut f = std::fs::File::create(&index_path).unwrap();
        f.write_all(b"<!doctype html><html><body>SPA</body></html>").unwrap();

        let assets_dir = spa_path.join("assets");
        std::fs::create_dir(&assets_dir).unwrap();
        let mut f = std::fs::File::create(assets_dir.join("app.js")).unwrap();
        f.write_all(b"console.log('hello');").unwrap();

        (dir, spa_path)
    }

    fn test_state() -> (tempfile::TempDir, AppState) {
        let auth = AdminAuth::new(None);
        let (dir, spa_dir) = make_spa_dir();
        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let state = AppState {
            admin_auth: auth,
            admin_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
            spa_dir,
        };
        (dir, state)
    }

    #[tokio::test]
    async fn health_endpoint_returns_200() {
        let (_dir, state) = test_state();
        let port = start_test_server(state).await;
        let url = format!("http://127.0.0.1:{port}/health");

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");
    }

    #[tokio::test]
    async fn admin_root_returns_index_html() {
        let (_dir, state) = test_state();
        let port = start_test_server(state).await;
        let url = format!("http://127.0.0.1:{port}/admin/");

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("SPA"), "Expected SPA content, got: {body}");
    }

    #[tokio::test]
    async fn admin_static_assets_served_with_correct_content_type() {
        let (_dir, state) = test_state();
        let port = start_test_server(state).await;
        let url = format!("http://127.0.0.1:{port}/admin/assets/app.js");

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            content_type.contains("javascript"),
            "Expected javascript content-type, got: {content_type}"
        );
    }

    #[tokio::test]
    async fn admin_spa_fallback_returns_index_html_for_client_side_route() {
        let (_dir, state) = test_state();
        let port = start_test_server(state).await;
        let url = format!("http://127.0.0.1:{port}/admin/dashboard");

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("SPA"), "SPA fallback should return index.html");
    }

    #[test]
    fn crate_name_is_zbrain_web() {
        assert_eq!(crate_name(), "zbrain-web");
    }
}

/// Static crate name.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-web"
}
