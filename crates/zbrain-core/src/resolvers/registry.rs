//! ResolverRegistry — in-memory map id -> Resolver.
//!
//! Ported from TS `src/core/resolvers/registry.ts`. Single source of truth for
//! resolver lookup. Wired at boot in each CLI entry point (or test setUp) via
//! [`ResolverRegistry::register`]. Consumers call
//! [`ResolverRegistry::resolve`] rather than instantiating a Resolver directly,
//! so the set of available resolvers can grow via plugins later without
//! touching every caller.
//!
//! This file is intentionally dependency-free beyond `./interface` — keep it
//! that way so it can be unit-tested without mocking engine/storage.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use serde_json::Value as Json;

use super::interface::{
    Resolver, ResolverContext, ResolverCost, ResolverError, ResolverErrorCode, ResolverRequest,
    ResolverResult,
};
use super::{
    dns::DnsResolver, http::HttpClient, url_reachable::UrlReachableResolver,
    x_api::XHandleToTweetResolver,
};

/// Filter for [`ResolverRegistry::list`]. Mirrors TS `ResolverListFilter`.
#[derive(Debug, Clone, Default)]
pub struct ResolverListFilter {
    pub cost: Option<ResolverCost>,
    pub backend: Option<String>,
}

/// Summary shape returned by list(). Same data as the Resolver minus the
/// resolve()/available() methods — suitable for `zbrain resolvers list` and
/// plugin-discovery UX. Mirrors TS `ResolverSummary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolverSummary {
    pub id: String,
    pub cost: ResolverCost,
    pub backend: String,
    pub description: Option<String>,
    pub has_input_schema: bool,
    pub has_output_schema: bool,
}

/// In-memory resolver registry. Single source of truth for lookup. Mirrors TS
/// `ResolverRegistry` (erased to `Resolver<unknown, unknown>`; here we store
/// `Arc<dyn Resolver>`).
pub struct ResolverRegistry {
    resolvers: HashMap<String, Arc<dyn Resolver>>,
}

impl Default for ResolverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ResolverRegistry {
    pub fn new() -> Self {
        Self {
            resolvers: HashMap::new(),
        }
    }

