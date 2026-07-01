//! Magic-link admin authentication.
//!
//! Independent from AdminAuth. Owns nonce lifecycle and rate limiting.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Magic-link admin authentication.
///
/// Manages nonce lifecycle (issue → redeem → consume) and rate limiting.
/// Independent from `AdminAuth` — callers coordinate session creation.
#[derive(Clone)]
pub struct MagicLinkAuth {
    /// Active nonces: nonce → expires_at (Unix timestamp).
    nonces: Arc<RwLock<HashMap<String, i64>>>,
    /// Consumed nonces (anti-replay).
    consumed: Arc<RwLock<HashSet<String>>>,
    /// Rate limit state: IP → sliding window timestamps.
    rate_limits: Arc<RwLock<HashMap<IpAddr, Vec<i64>>>>,
}

impl MagicLinkAuth {
    /// Create a new `MagicLinkAuth` with empty state.
    pub fn new() -> Self {
        Self {
            nonces: Arc::new(RwLock::new(HashMap::new())),
            consumed: Arc::new(RwLock::new(HashSet::new())),
            rate_limits: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Issue a new magic-link nonce.
    ///
    /// Returns `(nonce, url, expires_in_seconds)`.
    /// The nonce is a 32-byte random hex string (64 chars), valid for 5 minutes.
    pub async fn issue_nonce(&self, host: &str) -> (String, String, i64) {
        let mut buf = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut buf);
        let nonce: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        let expires_at = now_unix() + 300; // 5 minutes
        self.nonces.write().await.insert(nonce.clone(), expires_at);
        let url = format!("http://{host}/admin/auth/{nonce}");
        (nonce, url, 300)
    }

    /// Redeem a nonce token.
    ///
    /// Validates the nonce exists, is not expired, and has not already been
    /// consumed (replay protection). On success, the nonce is atomically
    /// moved to the consumed set.
    pub async fn redeem_nonce(&self, nonce: &str) -> Result<Redeemed, RedeemError> {
        let mut nonces = self.nonces.write().await;
        let Some(expires_at) = nonces.remove(nonce) else {
            return if self.consumed.read().await.contains(nonce) {
                Err(RedeemError::AlreadyConsumed)
            } else {
                Err(RedeemError::Invalid)
            };
        };
        if now_unix() > expires_at {
            return Err(RedeemError::Expired);
        }
        self.consumed.write().await.insert(nonce.to_string());
        Ok(Redeemed)
    }

    /// Remove expired nonces.
    pub async fn prune_expired(&self) {
        let now = now_unix();
        let mut nonces = self.nonces.write().await;
        nonces.retain(|_k, expires| *expires > now);
    }

    /// Check rate limit for an IP address.
    ///
    /// Sliding window: max 10 requests per 60 seconds per IP.
    /// Returns `Err(RateLimitError::TooManyRequests)` if the limit is exceeded.
    pub async fn check_rate_limit(&self, ip: IpAddr) -> Result<(), RateLimitError> {
        const MAX_REQUESTS: usize = 10;
        const WINDOW_SECS: i64 = 60;

        let now = now_unix();
        let cutoff = now - WINDOW_SECS;
        let mut limits = self.rate_limits.write().await;
        let timestamps = limits.entry(ip).or_default();

        // Remove timestamps outside the window.
        timestamps.retain(|t| *t > cutoff);

        if timestamps.len() >= MAX_REQUESTS {
            return Err(RateLimitError::TooManyRequests);
        }

        timestamps.push(now);
        Ok(())
    }
}

/// Return the current Unix timestamp.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[derive(Clone, Debug, PartialEq)]
pub struct Redeemed;

#[derive(Clone, Debug, PartialEq)]
pub enum RedeemError {
    Invalid,
    Expired,
    AlreadyConsumed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RateLimitError {
    TooManyRequests,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn issue_nonce_returns_64_char_hex_and_url() {
        let ml = MagicLinkAuth::new();
        let (nonce, url, expires_in) = ml.issue_nonce("localhost:3000").await;

        assert_eq!(nonce.len(), 64, "nonce must be 64 hex characters (32 bytes)");
        assert!(nonce.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            url,
            format!("http://localhost:3000/admin/auth/{nonce}")
        );
        assert_eq!(expires_in, 300);
    }

    #[tokio::test]
    async fn redeem_valid_nonce_succeeds() {
        let ml = MagicLinkAuth::new();
        let (nonce, _url, _expires_in) = ml.issue_nonce("localhost:3000").await;
        let result = ml.redeem_nonce(&nonce).await;
        assert_eq!(result, Ok(Redeemed));
    }

    #[tokio::test]
    async fn redeem_invalid_nonce_returns_error() {
        let ml = MagicLinkAuth::new();
        let result = ml.redeem_nonce("unknown-nonce-64-chars-----------------placeholder-xx").await;
        assert_eq!(result, Err(RedeemError::Invalid));
    }

    #[tokio::test]
    async fn redeem_expired_nonce_returns_error() {
        let ml = MagicLinkAuth::new();
        let (nonce, _url, _expires_in) = ml.issue_nonce("localhost:3000").await;
        // Manually expire the nonce by setting its timestamp to the past.
        ml.nonces.write().await.insert(nonce.clone(), now_unix() - 1);
        let result = ml.redeem_nonce(&nonce).await;
        assert_eq!(result, Err(RedeemError::Expired));
    }

    #[tokio::test]
    async fn redeem_consumed_nonce_is_replay_protected() {
        let ml = MagicLinkAuth::new();
        let (nonce, _url, _expires_in) = ml.issue_nonce("localhost:3000").await;
        // First redeem succeeds.
        assert_eq!(ml.redeem_nonce(&nonce).await, Ok(Redeemed));
        // Second redeem must fail as AlreadyConsumed.
        assert_eq!(ml.redeem_nonce(&nonce).await, Err(RedeemError::AlreadyConsumed));
    }

    #[tokio::test]
    async fn prune_removes_expired_nonces() {
        let ml = MagicLinkAuth::new();
        let (nonce, _url, _expires_in) = ml.issue_nonce("localhost:3000").await;
        // Expire it.
        ml.nonces.write().await.insert(nonce.clone(), now_unix() - 1);
        ml.prune_expired().await;
        // After prune, the nonce should be gone, so redeem fails as Invalid.
        assert_eq!(ml.redeem_nonce(&nonce).await, Err(RedeemError::Invalid));
    }

    #[tokio::test]
    async fn lru_eviction_caps_live_nonces() {
        let ml = MagicLinkAuth::new();
        let mut nonces: Vec<String> = Vec::new();
        for _ in 0..10 {
            let (n, _, _) = ml.issue_nonce("localhost").await;
            nonces.push(n);
        }
        // All 10 should be redeemable.
        for n in &nonces {
            assert_eq!(ml.redeem_nonce(n).await, Ok(Redeemed));
        }
    }

    // -- rate limiting --

    #[tokio::test]
    async fn rate_limit_allows_exactly_10_requests_per_window() {
        use std::net::{IpAddr, Ipv4Addr};
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let ml = MagicLinkAuth::new();

        for _ in 0..10 {
            assert_eq!(ml.check_rate_limit(ip).await, Ok(()));
        }
        assert_eq!(ml.check_rate_limit(ip).await, Err(RateLimitError::TooManyRequests));
    }

    #[tokio::test]
    async fn rate_limit_resets_for_different_ips() {
        use std::net::{IpAddr, Ipv4Addr};
        let ip1 = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let ip2 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let ml = MagicLinkAuth::new();

        for _ in 0..10 {
            ml.check_rate_limit(ip1).await.unwrap();
        }
        assert_eq!(ml.check_rate_limit(ip1).await, Err(RateLimitError::TooManyRequests));
        assert_eq!(ml.check_rate_limit(ip2).await, Ok(()));
    }
}
