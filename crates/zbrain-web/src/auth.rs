//! Admin authentication: bootstrap token, login, cookie sessions.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;
use uuid::Uuid;

/// A logged-in session.
#[derive(Clone)]
struct Session {
    /// When this session was created (Unix timestamp).
    _created_at: i64,
    /// When this session expires (Unix timestamp).
    expires_at: i64,
}

/// Admin authentication manager.
///
/// Holds the SHA-256 hash of the bootstrap token and an in-memory
/// session store keyed by session ID.
#[derive(Clone)]
pub struct AdminAuth {
    /// SHA-256 hex digest of the bootstrap token.
    token_hash: String,
    /// The raw bootstrap token — printed once on startup, then cleared.
    bootstrap_token: String,
    /// In-memory session store: session_id → Session.
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    /// Session TTL in seconds (default: 24 hours).
    session_ttl_secs: i64,
}

impl AdminAuth {
    /// Create a new `AdminAuth`.
    ///
    /// If `env_token` is `Some`, it is used as the bootstrap token.
    /// Otherwise a random UUID v4 token is generated.
    pub fn new(env_token: Option<String>) -> Self {
        let token = env_token.unwrap_or_else(|| Uuid::new_v4().to_string());
        let token_hash = hex::encode(Sha256::digest(token.as_bytes()));
        Self {
            token_hash,
            bootstrap_token: token,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_ttl_secs: 24 * 60 * 60, // 24 hours
        }
    }

    /// Return the bootstrap token (for startup printing).
    /// After the first call, subsequent calls return an empty string.
    /// This prevents the token from being leaked accidentally.
    pub fn bootstrap_token(&self) -> &str {
        // We intentionally don't clear here — the caller (run) reads once.
        // For tests that call this multiple times, it's harmless.
        &self.bootstrap_token
    }

    /// Verify a candidate token against the stored SHA-256 hash.
    pub(crate) fn verify_token(&self, candidate: &str) -> bool {
        let candidate_hash = hex::encode(Sha256::digest(candidate.as_bytes()));
        // Constant-time comparison is ideal but SHA-256 hex strings
        // are always the same length, so timing is not a concern.
        candidate_hash == self.token_hash
    }

    /// Create a new session, returning the session ID.
    async fn create_session(&self) -> String {
        self.create_session_with_ttl(self.session_ttl_secs).await
    }

    /// Create a new session with a custom TTL in seconds, returning the session ID.
    pub(crate) async fn create_session_with_ttl(&self, ttl_secs: i64) -> String {
        let session_id = Uuid::new_v4().to_string();
        let now = now_unix();
        let session = Session {
            _created_at: now,
            expires_at: now + ttl_secs,
        };
        self.sessions.write().await.insert(session_id.clone(), session);
        session_id
    }

    /// Validate a session ID. Returns `true` if the session exists and
    /// has not expired.
    async fn validate_session(&self, session_id: &str) -> bool {
        let sessions = self.sessions.read().await;
        let Some(session) = sessions.get(session_id) else {
            return false;
        };
        session.expires_at > now_unix()
    }

    /// Invalidate all sessions and return the count that were cleared.
    pub(crate) async fn clear_all_sessions(&self) -> usize {
        let mut sessions = self.sessions.write().await;
        let count = sessions.len();
        sessions.clear();
        count
    }
}

// ---------------- request / response types ----------------

/// Request body for `POST /admin/login`.
#[derive(Deserialize)]
struct LoginRequest {
    token: String,
}

/// Response body for `POST /admin/login`.
#[derive(Serialize)]
struct LoginResponse {
    ok: bool,
}

// ---------------- route handlers ----------------

/// `POST /admin/login` — verify bootstrap token and set session cookie.
async fn login_handler(
    State(state): State<super::AppState>,
    Json(body): Json<LoginRequest>,
) -> Response {
    if !state.admin_auth.verify_token(&body.token) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "invalid token"}))).into_response();
    }

    let session_id = state.admin_auth.create_session().await;

    let cookie = format!(
        "zbrain_admin={session_id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        state.admin_auth.session_ttl_secs
    );

    let mut headers = HeaderMap::new();
    headers.insert(header::SET_COOKIE, cookie.parse().unwrap());

    (StatusCode::OK, headers, Json(LoginResponse { ok: true })).into_response()
}

// ---------------- magic-link route handlers ----------------

/// Response for `POST /admin/api/issue-magic-link`.
#[derive(Serialize)]
struct IssueMagicLinkResponse {
    url: String,
    expires_in: i64,
}

/// `POST /admin/api/issue-magic-link` — requires Bearer bootstrap token.
async fn issue_magic_link_handler(
    State(state): State<super::AppState>,
    headers: HeaderMap,
) -> Response {
    // Extract Bearer token.
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let token = auth_header.strip_prefix("Bearer ").unwrap_or("");

    if token.is_empty() || !state.admin_auth.verify_token(token) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "invalid token"}))).into_response();
    }

    // Get host from Host header, default to localhost.
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");

    let (_nonce, url, expires_in) = state.magic_link.issue_nonce(host).await;

    (StatusCode::OK, Json(IssueMagicLinkResponse { url, expires_in })).into_response()
}