    /// Register a resolver. Returns Err if the id is empty or already taken —
    /// catches copy-paste bugs early. Mirrors TS `register`.
    pub fn register(&mut self, resolver: Arc<dyn Resolver>) -> Result<(), ResolverError> {
        if resolver.id().is_empty() {
            return Err(ResolverError::new(
                ResolverErrorCode::Schema,
                "Resolver.id must be a non-empty string",
            ));
        }
        if self.resolvers.contains_key(resolver.id()) {
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::AlreadyRegistered,
                format!("Resolver '{}' is already registered", resolver.id()),
                resolver.id(),
            ));
        }
        self.resolvers.insert(resolver.id().to_string(), resolver);
        Ok(())
    }

    /// Return the resolver for id, or Err(NotFound). Mirrors TS `get`.
    pub fn get(&self, id: &str) -> Result<Arc<dyn Resolver>, ResolverError> {
        self.resolvers
            .get(id)
            .cloned()
            .ok_or_else(|| ResolverError::with_resolver(ResolverErrorCode::NotFound, format!("Resolver '{id}' not found"), id))
    }

    pub fn has(&self, id: &str) -> bool {
        self.resolvers.contains_key(id)
    }

    /// List all resolvers, optionally filtered by cost or backend. Sorted by id
    /// ascending. Mirrors TS `list`.
    pub fn list(&self, filter: Option<&ResolverListFilter>) -> Vec<ResolverSummary> {
        let mut all: Vec<&Arc<dyn Resolver>> = self.resolvers.values().collect();
        if let Some(f) = filter {
            if let Some(cost) = f.cost {
                all.retain(|r| r.cost() == cost);
            }
            if let Some(backend) = &f.backend {
                all.retain(|r| r.backend() == backend);
            }
        }
        let mut summaries: Vec<ResolverSummary> = all.iter().map(|r| to_summary(r)).collect();
        summaries.sort_by(|a, b| a.id.cmp(&b.id));
        summaries
    }

    /// Resolve an input through the given resolver id. Flow: get (NotFound) ->
    /// available (Unavailable) -> resolve. Does NOT wrap in FailImproveLoop or
    /// AbortSignal handling — those are concerns of the individual resolver.
    /// Mirrors TS `resolve`.
    pub async fn resolve(
        &self,
        id: &str,
        input: Json,
        ctx: &ResolverContext,
        opts: Option<ResolverResolveOpts>,
    ) -> Result<ResolverResult, ResolverError> {
        let resolver = self.get(id)?;
        if !resolver.available(ctx).await {
            return Err(ResolverError::with_resolver(
                ResolverErrorCode::Unavailable,
                format!("Resolver '{id}' is not available (check config/env)"),
                id,
            ));
        }
        let req = ResolverRequest {
            input,
            context: ctx.clone(),
            timeout_ms: opts.and_then(|o| o.timeout_ms),
        };
        resolver.resolve(req).await
    }

    /// Unregister all resolvers. Useful for tests and hot-reload.
    pub fn clear(&mut self) {
        self.resolvers.clear();
    }

    /// Number of registered resolvers.
    pub fn size(&self) -> usize {
        self.resolvers.len()
    }

    /// Register the two built-in resolvers: `url_reachable` and
    /// `x_handle_to_tweet`. The caller injects the HTTP + DNS clients, so this
    /// is fully offline-testable; production passes the reqwest-backed
    /// `ReqwestHttpClient` + `LiveDnsResolver` (behind the `resolvers` feature).
    /// Mirrors `src/cli/.../resolvers.ts` wiring `registerBuiltinResolvers`.
    pub fn register_builtin_resolvers(
        &mut self,
        http: Arc<dyn HttpClient>,
        dns: Arc<dyn DnsResolver>,
    ) {
        self.register(Arc::new(UrlReachableResolver::new(http.clone(), dns))).ok();
        self.register(Arc::new(XHandleToTweetResolver::new(http))).ok();
    }
}

fn to_summary(r: &Arc<dyn Resolver>) -> ResolverSummary {
    ResolverSummary {
        id: r.id().to_string(),
        cost: r.cost(),
        backend: r.backend().to_string(),
        description: r.description().map(str::to_string),
        has_input_schema: r.input_schema().is_some(),
        has_output_schema: r.output_schema().is_some(),
    }
}

