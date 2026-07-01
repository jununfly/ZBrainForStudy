//! Contract tests for TokenQueries — verifies the InMemoryEngine stub behaves
//! according to the trait contract.

use zbrain_core::{token_queries::TokenQueries, AuthInfo, TokenError, InMemoryEngine};
use std::sync::Arc;

fn engine() -> Arc<dyn TokenQueries> {
    Arc::new(InMemoryEngine::default())
}

#[tokio::test]
async fn valid_token_returns_auth_info() {
    let q = engine();
    let result = q.verify_access_token("any-token").await;
    assert!(result.is_ok(), "expected Ok, got {result:?}");
    let info = result.unwrap();
    assert_eq!(info.token, "any-token");
    assert!(!info.client_id.is_empty());
    assert!(!info.scopes.is_empty());
    // expires_at should be in the future (stub returns i64::MAX)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(info.expires_at > now, "stub must return non-expired token");
}

#[tokio::test]
async fn empty_token_returns_invalid() {
    let q = engine();
    let result = q.verify_access_token("").await;
    assert_eq!(result.unwrap_err(), TokenError::Invalid);
}

#[tokio::test]
async fn auth_info_has_non_empty_scopes() {
    let q = engine();
    let info = q.verify_access_token("tok").await.unwrap();
    assert!(!info.scopes.is_empty(), "scopes must not be empty");
}

#[tokio::test]
async fn auth_info_is_clone_and_debug() {
    let q = engine();
    let info = q.verify_access_token("tok").await.unwrap();
    let _clone = info.clone();
    let _debug = format!("{info:?}");
}
