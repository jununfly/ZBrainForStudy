//! Resolver SDK — typed interface for external lookups.
//!
//! A Resolver takes an erased input (`serde_json::Value`), hits some backend
//! (X API, URL HEAD check, local brain lookup, LLM extraction), and returns a
//! [`ResolverResult`] with confidence + provenance.
//!
//! Slice 1-6-4-10-1 ports the SDK core (this module + `interface` +
//! `registry`). The `url_reachable` builtin plus the shared `HttpClient` /
//! `DnsResolver` transport seam land in 1-6-4-10-2; `x_api` in 1-6-4-10-3.

pub mod dns;
pub mod http;
pub mod interface;
pub mod registry;
pub mod url_reachable;

pub use dns::{DnsError, DnsResolver, MockDnsResolver};
pub use http::{
    HttpClient, HttpClientError, HttpMethod, HttpRequest, HttpResponse, MockHttpClient,
};
pub use interface::{
    NoopResolverLogger, Resolver, ResolverContext, ResolverCost, ResolverErrorCode, ResolverError,
    ResolverLogger, ResolverRequest, ResolverResult,
};
pub use registry::{
    get_default_registry, reset_default_registry, ResolverListFilter, ResolverRegistry,
    ResolverResolveOpts, ResolverSummary,
};
pub use url_reachable::UrlReachableResolver;

#[cfg(feature = "resolvers")]
pub use dns::TokioDnsResolver;
#[cfg(feature = "resolvers")]
pub use http::ReqwestHttpClient;
