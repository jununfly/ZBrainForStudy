//! Admin API routes mounted under `/admin/api/*`.
//!
//! All routes require a valid admin session (via `require_admin` middleware).
//! Each sub-module returns its own `Router`; `build_admin_router()` merges them
//! under a shared middleware layer.

mod agents;
mod api_keys;
mod requests;
mod session;
mod spend;
mod stats;
mod watch;

use axum::{middleware, Router};

/// Build the admin API router with all sub-routers merged under
/// `/admin/api` and protected by `require_admin` middleware.
pub fn build_admin_router(state: super::AppState) -> Router {
    Router::new()
        .merge(session::build_session_router())
        .merge(stats::build_stats_router())
        .merge(agents::build_agents_router())
        .merge(api_keys::build_api_keys_router())
        .merge(requests::build_requests_router())
        .merge(spend::build_spend_router())
        .merge(watch::build_watch_router())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            super::auth::require_admin,
        ))
        .with_state(state)
}