/// Options for [`ResolverRegistry::resolve`]. Mirrors TS
/// `resolve(id, input, ctx, { timeoutMs })`.
#[derive(Debug, Clone, Default)]
pub struct ResolverResolveOpts {
    pub timeout_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Default process-wide registry
// ---------------------------------------------------------------------------

static DEFAULT_REGISTRY: OnceLock<Mutex<ResolverRegistry>> = OnceLock::new();

/// Get the default process-wide registry, creating it if needed. Returns a
/// guard that locks the shared registry — drop it promptly. Mirrors TS
/// `getDefaultRegistry` (which returns the singleton itself; in Rust we return
/// a guard because the registry is mutable and shared across threads).
pub fn get_default_registry() -> MutexGuard<'static, ResolverRegistry> {
    DEFAULT_REGISTRY
        .get_or_init(|| Mutex::new(ResolverRegistry::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Reset the default registry to empty. For tests and hot-reload.
pub fn reset_default_registry() {
    let mut reg = get_default_registry();
    *reg = ResolverRegistry::new();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny fake resolver for contract tests (mirrors TS `echoResolver`).
    struct EchoResolver {
        id: String,
    }
    impl EchoResolver {
        fn new(id: &str) -> Arc<dyn Resolver> {
            Arc::new(Self { id: id.to_string() })
        }
    }
    #[async_trait::async_trait]
    impl Resolver for EchoResolver {
        fn id(&self) -> &str {
            &self.id
        }
        fn cost(&self) -> ResolverCost {
            ResolverCost::Free
        }
        fn backend(&self) -> &str {
            "local"
        }
        fn description(&self) -> Option<&str> {
            Some("Echo")
        }
        async fn available(&self, _ctx: &ResolverContext) -> bool {
            true
        }
        async fn resolve(&self, req: ResolverRequest) -> Result<ResolverResult, ResolverError> {
            Ok(ResolverResult {
                value: req.input,
                confidence: 1.0,
                source: "local".to_string(),
                fetched_at: chrono::Utc::now(),
                cost_estimate: Some(0.0),
                raw: None,
            })
        }
    }

    /// A resolver that answers `available()` == false. Used to exercise the
    /// Unavailable path. Mirrors the inline `blocked` resolver in TS.
    struct BlockedResolver;
    #[async_trait::async_trait]
    impl Resolver for BlockedResolver {
        fn id(&self) -> &str {
            "blocked"
        }
        fn cost(&self) -> ResolverCost {
            ResolverCost::Free
        }
        fn backend(&self) -> &str {
            "local"
        }
        async fn available(&self, _ctx: &ResolverContext) -> bool {
            false
        }
        async fn resolve(&self, _req: ResolverRequest) -> Result<ResolverResult, ResolverError> {
            Ok(ResolverResult {
                value: Json::Null,
                confidence: 1.0,
                source: "local".to_string(),
                fetched_at: chrono::Utc::now(),
                cost_estimate: None,
                raw: None,
            })
        }
    }

    /// A paid backend resolver (for the cost-filter test).
    struct PaidResolver;
    #[async_trait::async_trait]
    impl Resolver for PaidResolver {
        fn id(&self) -> &str {
            "paid-one"
        }
        fn cost(&self) -> ResolverCost {
            ResolverCost::Paid
        }
        fn backend(&self) -> &str {
            "local"
        }
        async fn available(&self, _ctx: &ResolverContext) -> bool {
            true
        }
        async fn resolve(&self, _req: ResolverRequest) -> Result<ResolverResult, ResolverError> {
            Ok(ResolverResult {
                value: Json::Null,
                confidence: 1.0,
                source: "local".to_string(),
                fetched_at: chrono::Utc::now(),
                cost_estimate: Some(0.01),
                raw: None,
            })
        }
    }

    /// An x-api-v2 backend resolver (for the backend-filter test).
    struct XResolver;
    #[async_trait::async_trait]
    impl Resolver for XResolver {
        fn id(&self) -> &str {
            "x-one"
        }
        fn cost(&self) -> ResolverCost {
            ResolverCost::Free
        }
        fn backend(&self) -> &str {
            "x-api-v2"
        }
        async fn available(&self, _ctx: &ResolverContext) -> bool {
            true
        }
        async fn resolve(&self, _req: ResolverRequest) -> Result<ResolverResult, ResolverError> {
            Ok(ResolverResult {
                value: Json::Null,
                confidence: 1.0,
                source: "x-api-v2".to_string(),
                fetched_at: chrono::Utc::now(),
                cost_estimate: None,
                raw: None,
            })
        }
    }

    fn ctx() -> ResolverContext {
        ResolverContext::new()
    }

    // --- lifecycle --------------------------------------------------------

    #[test]
    fn starts_empty() {
        let reg = ResolverRegistry::new();
        assert_eq!(reg.size(), 0);
        assert!(reg.list(None).is_empty());
    }

    #[test]
    fn register_get_has() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap();
        assert_eq!(reg.size(), 1);
        assert!(reg.has("echo"));
        assert_eq!(reg.get("echo").unwrap().id(), "echo");
    }

    #[test]
    fn register_rejects_duplicate() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap();
        let err = reg.register(EchoResolver::new("echo")).unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::AlreadyRegistered);
        assert_eq!(err.resolver_id.as_deref(), Some("echo"));
    }

