//! Webhook ingestion endpoints — POST /ingest + POST /webhooks/github.
//!
//! Ported from `src/commands/serve-http.ts` (lines 1437-1833).
//!
//! ## POST /ingest
//! - Bearer token auth (write scope required)
//! - Rate limit: 100 req/10s per IP
//! - Content-type detection via X-Zbrain-Content-Type header override
//! - Text content types only (markdown/plain/html/json) in v1
//! - Submits an `ingest_capture` MinionQueue job (worker chunks + stores)
//!
//! ## POST /webhooks/github
//! - Anonymous endpoint (no OAuth token)
//! - HMAC-SHA256 verification via X-Hub-Signature-256 header
//! - Filters for push events only
//! - Source lookup by github_repo config
//! - Branch ref matching against tracked_branch
//! - Submits a `sync` MinionQueue job with priority -10

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use zbrain_core::{
    compute_content_hash, detect_content_type, is_allowed_ingest_content_type,
    minions::queue::MinionQueue,
    minions::types::MinionJobInput,
    validate_ingestion_event, BrainEngine, IngestionEvent,
};

use crate::mcp::extract_bearer_token;

type HmacSha256 = Hmac<Sha256>;

/// Application state needed by webhook handlers.
#[derive(Clone)]
pub struct WebhookState {
    /// Engine for page storage and source lookup.
    pub engine: Arc<dyn BrainEngine>,
    /// Token verification (same as MCP uses).
    pub token_queries: Arc<dyn zbrain_core::TokenQueries>,
}

/// Response body for successful ingest submission.
#[derive(Serialize)]
struct IngestResponse {
    job_id: String,
    content_hash: String,
    source_id: String,
    message: String,
}

/// Response body for GitHub webhook success.
#[derive(Serialize)]
struct GithubWebhookResponse {
    job_id: String,
    source_id: String,
}

/// Response body for GitHub webhook ignored events.
#[derive(Serialize)]
struct GithubIgnoredResponse {
    status: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    received_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tracked_branch: Option<String>,
}

/// Build the webhook router with POST /ingest and POST /webhooks/github.
pub fn build_webhook_router(state: WebhookState) -> Router {
    Router::new()
        .route("/ingest", post(ingest_handler))
        .route("/webhooks/github", post(github_webhook_handler))
        .with_state(state)
}

