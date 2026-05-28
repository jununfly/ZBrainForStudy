//! Slice 4a — `PostgresEngine` lifecycle skeleton.
//!
//! Implements [`BrainEngine`] identity + lifecycle (`connect` / `disconnect`
//! / `init_schema`) against a `sqlx::PgPool`. Page CRUD lands in slice 4b so
//! this file stays a reviewable contract surface and the trait coverage can
//! be filled in incrementally.

use std::sync::OnceLock;

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::engine::{
    BrainEngine, EngineConfig, EngineKind, GetPageOpts, Page, PageFilters, PageInput,
};
use crate::error::{Error, Result};

/// Embedded SQL migrations, baked into the binary at compile time. Driven by
/// `init_schema`. Future migrations are append-only files under `migrations/`.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Connection-pool-backed engine for `PostgreSQL`.
///
/// The pool is lazily installed by [`PostgresEngine::connect`] and consumed
/// by [`PostgresEngine::disconnect`]. Calling `connect` twice on the same
/// instance is rejected to keep ownership of the pool unambiguous.
pub struct PostgresEngine {
    pool: OnceLock<PgPool>,
}

impl PostgresEngine {
    /// Construct a disconnected engine. Call [`PostgresEngine::connect`]
    /// before any other method.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pool: OnceLock::new(),
        }
    }

    /// Borrow the live pool, or return an `Engine` error if `connect` has
    /// not run yet (or the pool was torn down by `disconnect`).
    fn pool(&self) -> Result<&PgPool> {
        self.pool
            .get()
            .ok_or_else(|| Error::engine("PostgresEngine is not connected"))
    }
}

impl Default for PostgresEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PostgresEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresEngine")
            .field("connected", &self.pool.get().is_some())
            .finish()
    }
}

#[async_trait]
impl BrainEngine for PostgresEngine {
    fn kind(&self) -> EngineKind {
        EngineKind::Postgres
    }

    async fn connect(&self, config: &EngineConfig) -> Result<()> {
        let url = config
            .database_url
            .as_deref()
            .ok_or_else(|| Error::engine("PostgresEngine requires EngineConfig.database_url"))?;

        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(|e| Error::engine(format!("postgres connect failed: {e}")))?;

        self.pool
            .set(pool)
            .map_err(|_| Error::engine("PostgresEngine is already connected"))?;
        Ok(())
    }

    async fn disconnect(&self) -> Result<()> {
        // `OnceLock` has no `take`; the recommended teardown is to close the
        // pool reference we already hold. `sqlx::Pool::close` is idempotent
        // and safe to call concurrently — once closed, any future query
        // returns `PoolClosed` so subsequent calls through `pool()` still
        // surface a clear error.
        if let Some(pool) = self.pool.get() {
            pool.close().await;
        }
        Ok(())
    }

    async fn init_schema(&self) -> Result<()> {
        let pool = self.pool()?;
        MIGRATOR
            .run(pool)
            .await
            .map_err(|e| Error::engine(format!("migration failed: {e}")))?;
        Ok(())
    }

    // ── Page CRUD — slice 4b ──────────────────────────────────────────────
    // These return `Error::engine("not yet implemented")` so the trait is
    // satisfied without silently returning empty results that would mask
    // missing implementations in downstream tests.

    async fn get_page(&self, _slug: &str, _opts: &GetPageOpts) -> Result<Option<Page>> {
        Err(Error::engine(
            "PostgresEngine::get_page lands in slice 4b",
        ))
    }

    async fn put_page(&self, _slug: &str, _input: &PageInput) -> Result<Page> {
        Err(Error::engine(
            "PostgresEngine::put_page lands in slice 4b",
        ))
    }

    async fn delete_page(&self, _slug: &str) -> Result<()> {
        Err(Error::engine(
            "PostgresEngine::delete_page lands in slice 4b",
        ))
    }

    async fn list_pages(&self, _filters: &PageFilters) -> Result<Vec<Page>> {
        Err(Error::engine(
            "PostgresEngine::list_pages lands in slice 4b",
        ))
    }

    async fn resolve_slugs(&self, _partial: &str) -> Result<Vec<String>> {
        Err(Error::engine(
            "PostgresEngine::resolve_slugs lands in slice 4b",
        ))
    }
}
