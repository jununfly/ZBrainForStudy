//! Admin dashboard stats endpoints.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Serialize;

use super::super::AppState;

/// Build the stats router with 3 endpoints.
pub fn build_stats_router() -> Router<AppState> {
    Router::new()
        .route("/stats", get(get_stats_handler))
        .route("/health-indicators", get(get_health_indicators_handler))
        .route("/full-stats", get(get_full_stats_handler))
}

/// Wrapper to flatten the JSON response (avoids `{ok: true, data: ...}`).
#[derive(Serialize)]
struct OkResponse<T: Serialize> {
    ok: bool,
    #[serde(flatten)]
    data: T,
}

async fn get_stats_handler(
    State(state): State<AppState>,
) -> Response {
    match state.admin_queries.get_stats().await {
        Ok(stats) => Json(OkResponse { ok: true, data: stats }).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn get_health_indicators_handler(
    State(state): State<AppState>,
) -> Response {
    match state.admin_queries.check_health_indicators().await {
        Ok(indicators) => Json(OkResponse { ok: true, data: indicators }).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn get_full_stats_handler(
    State(state): State<AppState>,
) -> Response {
    match state.admin_queries.get_full_stats().await {
        Ok(full_stats) => Json(OkResponse { ok: true, data: full_stats }).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}
