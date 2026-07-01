//! Source CRUD API routes (1-7-1-1).
//!
//! Mounted under `/admin/api/*` (bare paths due to admin router merge).
//! All routes require admin auth via the admin router's middleware layer.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::super::AppState;
use zbrain_core::{
    add_source, AddSourceOpts, CreateSourceInput, SourceOpError, UpdateSourceInput,
};

/// Build the sources router with CRUD endpoints and import.
pub fn build_sources_router() -> Router<AppState> {
    Router::new()
        .route("/sources", get(list_sources_handler))
        .route("/sources", post(create_source_handler))
        .route("/sources/import", post(import_source_handler))
        .route("/sources/{id}", get(get_source_handler))
        .route("/sources/{id}", put(update_source_handler))
        .route("/sources/{id}", delete(delete_source_handler))
}

/// Request body for creating a source.
#[derive(Debug, Deserialize)]
struct CreateSourceRequest {
    id: String,
    name: String,
    #[serde(default)]
    config: Option<serde_json::Value>,
}

/// Request body for updating a source.
#[derive(Debug, Deserialize)]
struct UpdateSourceRequest {
    name: Option<String>,
    config: Option<serde_json::Value>,
    local_path: Option<String>,
    last_commit: Option<String>,
    last_sync_at: Option<String>,
    chunker_version: Option<String>,
    contextual_retrieval_mode: Option<String>,
    trust_frontmatter_overrides: Option<bool>,
}

/// Wrapper response matching the admin API convention.
#[derive(Serialize)]
struct OkResponse<T: Serialize> {
    ok: bool,
    data: T,
}

