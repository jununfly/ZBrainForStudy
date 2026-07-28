//! LibsqlEngine TokenQueries integration tests.
//!
//! Tests verify_access_token against a real SQLite DB with the full
//! migration stack (0009 includes oauth_tokens table).

use libsql::Builder;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::token_queries::{TokenError, TokenQueries};

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
    engine.init_schema().await.unwrap();
    (temp, engine)
}

async fn raw_conn(temp: &NamedTempFile) -> ::libsql::Connection {
    Builder::new_local(temp.path())
        .build()
        .await
        .unwrap()
        .connect()
        .unwrap()
}

/// Insert a raw access token row directly into the DB for testing.
async fn insert_token(
    conn: &::libsql::Connection,
    token_hash: &str,
    client_id: &str,
    scopes_json: &str,
    expires_at: i64,
) {
    conn.execute(
        "INSERT INTO oauth_tokens (token_hash, client_id, token_type, scopes, expires_at) \
         VALUES (?1, ?2, 'access', ?3, ?4)",
        ::libsql::params![token_hash, client_id, scopes_json, expires_at],
    )
    .await
    .unwrap();
}

/// Compute SHA-256 hex of a string (mirrors engine logic).
fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

fn future_expires_at() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 3600
}

fn past_expires_at() -> i64 {
    1_000_000i64 // long in the past
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn valid_oauth_token_returns_auth_info() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(&temp).await;

    // Insert a client and a valid token
    conn.execute(
        "INSERT INTO oauth_clients (client_id, client_name, client_secret_hash, scope, grant_types) \
         VALUES ('cli-1', 'My Agent', 'x', 'read write', 'client_credentials')",
        ::libsql::params![],
    )
    .await
    .unwrap();

    let raw_token = "my-secret-token-abc";
    let hash = sha256_hex(raw_token);
    insert_token(&conn, &hash, "cli-1", r#"["read","write"]"#, future_expires_at()).await;

    let info = engine.verify_access_token(raw_token).await.unwrap();
    assert_eq!(info.client_id, "cli-1");
    assert_eq!(info.client_name.as_deref(), Some("My Agent"));
    assert!(info.scopes.contains(&"read".to_string()));
    assert!(info.scopes.contains(&"write".to_string()));
    assert_eq!(info.token, raw_token);
}

#[tokio::test]
async fn unknown_token_returns_invalid() {
    let _guard = libsql_test_guard();
    let (_temp, engine) = temp_engine().await;
    let err = engine.verify_access_token("no-such-token").await.unwrap_err();
    assert_eq!(err, TokenError::Invalid);
}

#[tokio::test]
async fn expired_token_returns_expired() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(&temp).await;

    conn.execute(
        "INSERT INTO oauth_clients (client_id, client_name, client_secret_hash, scope, grant_types) \
         VALUES ('cli-2', 'expired-agent', 'x', 'read', 'client_credentials')",
        ::libsql::params![],
    )
    .await
    .unwrap();

    let raw_token = "expired-token-xyz";
    let hash = sha256_hex(raw_token);
    insert_token(&conn, &hash, "cli-2", r#"["read"]"#, past_expires_at()).await;

    let err = engine.verify_access_token(raw_token).await.unwrap_err();
    assert_eq!(err, TokenError::Expired);
}

#[tokio::test]
async fn legacy_access_token_returns_full_admin_scopes() {
    let _guard = libsql_test_guard();
    let (temp, engine) = temp_engine().await;
    let conn = raw_conn(&temp).await;

    // The legacy access_tokens table may or may not exist in migration 0009.
    // If it doesn't, skip this test gracefully.
    let table_exists = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='access_tokens'",
            ::libsql::params![],
        )
        .await
        .unwrap()
        .next()
        .await
        .unwrap()
        .is_some();

    if !table_exists {
        // Table doesn't exist in this schema version — skip.
        return;
    }

    let raw_token = "legacy-token-abc";
    let hash = sha256_hex(raw_token);
    conn.execute(
        "INSERT INTO access_tokens (name, token_hash) VALUES ('old-client', ?1)",
        ::libsql::params![hash],
    )
    .await
    .unwrap();

    let info = engine.verify_access_token(raw_token).await.unwrap();
    assert_eq!(info.client_id, "old-client");
    // Legacy tokens get full admin scopes
    assert!(info.scopes.contains(&"admin".to_string()));
    assert!(info.scopes.contains(&"write".to_string()));
    assert!(info.scopes.contains(&"read".to_string()));
}
