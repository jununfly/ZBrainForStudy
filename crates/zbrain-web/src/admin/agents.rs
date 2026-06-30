//! Admin agents list endpoint.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use super::super::AppState;

/// Build the agents router.
pub fn build_agents_router() -> Router<AppState> {
    Router::new().route("/agents", get(get_agents_handler))
}

async fn get_agents_handler(
    State(state): State<AppState>,
) -> Response {
    match state.admin_queries.list_agents().await {
        Ok(agents) => Json(serde_json::json!({"ok": true, "agents": agents})).into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}