/// POST /ingest — general webhook ingestion endpoint.
///
/// Auth: Bearer token with write scope.
/// Rate limit: 100 req/10s per IP (TODO: implement rate limiter).
/// Content: text types only (markdown, plain, html, json).
async fn ingest_handler(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // ── Auth: extract and verify bearer token ──────────────────────────
    let token = match extract_bearer_token(&headers) {
        Some(t) => t,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "missing_token",
                    "message": "Bearer token required"
                })),
            )
                .into_response();
        }
    };

    let auth_info = match state.token_queries.verify_access_token(&token).await {
        Ok(info) => info,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "invalid_token",
                    "message": "Access token is invalid or expired"
                })),
            )
                .into_response();
        }
    };

    // Check write scope.
    let has_write = auth_info
        .scopes
        .iter()
        .any(|s| s == "write" || s == "admin");
    if !has_write {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "insufficient_scope",
                "message": "write scope required for POST /ingest"
            })),
        )
            .into_response();
    }

    // ── Empty body check ──────────────────────────────────────────────
    if body.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "empty_body",
                "message": "POST /ingest requires a non-empty body"
            })),
        )
            .into_response();
    }

    // ── Content-type detection ────────────────────────────────────────
    let zbrain_ct = headers
        .get("x-zbrain-content-type")
        .and_then(|v| v.to_str().ok());
    let http_ct = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok());
    let content_type = detect_content_type(zbrain_ct, http_ct);

    if !is_allowed_ingest_content_type(&content_type) {
        let allowed = zbrain_core::INGEST_ALLOWED_CONTENT_TYPES.join(", ");
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({
                "error": "unsupported_content_type",
                "message": format!(
                    "content_type '{}' not supported. Use one of: {}. Binary content (image/audio/video/pdf) is not yet supported via POST /ingest — install a content-type processor skillpack.",
                    content_type, allowed
                )
            })),
        )
            .into_response();
    }

    // ── Build IngestionEvent ──────────────────────────────────────────
    let content = String::from_utf8_lossy(&body).to_string();
    let content_hash = compute_content_hash(&content);

    let source_uri = headers
        .get("x-zbrain-source-uri")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&format!(
            "webhook:{}:{}",
            auth_info.client_id,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ))
        .chars()
        .take(1024)
        .collect::<String>();

    let source_id = headers
        .get("x-zbrain-source-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&format!("webhook-{}", auth_info.client_id))
        .chars()
        .take(256)
        .collect::<String>();

    let caller_slug = headers
        .get("x-zbrain-slug")
        .and_then(|v| v.to_str().ok());

    let received_at = chrono_now_iso();

    let event = IngestionEvent {
        source_id: source_id.clone(),
        source_kind: "webhook".to_string(),
        source_uri,
        received_at,
        content_type: content_type.clone(),
        content: content.clone(),
        content_hash: content_hash.clone(),
        untrusted_payload: Some(true),
        metadata: Some(serde_json::json!({
            "client_id": auth_info.client_id,
        })),
    };

    // Validate the event.
    if let Err(validation_err) = validate_ingestion_event(&event) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_event",
                "message": validation_err.to_string(),
                "field": validation_err.field,
            })),
        )
            .into_response();
    }

    // ── Submit ingest_capture job to MinionQueue ──────────────────────
    // Mirrors TS: MinionQueue.add('ingest_capture', { slug, content, source, ... })
    // The worker picks up the job and calls import_from_content to chunk +
    // store the content as brain pages.

    let slug = caller_slug
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Default slug: inbox/YYYY-MM-DD-{hash_prefix}
            let today = chrono_now_iso()
                .chars()
                .take(10)
                .collect::<String>();
            let hash_prefix: String = content_hash.chars().take(6).collect();
            format!("inbox/{}-{}", today, hash_prefix)
        });

    let job_input = MinionJobInput {
        name: "ingest_capture".to_string(),
        data: Some(serde_json::json!({
            "slug": slug,
            "title": slug,
            "content": content,
            "source": source_id,
        })),
        queue: None,
        priority: None,
        max_attempts: None,
        backoff_type: None,
        backoff_delay: None,
        backoff_jitter: None,
        max_stalled: None,
        delay: None,
        parent_job_id: None,
        on_child_fail: None,
        max_children: None,
        timeout_ms: None,
        remove_on_complete: None,
        remove_on_fail: None,
        idempotency_key: None,
    };

    let queue = MinionQueue::new(state.engine.as_ref());

    match queue.add(&job_input).await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "job_id": job.id.to_string(),
                "content_hash": content_hash,
                "source_id": source_id,
                "message": "Accepted. Event queued for ingestion.",
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "queue_submission_failed",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

