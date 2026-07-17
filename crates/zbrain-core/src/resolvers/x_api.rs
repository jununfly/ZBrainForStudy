//! `x_handle_to_tweet` — resolve an X handle + keyword hint to the tweet URL.
//!
//! Ported from TS `src/core/resolvers/builtin/x-api/handle-to-tweet.ts`.
//!
//! Input:  `{ handle: string, keywords?: string, maxCandidates?: number }`
//! Output: `{ url?, tweet_id?, text?, created_at?, candidates[] }`
//!
//! Driven by `zbrain integrity --auto`: a brain page says "Garry tweeted about
//! foo" without a link. This resolver calls the X API v2 recent-search, finds
//! the matching tweet, and returns the URL + an honest confidence score.
//!
//! Confidence buckets (the contract `zbrain integrity --auto` relies on):
//!   - 1 candidate AND (no keywords OR keywords match well): 0.85 / 0.9
//!   - 1 candidate but weak keyword match:               0.6
//!   - 2-4 candidates, strong margin:                    0.85 / 0.7
//!   - 2-4 candidates, weak margin:                      0.5
//!   - 5+ candidates, dominant match:                    0.75
//!   - 5+ candidates, ambiguous:                         0.4
//!   - Zero candidates:                                  0.0
//!
//! Security:
//!   - Bearer token via `ResolverContext` (`config.x_api_bearer_token` then
//!     `secret("X_API_BEARER_TOKEN")`), never logged.
//!   - Handle regex strictly matches X's username rules (1-15 chars, A-Za-z0-9_).
//!   - Query is URL-encoded via `url.query_pairs_mut`, no string interpolation
//!     into the API path; free-text keywords are sanitized before inclusion.
//!   - Abort is threaded through `HttpClient` (per-request `Notify`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use serde_json::Value as Json;
use url::Url;

use super::http::{HttpClient, HttpClientError, HttpMethod, HttpRequest, HttpResponse};
use super::interface::{
    Resolver, ResolverContext, ResolverCost, ResolverError, ResolverErrorCode, ResolverRequest,
    ResolverResult,
};
use tokio::sync::Notify;

const HANDLE_RE: &str = r"^[A-Za-z0-9_]{1,15}$";
const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const MAX_RETRIES_ON_429: u32 = 2;
const X_API_BASE: &str = "https://api.twitter.com/2";

/// `x_handle_to_tweet` resolver. Holds an injectable HTTP client so it is
/// fully offline-testable (tests pass a `MockHttpClient`, production passes the
/// reqwest-backed `ReqwestHttpClient` behind the `resolvers` feature).
pub struct XHandleToTweetResolver {
    client: Arc<dyn HttpClient>,
}

impl XHandleToTweetResolver {
    pub fn new(client: Arc<dyn HttpClient>) -> Self {
        Self { client }
    }

    /// Resolve the bearer token. Mirrors TS `getBearerToken`: config override
    /// (`x_api_bearer_token`) wins; otherwise the injected `secret` closure
    /// (which the runtime wires to `X_API_BEARER_TOKEN`). Offline-testable.
    fn token(ctx: &ResolverContext) -> Option<String> {
        if let Some(v) = ctx.config.get("x_api_bearer_token") {
            if let Some(s) = v.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        ctx.secret("X_API_BEARER_TOKEN")
    }
}

fn input_schema() -> &'static Json {
    static S: std::sync::OnceLock<Json> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        json!({
            "type": "object",
            "properties": {
                "handle": { "type": "string", "pattern": "^[A-Za-z0-9_]{1,15}$" },
                "keywords": { "type": "string" },
                "maxCandidates": { "type": "number", "minimum": 1, "maximum": 25 }
            },
            "required": ["handle"]
        })
    })
}

fn output_schema() -> &'static Json {
    static S: std::sync::OnceLock<Json> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "format": "uri" },
                "tweet_id": { "type": "string" },
                "text": { "type": "string" },
                "created_at": { "type": "string", "format": "date-time" },
                "candidates": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "tweet_id": { "type": "string" },
                            "text": { "type": "string" },
                            "created_at": { "type": "string", "format": "date-time" },
                            "score": { "type": "number" },
                            "url": { "type": "string", "format": "uri" }
                        },
                        "required": ["tweet_id", "text", "created_at", "score", "url"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["candidates"]
        })
    })
}