    #[test]
    fn register_rejects_empty_id() {
        let mut reg = ResolverRegistry::new();
        let err = reg.register(EchoResolver::new("")).unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Schema);
    }

    #[test]
    fn get_throws_not_found() {
        let reg = ResolverRegistry::new();
        // `get` returns `Result<Arc<dyn Resolver>, _>`; `Arc<dyn Resolver>` is
        // not `Debug`, so we cannot use `unwrap_err()` (which would print the
        // Ok value). Extract the error via match instead.
        let err = match reg.get("nope") {
            Ok(_) => panic!("expected get('nope') to error"),
            Err(e) => e,
        };
        assert_eq!(err.code, ResolverErrorCode::NotFound);
        assert_eq!(err.resolver_id.as_deref(), Some("nope"));
    }

    #[test]
    fn clear_empties() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap();
        reg.clear();
        assert_eq!(reg.size(), 0);
    }

    // --- list filtering + ordering ---------------------------------------

    #[test]
    fn list_sorted_by_id() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap();
        reg.register(EchoResolver::new("alpha")).unwrap();
        let ids: Vec<String> = reg.list(None).into_iter().map(|s| s.id).collect();
        assert_eq!(ids, vec!["alpha".to_string(), "echo".to_string()]);
        let first = &reg.list(None)[0];
        assert_eq!(first.cost, ResolverCost::Free);
        assert_eq!(first.backend, "local");
        assert_eq!(first.description.as_deref(), Some("Echo"));
    }

    #[test]
    fn list_filters_by_cost() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap(); // free
        reg.register(Arc::new(PaidResolver)).unwrap(); // paid
        let paid: Vec<String> = reg
            .list(Some(&ResolverListFilter {
                cost: Some(ResolverCost::Paid),
                backend: None,
            }))
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(paid, vec!["paid-one".to_string()]);
        let free: Vec<String> = reg
            .list(Some(&ResolverListFilter {
                cost: Some(ResolverCost::Free),
                backend: None,
            }))
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(free, vec!["echo".to_string()]);
    }

    #[test]
    fn list_filters_by_backend() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap();
        reg.register(Arc::new(XResolver)).unwrap();
        let ids: Vec<String> = reg
            .list(Some(&ResolverListFilter {
                cost: None,
                backend: Some("x-api-v2".to_string()),
            }))
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["x-one".to_string()]);
    }

    // --- resolve flow -----------------------------------------------------

    #[tokio::test]
    async fn resolve_returns_result() {
        let mut reg = ResolverRegistry::new();
        reg.register(EchoResolver::new("echo")).unwrap();
        let r = reg
            .resolve("echo", serde_json::json!({"v": "hi"}), &ctx(), None)
            .await
            .unwrap();
        assert_eq!(r.value, serde_json::json!({"v": "hi"}));
        assert_eq!(r.confidence, 1.0);
        assert_eq!(r.source, "local");
    }

    #[tokio::test]
    async fn resolve_throws_not_found() {
        let reg = ResolverRegistry::new();
        let err = reg
            .resolve("nope", Json::Null, &ctx(), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::NotFound);
    }

    #[tokio::test]
    async fn resolve_throws_unavailable_when_available_false() {
        let mut reg = ResolverRegistry::new();
        reg.register(Arc::new(BlockedResolver)).unwrap();
        let err = reg
            .resolve("blocked", Json::Null, &ctx(), None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ResolverErrorCode::Unavailable);
        assert_eq!(err.resolver_id.as_deref(), Some("blocked"));
    }

    // --- default process-wide registry -----------------------------------

    #[test]
    fn default_registry_persists_across_calls() {
        reset_default_registry();
        {
            let mut reg = get_default_registry();
            reg.register(EchoResolver::new("echo")).unwrap();
        }
        {
            let reg = get_default_registry();
            assert_eq!(reg.size(), 1);
            assert!(reg.has("echo"));
        }
        reset_default_registry();
        {
            let reg = get_default_registry();
            assert_eq!(reg.size(), 0);
        }
    }
}