/// POST /webhooks/github — GitHub push-triggered sync.
///
/// Anonymous endpoint (GitHub doesn't carry an OAuth token).
/// Auth is via HMAC-SHA256 in X-Hub-Signature-256 header.
async fn github_webhook_handler(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // ── D3: Missing signature → 401 ───────────────────────────────────
    let sig_header = match headers
        .get("X-Hub-Signature-256")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "missing_signature",
                    "message": "X-Hub-Signature-256 header is required"
                })),
            )
                .into_response();
        }
    };

    // ── D5: Filter by event type ──────────────────────────────────────
    let event_type = headers
        .get("X-GitHub-Event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if event_type != "push" {
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "ignored",
                "reason": format!("event={}", if event_type.is_empty() { "(missing)" } else { event_type }),
            })),
        )
            .into_response();
    }

    // ── Empty body check ──────────────────────────────────────────────
    if body.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "empty_body"})),
        )
            .into_response();
    }

    // ── Parse JSON payload ────────────────────────────────────────────
    let payload_str = String::from_utf8_lossy(&body);
    let parsed: serde_json::Value = match serde_json::from_str(&payload_str) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "malformed_json"})),
            )
                .into_response();
        }
    };

    let full_name = parsed
        .get("repository")
        .and_then(|r| r.get("full_name"))
        .and_then(|v| v.as_str());
    let ref_name = parsed.get("ref").and_then(|v| v.as_str());

    let (Some(full_name), Some(ref_name)) = (full_name, ref_name) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "missing_fields",
                "message": "repository.full_name and ref are required"
            })),
        )
            .into_response();
    };

    // ── Source lookup by github_repo ──────────────────────────────────
    let source = match state
        .engine
        .get_source_by_github_repo(full_name)
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": "unknown_repo",
                    "repo": full_name
                })),
            )
                .into_response();
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "lookup_failed",
                    "message": e.to_string()
                })),
            )
                .into_response();
        }
    };

    // ── Extract config ────────────────────────────────────────────────
    let webhook_secret = source
        .config
        .get("webhook_secret")
        .and_then(|v| v.as_str());
    let tracked_branch = source
        .config
        .get("tracked_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");

    // ── D5: Branch ref matching ───────────────────────────────────────
    let expected_ref = format!("refs/heads/{}", tracked_branch);
    if ref_name != expected_ref {
        return (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "ignored",
                "reason": "ref_mismatch",
                "received_ref": ref_name,
                "tracked_branch": tracked_branch,
            })),
        )
            .into_response();
    }

    let Some(secret) = webhook_secret else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "webhook_not_configured",
                "message": format!("Run: zbrain sources webhook set {}", source.id)
            })),
        )
            .into_response();
    };

    // ── HMAC-SHA256 verification ──────────────────────────────────────
    // GitHub sends "sha256=<hex>". Strip prefix BEFORE constant-time compare.
    // Pinned by TS tests/unit/sources-webhook.test.ts.
    const PREFIX: &str = "sha256=";
    if !sig_header.starts_with(PREFIX) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "signature_mismatch",
                "message": "expected sha256= prefix"
            })),
        )
            .into_response();
    }

    let expected_hex = sig_header
        .strip_prefix(PREFIX)
        .unwrap_or("");
    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal_error"})),
            )
                .into_response();
        }
    };
    mac.update(&body);

    // Decode expected hex to bytes for constant-time comparison.
    let expected_bytes = match hex::decode(expected_hex) {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "signature_mismatch"})),
            )
                .into_response();
        }
    };

    // Use constant-time comparison via the subtle crate.
    // hmac.verify_slice() already uses constant-time comparison internally.
    if mac.verify_slice(&expected_bytes).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "signature_mismatch"})),
        )
            .into_response();
    }

    // ── Submit sync job to MinionQueue ────────────────────────────────
    // Mirrors TS: MinionQueue.add('sync', { sourceId, repoPath, ... }, { priority: -10 })
    // Push-triggered sync preempts autopilot's default priority 0.

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let idem_key = format!("sync-trigger:{}:{}", source.id, now_secs / 30);

    let job_input = MinionJobInput {
        name: "sync".to_string(),
        data: Some(serde_json::json!({
            "sourceId": source.id,
            "repoPath": source.local_path,
            "auto_embed_backfill": true,
            "embed_reason": "sync_trigger",
        })),
        queue: None,
        priority: Some(-10),
        max_attempts: None,
        backoff_type: None,
        backoff_delay: None,
        backoff_jitter: None,
        max_stalled: None,
        delay: None,
        parent_job_id: None,
        on_child_fail: None,
        max_children: None,
        timeout_ms: None,
        remove_on_complete: None,
        remove_on_fail: None,
        idempotency_key: Some(idem_key),
    };

    let queue = MinionQueue::new(state.engine.as_ref());

    match queue.add(&job_input).await {
        Ok(job) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "job_id": job.id.to_string(),
                "source_id": source.id,
            })),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "queue_submission_failed",
                "message": e.to_string(),
            })),
        )
            .into_response(),
    }
}

