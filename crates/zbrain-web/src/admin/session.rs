//! Session management endpoints.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde::Serialize;

use super::super::AppState;

/// Response body for `POST /admin/api/sign-out-everywhere`.
#[derive(Serialize)]
struct SignOutEverywhereResponse {
    ok: bool,
    cleared: usize,
}

/// `POST /admin/api/sign-out-everywhere` — invalidate all admin sessions.
async fn sign_out_everywhere_handler(
    State(state): State<AppState>,
) -> Response {
    let cleared = state.admin_auth.clear_all_sessions().await;

    (
        StatusCode::OK,
        Json(SignOutEverywhereResponse {
            ok: true,
            cleared,
        }),
    )
        .into_response()
}

/// Build the session management router subtree.
pub fn build_session_router() -> Router<AppState> {
    Router::new().route("/sign-out-everywhere", post(sign_out_everywhere_handler))
}
