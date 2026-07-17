//! `url_reachable` — deterministic HEAD-check resolver.
//!
//! Ported from TS `src/core/resolvers/builtin/url-reachable.ts`.
//!
//! Input:  `{ url: string }`
//! Output: `{ reachable: boolean, status?: number, finalUrl?: string, reason?: string }`
//!
//! Used by `zbrain integrity` to detect dead-link citations on brain pages.
//! Confidence is always 1.0 (status codes are ground truth); it is only 0 when
//! the HTTP call itself fails and we genuinely don't know.
//!
//! Security:
//! - SSRF gate reuses [`crate::url_safety::is_internal_url`] (the same
//!   wave-3 hardening that protects recipe health_checks / git clone).
//! - Redirect chain followed manually (max 5 hops) with per-hop re-validation
//!   so no new SSRF bypass surface appears.
//! - HEAD first, GET fallback when the server rejects HEAD (405/501).
//! - DNS-rebinding defense: resolved A/AAAA records are checked against
//!   private ranges via [`crate::url_safety::is_private_addr`].

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value as Json;
use serde_json::json;
use url::Url;

use super::dns::DnsResolver;
use super::http::{HttpClient, HttpClientError, HttpMethod, HttpRequest, HttpResponse};
use super::interface::{
    Resolver, ResolverContext, ResolverCost, ResolverError, ResolverErrorCode, ResolverRequest,
    ResolverResult,
};

const DEFAULT_TIMEOUT_MS: u64 = 10_000;
const MAX_REDIRECTS: usize = 5;

/// Deterministic HEAD-check resolver. Holds injectable HTTP + DNS clients so
/// it is fully offline-testable.
pub struct UrlReachableResolver {
    client: Arc<dyn HttpClient>,
    dns: Arc<dyn DnsResolver>,
}

impl UrlReachableResolver {
    pub fn new(client: Arc<dyn HttpClient>, dns: Arc<dyn DnsResolver>) -> Self {
        Self { client, dns }
    }

    /// DNS-rebinding defense. Resolves the hostname and rejects any private
    /// target. IP literals skip DNS (`is_internal_url` already blocked private
    /// ones). Resolution failure → `None` (let the real fetch surface error).
    async fn check_dns_rebinding(&self, url_str: &str) -> Option<String> {
        let parsed = Url::parse(url_str).ok()?;
        let host = parsed.host_str()?.to_lowercase();
        if host.parse::<std::net::IpAddr>().is_ok() {
            return None; // IP literal → already SSRF-checked
        }
        let addrs = self.dns.lookup(&host).await.ok()?;
        for ip in addrs {
            if crate::url_safety::is_private_addr(ip) {
                return Some(format!(
                    "DNS resolution of {host} yielded private IP {ip} (rebinding defense)"
                ));
            }
        }
        None
    }
}

fn input_schema() -> &'static Json {
    static S: OnceLock<Json> = OnceLock::new();
    S.get_or_init(|| {
        json!({
            "type": "object",
            "properties": { "url": { "type": "string", "format": "uri" } },
            "required": ["url"]
        })
    })
}

fn output_schema() -> &'static Json {
    static S: OnceLock<Json> = OnceLock::new();
    S.get_or_init(|| {
        json!({
            "type": "object",
            "properties": {
                "reachable": { "type": "boolean" },
                "status": { "type": "number" },
                "finalUrl": { "type": "string" },
                "reason": { "type": "string" }
            },
            "required": ["reachable"]
        })
    })
}

#[async_trait]
impl Resolver for UrlReachableResolver {
    fn id(&self) -> &str {
        "url_reachable"
    }
    fn cost(&self) -> ResolverCost {
        ResolverCost::Free
    }
    fn backend(&self) -> &str {
        "head-check"
    }
    fn description(&self) -> Option<&str> {
        Some("HEAD-check a URL, follow redirects, detect dead links. SSRF-protected.")
    }
    fn input_schema(&self) -> Option<&Json> {
        Some(input_schema())
    }
    fn output_schema(&self) -> Option<&Json> {
        Some(output_schema())
    }

    async fn available(&self, _ctx: &ResolverContext) -> bool {
        true
    }

