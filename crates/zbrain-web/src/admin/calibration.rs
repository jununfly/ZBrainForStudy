//! Admin calibration endpoints: profile, charts, and pattern drill-down.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use super::super::AppState;

/// Build the calibration router.
pub fn build_calibration_router() -> Router<AppState> {
    Router::new()
        .route("/calibration/profile", get(get_profile_handler))
        .route("/calibration/charts/{chart_type}", get(get_chart_handler))
        .route("/calibration/pattern/{id}", get(get_pattern_handler))
}

// ─── Handlers ──────────────────────────────────────────────────────────

async fn get_profile_handler(State(state): State<AppState>) -> Response {
    match state.calibration_queries.get_latest_profile("garry").await {
        Ok(profile) => Json(serde_json::json!({
            "ok": true,
            "data": profile,
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn get_chart_handler(
    State(state): State<AppState>,
    Path(chart_type): Path<String>,
) -> Response {
    // Fetch profile data for chart rendering
    let profile = match state.calibration_queries.get_latest_profile("garry").await {
        Ok(p) => p,
        Err(e) => {
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"ok": false, "error": e.to_string()})),
            )
                .into_response();
        }
    };

    let svg = match chart_type.as_str() {
        "brier-trend" => {
            let series: Vec<zbrain_svg::BrierTrendPoint> = profile
                .as_ref()
                .and_then(|p| p.domain_scorecards.as_ref())
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            Some(zbrain_svg::BrierTrendPoint {
                                date: item.get("date")?.as_str()?.to_string(),
                                brier: item.get("brier")?.as_f64()?,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            zbrain_svg::render_brier_trend(&zbrain_svg::BrierTrendOpts {
                series,
                ..Default::default()
            })
        }
        "domain-bars" => {
            let bars: Vec<zbrain_svg::DomainBar> = profile
                .as_ref()
                .and_then(|p| p.domain_scorecards.as_ref())
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            Some(zbrain_svg::DomainBar {
                                label: item.get("label")?.as_str()?.to_string(),
                                accuracy: item.get("accuracy")?.as_f64()?,
                                n: item.get("n")?.as_u64()? as u32,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            zbrain_svg::render_domain_bars(&zbrain_svg::DomainBarsOpts {
                bars,
                ..Default::default()
            })
        }
        "pattern-statements" => {
            let stmts: Vec<zbrain_svg::PatternStatementsCardItem> = profile
                .as_ref()
                .and_then(|p| p.pattern_statements.as_ref())
                .map(|ps| {
                    ps.iter()
                        .enumerate()
                        .map(|(i, text)| zbrain_svg::PatternStatementsCardItem {
                            text: text.clone(),
                            drill_href: Some(format!(
                                "/admin/calibration/pattern/{}",
                                i + 1
                            )),
                        })
                        .collect()
                })
                .unwrap_or_default();
            zbrain_svg::render_pattern_statements_card(&stmts, 600)
        }
        "abandoned-threads" => {
            let threads: Vec<zbrain_svg::AbandonedThread> = profile
                .as_ref()
                .and_then(|p| p.domain_scorecards.as_ref())
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            Some(zbrain_svg::AbandonedThread {
                                take_id: item.get("take_id")?.as_u64()? as u32,
                                page_slug: item.get("page_slug")?.as_str()?.to_string(),
                                claim: item.get("claim")?.as_str()?.to_string(),
                                months_silent: item.get("months_silent")?.as_u64()? as u32,
                                conviction: item.get("conviction")?.as_f64()?,
                                revisit_href: None,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            zbrain_svg::render_abandoned_threads_card(&threads, 600)
        }
        _ => {
            let supported = [
                "brier-trend",
                "domain-bars",
                "pattern-statements",
                "abandoned-threads",
            ];
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "ok": false,
                    "error": "unknown_chart_type",
                    "supported": supported,
                })),
            )
                .into_response();
        }
    };

    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "image/svg+xml")],
        svg,
    )
        .into_response()
}

async fn get_pattern_handler(
    State(state): State<AppState>,
    Path(id): Path<usize>,
) -> Response {
    match state.calibration_queries.get_pattern_detail("garry", id).await {
        Ok(detail) => Json(serde_json::json!({
            "ok": true,
            "data": detail,
        }))
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"ok": false, "error": e.to_string()})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use crate::MagicLinkAuth;

    fn make_spa_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        std::fs::write(path.join("index.html"), b"<html></html>").unwrap();
        (dir, path)
    }

    async fn start_admin_server() -> (u16, String) {
        let auth = super::super::super::auth::AdminAuth::new(None);
        let token = auth.bootstrap_token().to_string();
        let (_dir, spa_path) = make_spa_dir();

        let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
        let (tx, _rx) = tokio::sync::broadcast::channel(64);
        let state = super::super::super::AppState {
            admin_auth: auth.clone(),
            magic_link: MagicLinkAuth::new(),
            admin_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::AdminQueries>,
            calibration_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
            oauth_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
            token_queries: engine.clone()
                as std::sync::Arc<dyn zbrain_core::TokenQueries>,
            activity_tx: tx,
            spa_dir: spa_path,
            operation_registry: Arc::new(zbrain_core::operation::OperationRegistry::new()),
            engine: engine as std::sync::Arc<dyn zbrain_core::BrainEngine>,
            zbrain_home: std::env::temp_dir().join("zbrain-test"),
        };

        let app = super::super::super::build_router(state);
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        (port, token)
    }

    // ─── RED tests for auth ──────────────────────────────────────────

    #[tokio::test]
    async fn calibration_profile_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;
        let url = format!("http://127.0.0.1:{port}/calibration/profile");
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED,
            "calibration/profile must require admin auth");
    }

    #[tokio::test]
    async fn calibration_charts_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;
        let url = format!("http://127.0.0.1:{port}/calibration/charts/brier-trend");
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED,
            "calibration/charts must require admin auth");
    }

    #[tokio::test]
    async fn calibration_pattern_requires_admin_auth() {
        let (port, _token) = start_admin_server().await;
        let url = format!("http://127.0.0.1:{port}/calibration/pattern/1");
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED,
            "calibration/pattern must require admin auth");
    }

    // ─── Happy-path tests ───────────────────────────────────────────

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
    async fn calibration_profile_returns_ok_with_admin() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://127.0.0.1:{port}/calibration/profile"))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"], serde_json::Value::Null); // InMemory returns None
    }

    #[tokio::test]
    async fn calibration_charts_valid_type_returns_svg() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/calibration/charts/brier-trend"
            ))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.contains("image/svg+xml"), "must return SVG content type: {ct}");
        let body = resp.text().await.unwrap();
        assert!(body.contains("<svg"), "must contain SVG element");
        assert!(body.contains("No Brier-trend data"), "empty data → placeholder SVG");
    }

    #[tokio::test]
    async fn calibration_charts_invalid_type_returns_400() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/calibration/charts/invalid-type"
            ))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], false);
        assert_eq!(body["error"], "unknown_chart_type");
        let supported = body["supported"].as_array().unwrap();
        assert!(supported.contains(&serde_json::json!("brier-trend")));
        assert!(supported.contains(&serde_json::json!("domain-bars")));
    }

    #[tokio::test]
    async fn calibration_pattern_returns_ok_with_admin() {
        let (port, token) = start_admin_server().await;
        let cookie = login_admin(port, &token).await;

        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "http://127.0.0.1:{port}/calibration/pattern/1"
            ))
            .header("Cookie", cookie)
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["ok"], true);
        assert_eq!(body["data"], serde_json::Value::Null); // InMemory returns None
    }
}
