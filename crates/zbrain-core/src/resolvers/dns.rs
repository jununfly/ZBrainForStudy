//! DNS resolution primitive for the `url_reachable` resolver's rebinding
//! defense. Mirrors TS `dns.lookup(host, { all: true })` but injectable so the
//! rebinding check is offline-testable.
//!
//! In production the live resolver uses `tokio::net::lookup_host` (no extra
//! dependency). On resolution failure it returns `Err` and the caller lets the
//! real HTTP fetch surface the error — matching the TS behavior of not
//! blocking on ambiguous DNS.

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;

/// Opaque DNS failure (NXDOMAIN, network glitch, …). Callers treat it as
/// "let the fetch decide" rather than a hard block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsError;

/// Resolves a hostname to its A/AAAA addresses.
#[async_trait::async_trait]
pub trait DnsResolver: Send + Sync {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, DnsError>;
}

/// Test double: answers from a closure. `MockDnsResolver::empty()` models the
/// common case (no records → safe, let fetch surface errors).
pub struct MockDnsResolver {
    pub handler: Arc<dyn Fn(&str) -> Result<Vec<IpAddr>, DnsError> + Send + Sync>,
}

#[async_trait::async_trait]
impl DnsResolver for MockDnsResolver {
    async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, DnsError> {
        (self.handler)(host)
    }
}

impl MockDnsResolver {
    pub fn new<F>(f: F) -> Self
    where
        F: Fn(&str) -> Result<Vec<IpAddr>, DnsError> + Send + Sync + 'static,
    {
        Self {
            handler: Arc::new(f),
        }
    }

    /// No addresses — models a host that resolves to nothing suspicious.
    pub fn empty() -> Self {
        Self::new(|_| Ok(vec![]))
    }
}

// ---------------------------------------------------------------------------
// Live resolver (feature `resolvers`)
// ---------------------------------------------------------------------------

#[cfg(feature = "resolvers")]
mod live {
    use super::*;

    /// Production DNS resolver backed by the OS via `tokio::net::lookup_host`.
    pub struct TokioDnsResolver;

    #[async_trait::async_trait]
    impl DnsResolver for TokioDnsResolver {
        async fn lookup(&self, host: &str) -> Result<Vec<IpAddr>, DnsError> {
            tokio::net::lookup_host((host, 0))
                .await
                .map(|addrs| addrs.map(|a| a.ip()).collect())
                .map_err(|_| DnsError)
        }
    }
}

#[cfg(feature = "resolvers")]
pub use live::TokioDnsResolver;
