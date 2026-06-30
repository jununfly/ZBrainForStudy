//! Admin API keys management endpoints.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::super::AppState;

/// Build the api_keys router.
pub fn build_api_keys_router() -> Router<AppState> {
    Router::new()
        .route("/api-keys", get(list_api_keys_handler))
        .route("/api-keys", post(create_api_key_handler))
        .route("/api-keys/revoke", post(revoke_api_key_handler))
}

#[derive(Serialize)]
struct ListApiKeysResponse {
    ok: bool,
    keys: Vec<zbrain_core::ApiKey>,
}

async fn list_api_keys_handler(
    State(state): State<AppState>,
) -> Response {
    match state.admin_queries.list_api_keys().await {
        Ok(keys) => Json(ListApiKeysResponse { ok: true, keys }).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CreateApiKeyRequest {
    name: String,
}

#[derive(Serialize)]
struct CreateApiKeyResponse {
    ok: bool,
    key: Option<zbrain_core::ApiKey>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn create_api_key_handler(
    State(state): State<AppState>,
    Json(body): Json<CreateApiKeyRequest>,
) -> Response {
    match state.admin_queries.create_api_key(&body.name).await {
        Ok(key) => Json(CreateApiKeyResponse {
            ok: true,
            key: Some(key),
            error: None,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(CreateApiKeyResponse {
                ok: false,
                key: None,
                error: Some(e.to_string()),
            }),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RevokeApiKeyRequest {
    name: String,
}

async fn revoke_api_key_handler(
    State(state): State<AppState>,
    Json(body): Json<RevokeApiKeyRequest>,
) -> Response {
    match state.admin_queries.revoke_api_key(&body.name).await {
        Ok(()) => Json(serde_json::json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}
