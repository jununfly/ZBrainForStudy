//! Minion worker — handler contract layer (roadmap 1-2-1).
//!
//! This is the pure contract surface a minion worker hands to a job handler:
//! the [`MinionHandler`] trait, the [`MinionJobContext`] passed into each
//! `handle` call, and the [`MinionWorkerOpts`] tuning knobs. It deliberately
//! contains no polling loop, no concurrency scheduler, and no supervisor — those
//! are the C/D layers (1-2-2 .. 1-2-5). Keeping the contract in its own module
//! lets handlers and tests depend on the shape without pulling in the runtime.
//!
//! ## TS reference
//!
//! - `MinionHandler` — `src/core/minions/types.ts` L220
//!   (`type MinionHandler = (job: MinionJobContext) => Promise<unknown>`).
//! - `MinionJobContext` — `types.ts` L196-218.
//! - `MinionWorkerOpts` — `types.ts` L156-192.
//! - Context construction + capability wiring — `src/core/minions/worker.ts`
//!   L690-722 (the worker builds one context per claimed job and delegates the
//!   five async capabilities to the queue / engine).
//!
//! ## Object safety
//!
//! [`MinionHandler`] is object-safe: a worker stores handlers as
//! `Arc<dyn MinionHandler>` keyed by job name in a registry map, exactly as the
//! TS worker keeps a `Map<string, MinionHandler>`. `#[async_trait]` erases the
//! returned future so the trait stays dyn-compatible.
//!
//! ## Cancellation model (two independent signals)
//!
//! The TS context carries two distinct `AbortSignal`s: `signal` (per-job:
//! timeout / cancel / pause / lock-loss) and `shutdownSignal` (process
//! SIGTERM/SIGINT). We model each as an independent [`CancellationToken`].
//! Cancelling one must never cancel the other — the worker fires `signal` when
//! a single job must abort, but `shutdown` only when the whole process is going
//! down. A handler doing cleanup on deploy restarts listens to both.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::engine::BrainEngine;
use crate::minions::types::{InboxMessage, TokenUpdate};
use crate::Result;

/// A registered job handler. The worker looks one up by job name and calls
/// [`handle`](MinionHandler::handle) with a freshly built [`MinionJobContext`]
/// for the claimed job. Mirrors the TS `MinionHandler` function type.
///
/// Object-safe by design: workers store these as `Arc<dyn MinionHandler>`.
#[async_trait]
pub trait MinionHandler: Send + Sync {
    /// Run the job. The returned JSON becomes the job's `result` on success;
    /// an `Err` drives the fail/retry path. Mirrors the TS handler's
    /// `Promise<unknown>` — a resolved value completes the job, a throw fails it.
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value>;
}

/// Per-job context handed to a [`MinionHandler`]. Faithfully maps the TS
/// `MinionJobContext` object (`types.ts` L196-218): read-only job snapshot
/// fields plus five async capabilities that delegate back to the engine so the
/// handler can report progress, accumulate tokens, log, poll liveness, and read
/// its inbox — all token-fenced by the job's active lease.
///
/// The engine handle + `job_id` + `lock_token` are the fence: every capability
/// call carries them so a stale handler (lock lost / job reclaimed) becomes a
/// no-op at the engine layer rather than corrupting another worker's job.
pub struct MinionJobContext {
    /// Job row id. Read-only snapshot (TS `id`).
    pub id: i64,
    /// Job type name. Read-only snapshot (TS `name`).
    pub name: String,
    /// Job payload. Read-only snapshot (TS `data`).
    pub data: Value,
    /// Attempt counter at claim time. Read-only snapshot (TS `attempts_made`).
    pub attempts_made: i32,

    /// Per-job cancellation (timeout / cancel / pause / lock-loss). Maps the TS
    /// `signal: AbortSignal`. Independent from [`shutdown`](Self::shutdown).
    pub signal: CancellationToken,
    /// Process-shutdown cancellation (SIGTERM/SIGINT). Maps the TS
    /// `shutdownSignal`. Independent from [`signal`](Self::signal); most
    /// handlers ignore it.
    pub shutdown: CancellationToken,

