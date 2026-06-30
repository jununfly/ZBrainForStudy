//! SSE live activity feed — `/admin/events`.
//!
//! Publishes MCP request events to connected admin-dashboard browsers
//! via Server-Sent Events. The broadcast channel lives in `AppState`
//! so that the MCP handler (once wired) can publish through the same sender.

use axum::{
    extract::State,
    middleware,
    response::{sse::Event, Sse},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

/// An activity event published to SSE clients.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    /// Agent / client identifier.
    pub agent: String,
    /// Operation name (e.g. "tools/call", "tools/list").
    pub operation: String,
    /// Comma-separated scopes.
    pub scopes: String,
    /// Request latency in milliseconds.
    pub latency_ms: u64,
    /// Outcome status: "success" or "error".
    pub status: String,
    /// ISO 8601 timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Operation parameters (redacted summary or full payload).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Error details (only present when status is "error").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

impl ActivityEvent {
    /// Create a minimal "connected" heartbeat event.
    pub fn connected() -> Self {
        Self {
            agent: "system".into(),
            operation: "connected".into(),
            scopes: String::new(),
            latency_ms: 0,
            status: "success".into(),
            timestamp: None,
            params: None,
            error: None,
        }
    }
}

/// Build the SSE router subtree. Mount at `/admin/events`.
/// Protected by `require_admin` middleware.
pub fn build_events_router(state: crate::AppState) -> Router {
    Router::new()
        .route("/admin/events", get(sse_handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_admin,
        ))
        .with_state(state)
}

/// SSE handler — subscribe to the broadcast channel and stream events.
async fn sse_handler(
    State(state): State<crate::AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.activity_tx.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let data = serde_json::to_string(&event).unwrap_or_default();
            Some(Ok(Event::default().data(data)))
        }
        Err(_) => {
            // Client fell behind or sender dropped; skip.
            None
        }
    });

    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        )
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;

    use crate::auth::AdminAuth;
    use crate::{build_router, AppState};

    fn make_spa_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("index.html"), b"<html></html>").unwrap();
        (dir, path)
    }

    /// Start admin server with SSE broadcast channel.
    /// Returns (port, token, sender) so callers can publish events.
    async fn start_sse_server() -> (u16, String, broadcast::Sender<ActivityEvent>) {
        let auth = AdminAuth::new(None);
        let token = auth.bootstrap_token().to_string();
        let (_dir, spa_path) = make_spa_dir();

        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let (tx, _rx) = broadcast::channel(64);

        let state = AppState {
            admin_auth: auth.clone(),
            admin_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
            activity_tx: tx.clone(),
            spa_dir: spa_path,
        };

        let app = build_router(state);
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        (port, token, tx)
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
    async fn sse_endpoint_returns_text_event_stream_and_keep_alive() {
        let (port, token, _tx) = start_sse_server().await;
        let cookie = login_admin(port, &token).await;

        let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let request = format!(
            "GET /admin/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\nAccept: text/event-stream\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read the HTTP response headers
        let mut buf = Vec::new();
        let mut temp = [0u8; 1];
        loop {
            stream.readable().await.unwrap();
            match stream.try_read(&mut temp) {
                Ok(1) => {
                    buf.push(temp[0]);
                    let len = buf.len();
                    if len >= 4
                        && buf[len - 4] == b'\r'
                        && buf[len - 3] == b'\n'
                        && buf[len - 2] == b'\r'
                        && buf[len - 1] == b'\n'
                    {
                        break;
                    }
                }
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }

        let header_text = String::from_utf8_lossy(&buf);
        assert!(
            header_text.contains("200"),
            "expected 200, got: {header_text}"
        );
        assert!(
            header_text.to_lowercase().contains("text/event-stream"),
            "expected text/event-stream content-type, got: {header_text}"
        );
    }

    #[tokio::test]
    async fn sse_receives_published_event() {
        let (port, token, tx) = start_sse_server().await;
        let cookie = login_admin(port, &token).await;

        let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let request = format!(
            "GET /admin/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\nAccept: text/event-stream\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read the HTTP response headers byte-by-byte until we find \r\n\r\n
        let mut buf = Vec::new();
        let mut temp = [0u8; 1];
        loop {
            stream.readable().await.unwrap();
            match stream.try_read(&mut temp) {
                Ok(1) => {
                    buf.push(temp[0]);
                    // Check for end of headers: \r\n\r\n
                    let len = buf.len();
                    if len >= 4
                        && buf[len - 4] == b'\r'
                        && buf[len - 3] == b'\n'
                        && buf[len - 2] == b'\r'
                        && buf[len - 1] == b'\n'
                    {
                        break;
                    }
                }
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }

        // Verify 200 OK in headers
        let header_text = String::from_utf8_lossy(&buf);
        assert!(
            header_text.contains("200"),
            "expected 200, got headers: {header_text}"
        );

        // Publish a test event
        let test_event = ActivityEvent {
            agent: "test-agent".into(),
            operation: "tools/call".into(),
            scopes: "read,write".into(),
            latency_ms: 42,
            status: "success".into(),
            timestamp: Some("2024-01-01T00:00:00Z".into()),
            params: Some(serde_json::json!({"name": "put_page"})),
            error: None,
        };
        tx.send(test_event.clone()).unwrap();

        // Now read SSE data — use a timeout since we know an event should arrive
        let mut body = Vec::new();
        let start = std::time::Instant::now();
        loop {
            if start.elapsed() > Duration::from_secs(5) {
                break;
            }
            match stream.try_read(&mut temp) {
                Ok(1) => {
                    body.push(temp[0]);
                    // SSE events end with \n\n
                    let len = body.len();
                    if len >= 2 && body[len - 2] == b'\n' && body[len - 1] == b'\n' {
                        break;
                    }
                }
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }

        let body_text = String::from_utf8_lossy(&body);
        assert!(
            body_text.contains("data:"),
            "expected 'data:' in SSE body, got: {body_text}"
        );
        assert!(
            body_text.contains("test-agent"),
            "expected 'test-agent' in SSE body, got: {body_text}"
        );
        assert!(
            body_text.contains("tools/call"),
            "expected 'tools/call' in SSE body, got: {body_text}"
        );
    }

    #[tokio::test]
    async fn sse_requires_admin_auth() {
        let (port, _token, _tx) = start_sse_server().await;

        let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();

        let request = format!(
            "GET /admin/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read the HTTP response headers
        let mut buf = Vec::new();
        let mut temp = [0u8; 1];
        loop {
            stream.readable().await.unwrap();
            match stream.try_read(&mut temp) {
                Ok(1) => {
                    buf.push(temp[0]);
                    let len = buf.len();
                    if len >= 4
                        && buf[len - 4] == b'\r'
                        && buf[len - 3] == b'\n'
                        && buf[len - 2] == b'\r'
                        && buf[len - 1] == b'\n'
                    {
                        break;
                    }
                }
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }

        let header_text = String::from_utf8_lossy(&buf);
        assert!(
            header_text.contains("401"),
            "expected 401 without auth, got: {header_text}"
        );
    }

    #[tokio::test]
    async fn broadcast_channel_distributes_to_multiple_subscribers() {
        let (tx, _rx) = broadcast::channel(16);

        let mut rx1 = tx.subscribe();
        let mut rx2 = tx.subscribe();

        let event = ActivityEvent {
            agent: "multi".into(),
            operation: "test".into(),
            scopes: "read".into(),
            latency_ms: 10,
            status: "success".into(),
            timestamp: None,
            params: None,
            error: None,
        };

        tx.send(event).unwrap();

        let received1 = rx1.recv().await.unwrap();
        let received2 = rx2.recv().await.unwrap();

        assert_eq!(received1.agent, "multi");
        assert_eq!(received2.agent, "multi");
    }
}