async fn list_sources_handler(
    State(state): State<AppState>,
) -> impl IntoResponse {
    match state.engine.list_sources(false).await {
        Ok(sources) => Json(OkResponse { ok: true, data: sources }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn get_source_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.engine.get_source(&id).await {
        Ok(Some(source)) => Json(OkResponse { ok: true, data: source }).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": format!("source '{}' not found", id)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn create_source_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateSourceRequest>,
) -> impl IntoResponse {
    let input = CreateSourceInput {
        id: body.id,
        name: body.name,
        config: body.config,
    };
    match state.engine.create_source(&input).await {
        Ok(source) => (
            StatusCode::CREATED,
            Json(OkResponse { ok: true, data: source }),
        )
            .into_response(),
        Err(e) => {
            let status = if e.to_string().contains("invalid source id") {
                StatusCode::BAD_REQUEST
            } else if e.to_string().contains("already exists") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
    }
}

async fn update_source_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateSourceRequest>,
) -> impl IntoResponse {
    let input = UpdateSourceInput {
        name: body.name,
        config: body.config,
        local_path: body.local_path,
        last_commit: body.last_commit,
        last_sync_at: body.last_sync_at,
        chunker_version: body.chunker_version,
        contextual_retrieval_mode: body.contextual_retrieval_mode,
        trust_frontmatter_overrides: body.trust_frontmatter_overrides,
    };
    match state.engine.update_source(&id, &input).await {
        Ok(source) => Json(OkResponse { ok: true, data: source }).into_response(),
        Err(e) => {
            let status = if e.to_string().contains("not found") {
                StatusCode::NOT_FOUND
            } else if e.to_string().contains("already taken") {
                StatusCode::CONFLICT
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (
                status,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
    }
}

async fn delete_source_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match state.engine.delete_source(&id).await {
        Ok(true) => Json(OkResponse {
            ok: true,
            data: serde_json::json!({"archived": id}),
        })
        .into_response(),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"ok": false, "error": format!("source '{}' not found or already archived", id)})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Request body for importing a source (clone from remote URL).
#[derive(Debug, Deserialize)]
struct ImportSourceRequest {
    id: String,
    /// Remote git URL (https:// only)
    url: String,
    /// Display name (defaults to id)
    #[serde(default)]
    name: Option<String>,
    /// Clone depth (default 1, 0 for full clone)
    #[serde(default = "default_depth")]
    depth: u32,
    /// Branch to clone (default: repo default branch)
    #[serde(default)]
    branch: Option<String>,
    /// Whether this is a federated source
    #[serde(default)]
    federated: Option<bool>,
}

fn default_depth() -> u32 {
    1
}

async fn import_source_handler(
    State(state): State<AppState>,
    Json(body): Json<ImportSourceRequest>,
) -> impl IntoResponse {
    let opts = AddSourceOpts {
        id: body.id,
        name: body.name,
        local_path: None,
        remote_url: Some(body.url),
        federated: body.federated,
        clone_dir: None,
        depth: body.depth,
        branch: body.branch,
    };

    match add_source(state.engine.as_ref(), opts, &state.zbrain_home).await {
        Ok(source) => (
            StatusCode::CREATED,
            Json(OkResponse {
                ok: true,
                data: source,
            }),
        )
            .into_response(),
        Err(e) => {
            let status = source_op_error_to_status(&e);
            (
                status,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// Map a SourceOpError code to an HTTP status code.
fn source_op_error_to_status(e: &SourceOpError) -> StatusCode {
    match e.code {
        zbrain_core::SourceOpErrorCode::InvalidId
        | zbrain_core::SourceOpErrorCode::InvalidRemoteUrl => StatusCode::BAD_REQUEST,
        zbrain_core::SourceOpErrorCode::SourceIdTaken
        | zbrain_core::SourceOpErrorCode::OverlappingPath => StatusCode::CONFLICT,
        zbrain_core::SourceOpErrorCode::NotFound => StatusCode::NOT_FOUND,
        zbrain_core::SourceOpErrorCode::ProtectedId => StatusCode::FORBIDDEN,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, header};
    use std::sync::Arc;
    use tower::ServiceExt;
    use zbrain_core::{InMemoryEngine, SourceRow};
    use crate::AdminAuth;
    use crate::MagicLinkAuth;

    fn test_state() -> AppState {
        let engine = Arc::new(InMemoryEngine::default());
        // Pre-seed the default source that 0001_init creates
        engine.add_source(SourceRow {
            id: "default".into(),
            name: "default".into(),
            local_path: None,
            last_commit: None,
            last_sync_at: None,
            config: serde_json::json!({}),
            created_at: None,
            chunker_version: None,
            archived: false,
            archived_at: None,
            archive_expires_at: None,
            contextual_retrieval_mode: None,
            trust_frontmatter_overrides: false,
        });
        AppState {
            admin_auth: AdminAuth::new(None),
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone() as Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone() as Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone() as Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine.clone() as Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tokio::sync::broadcast::channel(64).0,
            spa_dir: std::env::temp_dir(),
            operation_registry: Arc::new(zbrain_core::operation::OperationRegistry::new()),
            engine: engine as Arc<dyn zbrain_core::BrainEngine>,
            zbrain_home: std::env::temp_dir().join("zbrain-test"),
        }
    }

    #[tokio::test]
    async fn list_sources_returns_seeded_default() {
        let state = test_state();
        let router = build_sources_router().with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/sources")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["ok"], true);
        let sources = json["data"].as_array().unwrap();
        assert!(sources.iter().any(|s| s["id"] == "default"));
    }

    #[tokio::test]
    async fn create_and_get_source() {
        let state = test_state();
        let router = build_sources_router().with_state(state);

        // Create
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sources")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"id": "test-src", "name": "Test Source"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        // Get
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/sources/test-src")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["id"], "test-src");
        assert_eq!(json["data"]["name"], "Test Source");
    }

    #[tokio::test]
    async fn create_source_rejects_invalid_id() {
        let state = test_state();
        let router = build_sources_router().with_state(state);

        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sources")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"id": "INVALID_ID", "name": "Bad"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_source_changes_name() {
        let state = test_state();
        let router = build_sources_router().with_state(state.clone());

        // Create first
        let _ = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sources")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"id": "update-me", "name": "Original"}"#,
                    ))
                    .unwrap(),
            )
            .await;

        // Update
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/sources/update-me")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"name": "Renamed"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["data"]["name"], "Renamed");
    }

    #[tokio::test]
    async fn delete_source_archives() {
        let state = test_state();
        let router = build_sources_router().with_state(state.clone());

        // Create
        let _ = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sources")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"id": "to-archive", "name": "Archive Me"}"#,
                    ))
                    .unwrap(),
            )
            .await;

        // Delete (archive)
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/sources/to-archive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // Verify archived — still in list when include_archived is not exposed via route,
        // but second delete should return 404
        let response = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/sources/to-archive")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