    /// Engine handle backing the five capabilities. Not exposed to handlers
    /// directly — they go through the methods below so the lease fence is
    /// always applied.
    engine: Arc<dyn BrainEngine>,
    /// Lease token proving this context currently owns the job. Passed to every
    /// capability call for token-fencing.
    lock_token: String,
}

impl MinionJobContext {
    /// Build a context for a claimed job. Called by the worker (1-2-2) once per
    /// claim; `signal`/`shutdown` are the worker's per-job and process tokens.
    #[must_use]
    pub fn new(
        engine: Arc<dyn BrainEngine>,
        id: i64,
        name: String,
        data: Value,
        attempts_made: i32,
        lock_token: String,
        signal: CancellationToken,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            id,
            name,
            data,
            attempts_made,
            signal,
            shutdown,
            engine,
            lock_token,
        }
    }

    /// Access the engine backing this context. Read-only; handlers that need to
    /// call engine methods directly (e.g. building brain tools for subagent
    /// loops) can obtain the handle here. Lease-fenced capabilities
    /// (progress/tokens/log/is_active/read_inbox) should go through the typed
    /// methods below.
    #[must_use]
    pub fn engine(&self) -> &Arc<dyn BrainEngine> {
        &self.engine
    }

    /// Update structured progress on the job. Delegates to
    /// [`BrainEngine::update_progress`], token-fenced by the lease. Mirrors the
    /// TS `context.updateProgress` (`worker.ts` L697-699).
    pub async fn update_progress(&self, progress: &Value) -> Result<bool> {
        self.engine
            .update_progress(self.id, &self.lock_token, progress)
            .await
    }

    /// Accumulate token usage on the job. Delegates to
    /// [`BrainEngine::update_tokens`]. Mirrors TS `context.updateTokens`
    /// (`worker.ts` L700-702).
    pub async fn update_tokens(&self, tokens: &TokenUpdate) -> Result<bool> {
        self.engine
            .update_tokens(self.id, &self.lock_token, tokens)
            .await
    }

    /// Append one entry to the job's `stacktrace` log array. Delegates to
    /// [`BrainEngine::append_log`]. Mirrors TS `context.log` (`worker.ts`
    /// L703-711), which the worker implemented with inline `executeRaw`; the
    /// Rust port routes through a dedicated trait method so zbrain-core has no
    /// raw-SQL escape hatch.
    pub async fn log(&self, entry: &str) -> Result<bool> {
        self.engine
            .append_log(self.id, &self.lock_token, entry)
            .await
    }

    /// Whether the job is still actively leased by this context (lock not lost).
    /// Delegates to [`BrainEngine::is_job_active`]. Mirrors TS
    /// `context.isActive` (`worker.ts` L712-718).
    pub async fn is_active(&self) -> Result<bool> {
        self.engine.is_job_active(self.id, &self.lock_token).await
    }

    /// Read and consume unread inbox messages for the job (marks read).
    /// Delegates to [`BrainEngine::read_inbox`]. Mirrors TS `context.readInbox`
    /// (`worker.ts` L719-721).
    pub async fn read_inbox(&self) -> Result<Vec<InboxMessage>> {
        self.engine.read_inbox(self.id, &self.lock_token).await
    }
}

/// Worker tuning knobs. 1:1 with the TS `MinionWorkerOpts` (`types.ts`
/// L156-192). [`Default`] reproduces the TS per-field defaults so a bare
/// `MinionWorkerOpts::default()` behaves like a TS worker constructed with `{}`.
///
/// Fields the TS type expresses as optional-with-default are non-`Option` here
/// (the default *is* the documented value); genuinely optional injection points
/// (`max_rss_mb`) stay `Option`. `get_rss` (a TS function injection point for
/// deterministic RSS in tests) is a C-layer concern (1-2-4) and is not part of
/// this contract struct — it will be wired as a worker field, not an opt.
#[derive(Debug, Clone, PartialEq)]
pub struct MinionWorkerOpts {
    /// Queue name to pull from. TS default `"default"`.
    pub queue: String,
    /// Max concurrent in-flight jobs. TS default `1`.
    pub concurrency: u32,
    /// Lease duration in ms. TS default `30000`.
    pub lock_duration_ms: i64,
    /// Stall sweep interval in ms. TS default `30000`.
    pub stalled_interval_ms: i64,
    /// Max stall requeues before dead-letter. TS default `1`.
    pub max_stalled_count: i32,
    /// Poll interval in ms (PGLite fallback). TS default `5000`.
    pub poll_interval_ms: i64,
    /// RSS threshold in MB before graceful shutdown. `0`/`None` = disabled.
    /// TS `maxRssMb?` (undefined = disabled).
    pub max_rss_mb: Option<u64>,
    /// Periodic RSS check interval in ms. TS default `60000`.
    pub rss_check_interval_ms: i64,
    /// Self-health-check interval in ms. `0` = disabled. TS default `60000`.
    pub health_check_interval_ms: i64,
    /// Idle ms before the first stall warning. TS default `300000`.
    pub stall_warn_after_ms: i64,
    /// Idle ms before emitting `unhealthy(stalled)`. TS default `600000`.
    pub stall_exit_after_ms: i64,
    /// Consecutive failed DB probes before `unhealthy(db_dead)`. TS default `3`.
    pub db_fail_exit_after: u32,
    /// Per-probe wall-clock timeout in ms. TS default `10000`.
    pub db_probe_timeout_ms: i64,
}

