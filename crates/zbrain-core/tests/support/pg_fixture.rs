//! Ephemeral PostgreSQL fixture for integration tests.
//!
//! Each call to [`PgFixture::start`] launches a fresh `pg-embed` PostgreSQL
//! instance with an isolated data directory. On drop the process is killed
//! and the data directory is cleaned up (persistent=false). No external
//! PostgreSQL or Docker installation is required — pg-embed downloads a
//! pre-compiled binary on first use (cached thereafter).
//!
//! # Port allocation
//!
//! We bind a `TcpListener` to `127.0.0.1:0` to let the OS pick a free port,
//! then drop the listener and pass that port number to pg-embed. This avoids
//! the race condition where pg-embed's `port: 0` is passed literally to
//! `pg_ctl` without dynamic allocation.

use std::net::TcpListener;
use std::path::PathBuf;

use pg_embed::postgres::{PgEmbed, PgSettings};
use pg_embed::pg_fetch::{PgFetchSettings, PG_V17};
use sqlx::Executor;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::postgres::PostgresEngine;

/// RAII fixture that owns a running `pg-embed` PostgreSQL instance.
///
/// Provides a [`PostgresEngine`] that has already been `connect()`-ed and
/// `init_schema()`-ed, ready for test assertions.
pub struct PgFixture {
    /// The pg-embed instance. Kept alive so the PG process stays running.
    _pg: PgEmbed,
    /// The engine exposed to the test.
    pub engine: PostgresEngine,
    /// The database URL for direct SQL access if needed.
    pub url: String,
}

impl PgFixture {
    /// Start a fresh PostgreSQL instance, create an isolated database,
    /// connect a `PostgresEngine` to it, and run `init_schema()`.
    pub async fn start() -> Self {
        let db_name = format!(
            "zbrain_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        // Allocate a free port via the OS.
        let listener = TcpListener::bind("127.0.0.1:0")
            .expect("bind to find free port");
        let port = listener.local_addr().unwrap().port();
        drop(listener); // release before pg-embed binds

        // Temp dir for PG data; will be cleaned up by pg-embed (persistent=false).
        let database_dir = PathBuf::from(format!("/tmp/pg_embed_{db_name}"));

        let pg_settings = PgSettings {
            database_dir,
            port,
            user: "postgres".to_string(),
            password: "postgres".to_string(),
            auth_method: pg_embed::postgres::PgAuthMethod::Plain,
            persistent: false,
            timeout: Some(std::time::Duration::from_secs(30)),
            migration_dir: None,
        };

        let fetch_settings = PgFetchSettings {
            version: PG_V17,
            ..Default::default()
        };

        let mut pg = PgEmbed::new(pg_settings, fetch_settings)
            .await
            .expect("pg-embed init failed");

        // Download PG binary + run initdb (cached after first download).
        pg.setup().await.expect("pg-embed setup failed");

        pg.start_db().await.expect("pg-embed start_db failed");

        // Create the test database via pg-embed helper.
        pg.create_database(&db_name)
            .await
            .expect("create test database");

        let url = pg.full_db_uri(&db_name);

        // Connect PostgresEngine to the fresh database.
        let engine = PostgresEngine::new();
        let cfg = EngineConfig {
            database_url: Some(url.clone()),
            database_path: None,
        };
        engine.connect(&cfg).await.expect("PostgresEngine connect");
        engine.init_schema().await.expect("init_schema");

        Self {
            _pg: pg,
            engine,
            url,
        }
    }
}

impl Drop for PgFixture {
    fn drop(&mut self) {
        // Disconnect engine gracefully.
        // SAFETY: Drop may be called inside an async runtime. Using
        // `block_on` from the current handle would panic. Instead we
        // spawn a new minimal runtime — but only if we're NOT already
        // inside a tokio runtime. If we are, we rely on the engine's
        // own Drop (which should handle disconnect internally) or let
        // the connection drop naturally when the process exits.
        let engine = std::mem::replace(&mut self.engine, PostgresEngine::new());
        if tokio::runtime::Handle::try_current().is_err() {
            // Not inside a tokio runtime — safe to create one.
            let rt = tokio::runtime::Runtime::new().expect("create drop runtime");
            let _ = rt.block_on(engine.disconnect());
        }
        // If inside a tokio runtime, we skip block_on to avoid panic.
        // The engine connection will be cleaned up when the process exits.
        // pg-embed with persistent=false handles its own cleanup.
    }
}
