//! Transport primitive shared by resolvers (`url_reachable`, `x_api`).
//!
//! Mirrors the TS `globalThis.fetch` abstraction but with an *injectable*
//! client so resolvers stay fully offline-testable — tests pass a
//! [`MockHttpClient`], production passes the reqwest-backed
//! [`ReqwestHttpClient`] (compiled only behind the `resolvers` feature).
//!
//! `redirect` is always `manual`: resolvers drive their own redirect chains so
//! each hop can be re-validated against the SSRF guard. Abort + timeout are
//! threaded through [`HttpRequest`] and honored by the live client.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

/// HTTP verb a resolver may issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Head,
    Get,
    Post,
}

/// A single HTTP request issued by a resolver. Faithful to the subset of
/// `fetch(init)` the resolvers need.
pub struct HttpRequest {
    pub url: String,
    pub method: HttpMethod,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// Per-request budget. The live client applies it as a reqwest timeout.
    pub timeout: Duration,
    /// Optional shared abort switch. When notified, the live client races the
    /// in-flight request and yields [`HttpClientError::Aborted`].
    pub abort: Option<Arc<Notify>>,
}

impl HttpRequest {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// A single HTTP response returned to a resolver.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Transport-level failure. Resolvers translate these into either a
/// `ResolverError` (abort) or a `reachable=false` answer (timeout/transport).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpClientError {
    /// Caller aborted (AbortSignal / Notify fired).
    Aborted,
    /// Request exceeded its timeout budget.
    Timeout,
    /// DNS / connection-refused / TLS / 5xx-streaming failure.
    Transport(String),
}

/// Injectable HTTP client. Mirrors the `fetch` boundary that the TS resolvers
/// call, but decoupled from any global so it can be mocked.
#[async_trait::async_trait]
pub trait HttpClient: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpClientError>;
}

/// Test double: answers from a closure. Lets `url_reachable` / `x_api` tests
/// exercise redirect chains, status codes, and error paths with zero network.
pub struct MockHttpClient {
    pub handler: Arc<dyn Fn(&HttpRequest) -> Result<HttpResponse, HttpClientError> + Send + Sync>,
}

#[async_trait::async_trait]
impl HttpClient for MockHttpClient {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        (self.handler)(&req)
    }
}

impl MockHttpClient {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&HttpRequest) -> Result<HttpResponse, HttpClientError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(f),
        }
    }

    /// Answer every request with a fixed status and no body. Covers most
    /// `url_reachable` paths (200/404/405/redirect/network-failure).
    pub fn fixed_status(status: u16) -> Self {
        Self::new(move |_| {
            Ok(HttpResponse {
                status,
                headers: vec![],
                body: vec![],
            })
        })
    }
}

// ---------------------------------------------------------------------------
// Live reqwest-backed client (feature `resolvers`)
// ---------------------------------------------------------------------------

#[cfg(feature = "resolvers")]
mod live {
    use super::*;

    /// Production HTTP client: reqwest with `redirect=manual`, honoring the
    /// per-request timeout and abort [`Notify`].
    pub struct ReqwestHttpClient {
        client: reqwest::Client,
    }

    impl ReqwestHttpClient {
        pub fn new() -> Self {
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest client build");
            Self { client }
        }
    }

    fn map_err(e: reqwest::Error) -> HttpClientError {
        if e.is_timeout() {
            HttpClientError::Timeout
        } else {
            HttpClientError::Transport(e.to_string())
        }
    }

    async fn to_response(resp: reqwest::Response) -> Result<HttpResponse, HttpClientError> {
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        let body = resp.bytes().await.map_err(map_err)?;
        Ok(HttpResponse {
            status,
            headers,
            body: body.to_vec(),
        })
    }

    #[async_trait::async_trait]
    impl HttpClient for ReqwestHttpClient {
        async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            let method = match req.method {
                HttpMethod::Head => reqwest::Method::HEAD,
                HttpMethod::Get => reqwest::Method::GET,
                HttpMethod::Post => reqwest::Method::POST,
            };
            let mut rb = self.client.request(method, &req.url);
            for (k, v) in &req.headers {
                rb = rb.header(k, v);
            }
            if let Some(b) = &req.body {
                rb = rb.body(b.clone());
            }
            rb = rb.timeout(req.timeout);

            let fut = rb.send();
            match req.abort {
                Some(notify) => {
                    let notified = notify.notified();
                    tokio::pin!(notified);
                    tokio::select! {
                        _ = &mut notified => Err(HttpClientError::Aborted),
                        resp = fut => match resp {
                            Ok(r) => to_response(r).await,
                            Err(e) => Err(map_err(e)),
                        },
                    }
                }
                None => match fut.await {
                    Ok(r) => to_response(r).await,
                    Err(e) => Err(map_err(e)),
                },
            }
        }
    }
}

#[cfg(feature = "resolvers")]
pub use live::ReqwestHttpClient;