impl Default for MinionWorkerOpts {
    fn default() -> Self {
        Self {
            queue: "default".to_string(),
            concurrency: 1,
            lock_duration_ms: 30_000,
            stalled_interval_ms: 30_000,
            max_stalled_count: 1,
            poll_interval_ms: 5_000,
            max_rss_mb: None,
            rss_check_interval_ms: 60_000,
            health_check_interval_ms: 60_000,
            stall_warn_after_ms: 300_000,
            stall_exit_after_ms: 600_000,
            db_fail_exit_after: 3,
            db_probe_timeout_ms: 10_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minions::types::MinionJobInput;
    use crate::InMemoryEngine;
    use serde_json::json;

    /// Enqueue one job then claim it, returning an engine handle plus a context
    /// wired to the claimed job's real id + lock token. This is the shared
    /// fixture for the delegation behaviors: the context's capabilities are
    /// token-fenced, so they only succeed against a genuinely active lease.
    async fn active_ctx(name: &str, data: Value) -> (Arc<dyn BrainEngine>, MinionJobContext) {
        let engine: Arc<dyn BrainEngine> = Arc::new(InMemoryEngine::new());
        engine
            .enqueue_job(&MinionJobInput {
                name: name.to_string(),
                data: Some(data.clone()),
                ..Default::default()
            })
            .await
            .expect("enqueue");
        let claimed = engine
            .claim_job("tok-ctx", 30_000, "default", &[name.to_string()])
            .await
            .expect("claim")
            .expect("a job to claim");
        let ctx = MinionJobContext::new(
            Arc::clone(&engine),
            claimed.id,
            claimed.name.clone(),
            claimed.data.clone(),
            claimed.attempts_made,
            claimed.lock_token.clone().expect("claimed job has lock token"),
            CancellationToken::new(),
            CancellationToken::new(),
        );
        (engine, ctx)
    }

    // behavior 1 --------------------------------------------------------------
    #[tokio::test]
    async fn context_exposes_readonly_job_snapshot() {
        let (_engine, ctx) = active_ctx("build", json!({"target": "release"})).await;
        assert_eq!(ctx.name, "build");
        assert_eq!(ctx.data, json!({"target": "release"}));
        // Snapshot faithfully mirrors the claimed job. A freshly claimed job has
        // not recorded a completed attempt yet, so attempts_made is 0 (claim
        // bumps attempts_started, not attempts_made).
        assert_eq!(ctx.attempts_made, 0);
        assert!(ctx.id > 0);
    }

    // behavior 2 --------------------------------------------------------------
    #[tokio::test]
    async fn signal_and_shutdown_are_independent_tokens() {
        let (_engine, ctx) = active_ctx("x", Value::Null).await;
        assert!(!ctx.signal.is_cancelled());
        assert!(!ctx.shutdown.is_cancelled());

        ctx.signal.cancel();
        assert!(ctx.signal.is_cancelled(), "signal should be cancelled");
        assert!(
            !ctx.shutdown.is_cancelled(),
            "cancelling signal must not cancel shutdown"
        );

        let (_e2, ctx2) = active_ctx("y", Value::Null).await;
        ctx2.shutdown.cancel();
        assert!(ctx2.shutdown.is_cancelled());
        assert!(
            !ctx2.signal.is_cancelled(),
            "cancelling shutdown must not cancel signal"
        );
    }

    // behavior 3 --------------------------------------------------------------
    #[tokio::test]
    async fn handler_is_object_safe_in_a_registry_map() {
        use std::collections::HashMap;

        struct Echo;
        #[async_trait]
        impl MinionHandler for Echo {
            async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
                Ok(json!({"echoed": ctx.name.clone()}))
            }
        }

        let mut registry: HashMap<String, Arc<dyn MinionHandler>> = HashMap::new();
        registry.insert("build".to_string(), Arc::new(Echo));

        let (_engine, ctx) = active_ctx("build", Value::Null).await;
        let handler = registry.get("build").expect("handler registered");
        let out = handler.handle(&ctx).await.expect("handle ok");
        assert_eq!(out, json!({"echoed": "build"}));
    }

    // behavior 4 --------------------------------------------------------------
    #[tokio::test]
    async fn update_progress_delegates_to_engine() {
        let (engine, ctx) = active_ctx("p", Value::Null).await;
        let applied = ctx
            .update_progress(&json!({"pct": 42}))
            .await
            .expect("update_progress");
        assert!(applied, "token-fenced update should apply on active lease");

        let stored = engine.get_job(ctx.id).await.expect("get_job").expect("job");
        assert_eq!(stored.progress, Some(json!({"pct": 42})));
    }

    // behavior 5 --------------------------------------------------------------
    #[tokio::test]
    async fn update_tokens_delegates_and_accumulates() {
        let (engine, ctx) = active_ctx("t", Value::Null).await;
        ctx.update_tokens(&TokenUpdate {
            input: Some(10),
            output: Some(5),
            cache_read: None,
        })
        .await
        .expect("first bump");
        ctx.update_tokens(&TokenUpdate {
            input: Some(3),
            output: None,
            cache_read: Some(7),
        })
        .await
        .expect("second bump");

        let stored = engine.get_job(ctx.id).await.expect("get_job").expect("job");
        assert_eq!(stored.tokens_input, 13);
        assert_eq!(stored.tokens_output, 5);
        assert_eq!(stored.tokens_cache_read, 7);
    }

    // behavior 6 --------------------------------------------------------------
    #[tokio::test]
    async fn log_appends_to_stacktrace() {
        let (engine, ctx) = active_ctx("l", Value::Null).await;
        ctx.log("step one").await.expect("log 1");
        ctx.log("step two").await.expect("log 2");

        let stored = engine.get_job(ctx.id).await.expect("get_job").expect("job");
        assert_eq!(stored.stacktrace, vec!["step one", "step two"]);
    }

    // behavior 7 --------------------------------------------------------------
    #[tokio::test]
    async fn is_active_reflects_lease_state() {
        let (engine, ctx) = active_ctx("a", Value::Null).await;
        assert!(ctx.is_active().await.expect("is_active while leased"));

        // Complete the job (token-fenced) -> lease gone -> is_active false.
        engine
            .complete_job(ctx.id, "tok-ctx", Some(&json!({"ok": true})))
            .await
            .expect("complete");
        assert!(
            !ctx.is_active().await.expect("is_active after complete"),
            "is_active must be false once the job is no longer actively leased"
        );
    }

    // behavior 8 --------------------------------------------------------------
    #[test]
    fn worker_opts_default_matches_ts() {
        let o = MinionWorkerOpts::default();
        assert_eq!(o.queue, "default");
        assert_eq!(o.concurrency, 1);
        assert_eq!(o.lock_duration_ms, 30_000);
        assert_eq!(o.stalled_interval_ms, 30_000);
        assert_eq!(o.max_stalled_count, 1);
        assert_eq!(o.poll_interval_ms, 5_000);
        assert_eq!(o.max_rss_mb, None);
        assert_eq!(o.rss_check_interval_ms, 60_000);
        assert_eq!(o.health_check_interval_ms, 60_000);
        assert_eq!(o.stall_warn_after_ms, 300_000);
        assert_eq!(o.stall_exit_after_ms, 600_000);
        assert_eq!(o.db_fail_exit_after, 3);
        assert_eq!(o.db_probe_timeout_ms, 10_000);
    }
}