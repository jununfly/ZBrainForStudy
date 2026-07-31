//! Sync concurrency strategy selection.
//!
//! Different engine backends have different concurrency characteristics:
//! - Postgres: supports parallel writes via connection pool.
//! - Libsql/PGLite: single-writer, best with serial execution.
//! - InMemory: fine with parallel (used in tests).
//!
//! This module detects the engine type and returns the appropriate
//! concurrency strategy for the sync pipeline.

use crate::engine::{BrainEngine, EngineKind};
use std::sync::Arc;

/// Concurrency strategy for sync operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncConcurrency {
    /// Run sync operations one at a time (serial).
    Serial,
    /// Run sync operations concurrently with the given parallelism level.
    Parallel(usize),
}

impl SyncConcurrency {
    /// Maximum number of concurrent file imports.
    pub fn max_concurrency(&self) -> usize {
        match self {
            SyncConcurrency::Serial => 1,
            SyncConcurrency::Parallel(n) => *n,
        }
    }
}

/// Default parallelism level for Postgres engine.
pub const DEFAULT_POSTGRES_PARALLELISM: usize = 8;

/// Determine the appropriate concurrency strategy for a given engine.
pub fn detect_concurrency(engine: &dyn BrainEngine) -> SyncConcurrency {
    match engine.kind() {
        EngineKind::Postgres => SyncConcurrency::Parallel(DEFAULT_POSTGRES_PARALLELISM),
        EngineKind::Libsql | EngineKind::InMemory => SyncConcurrency::Serial,
    }
}

/// Determine concurrency strategy with an optional user override.
///
/// If `user_parallelism` is `Some(n)` with `n > 1`, uses `Parallel(n)`
/// regardless of engine type. If `Some(1)`, uses `Serial`.
/// If `None`, auto-detects based on engine type.
pub fn detect_concurrency_with_override(
    engine: &dyn BrainEngine,
    user_parallelism: Option<usize>,
) -> SyncConcurrency {
    match user_parallelism {
        Some(0 | 1) => SyncConcurrency::Serial,
        Some(n) => SyncConcurrency::Parallel(n),
        None => detect_concurrency(engine),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    fn test_engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::default())
    }

    #[test]
    fn in_memory_is_serial() {
        let engine = test_engine();
        assert_eq!(detect_concurrency(&*engine), SyncConcurrency::Serial);
    }

    #[test]
    fn max_concurrency_serial_is_1() {
        assert_eq!(SyncConcurrency::Serial.max_concurrency(), 1);
    }

    #[test]
    fn max_concurrency_parallel() {
        assert_eq!(SyncConcurrency::Parallel(4).max_concurrency(), 4);
        assert_eq!(SyncConcurrency::Parallel(16).max_concurrency(), 16);
    }

    #[test]
    fn override_serial() {
        let engine = test_engine();
        let strategy = detect_concurrency_with_override(&*engine, Some(1));
        assert_eq!(strategy, SyncConcurrency::Serial);
    }

    #[test]
    fn override_parallel() {
        let engine = test_engine();
        let strategy = detect_concurrency_with_override(&*engine, Some(4));
        assert_eq!(strategy, SyncConcurrency::Parallel(4));
    }

    #[test]
    fn override_zero_is_serial() {
        let engine = test_engine();
        let strategy = detect_concurrency_with_override(&*engine, Some(0));
        assert_eq!(strategy, SyncConcurrency::Serial);
    }

    #[test]
    fn no_override_uses_auto_detect() {
        let engine = test_engine();
        let strategy = detect_concurrency_with_override(&*engine, None);
        // InMemory → Serial
        assert_eq!(strategy, SyncConcurrency::Serial);
    }
}
