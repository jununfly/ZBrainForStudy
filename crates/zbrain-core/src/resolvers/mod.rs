//! Resolver SDK — typed interface for external lookups.
//!
//! A Resolver takes an erased input (`serde_json::Value`), hits some backend
//! (X API, URL HEAD check, local brain lookup, LLM extraction), and returns a
//! [`ResolverResult`] with confidence + provenance.
//!
//! Slice 1-6-4-10-1 ports the SDK core (this module + `interface` +
//! `registry`). The `url_reachable` and `x_api` builtins, plus the
//! `HttpClient` transport seam, land in follow-up slices (1-6-4-10-2 / -3).

pub mod interface;
pub mod registry;

pub use interface::{
    NoopResolverLogger, Resolver, ResolverContext, ResolverCost, ResolverErrorCode, ResolverError,
    ResolverLogger, ResolverRequest, ResolverResult,
};
pub use registry::{
    get_default_registry, reset_default_registry, ResolverListFilter, ResolverRegistry,
    ResolverResolveOpts, ResolverSummary,
};