#[async_trait]
impl Resolver for XHandleToTweetResolver {
    fn id(&self) -> &str {
        "x_handle_to_tweet"
    }
    fn cost(&self) -> ResolverCost {
        ResolverCost::RateLimited
    }
    fn backend(&self) -> &str {
        "x-api-v2"
    }
    fn description(&self) -> Option<&str> {
        Some("Find a tweet by handle + keyword hint. Used by integrity to repair bare-tweet citations.")
    }
    fn input_schema(&self) -> Option<&Json> {
        Some(input_schema())
    }
    fn output_schema(&self) -> Option<&Json> {
        Some(output_schema())
    }

    async fn available(&self, ctx: &ResolverContext) -> bool {
        Self::token(ctx).is_some()
    }

    async fn resolve(&self, req: ResolverRequest) -> Result<ResolverResult, ResolverError> {
        let handle = req
            .input
            .get("handle")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let keywords = req
            .input
            .get("keywords")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let max_candidates = req
            .input
            .get("maxCandidates")
            .and_then(|v| v.as_u64())
            .unwrap_or(10);

        // Input validation — faithful to TS `HANDLE_RE.test(handle)`.
        if handle.is_empty() || !regex_is_valid_handle(handle) {
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::Schema,
                format!("x_handle_to_tweet: invalid handle \"{handle}\" (must match {HANDLE_RE})"),
                "x_handle_to_tweet",
            ));
        }
        let clamped_max = max_candidates.clamp(1, 25) as u32;

        let token = match Self::token(&req.context) {
            Some(t) => t,
            None => {
                return Err(ResolverError::with_resolver(
                    ResolverErrorCode::Unavailable,
                    "x_handle_to_tweet: X_API_BEARER_TOKEN not set",
                    "x_handle_to_tweet",
                ));
            }
        };

        let timeout_ms = req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);

        // Query: from:handle + optional sanitized free-text keywords (hint).
        let mut query_parts = vec![format!("from:{handle}")];
        if let Some(kw) = &keywords {
            let cleaned = sanitize_keywords(kw);
            if !cleaned.is_empty() {
                query_parts.push(cleaned);
            }
        }
        let api_query = query_parts.join(" ");

        let mut url = Url::parse(&format!("{X_API_BASE}/tweets/search/recent"))
            .expect("x_api base url is valid");
        {
            let mut qp = url.query_pairs_mut();
            qp.append_pair("query", &api_query);
            qp.append_pair("max_results", &clamped_max.to_string());
            qp.append_pair("tweet.fields", "created_at,text");
        }

        let http_req = HttpRequest {
            url: url.to_string(),
            method: HttpMethod::Get,
            headers: vec![("authorization".to_string(), format!("Bearer {token}"))],
            body: None,
            timeout: Duration::from_millis(timeout_ms),
            abort: Some(req.context.abort.clone()),
        };

        // Fire with retry-on-429 (up to MAX_RETRIES_ON_429 extra attempts).
        let mut resp: Option<HttpResponse> = None;
        for attempt in 0..=MAX_RETRIES_ON_429 {
            match self.client.send(http_req.clone()).await {
                Ok(r) => {
                    if r.status == 429 && attempt < MAX_RETRIES_ON_429 {
                        let wait = compute_backoff_ms(&r, now_ms());
                        req.context.logger.warn(
                            "x_handle_to_tweet: 429, backing off",
                            Some(&json!({ "handle": handle, "waitMs": wait, "attempt": attempt })),
                        );
                        sleep_backoff(wait, &req.context.abort).await;
                        resp = Some(r);
                        continue;
                    }
                    resp = Some(r);
                    break;
                }
                Err(e) => {
                    if e == HttpClientError::Aborted {
                        return Err(ResolverError::with_resolver(
                            ResolverErrorCode::Aborted,
                            "x_handle_to_tweet aborted",
                            "x_handle_to_tweet",
                        ));
                    }
                    return Err(ResolverError::with_cause(
                        ResolverErrorCode::Upstream,
                        format!("x_handle_to_tweet fetch failed: {e}"),
                        e.to_string(),
                    ));
                }
            }
        }

        let resp = resp.ok_or_else(|| {
            ResolverError::with_resolver(
                ResolverErrorCode::Upstream,
                "x_handle_to_tweet: no response after retries",
                "x_handle_to_tweet",
            )
        })?;

        // Terminal error codes.
        if resp.status == 401 || resp.status == 403 {
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::Auth,
                format!(
                    "x_handle_to_tweet: auth failed (HTTP {}) — check X_API_BEARER_TOKEN",
                    resp.status
                ),
                "x_handle_to_tweet",
            ));
        }
        if resp.status == 429 {
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::RateLimited,
                "x_handle_to_tweet: rate-limited after retries",
                "x_handle_to_tweet",
            ));
        }
        if !(200..300).contains(&resp.status) {
            let body = String::from_utf8_lossy(&resp.body);
            let snippet: String = body.chars().take(200).collect();
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::Upstream,
                format!("x_handle_to_tweet: HTTP {} — {}", resp.status, snippet),
                "x_handle_to_tweet",
            ));
        }

        let parsed: Json = serde_json::from_slice(&resp.body).unwrap_or(Json::Object(Default::default()));
        let tweets = parsed
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        if tweets.is_empty() {
            return Ok(ResolverResult {
                value: json!({ "candidates": [] }),
                confidence: 0.0,
                source: "x-api-v2".to_string(),
                fetched_at: Utc::now(),
                cost_estimate: Some(0.0),
                raw: Some(parsed),
            });
        }

        // Score by keyword overlap with tweet text, build candidates, sort desc.
        let mut candidates: Vec<Json> = tweets
            .iter()
            .map(|t| {
                let id = t.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let text = t.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let created_at = t
                    .get("created_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let score = score_match(&text, keywords.as_deref());
                let url = format!("https://x.com/{handle}/status/{id}");
                json!({
                    "tweet_id": id,
                    "text": text,
                    "created_at": created_at,
                    "score": score,
                    "url": url
                })
            })
            .collect();
        candidates.sort_by(|a, b| {
            let sa = a.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let sb = b.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        let top = candidates[0].clone();
        let top_score = top.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let rest_scores: Vec<f64> = candidates[1..]
            .iter()
            .map(|c| c.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0))
            .collect();
        let confidence = compute_confidence(top_score, &rest_scores, keywords.as_deref());

        let include = confidence >= 0.5;
        let mut value = serde_json::Map::new();
        if include {
            // Faithful to TS: when confidence < 0.5 the top fields are omitted
            // entirely (value.url is `undefined`), so integrity won't surface a
            // low-confidence link.
            for key in ["url", "tweet_id", "text", "created_at"] {
                if let Some(v) = top.get(key) {
                    value.insert(key.to_string(), v.clone());
                }
            }
        }
        value.insert("candidates".to_string(), json!(candidates));
        let value = Json::Object(value);

        Ok(ResolverResult {
            value,
            confidence,
            source: "x-api-v2".to_string(),
            fetched_at: Utc::now(),
            cost_estimate: Some(0.0),
            raw: Some(parsed),
        })
    }
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

/// Confidence buckets. Faithful to TS `computeConfidence`.
pub fn compute_confidence(top_score: f64, rest_scores: &[f64], keywords: Option<&str>) -> f64 {
    let kw = keywords.unwrap_or("").trim();

    // Single candidate.
    if rest_scores.is_empty() {
        if kw.is_empty() {
            return 0.85; // handle-only, recency-most-likely
        }
        return if top_score >= 0.5 { 0.9 } else { 0.6 };
    }

    // Many candidates: ambiguous.
    if rest_scores.len() >= 5 {
        let margin = top_score - rest_scores.first().copied().unwrap_or(0.0);
        if top_score >= 0.7 && margin >= 0.4 {
            return 0.75;
        }
        return 0.4;
    }

    // 2-4 candidates: margin between top and runner-up.
    let runner_up = rest_scores.first().copied().unwrap_or(0.0);
    let margin = top_score - runner_up;
    if top_score >= 0.7 && margin >= 0.3 {
        return 0.85;
    }
    if top_score >= 0.5 && margin >= 0.15 {
        return 0.7;
    }
    0.5
}

/// Keyword-overlap score in [0, 1]. Faithful to TS `scoreMatch`.
pub fn score_match(text: &str, keywords: Option<&str>) -> f64 {
    let kw = match keywords {
        Some(k) if !k.trim().is_empty() => k,
        _ => return 0.5, // no hint, neutral prior
    };
    let kw_tokens = tokenize(kw);
    if kw_tokens.is_empty() {
        return 0.5;
    }
    let text_tokens: std::collections::HashSet<String> = tokenize(text).into_iter().collect();
    let hits = kw_tokens.iter().filter(|kt| text_tokens.contains(*kt)).count();
    hits as f64 / kw_tokens.len() as f64
}

const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "for", "with", "by", "is",
    "was", "are", "were", "be", "been", "it", "this", "that", "these", "those", "i", "you", "he",
    "she", "we", "they", "his", "her", "its",
];

