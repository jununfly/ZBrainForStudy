//! Admin request log endpoint with pagination and filters.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;

use super::super::AppState;
use zbrain_core::RequestLogFilters;

/// Build the requests router.
pub fn build_requests_router() -> Router<AppState> {
    Router::new().route("/requests", get(get_requests_handler))
}

/// Query parameters for request log filtering.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestsQuery {
    page: Option<u32>,
    limit: Option<u32>,
    source: Option<String>,
    method: Option<String>,
    status: Option<String>,
}

impl From<RequestsQuery> for RequestLogFilters {
    fn from(q: RequestsQuery) -> Self {
        RequestLogFilters {
            source: q.source,
            method: q.method,
            status: q.status,
            page: q.page,
            limit: q.limit,
        }
    }
}

async fn get_requests_handler(
    State(state): State<AppState>,
    Query(query): Query<RequestsQuery>,
) -> Response {
    let filters: RequestLogFilters = query.into();
    match state.admin_queries.list_requests(&filters).await {
        Ok(result) => {
            let response = serde_json::json!({
                "ok": true,
                "items": result.items,
                "total": result.total,
                "page": result.page,
                "limit": result.limit,
            });
            Json(response).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}
