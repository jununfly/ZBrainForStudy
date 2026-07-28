//! LibsqlEngine OAuthQueries integration tests.
//!
//! Requires migration 0009 (oauth_clients, oauth_tokens, oauth_codes tables).

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::oauth_queries::OAuthQueries;
use zbrain_core::RegisterClientRequest;

/// Serialize all libsql FFI access in this binary. The `libsql` native
/// library is not safe to drive from multiple OS threads concurrently on
/// Windows (parallel `cargo test` threads crash with STATUS_ACCESS_VIOLATION
/// 0xc0000005). Each test grabs this guard for its whole body so the suite
/// stays green under default parallelism; serial runs are unaffected.
static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
fn libsql_test_guard() -> std::sync::MutexGuard<'static, ()> {
    LIBSQL_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}


fn temp_db() -> NamedTempFile {
    NamedTempFile::new().expect("alloc temp db file")
}

async fn temp_engine() -> (NamedTempFile, LibsqlEngine) {
    let temp = temp_db();
    let path = temp.path().to_string_lossy().to_string();
    let config = EngineConfig {
        database_path: Some(path),
        database_url: None,
    };
    let engine = LibsqlEngine::new();
    engine.connect(&config).await.unwrap();
    (temp, engine)
}

/// Open a fresh direct connection to the temp DB for raw verification queries.
async fn raw_conn(temp: &NamedTempFile) -> ::libsql::Connection {
    Builder::new_local(temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn register_client_persists_and_returns_id_and_secret() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let resp = engine
        .register_client(RegisterClientRequest {
            name: "test-agent".into(),
            scope: "read write".into(),
            grant_types: vec!["client_credentials".into()],
            redirect_uris: vec![],
            token_endpoint_auth_method: Some("client_secret_basic".into()),
            token_ttl: Some(3600),
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await
        .expect("register_client should succeed");

    assert!(!resp.client_id.is_empty(), "client_id must not be empty");
    assert!(!resp.client_secret.is_empty(), "client_secret must not be empty");
    // client_id should be a UUID (36 chars with hyphens)
    assert_eq!(resp.client_id.len(), 36, "client_id should be a UUID v4");
    assert!(resp.client_id.contains('-'), "client_id should contain hyphens");
}

#[tokio::test]
async fn register_client_persists_row_in_oauth_clients_table() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let resp = engine
        .register_client(RegisterClientRequest {
            name: "another-agent".into(),
            scope: "read".into(),
            grant_types: vec!["authorization_code".into()],
            redirect_uris: vec!["http://localhost/callback".into()],
            token_endpoint_auth_method: None,
            token_ttl: None,
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await
        .expect("register_client should succeed");

    // Query the table directly to verify the row exists
    let conn = raw_conn(&_temp).await;
    let mut rows = conn
        .query(
            "SELECT client_name, scope, token_ttl FROM oauth_clients WHERE client_id = ?1",
            libsql::params![resp.client_id],
        )
        .await
        .unwrap();

    let row = rows.next().await.unwrap().unwrap();
    let name: String = row.get(0).unwrap();
    let scope: String = row.get(1).unwrap();
    let ttl: Option<i64> = row.get(2).unwrap();

    assert_eq!(name, "another-agent");
    assert_eq!(scope, "read");
    assert_eq!(ttl, None);
}

#[tokio::test]
async fn update_client_ttl_persists_and_returns() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    // First register a client
    let resp = engine
        .register_client(RegisterClientRequest {
            name: "ttl-test".into(),
            scope: "read".into(),
            grant_types: vec!["client_credentials".into()],
            redirect_uris: vec![],
            token_endpoint_auth_method: None,
            token_ttl: None,
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await
        .unwrap();

    // Update TTL
    let update = engine
        .update_client_ttl(&resp.client_id, Some(7200))
        .await
        .expect("update_client_ttl should succeed");
    assert!(update.updated);
    assert_eq!(update.token_ttl, Some(7200));

    // Verify in DB
    let conn = raw_conn(&_temp).await;
    let mut rows = conn
        .query(
            "SELECT token_ttl FROM oauth_clients WHERE client_id = ?1",
            libsql::params![resp.client_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let ttl: i64 = row.get(0).unwrap();
    assert_eq!(ttl, 7200);
}

#[tokio::test]
async fn update_client_ttl_null_resets_to_null() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let resp = engine
        .register_client(RegisterClientRequest {
            name: "ttl-null".into(),
            scope: "read".into(),
            grant_types: vec!["client_credentials".into()],
            redirect_uris: vec![],
            token_endpoint_auth_method: None,
            token_ttl: Some(3600),
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await
        .unwrap();

    // Reset to null
    let update = engine
        .update_client_ttl(&resp.client_id, None)
        .await
        .expect("update_client_ttl with None should succeed");
    assert!(update.updated);
    assert_eq!(update.token_ttl, None);

    // Verify null in DB
    let conn = raw_conn(&_temp).await;
    let mut rows = conn
        .query(
            "SELECT token_ttl FROM oauth_clients WHERE client_id = ?1",
            libsql::params![resp.client_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let ttl: Option<i64> = row.get(0).unwrap();
    assert_eq!(ttl, None);
}

#[tokio::test]
async fn revoke_client_soft_deletes_and_clears_tokens() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    // Register a client
    let resp = engine
        .register_client(RegisterClientRequest {
            name: "revoke-me".into(),
            scope: "read".into(),
            grant_types: vec!["client_credentials".into()],
            redirect_uris: vec![],
            token_endpoint_auth_method: None,
            token_ttl: None,
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await
        .unwrap();

    // Insert a fake token for this client
    let conn = raw_conn(&_temp).await;
    conn.execute(
        "INSERT INTO oauth_tokens (token_hash, token_type, client_id, scopes) VALUES (?1, 'access', ?2, 'read')",
        libsql::params!["fake-hash", resp.client_id.clone()],
    )
    .await
    .unwrap();

    // Revoke
    let result = engine
        .revoke_client(&resp.client_id)
        .await
        .expect("revoke_client should succeed");
    assert!(result.revoked);

    // Verify soft-delete (use fresh conn to avoid WAL visibility issues)
    let conn2 = raw_conn(&_temp).await;
    let mut rows = conn2
        .query(
            "SELECT deleted_at FROM oauth_clients WHERE client_id = ?1",
            libsql::params![resp.client_id.clone()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let deleted_at: String = row.get(0).unwrap();
    assert!(!deleted_at.is_empty(), "deleted_at should be set");

    // Verify tokens cleared
    let mut rows = conn2
        .query(
            "SELECT COUNT(*) FROM oauth_tokens WHERE client_id = ?1",
            libsql::params![resp.client_id],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    let count: i64 = row.get(0).unwrap();
    assert_eq!(count, 0, "all tokens should be deleted");
}

#[tokio::test]
async fn revoked_client_cannot_be_revoked_again() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    engine.init_schema().await.unwrap();

    let resp = engine
        .register_client(RegisterClientRequest {
            name: "revoke-once".into(),
            scope: "read".into(),
            grant_types: vec!["client_credentials".into()],
            redirect_uris: vec![],
            token_endpoint_auth_method: None,
            token_ttl: None,
            source_id: "default".into(),
            federated_read: vec![],
        })
        .await
        .unwrap();

    // First revoke
    let r1 = engine.revoke_client(&resp.client_id).await.unwrap();
    assert!(r1.revoked);

    // Second revoke — should still return ok (idempotent, deleted_at already set)
    let r2 = engine.revoke_client(&resp.client_id).await.unwrap();
    assert!(r2.revoked);
}