/// Tokenize: lowercase, strip punctuation, drop stopwords and tokens <= 2 chars.
fn tokenize(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace(|c: char| !c.is_ascii_alphanumeric() && !c.is_whitespace(), " ")
        .split_whitespace()
        .filter(|t| t.len() > 2 && !STOP_WORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

/// Sanitize free-text keywords before passing to X API query. Faithful to TS
/// `sanitizeKeywords`: strip X operators, strip shell-escape metacharacters,
/// trim, cap length.
fn sanitize_keywords(kw: &str) -> String {
    static OP_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // Strip X operators the caller didn't explicitly set.
    let re = OP_RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(from|to|url|lang|is|has|filter):\S+").expect("valid operator regex")
    });
    let s = re.replace_all(kw, "").into_owned();
    // Strip shell-escape-looking metacharacters.
    let s = s.replace(['`', '$', '(', ')', ';', '|', '&', '<', '>', '\\'], "");
    s.trim().chars().take(200).collect()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn regex_is_valid_handle(handle: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(HANDLE_RE).expect("valid handle regex"))
        .is_match(handle)
}

/// Compute how long to sleep before retrying after a 429. Faithful to TS
/// `computeBackoffMs`. `now_ms` is epoch milliseconds (TS `Date.now()`).
pub fn compute_backoff_ms(resp: &HttpResponse, now_ms: u64) -> u64 {
    const MIN_MS: u64 = 2_000;
    const MAX_MS: u64 = 60_000;

    // Retry-After parsing: seconds or HTTP-date.
    let mut retry_after_ms = 0u64;
    if let Some(ra) = resp.header("retry-after") {
        if let Ok(secs) = ra.trim().parse::<u64>() {
            retry_after_ms = secs * 1000;
        } else if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(ra.trim()) {
            let delta = (dt.timestamp_millis() as i64 - now_ms as i64).max(0) as u64;
            retry_after_ms = delta;
        } else if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ra.trim()) {
            let delta = (dt.timestamp_millis() as i64 - now_ms as i64).max(0) as u64;
            retry_after_ms = delta;
        }
    }

    // x-rate-limit-reset is an epoch second.
    let mut rate_reset_ms = 0u64;
    if let Some(rr) = resp.header("x-rate-limit-reset") {
        if let Ok(epoch_sec) = rr.trim().parse::<u64>() {
            if epoch_sec > 0 {
                rate_reset_ms = epoch_sec.saturating_mul(1000).saturating_sub(now_ms);
            }
        }
    }

    let wait = MIN_MS.max(retry_after_ms).max(rate_reset_ms);
    wait.min(MAX_MS)
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Abort-aware sleep. Faithful to TS `sleep(ms, signal)`: returns early if the
/// shared `Notify` fires.
async fn sleep_backoff(ms: u64, abort: &Notify) {
    if ms == 0 {
        tokio::select! {
            _ = abort.notified() => {}
            _ = std::future::ready(()) => {}
        }
        return;
    }
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
        _ = abort.notified() => {}
    }
}