/// Get current UTC time as ISO 8601 string.
/// Uses a simple format since we avoid chrono dependency for now.
fn chrono_now_iso() -> String {
    // We use std::time and format manually.
    // For a real ISO timestamp, we'd use chrono, but to avoid the dependency:
    // Generate a simple ISO-like timestamp.
    // In practice, the engine should provide a time utility.
    // For now, use a fixed format.
    zbrain_core::time::current_utc_iso8601()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tower::ServiceExt;
    use zbrain_core::{InMemoryEngine, OAuthQueries, SourceRow};

    /// Create a test engine with webhook source pre-configured.
    fn test_engine_with_source() -> Arc<dyn BrainEngine> {
        let engine = InMemoryEngine::default();
        engine.add_source(SourceRow {
            id: "gh-source-1".into(),
            name: "test-repo".into(),
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: serde_json::json!({
                "github_repo": "owner/test-repo",
                "webhook_secret": "super-secret-key",
                "tracked_branch": "main",
            }),
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        });
        Arc::new(engine)
    }

    fn test_webhook_state(
        engine: Arc<dyn BrainEngine>,
    ) -> WebhookState {
        let token_engine = InMemoryEngine::default();
        // Register a client and issue a token for testing /ingest
        WebhookState {
            engine,
            token_queries: Arc::new(token_engine) as Arc<dyn zbrain_core::TokenQueries>,
        }
    }

    fn build_router(state: WebhookState) -> Router {
        build_webhook_router(state)
    }

    /// Build a GitHub HMAC signature for a payload.
    fn github_sig(secret: &str, payload: &[u8]) -> String {
        use hmac::Mac;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(payload);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    /// Minimal GitHub push payload.
    fn push_payload(repo: &str, ref_name: &str) -> serde_json::Value {
        serde_json::json!({
            "ref": ref_name,
            "repository": {
                "full_name": repo,
                "name": repo.split('/').last().unwrap_or(""),
                "owner": { "name": repo.split('/').next().unwrap_or("") }
            },
            "head_commit": { "id": "abc123def456" }
        })
    }

    // ─── POST /webhooks/github tests ──────────────────────────────────

    #[tokio::test]
    async fn github_webhook_missing_signature_returns_401() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let req = Request::post("/webhooks/github")
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn github_webhook_non_push_event_returns_202_ignored() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let sig = github_sig("super-secret-key", &serde_json::to_vec(&payload).unwrap());
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "ping")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&payload).unwrap()))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let body: serde_json::Value = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .map(|b| serde_json::from_slice(&b).unwrap_or_default())
            .unwrap_or_default();
        assert_eq!(body["status"], "ignored");
        assert!(body["reason"].as_str().unwrap().contains("ping"));
    }

    #[tokio::test]
    async fn github_webhook_valid_signature_on_push_returns_202() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("super-secret-key", &payload_bytes);
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let body: serde_json::Value = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .map(|b| serde_json::from_slice(&b).unwrap_or_default())
            .unwrap_or_default();
        assert!(body["job_id"].is_string());
        assert_eq!(body["source_id"], "gh-source-1");
    }

    #[tokio::test]
    async fn github_webhook_wrong_secret_returns_401() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("wrong-secret", &payload_bytes);
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn github_webhook_tampered_payload_returns_401() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("super-secret-key", &payload_bytes);

        // Tamper with the payload
        let mut tampered = payload_bytes.clone();
        if !tampered.is_empty() {
            tampered[10] ^= 0xff;
        }

        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(tampered))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn github_webhook_unknown_repo_returns_404() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/unknown-repo", "refs/heads/main");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("some-secret", &payload_bytes);
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn github_webhook_wrong_branch_returns_202_ignored() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/develop");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("super-secret-key", &payload_bytes);
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let body: serde_json::Value = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .map(|b| serde_json::from_slice(&b).unwrap_or_default())
            .unwrap_or_default();
        assert_eq!(body["status"], "ignored");
        assert_eq!(body["reason"], "ref_mismatch");
        assert_eq!(body["tracked_branch"], "main");
    }

    #[tokio::test]
    async fn github_webhook_malformed_json_returns_400() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", "sha256=0000000000000000000000000000000000000000000000000000000000000000")
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from("{not valid json"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn github_webhook_missing_repo_fields_returns_400() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = serde_json::json!({"ref": "refs/heads/main"});
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("super-secret-key", &payload_bytes);
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn github_webhook_sig_without_prefix_returns_401() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(engine);
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        // Send raw hex without sha256= prefix
        let mut mac = HmacSha256::new_from_slice("super-secret-key".as_bytes()).unwrap();
        mac.update(&payload_bytes);
        let raw_hex = hex::encode(mac.finalize().into_bytes());

        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &raw_hex)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    // ─── POST /ingest tests ───────────────────────────────────────────

    /// Create an engine that has a token with write scope.
    fn ingest_test_state() -> WebhookState {
        let engine = Arc::new(InMemoryEngine::default()) as Arc<dyn BrainEngine>;
        let token_engine = InMemoryEngine::default();
        WebhookState {
            engine,
            token_queries: Arc::new(token_engine) as Arc<dyn zbrain_core::TokenQueries>,
        }
    }

    #[tokio::test]
    async fn ingest_missing_token_returns_401() {
        let state = ingest_test_state();
        let app = build_router(state);

        let req = Request::post("/ingest")
            .header("content-type", "text/markdown")
            .body(Body::from("# Hello"))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn ingest_empty_body_with_any_token_returns_400() {
        // InMemoryEngine accepts any non-empty token (test behavior).
        // The empty body check fires after auth, returning 400.
        let state = ingest_test_state();
        let app = build_router(state);

        let req = Request::post("/ingest")
            .header("Authorization", "Bearer some-token")
            .header("content-type", "text/markdown")
            .body(Body::from(""))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn ingest_binary_content_type_returns_415() {
        let engine = Arc::new(InMemoryEngine::default()) as Arc<dyn BrainEngine>;
        let token_engine = InMemoryEngine::default();
        // Register a client with write scope
        let _ = token_engine
            .exchange_client_credentials("test-client", "test-secret", Some("write"))
            .await;
        // Issue a token
        let tokens = token_engine
            .exchange_client_credentials("test-client", "test-secret", Some("write"))
            .await
            .unwrap();
        let state = WebhookState {
            engine,
            token_queries: Arc::new(token_engine) as Arc<dyn zbrain_core::TokenQueries>,
        };
        let app = build_router(state);

        let req = Request::post("/ingest")
            .header("Authorization", format!("Bearer {}", tokens.access_token))
            .header("content-type", "image/png")
            .body(Body::from(vec![0x89, 0x50, 0x4E, 0x47]))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn ingest_submits_ingest_capture_job_to_queue() {
        let engine = Arc::new(InMemoryEngine::default()) as Arc<dyn BrainEngine>;
        let token_engine = InMemoryEngine::default();
        let _ = token_engine
            .exchange_client_credentials("test-client", "test-secret", Some("write"))
            .await;
        let tokens = token_engine
            .exchange_client_credentials("test-client", "test-secret", Some("write"))
            .await
            .unwrap();
        let state = WebhookState {
            engine: Arc::clone(&engine),
            token_queries: Arc::new(token_engine) as Arc<dyn zbrain_core::TokenQueries>,
        };
        let app = build_router(state);

        let req = Request::post("/ingest")
            .header("Authorization", format!("Bearer {}", tokens.access_token))
            .header("content-type", "text/markdown")
            .header("x-zbrain-slug", "test/ingest-job")
            .body(Body::from("# Hello World\n\nSome content."))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let body: serde_json::Value = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .map(|b| serde_json::from_slice(&b).unwrap_or_default())
            .unwrap_or_default();
        assert!(body["job_id"].is_string());
        assert!(body["content_hash"].is_string());

        // Verify the job was submitted to the queue
        let queue = zbrain_core::minions::queue::MinionQueue::new(engine.as_ref());
        let filters = zbrain_core::minions::types::JobFilters::default();
        let jobs = queue.get_jobs(&filters).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "ingest_capture");
        assert_eq!(jobs[0].data["slug"], "test/ingest-job");
        assert_eq!(jobs[0].data["content"], "# Hello World\n\nSome content.");
        assert_eq!(jobs[0].data["source"], "webhook-test-client");
    }

    #[tokio::test]
    async fn github_webhook_submits_sync_job_with_priority_neg10() {
        let engine = test_engine_with_source();
        let state = test_webhook_state(Arc::clone(&engine));
        let app = build_router(state);

        let payload = push_payload("owner/test-repo", "refs/heads/main");
        let payload_bytes = serde_json::to_vec(&payload).unwrap();
        let sig = github_sig("super-secret-key", &payload_bytes);
        let req = Request::post("/webhooks/github")
            .header("X-Hub-Signature-256", &sig)
            .header("X-GitHub-Event", "push")
            .header("content-type", "application/json")
            .body(Body::from(payload_bytes))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);

        let body: serde_json::Value = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .map(|b| serde_json::from_slice(&b).unwrap_or_default())
            .unwrap_or_default();
        assert!(body["job_id"].is_string());
        assert_eq!(body["source_id"], "gh-source-1");

        // Verify the sync job was submitted with priority -10
        let queue = zbrain_core::minions::queue::MinionQueue::new(engine.as_ref());
        let filters = zbrain_core::minions::types::JobFilters::default();
        let jobs = queue.get_jobs(&filters).await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "sync");
        assert_eq!(jobs[0].priority, -10);
        assert_eq!(jobs[0].data["sourceId"], "gh-source-1");
        assert_eq!(jobs[0].data["auto_embed_backfill"], true);
        assert_eq!(jobs[0].data["embed_reason"], "sync_trigger");
        // Idempotency key should be set
        assert!(jobs[0]
            .idempotency_key
            .as_ref()
            .unwrap()
            .starts_with("sync-trigger:gh-source-1:"));
    }
}