    async fn resolve(&self, req: ResolverRequest) -> Result<ResolverResult, ResolverError> {
        // Faithful to TS checkReachable: bail immediately if the caller already
        // fired the abort signal before we do any work. Uses a biased select so
        // a pre-fired abort always wins the race over the (also-immediately-
        // ready) mocked transport future.
        {
            let n = req.context.abort.notified();
            tokio::pin!(n);
            let aborted = tokio::select! {
                biased;
                _ = &mut n => true,
                _ = std::future::ready(()) => false,
            };
            if aborted {
                return Err(ResolverError::with_resolver(
                    ResolverErrorCode::Aborted,
                    "url_reachable aborted".to_string(),
                    "url_reachable",
                ));
            }
        }

        let url = req
            .input
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if url.is_empty() {
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::Schema,
                "url_reachable: url must be a non-empty string",
                "url_reachable",
            ));
        }

        // SSRF gate — refuse to probe internal/private/metadata endpoints (by hostname string).
        if crate::url_safety::is_internal_url(url) {
            return Ok(reach_no(json!({
                "reason": "blocked: internal/private/metadata hostname or non-http(s) scheme"
            })));
        }

        // DNS rebinding defense (resolved A/AAAA vs private ranges).
        if let Some(reason) = self.check_dns_rebinding(url).await {
            return Ok(reach_no(json!({ "reason": reason })));
        }

        let timeout = Duration::from_millis(req.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
        let mut current_url = url.to_string();
        let mut used_method = HttpMethod::Head;
        let mut status: Option<u16> = None;

        for _hop in 0..=MAX_REDIRECTS {
            let http_req = HttpRequest {
                url: current_url.clone(),
                method: used_method,
                headers: vec![],
                body: None,
                timeout,
                abort: Some(req.context.abort.clone()),
            };
            let send_fut = self.client.send(http_req);
            tokio::pin!(send_fut);
            let notified = req.context.abort.notified();
            tokio::pin!(notified);
            let send_result: Result<HttpResponse, HttpClientError> = tokio::select! {
                _ = &mut notified => {
                    return Err(ResolverError::with_resolver(
                        ResolverErrorCode::Aborted,
                        format!("url_reachable aborted ({current_url})"),
                        "url_reachable",
                    ));
                }
                r = &mut send_fut => r,
            };

            let resp = match send_result {
                Ok(r) => r,
                Err(HttpClientError::Aborted) => {
                    return Err(ResolverError::with_resolver(
                        ResolverErrorCode::Aborted,
                        format!("url_reachable aborted ({current_url})"),
                        "url_reachable",
                    ));
                }
                Err(transport) => {
                    // fetch threw (DNS, connection refused, timeout). Not reachable, no status.
                    return Ok(reach_no(json!({
                        "reason": format!("fetch error: {}", err_brief(&transport))
                    })));
                }
            };

            status = Some(resp.status);

            // Some servers reject HEAD with 405/501. Retry once as GET (same hop).
            if used_method == HttpMethod::Head && (resp.status == 405 || resp.status == 501) {
                used_method = HttpMethod::Get;
                continue;
            }

            // 3xx redirect handling.
            if (300..400).contains(&resp.status) {
                let location = match resp.header("location") {
                    Some(l) => l.to_string(),
                    None => {
                        return Ok(reach_no(json!({
                            "status": resp.status,
                            "finalUrl": final_url(&current_url, url),
                            "reason": "redirect without Location header"
                        })));
                    }
                };
                let next_url = match Url::parse(&current_url).and_then(|b| b.join(&location)) {
                    Ok(u) => u.to_string(),
                    Err(_) => {
                        return Ok(reach_no(json!({
                            "status": resp.status,
                            "reason": format!("malformed redirect Location: {location}")
                        })));
                    }
                };
                // Re-validate each hop against SSRF (hostname string).
                if crate::url_safety::is_internal_url(&next_url) {
                    return Ok(reach_no(json!({
                        "status": resp.status,
                        "finalUrl": current_url.clone(),
                        "reason": format!("redirect to blocked hostname: {next_url}")
                    })));
                }
                // DNS rebinding defense on the redirect target too.
                if let Some(reason) = self.check_dns_rebinding(&next_url).await {
                    return Ok(reach_no(json!({
                        "status": resp.status,
                        "finalUrl": current_url.clone(),
                        "reason": format!("redirect blocked by DNS check: {reason}")
                    })));
                }
                current_url = next_url;
                used_method = HttpMethod::Head; // reset to HEAD for the new hop
                continue;
            }

            // Terminal status. 2xx/3xx (200-399) = deterministic answer;
            // 4xx/5xx flag unreachable for integrity purposes.
            let reachable = (200..400).contains(&resp.status);
            return Ok(ResolverResult {
                value: json!({
                    "reachable": reachable,
                    "status": resp.status,
                    "finalUrl": final_url(&current_url, url),
                    "reason": if reachable { Json::Null } else { json!(format!("HTTP {}", resp.status)) }
                }),
                confidence: 1.0,
                source: "head-check".to_string(),
                fetched_at: Utc::now(),
                cost_estimate: Some(0.0),
                raw: None,
            });
        }

        // Ran out of redirect budget.
        Ok(reach_no(json!({
            "status": status,
            "finalUrl": current_url.clone(),
            "reason": format!("exceeded {MAX_REDIRECTS} redirects")
        })))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a `reachable=false` result (confidence 1, head-check source).
fn reach_no(extra: Json) -> ResolverResult {
    let mut value = json!({ "reachable": false });
    if let (Some(obj), Some(extra_obj)) = (value.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    ResolverResult {
        value,
        confidence: 1.0,
        source: "head-check".to_string(),
        fetched_at: Utc::now(),
        cost_estimate: Some(0.0),
        raw: None,
    }
}

/// `finalUrl` is set only when the URL actually changed during redirects.
fn final_url(current: &str, original: &str) -> Json {
    if current != original {
        json!(current)
    } else {
        Json::Null
    }
}

fn err_brief(e: &HttpClientError) -> String {
    match e {
        HttpClientError::Timeout => "timeout".to_string(),
        HttpClientError::Transport(s) => s.clone(),
        HttpClientError::Aborted => "aborted".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests — offline, fully mocked (HttpClient + DnsResolver injected)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolvers::dns::MockDnsResolver;
    use crate::resolvers::http::{MockHttpClient, HttpRequest, HttpResponse};
    use std::sync::Arc;
    use tokio::sync::Notify;

    fn ctx_with_abort() -> (ResolverContext, Arc<Notify>) {
        let notify = Arc::new(Notify::new());
        let mut ctx = ResolverContext::new();
        ctx.abort = notify.clone();
        (ctx, notify)
    }

    fn resolver(status: u16) -> (UrlReachableResolver, Arc<Notify>) {
        let (ctx, notify) = ctx_with_abort();
        let client = Arc::new(MockHttpClient::fixed_status(status));
        let dns = Arc::new(MockDnsResolver::empty());
        (UrlReachableResolver::new(client, dns), notify)
    }

    fn input(url: &str) -> ResolverRequest {
        let (ctx, _) = ctx_with_abort();
        ResolverRequest {
            input: json!({ "url": url }),
            context: ctx,
            timeout_ms: None,
        }
    }

    #[test]
    fn contract_id_cost_backend() {
        let (r, _) = resolver(200);
        assert_eq!(r.id(), "url_reachable");
        assert_eq!(r.cost(), ResolverCost::Free);
        assert_eq!(r.backend(), "head-check");
    }

    #[tokio::test]
    async fn available_is_true() {
        let (r, _) = resolver(200);
        let (ctx, _) = ctx_with_abort();
        assert!(r.available(&ctx).await);
    }

    #[tokio::test]
    async fn blocks_localhost_ssrf() {
        let (r, _) = resolver(200);
        let res = r.resolve(input("http://127.0.0.1:1")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
        let reason = res.value["reason"].as_str().unwrap();
        assert!(reason.contains("internal") || reason.contains("private"));
    }

    #[tokio::test]
    async fn blocks_rfc1918() {
        let (r, _) = resolver(200);
        let res = r.resolve(input("http://10.0.0.1/")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
    }

    #[tokio::test]
    async fn blocks_aws_metadata() {
        let (r, _) = resolver(200);
        let res = r.resolve(input("http://169.254.169.254/latest/meta-data/")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
    }

    #[tokio::test]
    async fn blocks_non_http_scheme() {
        let (r, _) = resolver(200);
        let res = r.resolve(input("file:///etc/passwd")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
    }

    #[tokio::test]
    async fn empty_url_throws_schema() {
        let (r, _) = resolver(200);
        let (ctx, _) = ctx_with_abort();
        let req = ResolverRequest {
            input: json!({ "url": "" }),
            context: ctx,
            timeout_ms: None,
        };
        let err = r.resolve(req).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Schema);
    }

    #[tokio::test]
    async fn status_200_reachable() {
        let (r, _) = resolver(200);
        let res = r.resolve(input("https://example.com/ok")).await.unwrap();
        assert_eq!(res.value["reachable"], true);
        assert_eq!(res.value["status"], 200);
        assert_eq!(res.confidence, 1.0);
    }

    #[tokio::test]
    async fn status_404_unreachable_with_reason() {
        let (r, _) = resolver(404);
        let res = r.resolve(input("https://example.com/dead")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
        assert_eq!(res.value["status"], 404);
        assert_eq!(res.value["reason"].as_str().unwrap(), "HTTP 404");
    }

    #[tokio::test]
    async fn head_405_falls_back_to_get() {
        let (ctx, _) = ctx_with_abort();
        let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cc = call_count.clone();
        let client = Arc::new(MockHttpClient::new(move |req: &HttpRequest| {
            cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if req.method == HttpMethod::Head {
                Ok(HttpResponse { status: 405, headers: vec![], body: vec![] })
            } else {
                Ok(HttpResponse { status: 200, headers: vec![], body: vec![] })
            }
        }));
        let dns = Arc::new(MockDnsResolver::empty());
        let r = UrlReachableResolver::new(client, dns);
        let res = r.resolve(input("https://example.com/post-only")).await.unwrap();
        assert_eq!(res.value["reachable"], true);
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn follows_redirect_to_external() {
        let (ctx, _) = ctx_with_abort();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_c = calls.clone();
        let client = Arc::new(MockHttpClient::new(move |_req: &HttpRequest| {
            let i = calls_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if i == 0 {
                Ok(HttpResponse {
                    status: 301,
                    headers: vec![("location".to_string(), "https://example.org/final".to_string())],
                    body: vec![],
                })
            } else {
                Ok(HttpResponse { status: 200, headers: vec![], body: vec![] })
            }
        }));
        let dns = Arc::new(MockDnsResolver::empty());
        let r = UrlReachableResolver::new(client, dns);
        let res = r.resolve(input("https://example.com/redirect")).await.unwrap();
        assert_eq!(res.value["reachable"], true);
        assert_eq!(res.value["finalUrl"].as_str().unwrap(), "https://example.org/final");
    }

    #[tokio::test]
    async fn blocks_redirect_to_internal_per_hop() {
        let (ctx, _) = ctx_with_abort();
        let client = Arc::new(MockHttpClient::new(|_req: &HttpRequest| {
            Ok(HttpResponse {
                status: 302,
                headers: vec![("location".to_string(), "http://127.0.0.1/admin".to_string())],
                body: vec![],
            })
        }));
        let dns = Arc::new(MockDnsResolver::empty());
        let r = UrlReachableResolver::new(client, dns);
        let res = r.resolve(input("https://example.com/redirects-to-local")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
        assert!(res.value["reason"].as_str().unwrap().contains("redirect to blocked"));
    }

    #[tokio::test]
    async fn network_failure_reachable_false_confidence_one() {
        let (ctx, _) = ctx_with_abort();
        let client = Arc::new(MockHttpClient::new(|_req: &HttpRequest| {
            Err(HttpClientError::Transport("fetch failed".to_string()))
        }));
        let dns = Arc::new(MockDnsResolver::empty());
        let r = UrlReachableResolver::new(client, dns);
        let res = r.resolve(input("https://nonexistent.example/")).await.unwrap();
        assert_eq!(res.value["reachable"], false);
        assert!(res.value["reason"].as_str().unwrap().contains("fetch error"));
        assert_eq!(res.confidence, 1.0);
    }

    #[tokio::test]
    async fn abort_signal_yields_aborted() {
        // Use ONE notify shared between the context and the pre-fire.
        let notify = Arc::new(Notify::new());
        let mut ctx = ResolverContext::new();
        ctx.abort = notify.clone();
        notify.notify_one(); // abort before the call
        let client = Arc::new(MockHttpClient::fixed_status(200));
        let dns = Arc::new(MockDnsResolver::empty());
        let r = UrlReachableResolver::new(client, dns);
        let req = ResolverRequest {
            input: json!({ "url": "https://example.com/" }),
            context: ctx,
            timeout_ms: None,
        };
        let err = r.resolve(req).await.unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Aborted);
    }

    // ---- check_dns_rebinding (mirrors TS checkDnsRebinding) ----

    #[tokio::test]
    async fn dns_rebinding_skips_ip_literals() {
        let (r, _) = resolver(200);
        assert!(r.check_dns_rebinding("http://8.8.8.8/").await.is_none());
        assert!(r.check_dns_rebinding("http://127.0.0.1/").await.is_none());
        assert!(r.check_dns_rebinding("http://[::1]/").await.is_none());
    }

    #[tokio::test]
    async fn dns_rebinding_unparseable_url_null() {
        let (r, _) = resolver(200);
        assert!(r.check_dns_rebinding("not a url").await.is_none());
    }

    #[tokio::test]
    async fn dns_rebinding_dns_failure_null() {
        let (r, _) = resolver(200);
        // Mock DNS returns Err → treated as "let fetch surface the error".
        let dns = Arc::new(MockDnsResolver::new(|_| Err(crate::resolvers::dns::DnsError)));
        let r2 = UrlReachableResolver::new(r.client.clone(), dns);
        assert!(r2.check_dns_rebinding("http://definitely-not-a-real-tld.invalidtld123/").await.is_none());
    }

    #[tokio::test]
    async fn dns_rebinding_private_target_blocked() {
        let (r, _) = resolver(200);
        let dns = Arc::new(MockDnsResolver::new(|_| {
            Ok(vec!["10.0.0.5".parse().unwrap()])
        }));
        let r2 = UrlReachableResolver::new(r.client.clone(), dns);
        let reason = r2.check_dns_rebinding("http://evil.example/").await.unwrap();
        assert!(reason.contains("private IP 10.0.0.5"));
    }
}