// ---------------------------------------------------------------------------
// Tests — offline, fully mocked (MockHttpClient injected)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolvers::http::MockHttpClient;
    use std::sync::Arc;

    fn ctx_with_token(token: Option<&str>, config_token: Option<&str>) -> ResolverContext {
        let mut ctx = ResolverContext::new();
        let owned = token.map(|t| t.to_string());
        ctx.secret = Arc::new(move |name: &str| {
            if name == "X_API_BEARER_TOKEN" {
                owned.clone()
            } else {
                None
            }
        });
        if let Some(c) = config_token {
            let mut cfg = serde_json::Map::new();
            cfg.insert("x_api_bearer_token".to_string(), json!(c));
            ctx.config = Json::Object(cfg);
        }
        ctx
    }

    fn resolver(client: Arc<dyn HttpClient>) -> XHandleToTweetResolver {
        XHandleToTweetResolver::new(client)
    }

    fn req(handle: &str, keywords: Option<&str>, token: Option<&str>, config_token: Option<&str>) -> ResolverRequest {
        let mut input = json!({ "handle": handle });
        if let Some(k) = keywords {
            input["keywords"] = json!(k);
        }
        ResolverRequest {
            input,
            context: ctx_with_token(token, config_token),
            timeout_ms: None,
        }
    }

    // ---- available() ----

    #[tokio::test]
    async fn available_false_without_token() {
        let r = resolver(Arc::new(MockHttpClient::fixed_status(200)));
        assert!(!r.available(&ctx_with_token(None, None)).await);
    }

    #[tokio::test]
    async fn available_true_with_secret() {
        let r = resolver(Arc::new(MockHttpClient::fixed_status(200)));
        assert!(r.available(&ctx_with_token(Some("tok"), None)).await);
    }

    #[tokio::test]
    async fn available_true_with_config() {
        let r = resolver(Arc::new(MockHttpClient::fixed_status(200)));
        assert!(r.available(&ctx_with_token(None, Some("cfg-tok"))).await);
    }

    // ---- input validation ----

    #[tokio::test]
    async fn rejects_invalid_handle() {
        let r = resolver(Arc::new(MockHttpClient::fixed_status(200)));
        let err = r
            .resolve(req("bad handle with spaces", None, Some("tok"), None))
            .await
            .unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Schema);
    }

    #[tokio::test]
    async fn rejects_too_long_handle() {
        let r = resolver(Arc::new(MockHttpClient::fixed_status(200)));
        let err = r
            .resolve(req(&"a".repeat(16), None, Some("tok"), None))
            .await
            .unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Schema);
    }

    #[tokio::test]
    async fn throws_unavailable_without_token() {
        let r = resolver(Arc::new(MockHttpClient::fixed_status(200)));
        let err = r.resolve(req("garrytan", None, None, None)).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Unavailable);
    }

    // ---- zero / confidence buckets ----

    #[tokio::test]
    async fn zero_candidates_confidence_zero() {
        let client = Arc::new(MockHttpClient::new(|_| {
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: br#"{"data":[],"meta":{"result_count":0}}"#.to_vec(),
            })
        }));
        let r = resolver(client);
        let res = r
            .resolve(req("garrytan", Some("nothing matches"), Some("tok"), None))
            .await
            .unwrap();
        assert_eq!(res.confidence, 0.0);
        assert!(res.value["candidates"].as_array().unwrap().is_empty());
        assert!(res.value.get("url").is_none());
    }

    #[tokio::test]
    async fn single_strong_match_auto_repair_bucket() {
        let client = Arc::new(MockHttpClient::new(|_| {
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: br#"{"data":[{"id":"123","text":"talking about building zbrain today","created_at":"2026-04-18T00:00:00Z"}]}"#.to_vec(),
            })
        }));
        let r = resolver(client);
        let res = r
            .resolve(req("garrytan", Some("building zbrain"), Some("tok"), None))
            .await
            .unwrap();
        assert!(res.confidence >= 0.8);
        assert_eq!(res.value["url"], "https://x.com/garrytan/status/123");
        assert_eq!(res.value["tweet_id"], "123");
    }

    #[tokio::test]
    async fn single_weak_match_review_bucket() {
        let client = Arc::new(MockHttpClient::new(|_| {
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: br#"{"data":[{"id":"1","text":"something unrelated entirely","created_at":"2026-04-18T00:00:00Z"}]}"#.to_vec(),
            })
        }));
        let r = resolver(client);
        let res = r
            .resolve(req("garrytan", Some("zbrain knowledge runtime specific terms"), Some("tok"), None))
            .await
            .unwrap();
        assert!(res.confidence >= 0.5);
        assert!(res.confidence < 0.8);
    }

    #[tokio::test]
    async fn many_ambiguous_skip_bucket() {
        let data: Vec<String> = (0..10)
            .map(|i| format!(r#"{{"id":"{}","text":"short noise text {}","created_at":"2026-04-18T00:00:00Z"}}"#, i + 1, i))
            .collect();
        let body = format!(r#"{{"data":[{}]}}"#, data.join(","));
        let client = Arc::new(MockHttpClient::new(move |_| {
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: body.clone().into_bytes(),
            })
        }));
        let r = resolver(client);
        let res = r
            .resolve(req("garrytan", Some("completely different signal words unlikely to match"), Some("tok"), None))
            .await
            .unwrap();
        assert!(res.confidence < 0.5);
        assert_eq!(res.value["candidates"].as_array().unwrap().len(), 10);
        assert!(res.value.get("url").is_none());
    }

    // ---- error codes ----

    #[tokio::test]
    async fn status_401_auth() {
        let client = Arc::new(MockHttpClient::fixed_status(401));
        let r = resolver(client);
        let err = r.resolve(req("garrytan", None, Some("tok"), None)).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Auth);
    }

    #[tokio::test]
    async fn status_403_auth() {
        let client = Arc::new(MockHttpClient::fixed_status(403));
        let r = resolver(client);
        let err = r.resolve(req("garrytan", None, Some("tok"), None)).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Auth);
    }

    #[tokio::test]
    async fn status_500_upstream_with_body() {
        let client = Arc::new(MockHttpClient::new(|_| {
            Ok(HttpResponse {
                status: 500,
                headers: vec![],
                body: b"internal err".to_vec(),
            })
        }));
        let r = resolver(client);
        let err = r.resolve(req("garrytan", None, Some("tok"), None)).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Upstream);
        assert!(err.message.contains("HTTP 500"));
    }

    #[tokio::test]
    async fn status_429_retries_then_rate_limited() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_c = calls.clone();
        let client = Arc::new(MockHttpClient::new(move |_| {
            calls_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(HttpResponse {
                status: 429,
                headers: vec![("retry-after".to_string(), "0".to_string())],
                body: vec![],
            })
        }));
        let r = resolver(client);
        let err = r.resolve(req("garrytan", None, Some("tok"), None)).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::RateLimited);
        assert!(calls.load(std::sync::atomic::Ordering::SeqCst) >= 3);
    }

    // ---- injection defense ----

    #[tokio::test]
    async fn strips_x_operators_from_keywords() {
        let captured = Arc::new(std::sync::Mutex::new(None::<String>));
        let cap_c = captured.clone();
        let client = Arc::new(MockHttpClient::new(move |rq: &HttpRequest| {
            *cap_c.lock().unwrap() = Some(rq.url.clone());
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".to_string(), "application/json".to_string())],
                body: br#"{"data":[]}"#.to_vec(),
            })
        }));
        let r = resolver(client);
        let _ = r
            .resolve(req(
                "garrytan",
                Some("from:evil_user lang:ja to:someone normal words"),
                Some("tok"),
                None,
            ))
            .await
            .unwrap();
        let url = captured.lock().unwrap().clone().unwrap();
        let query = url::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "query")
            .map(|(_, v)| v.to_string())
            .unwrap();
        assert!(query.contains("from:garrytan"));
        assert!(!query.contains("from:evil_user"));
        assert!(!query.contains("lang:ja"));
        assert!(!query.contains("to:someone"));
        assert!(query.contains("normal"));
        assert!(query.contains("words"));
    }

    // ---- pure scoring fns ----

    #[test]
    fn score_match_no_keywords_is_neutral() {
        assert_eq!(score_match("anything", None), 0.5);
        assert_eq!(score_match("anything", Some("   ")), 0.5);
    }

    #[test]
    fn score_match_full_overlap() {
        let s = score_match("talking about building zbrain today", Some("building zbrain"));
        assert_eq!(s, 1.0);
    }

    #[test]
    fn score_match_partial() {
        let s = score_match("the cat sat on the mat", Some("cat dog fish tree"));
        // "cat" hits; "dog","fish","tree" miss → 1/4.
        assert!((s - 0.25).abs() < 1e-9);
    }

    #[test]
    fn confidence_single_no_keywords() {
        assert_eq!(compute_confidence(0.5, &[], None), 0.85);
    }

    #[test]
    fn confidence_single_strong() {
        assert_eq!(compute_confidence(0.9, &[], Some("zbrain")), 0.9);
    }

    #[test]
    fn confidence_single_weak() {
        assert_eq!(compute_confidence(0.1, &[], Some("zbrain")), 0.6);
    }

    #[test]
    fn confidence_many_dominant() {
        assert_eq!(compute_confidence(0.9, &[0.4, 0.3, 0.2, 0.1, 0.1], Some("x")), 0.75);
    }

    #[test]
    fn confidence_many_ambiguous() {
        assert_eq!(compute_confidence(0.3, &[0.3, 0.3, 0.2, 0.1, 0.1], Some("x")), 0.4);
    }

    #[test]
    fn confidence_two_four_strong_margin() {
        assert_eq!(compute_confidence(0.8, &[0.4], Some("x")), 0.85);
    }

    #[test]
    fn confidence_two_four_weak_margin() {
        assert_eq!(compute_confidence(0.3, &[0.28], Some("x")), 0.5);
    }

    // ---- backoff ----

    #[test]
    fn backoff_takes_max_of_headers() {
        let now = 1_700_000_000_000u64;
        let reset_sec = (now / 1000) + 30;
        let resp = HttpResponse {
            status: 429,
            headers: vec![
                ("retry-after".to_string(), "5".to_string()),
                ("x-rate-limit-reset".to_string(), reset_sec.to_string()),
            ],
            body: vec![],
        };
        let ms = compute_backoff_ms(&resp, now);
        assert!(ms >= 29_000);
    }

    #[test]
    fn backoff_floor_when_no_headers() {
        let resp = HttpResponse {
            status: 429,
            headers: vec![],
            body: vec![],
        };
        assert!(compute_backoff_ms(&resp, 0) >= 2_000);
    }

    #[test]
    fn backoff_ceiling() {
        let now = 1_700_000_000_000u64;
        let reset_sec = (now / 1000) + 600;
        let resp = HttpResponse {
            status: 429,
            headers: vec![("x-rate-limit-reset".to_string(), reset_sec.to_string())],
            body: vec![],
        };
        assert!(compute_backoff_ms(&resp, now) <= 60_000);
    }

    // ---- schemas ----

    #[test]
    fn schemas_declare_candidates_items() {
        let r = XHandleToTweetResolver::new(Arc::new(MockHttpClient::fixed_status(200)));
        let out = r.output_schema().unwrap();
        let cand = &out["properties"]["candidates"];
        assert_eq!(cand["type"], "array");
        assert!(cand["items"]["type"] == "object");
        let required = cand["items"]["required"].as_array().unwrap();
        assert_eq!(required.len(), 5);
        assert_eq!(cand["items"]["additionalProperties"], false);
    }
}