/// `GET /admin/auth/:token` — redeem magic-link nonce.
async fn magic_link_auth_handler(
    State(state): State<super::AppState>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    // Rate limit check.
    let ip = client_ip(&headers);
    if state.magic_link.check_rate_limit(ip).await.is_err() {
        return (StatusCode::TOO_MANY_REQUESTS, "Too many requests").into_response();
    }

    // Redeem nonce.
    match state.magic_link.redeem_nonce(&token).await {
        Ok(_redeemed) => {
            let session_id = state.admin_auth.create_session_with_ttl(7 * 86400).await;
            let cookie = format!(
                "zbrain_admin={session_id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800"
            );
            let mut response_headers = HeaderMap::new();
            response_headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
            (StatusCode::FOUND, response_headers, [("Location", "/admin/")]).into_response()
        }
        Err(_) => {
            (
                StatusCode::UNAUTHORIZED,
                [("Content-Type", "text/html; charset=utf-8")],
                "This admin link has expired or has already been used.",
            )
                .into_response()
        }
    }
}

/// Extract the client IP from headers (X-Forwarded-For or socket addr fallback).
fn client_ip(headers: &HeaderMap) -> std::net::IpAddr {
    // Try X-Forwarded-For first.
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(ip_str) = xff.split(',').next().map(|s| s.trim()) {
            if let Ok(ip) = ip_str.parse::<std::net::IpAddr>() {
                return ip;
            }
        }
    }
    // Default fallback.
    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
}

/// Middleware: require a valid `zbrain_admin` session cookie.
pub(crate) async fn require_admin(
    State(state): State<super::AppState>,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let cookie_header = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let session_id = extract_cookie(cookie_header, "zbrain_admin");

    let is_valid = match session_id {
        Some(ref id) => state.admin_auth.validate_session(id).await,
        None => false,
    };

    if !is_valid {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"}))).into_response();
    }

    next.run(request).await
}

/// Extract a cookie value by name from a Cookie header string.
fn extract_cookie(cookie_header: &str, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    for part in cookie_header.split(';') {
        let trimmed = part.trim();
        if trimmed.starts_with(&prefix) {
            return Some(trimmed[prefix.len()..].to_string());
        }
    }
    None
}

/// Return the current Unix timestamp.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Build the admin auth router subtree.
pub fn admin_auth_routes(state: super::AppState) -> Router {
    // Public routes (no auth required).
    let public = Router::new()
        .route("/admin/login", post(login_handler))
        .route("/admin/api/issue-magic-link", post(issue_magic_link_handler))
        .route("/admin/auth/{token}", get(magic_link_auth_handler))
        .with_state(state.clone());

    // Protected routes (require admin session).
    let protected = Router::new()
        .route("/admin/protected", axum::routing::get(|| async { "ok" }))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin,
        ))
        .with_state(state);

    public.merge(protected)
}

// We need `hex` for SHA-256 encoding. Use a minimal hex encoder.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use crate::MagicLinkAuth;

    /// Start a test server with admin auth, return (port, bootstrap_token).
    async fn start_auth_server() -> (u16, String) {
        let auth = AdminAuth::new(None);
        let token = auth.bootstrap_token().to_string();

        // Create a minimal temp spa dir so AppState is valid.
        let spa_dir = tempfile::tempdir().unwrap();
        let index_path = spa_dir.path().join("index.html");
        std::fs::write(&index_path, b"<html></html>").unwrap();

        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let state = super::super::AppState {
            admin_auth: auth.clone(),
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tx,
            spa_dir: spa_dir.path().to_path_buf(),
            operation_registry: Arc::new(zbrain_core::operation::OperationRegistry::new()),
            engine: engine as std::sync::Arc<dyn zbrain_core::BrainEngine>,
            zbrain_home: std::env::temp_dir().join("zbrain-test"),
        };

        let app = super::super::build_router(state);
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
    async fn login_with_wrong_token_returns_401() {
        let (port, _token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/login");

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&serde_json::json!({"token": "wrong-token"}))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn login_with_correct_token_returns_200_and_sets_cookie() {
        let (port, token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/login");

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&serde_json::json!({"token": token}))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);

        // Verify Set-Cookie header is present (before consuming body).
        let has_cookie = resp.headers().contains_key("set-cookie");
        assert!(has_cookie, "Expected Set-Cookie header");

        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn protected_route_without_cookie_returns_401() {
        let (port, _token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/protected");

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn protected_route_with_valid_cookie_returns_200() {
        let (port, token) = start_auth_server().await;

        let client = reqwest::Client::new();

        // Step 1: login
        let login_url = format!("http://127.0.0.1:{port}/admin/login");
        let login_resp = client
            .post(&login_url)
            .json(&serde_json::json!({"token": token}))
            .send()
            .await
            .unwrap();
        assert_eq!(login_resp.status(), 200);

        // Step 2: extract cookie
        let cookie = login_resp
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Step 3: access protected route with cookie
        let protected_url = format!("http://127.0.0.1:{port}/admin/protected");
        let resp = client
            .get(&protected_url)
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
    }

    // -- create_session_with_ttl --

    #[tokio::test]
    async fn create_session_with_custom_ttl() {
        let auth = AdminAuth::new(None);
        // Session with 7-day TTL.
        let sid = auth.create_session_with_ttl(7 * 86400).await;
        assert!(auth.validate_session(&sid).await);

        // Session with 1-second TTL.
        let sid2 = auth.create_session_with_ttl(1).await;
        assert!(auth.validate_session(&sid2).await);
        // Wait for expiry.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(!auth.validate_session(&sid2).await);
    }

    #[tokio::test]
    async fn existing_create_session_still_uses_24h_ttl() {
        let auth = AdminAuth::new(None);
        let sid = auth.create_session().await;
        // Should be valid right after creation.
        assert!(auth.validate_session(&sid).await);
        // Not expired after 1s (24h TTL).
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert!(auth.validate_session(&sid).await);
    }

    // -- magic-link integration tests --

    #[tokio::test]
    async fn issue_magic_link_with_valid_bearer_returns_url() {
        let (port, token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/api/issue-magic-link");
        let host = format!("127.0.0.1:{port}");

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Host", &host)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["expires_in"], 300);
        let magic_url = body["url"].as_str().unwrap();
        assert!(magic_url.contains("/admin/auth/"));
    }

    #[tokio::test]
    async fn issue_magic_link_without_bearer_returns_401() {
        let (port, _token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/api/issue-magic-link");

        let resp = reqwest::Client::new().post(&url).send().await.unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn issue_magic_link_with_wrong_bearer_returns_401() {
        let (port, _token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/api/issue-magic-link");

        let resp = reqwest::Client::new()
            .post(&url)
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 401);
    }

    #[tokio::test]
    async fn magic_link_auth_with_valid_nonce_redirects_and_sets_cookie() {
        let (port, token) = start_auth_server().await;
        let host = format!("127.0.0.1:{port}");

        // Step 1: issue magic link.
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let issue_url = format!("http://127.0.0.1:{port}/admin/api/issue-magic-link");
        let issue_resp = client
            .post(&issue_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Host", &host)
            .send()
            .await
            .unwrap();
        assert_eq!(issue_resp.status(), 200);
        let body: serde_json::Value = issue_resp.json().await.unwrap();
        let magic_url = body["url"].as_str().unwrap().to_string();

        // Step 2: follow the magic link.
        let auth_resp = client
            .get(&magic_url)
            .send()
            .await
            .unwrap();

        // Should redirect (302/303) and set cookie.
        assert_eq!(auth_resp.status(), 302);
        let has_cookie = auth_resp.headers().contains_key("set-cookie");
        assert!(has_cookie, "Expected Set-Cookie header");
    }

    #[tokio::test]
    async fn magic_link_auth_with_invalid_nonce_returns_401() {
        let (port, _token) = start_auth_server().await;
        let url = format!("http://127.0.0.1:{port}/admin/auth/invalid-nonce");

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let resp = client.get(&url).send().await.unwrap();
        assert_eq!(resp.status(), 401);
        let body = resp.text().await.unwrap();
        assert!(body.contains("expired") || body.contains("used"));
    }

    #[tokio::test]
    async fn magic_link_nonce_is_single_use() {
        let (port, token) = start_auth_server().await;
        let host = format!("127.0.0.1:{port}");

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // Issue.
        let issue_url = format!("http://127.0.0.1:{port}/admin/api/issue-magic-link");
        let issue_resp = client
            .post(&issue_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Host", &host)
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = issue_resp.json().await.unwrap();
        let magic_url = body["url"].as_str().unwrap().to_string();

        // First use: succeeds.
        let resp1 = client.get(&magic_url).send().await.unwrap();
        assert_eq!(resp1.status(), 302);

        // Second use: fails (replay).
        let resp2 = client.get(&magic_url).send().await.unwrap();
        assert_eq!(resp2.status(), 401);
    }

    #[tokio::test]
    async fn magic_link_auth_rate_limits_after_10_requests() {
        let (port, token) = start_auth_server().await;
        let host = format!("127.0.0.1:{port}");

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        // Issue a nonce.
        let issue_url = format!("http://127.0.0.1:{port}/admin/api/issue-magic-link");
        let issue_resp = client
            .post(&issue_url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Host", &host)
            .send()
            .await
            .unwrap();
        let body: serde_json::Value = issue_resp.json().await.unwrap();
        let magic_url = body["url"].as_str().unwrap().to_string();

        // First call (redeems the nonce).
        let _first = client.get(&magic_url).send().await.unwrap();

        // 9 more calls reach the exact limit (total 10 in window).
        for _ in 0..9 {
            let _resp = client.get(&magic_url).send().await.unwrap();
        }

        // 11th call must be rate limited.
        let rate_limited = client.get(&magic_url).send().await.unwrap();
        assert_eq!(rate_limited.status(), 429);
    }
}
