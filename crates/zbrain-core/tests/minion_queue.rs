//! Phase 9 (slice 1-1-1 A+B): Minion job queue integration tests.
//!
//! Exercises the backend-agnostic queue contract (enqueue/get/claim/complete/
//! fail/retry/renew) against all three backends. Backend-agnostic `contract_*`
//! functions run once per backend so InMemory, Libsql, and Postgres are held to
//! the same behavior. The `minion_jobs` table has no FK to `sources` and A+B
//! never sets `parent_job_id`, so no seeding is required.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use tempfile::NamedTempFile;
use zbrain_core::engine::{BrainEngine, EngineConfig};
use zbrain_core::libsql::LibsqlEngine;
use zbrain_core::minions::types::{
    AttachmentInput, ChildOutcome, FailOutcome, JobFilters, MinionJobInput, MinionJobStatus,
    TokenUpdate,
};
use zbrain_core::minions::MinionQueue;
use zbrain_core::InMemoryEngine;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn job(name: &str) -> MinionJobInput {
    MinionJobInput {
        name: name.to_string(),
        ..Default::default()
    }
}

/// A child job spawned under `parent_id`. Used by the D-layer (1-1-3-1)
/// parent/child coordination contract tests.
fn child(name: &str, parent_id: i64) -> MinionJobInput {
    MinionJobInput {
        name: name.to_string(),
        parent_job_id: Some(parent_id),
        ..Default::default()
    }
}

async fn init_clean_libsql() -> (LibsqlEngine, NamedTempFile) {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let engine = LibsqlEngine::new();
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };
    engine.connect(&cfg).await.expect("connect");
    engine.init_schema().await.expect("init_schema");
    (engine, path)
}

// ---------------------------------------------------------------------------
// Backend-agnostic contract functions
// ---------------------------------------------------------------------------

async fn contract_enqueue_defaults(engine: &dyn BrainEngine) {
    let created = engine.enqueue_job(&job("build")).await.unwrap();
    assert_eq!(created.name, "build");
    assert_eq!(created.queue, "default");
    assert_eq!(created.status, MinionJobStatus::Waiting);
    assert_eq!(created.max_attempts, 3);
    assert_eq!(created.max_stalled, 5);
    assert_eq!(created.backoff_delay, 1000);
    assert!((created.backoff_jitter - 0.2).abs() < 1e-9);
    assert!(created.lock_token.is_none());

    let fetched = engine.get_job(created.id).await.unwrap().unwrap();
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "build");
}

async fn contract_idempotency(engine: &dyn BrainEngine) {
    let input = MinionJobInput {
        idempotency_key: Some("k1".to_string()),
        ..job("once")
    };
    let a = engine.enqueue_job(&input).await.unwrap();
    let b = engine.enqueue_job(&input).await.unwrap();
    assert_eq!(a.id, b.id, "same key returns the same row");

    let all = engine
        .get_jobs(&JobFilters {
            name: Some("once".to_string()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "no second row inserted");
}

async fn contract_delay_sets_delayed(engine: &dyn BrainEngine) {
    let before = zbrain_core::time::now_epoch_ms();
    let j = engine
        .enqueue_job(&MinionJobInput {
            delay: Some(60_000),
            ..job("later")
        })
        .await
        .unwrap();
    assert_eq!(j.status, MinionJobStatus::Delayed);
    assert!(j.delay_until.unwrap() >= before + 60_000);
}

async fn contract_claim_priority_and_exclusive(engine: &dyn BrainEngine) {
    engine
        .enqueue_job(&MinionJobInput {
            priority: Some(5),
            ..job("worker")
        })
        .await
        .unwrap();
    let hot = engine
        .enqueue_job(&MinionJobInput {
            priority: Some(0),
            ..job("worker")
        })
        .await
        .unwrap();

    let names = vec!["worker".to_string()];
    let first = engine
        .claim_job("tok-1", 30_000, "default", &names)
        .await
        .unwrap()
        .expect("claimable");
    assert_eq!(first.id, hot.id, "priority 0 before priority 5");
    assert_eq!(first.status, MinionJobStatus::Active);
    assert_eq!(first.lock_token.as_deref(), Some("tok-1"));
    assert_eq!(first.attempts_started, 1);
    assert!(first.started_at.is_some());
    assert!(first.lock_until.is_some());

    let second = engine
        .claim_job("tok-2", 30_000, "default", &names)
        .await
        .unwrap()
        .expect("second waiting job");
    assert_ne!(second.id, first.id);

    assert!(engine
        .claim_job("tok-3", 30_000, "default", &names)
        .await
        .unwrap()
        .is_none());
}

async fn contract_claim_filters(engine: &dyn BrainEngine) {
    engine
        .enqueue_job(&MinionJobInput {
            queue: Some("shell".to_string()),
            ..job("run")
        })
        .await
        .unwrap();

    // Wrong queue / unregistered name / empty names all claim nothing.
    assert!(engine
        .claim_job("t", 1000, "default", &["run".to_string()])
        .await
        .unwrap()
        .is_none());
    assert!(engine
        .claim_job("t", 1000, "shell", &["other".to_string()])
        .await
        .unwrap()
        .is_none());
    assert!(engine
        .claim_job("t", 1000, "shell", &[])
        .await
        .unwrap()
        .is_none());
    // Correct queue + name succeeds.
    assert!(engine
        .claim_job("t", 1000, "shell", &["run".to_string()])
        .await
        .unwrap()
        .is_some());
}

async fn contract_complete_token_fence(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("good", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    // Wrong token -> None, still active.
    assert!(engine
        .complete_job(claimed.id, "bad", None)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        engine.get_job(claimed.id).await.unwrap().unwrap().status,
        MinionJobStatus::Active
    );

    // Right token -> completed with result.
    let done = engine
        .complete_job(claimed.id, "good", Some(&serde_json::json!({"ok": true})))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, MinionJobStatus::Completed);
    assert_eq!(done.result, Some(serde_json::json!({"ok": true})));
    assert!(done.finished_at.is_some());
    assert!(done.lock_token.is_none());
}

async fn contract_fail_delayed_then_retry(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    let before = zbrain_core::time::now_epoch_ms();
    let failed = engine
        .fail_job(claimed.id, "tok", "boom", FailOutcome::Delayed, 5_000)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, MinionJobStatus::Delayed);
    assert_eq!(failed.attempts_made, 1);
    assert_eq!(failed.error_text.as_deref(), Some("boom"));
    assert_eq!(failed.stacktrace, vec!["boom".to_string()]);
    assert!(failed.finished_at.is_none());
    assert!(failed.delay_until.unwrap() >= before + 5_000);
}

async fn contract_fail_terminal_then_retry(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let failed = engine
        .fail_job(claimed.id, "tok", "nope", FailOutcome::Failed, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, MinionJobStatus::Failed);
    assert!(failed.finished_at.is_some());

    let requeued = engine
        .retry_job(claimed.id)
        .await
        .unwrap()
        .expect("failed job is retryable");
    assert_eq!(requeued.status, MinionJobStatus::Waiting);
    assert!(requeued.error_text.is_none());
    assert!(requeued.finished_at.is_none());
    assert!(requeued.delay_until.is_none());
    assert!(requeued.lock_token.is_none());

    // A waiting job is not retryable.
    assert!(engine.retry_job(claimed.id).await.unwrap().is_none());
}

async fn contract_renew_lock(engine: &dyn BrainEngine) {
    engine.enqueue_job(&job("w")).await.unwrap();
    let names = vec!["w".to_string()];
    let claimed = engine
        .claim_job("tok", 1_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    assert!(!engine
        .renew_job_lock(claimed.id, "bad", 30_000)
        .await
        .unwrap());
    assert!(engine
        .renew_job_lock(claimed.id, "tok", 30_000)
        .await
        .unwrap());
    let renewed = engine.get_job(claimed.id).await.unwrap().unwrap();
    assert!(renewed.lock_until.unwrap() >= zbrain_core::time::now_epoch_ms() + 29_000);
}

// ---------------------------------------------------------------------------
// Background sweeps (1-1-2). Time-driven state machines tested WITHOUT sleeping
// (roadmap 1-1-2 decision 6): the scheduling columns are epoch-ms integers, so
// a job made eligible with a zero/negative delay or a negative lock duration is
// already "in the past" the moment it is written; `started_at` is forced into
// the past via `set_started_at_for_test`. No injectable clock — the sweeps read
// wall-clock `now` in SQL exactly as production does.
// ---------------------------------------------------------------------------

/// A delayed job whose `delay_until` is already <= now is promoted to waiting,
/// with delay/lock fields cleared. A future-delayed job is left untouched.
async fn contract_promote_delayed(engine: &dyn BrainEngine) {
    // backoff_ms = 0 => delay_until = now, so `delay_until <= now` holds.
    engine.enqueue_job(&job("due")).await.unwrap();
    let names = vec!["due".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let delayed = engine
        .fail_job(claimed.id, "tok", "retry", FailOutcome::Delayed, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delayed.status, MinionJobStatus::Delayed);

    // A second job delayed far into the future must NOT be promoted.
    let future = engine
        .enqueue_job(&MinionJobInput {
            delay: Some(3_600_000),
            ..job("future")
        })
        .await
        .unwrap();
    assert_eq!(future.status, MinionJobStatus::Delayed);

    let promoted = engine.promote_delayed().await.unwrap();
    assert_eq!(promoted.len(), 1, "only the due job is promoted");
    let p = &promoted[0];
    assert_eq!(p.id, delayed.id);
    assert_eq!(p.status, MinionJobStatus::Waiting);
    assert!(p.delay_until.is_none());
    assert!(p.lock_token.is_none());
    assert!(p.lock_until.is_none());

    // The future job is still delayed.
    assert_eq!(
        engine.get_job(future.id).await.unwrap().unwrap().status,
        MinionJobStatus::Delayed
    );
}

/// A stalled active job (lease expired) under its stall budget is requeued to
/// waiting with `stalled_counter` bumped; at/over budget it is dead-lettered.
async fn contract_handle_stalled(engine: &dyn BrainEngine) {
    // Two jobs. Claim each with a NEGATIVE lock duration so lock_until < now
    // immediately (the lease is "already expired").
    engine.enqueue_job(&job("s")).await.unwrap();
    engine
        .enqueue_job(&MinionJobInput {
            // max_stalled = 1 => stalled_counter(0) + 1 >= 1 => dead-lettered.
            max_stalled: Some(1),
            ..job("s")
        })
        .await
        .unwrap();
    let names = vec!["s".to_string()];

    let requeue_target = engine
        .claim_job("t1", -1, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let dead_target = engine
        .claim_job("t2", -1, "default", &names)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(requeue_target.max_stalled, 5);
    assert_eq!(dead_target.max_stalled, 1);

    let sweep = engine.handle_stalled().await.unwrap();
    assert_eq!(sweep.requeued.len(), 1, "one under-budget job requeued");
    assert_eq!(sweep.dead.len(), 1, "one at-budget job dead-lettered");

    let requeued = &sweep.requeued[0];
    assert_eq!(requeued.id, requeue_target.id);
    assert_eq!(requeued.status, MinionJobStatus::Waiting);
    assert_eq!(requeued.stalled_counter, 1);
    assert!(requeued.lock_token.is_none());
    assert!(requeued.lock_until.is_none());

    let dead = &sweep.dead[0];
    assert_eq!(dead.id, dead_target.id);
    assert_eq!(dead.status, MinionJobStatus::Dead);
    assert_eq!(dead.stalled_counter, 1);
    assert_eq!(dead.error_text.as_deref(), Some("max stalled count exceeded"));
    assert!(dead.finished_at.is_some());

    // Idempotent: nothing active-and-stalled remains.
    let again = engine.handle_stalled().await.unwrap();
    assert!(again.requeued.is_empty() && again.dead.is_empty());
}

/// An active job whose per-job `timeout_at` has passed while the lease is still
/// held is dead-lettered. A stalled job (lease expired) is left for
/// `handle_stalled`, not timed out here.
async fn contract_handle_timeouts(engine: &dyn BrainEngine) {
    // A positive timeout_ms satisfies the chk_minion_timeout_positive CHECK on
    // the SQL backends; we then force timeout_at into the past via the test
    // helper. Positive lock duration keeps lock_until > now (not stalled).
    engine
        .enqueue_job(&MinionJobInput {
            timeout_ms: Some(30_000),
            ..job("t")
        })
        .await
        .unwrap();
    let names = vec!["t".to_string()];
    let claimed = engine
        .claim_job("tok", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    engine
        .set_timeout_at_for_test(claimed.id, zbrain_core::time::now_epoch_ms() - 1)
        .await
        .unwrap();

    // A second job with no timeout_ms must be untouched.
    engine.enqueue_job(&job("t")).await.unwrap();
    let safe = engine
        .claim_job("tok2", 30_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    let timed_out = engine.handle_timeouts().await.unwrap();
    assert_eq!(timed_out.len(), 1, "only the expired-timeout job dies");
    let t = &timed_out[0];
    assert_eq!(t.id, claimed.id);
    assert_eq!(t.status, MinionJobStatus::Dead);
    assert_eq!(t.error_text.as_deref(), Some("timeout exceeded"));
    assert!(t.finished_at.is_some());
    assert!(t.lock_token.is_none());

    assert_eq!(
        engine.get_job(safe.id).await.unwrap().unwrap().status,
        MinionJobStatus::Active
    );

    // A stalled job (expired lease) is NOT swept by handle_timeouts. Give it a
    // past timeout_at too, so only the lease-held guard keeps it out.
    engine.enqueue_job(&job("t")).await.unwrap();
    let stalled = engine
        .claim_job("tok3", -1, "default", &names)
        .await
        .unwrap()
        .unwrap();
    engine
        .set_timeout_at_for_test(stalled.id, zbrain_core::time::now_epoch_ms() - 1)
        .await
        .unwrap();
    let none = engine.handle_timeouts().await.unwrap();
    assert!(
        none.iter().all(|j| j.id != stalled.id),
        "stalled job left for handle_stalled, not timed out"
    );
}

/// An active job whose wall-clock runtime exceeds the threshold is dead-lettered
/// regardless of lease state. We force `started_at` far into the past via the
/// test-only helper so the SQL `now() - started_at` arithmetic trips.
async fn contract_handle_wall_clock_timeouts(engine: &dyn BrainEngine) {
    // No timeout_ms => threshold = lock_duration_ms * 2 * GREATEST(max_stalled, 1).
    // With lock_duration_ms = 1000 and max_stalled = 5 (default) => 10_000 ms.
    engine.enqueue_job(&job("wc")).await.unwrap();
    let names = vec!["wc".to_string()];
    let claimed = engine
        .claim_job("tok", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    // Force started_at to 10 minutes ago (well past any threshold).
    let past = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339();
    engine
        .set_started_at_for_test(claimed.id, &past)
        .await
        .unwrap();

    // A second freshly-started job must survive the same sweep.
    engine.enqueue_job(&job("wc")).await.unwrap();
    let fresh = engine
        .claim_job("tok2", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    let dead = engine.handle_wall_clock_timeouts(1_000).await.unwrap();
    assert_eq!(dead.len(), 1, "only the long-running job dies");
    let d = &dead[0];
    assert_eq!(d.id, claimed.id);
    assert_eq!(d.status, MinionJobStatus::Dead);
    assert_eq!(
        d.error_text.as_deref(),
        Some("wall-clock timeout exceeded")
    );
    assert!(d.finished_at.is_some());
    assert!(d.lock_token.is_none());

    assert_eq!(
        engine.get_job(fresh.id).await.unwrap().unwrap().status,
        MinionJobStatus::Active
    );
}

// ---------------------------------------------------------------------------
// D-layer (1-1-3-1): parent/child dependencies + inbox coordination
// ---------------------------------------------------------------------------

/// Claim + complete a job by name, returning the completed row. Convenience for
/// driving child jobs through their lifecycle in dependency tests.
async fn claim_and_complete(engine: &dyn BrainEngine, name: &str, tok: &str) -> i64 {
    let names = vec![name.to_string()];
    let claimed = engine
        .claim_job(tok, 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let done = engine
        .complete_job(claimed.id, tok, Some(&serde_json::json!({"ok": true})))
        .await
        .unwrap()
        .unwrap();
    done.id
}

/// Spawning a child flips the parent into `waiting-children`; the child records
/// its parent link and derived depth.
async fn contract_spawn_child_blocks_parent(engine: &dyn BrainEngine) {
    let parent = engine.enqueue_job(&job("parent")).await.unwrap();
    assert_eq!(parent.status, MinionJobStatus::Waiting);
    assert_eq!(parent.depth, 0);

    let kid = engine.enqueue_job(&child("kid", parent.id)).await.unwrap();
    assert_eq!(kid.parent_job_id, Some(parent.id));
    assert_eq!(kid.depth, 1, "child depth is parent.depth + 1");

    let parent_now = engine.get_job(parent.id).await.unwrap().unwrap();
    assert_eq!(
        parent_now.status,
        MinionJobStatus::WaitingChildren,
        "parent blocks on its child"
    );
}

/// Completing the last live child flips the parent back to `waiting` and posts a
/// `child_done` (outcome=complete) into the parent's inbox.
async fn contract_child_complete_resolves_parent(engine: &dyn BrainEngine) {
    let parent = engine.enqueue_job(&job("agg")).await.unwrap();
    let kid = engine.enqueue_job(&child("kid", parent.id)).await.unwrap();
    assert_eq!(
        engine.get_job(parent.id).await.unwrap().unwrap().status,
        MinionJobStatus::WaitingChildren
    );

    let done_id = claim_and_complete(engine, "kid", "ktok").await;
    assert_eq!(done_id, kid.id);

    let parent_now = engine.get_job(parent.id).await.unwrap().unwrap();
    assert_eq!(
        parent_now.status,
        MinionJobStatus::Waiting,
        "parent unblocks once its only child is terminal"
    );

    // Parent now claims and reads its child_done inbox.
    let names = vec!["agg".to_string()];
    let claimed = engine
        .claim_job("ptok", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let comps = engine
        .read_child_completions(claimed.id, "ptok", None)
        .await
        .unwrap();
    assert_eq!(comps.len(), 1, "one child_done envelope");
    assert_eq!(comps[0].child_id, kid.id);
    assert_eq!(comps[0].job_name, "kid");
    assert_eq!(comps[0].effective_outcome(), ChildOutcome::Complete);
}

/// A child failing terminally with the default `fail_parent` policy marks the
/// parent `failed` and still emits a `child_done` (outcome=failed) first.
async fn contract_child_fail_propagates_to_parent(engine: &dyn BrainEngine) {
    let parent = engine.enqueue_job(&job("fp")).await.unwrap();
    let kid = engine.enqueue_job(&child("kid", parent.id)).await.unwrap();

    let names = vec!["kid".to_string()];
    let claimed = engine
        .claim_job("ktok", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let failed = engine
        .fail_job(claimed.id, "ktok", "boom", FailOutcome::Failed, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, MinionJobStatus::Failed);

    let parent_now = engine.get_job(parent.id).await.unwrap().unwrap();
    assert_eq!(
        parent_now.status,
        MinionJobStatus::Failed,
        "fail_parent policy fails the parent"
    );
    assert!(parent_now
        .error_text
        .as_deref()
        .unwrap_or("")
        .contains(&kid.id.to_string()));

    // The child_done(failed) envelope survives in the parent inbox even though
    // the parent is now terminal (it was inserted before the parent flip).
    let comps = engine.read_child_completions(parent.id, "nope", None).await;
    // Parent isn't active/locked, so token fence returns empty — assert the fence
    // holds rather than the payload here.
    assert!(comps.unwrap().is_empty(), "token fence blocks unlocked read");
}

/// `cancel_job` cascades to the whole descendant subtree and emits
/// `child_done(cancelled)` to affected parents.
async fn contract_cancel_cascades_subtree(engine: &dyn BrainEngine) {
    let root = engine.enqueue_job(&job("root")).await.unwrap();
    let mid = engine.enqueue_job(&child("mid", root.id)).await.unwrap();
    let leaf = engine.enqueue_job(&child("leaf", mid.id)).await.unwrap();

    let cancelled = engine.cancel_job(root.id).await.unwrap().unwrap();
    assert_eq!(cancelled.id, root.id);
    assert_eq!(cancelled.status, MinionJobStatus::Cancelled);

    for id in [root.id, mid.id, leaf.id] {
        assert_eq!(
            engine.get_job(id).await.unwrap().unwrap().status,
            MinionJobStatus::Cancelled,
            "descendant {id} cancelled"
        );
    }

    // Cancelling an already-terminal job returns None.
    assert!(engine.cancel_job(root.id).await.unwrap().is_none());
}

/// Inbox `send_message` enforces sender validation and non-terminal target;
/// `read_inbox` is token-fenced and consumes (marks read).
async fn contract_inbox_send_and_read(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("box")).await.unwrap();

    // admin may message a non-terminal job.
    let msg = engine
        .send_message(j.id, &serde_json::json!({"cmd": "pause"}), "admin")
        .await
        .unwrap();
    assert!(msg.is_some(), "admin send accepted");

    // A bogus sender is rejected.
    let bogus = engine
        .send_message(j.id, &serde_json::json!({"x": 1}), "randogremlin")
        .await
        .unwrap();
    assert!(bogus.is_none(), "non-admin/non-parent sender rejected");

    // read_inbox is token-fenced: wrong/absent lock returns empty.
    assert!(engine.read_inbox(j.id, "wrongtok").await.unwrap().is_empty());

    // Claim to hold the lease, then read.
    let names = vec!["box".to_string()];
    let claimed = engine
        .claim_job("boxtok", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();
    let read = engine.read_inbox(claimed.id, "boxtok").await.unwrap();
    assert_eq!(read.len(), 1, "one unread message");
    assert_eq!(read[0].sender, "admin");

    // Second read finds nothing (first read marked it consumed).
    let read2 = engine.read_inbox(claimed.id, "boxtok").await.unwrap();
    assert!(read2.is_empty(), "message consumed on first read");
}

/// `update_tokens` accumulates counters under the active lease fence.
async fn contract_update_tokens_fenced(engine: &dyn BrainEngine) {
    let j = engine.enqueue_job(&job("tok")).await.unwrap();
    let names = vec!["tok".to_string()];
    let claimed = engine
        .claim_job("tt", 60_000, "default", &names)
        .await
        .unwrap()
        .unwrap();

    // Wrong token: no update.
    assert!(!engine
        .update_tokens(
            claimed.id,
            "badtok",
            &TokenUpdate {
                input: Some(5),
                ..Default::default()
            }
        )
        .await
        .unwrap());

    let ok = engine
        .update_tokens(
            claimed.id,
            "tt",
            &TokenUpdate {
                input: Some(10),
                output: Some(3),
                cache_read: Some(1),
            },
        )
        .await
        .unwrap();
    assert!(ok);

    // Accumulate again.
    engine
        .update_tokens(
            claimed.id,
            "tt",
            &TokenUpdate {
                input: Some(5),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let after = engine.get_job(claimed.id).await.unwrap().unwrap();
    assert_eq!(after.tokens_input, 15);
    assert_eq!(after.tokens_output, 3);
    assert_eq!(after.tokens_cache_read, 1);
}

// --- Attachments (1-1-3-2) ---------------------------------------------------

/// Build an [`AttachmentInput`] with base64-encoded `content` bytes.
fn attachment(filename: &str, content_type: &str, content: &[u8]) -> AttachmentInput {
    AttachmentInput {
        filename: filename.to_string(),
        content_type: content_type.to_string(),
        content_base64: BASE64.encode(content),
    }
}

/// add → list → get bytes round-trip → delete. Exercises the full facade
/// orchestration (validation + backend INSERT/SELECT/DELETE) and asserts the
/// bytes + sha256 survive the round-trip intact.
async fn contract_attachment_crud_round_trip(engine: &dyn BrainEngine) {
    let jid = engine.enqueue_job(&job("host")).await.unwrap().id;
    let q = MinionQueue::new(engine);

    // Empty list to start.
    assert!(q.list_attachments(jid).await.unwrap().is_empty());

    // Add two attachments.
    let payload_a = b"hello world";
    let meta_a = q
        .add_attachment(jid, &attachment("a.txt", "text/plain", payload_a))
        .await
        .unwrap();
    assert_eq!(meta_a.job_id, jid);
    assert_eq!(meta_a.filename, "a.txt");
    assert_eq!(meta_a.content_type, "text/plain");
    assert_eq!(meta_a.size_bytes, payload_a.len() as i64);
    assert_eq!(
        meta_a.sha256,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
    // storage_uri is always None for this port (inline content only).
    assert_eq!(meta_a.storage_uri, None);

    let payload_b = &[0u8, 159, 146, 150, 255, 1, 2, 3]; // arbitrary binary
    q.add_attachment(jid, &attachment("b.bin", "application/octet-stream", payload_b))
        .await
        .unwrap();

    // List returns both, metadata only, ordered created_at ASC, id ASC.
    let listed = q.list_attachments(jid).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].filename, "a.txt");
    assert_eq!(listed[1].filename, "b.bin");

    // Get bytes back exactly for both.
    let (got_a_meta, got_a_bytes) = q.get_attachment(jid, "a.txt").await.unwrap().unwrap();
    assert_eq!(got_a_meta.id, meta_a.id);
    assert_eq!(got_a_bytes, payload_a);
    let (_, got_b_bytes) = q.get_attachment(jid, "b.bin").await.unwrap().unwrap();
    assert_eq!(got_b_bytes, payload_b);

    // Missing filename → None.
    assert!(q.get_attachment(jid, "nope.txt").await.unwrap().is_none());

    // Delete a.txt → true, then gone; deleting again → false.
    assert!(q.delete_attachment(jid, "a.txt").await.unwrap());
    assert!(q.get_attachment(jid, "a.txt").await.unwrap().is_none());
    assert!(!q.delete_attachment(jid, "a.txt").await.unwrap());
    let remaining = q.list_attachments(jid).await.unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].filename, "b.bin");
}

/// A duplicate (job_id, filename) is rejected — the facade's existing-filename
/// early-out fires before the round-trip.
async fn contract_attachment_duplicate_rejected(engine: &dyn BrainEngine) {
    let jid = engine.enqueue_job(&job("host")).await.unwrap().id;
    let q = MinionQueue::new(engine);

    q.add_attachment(jid, &attachment("dup.txt", "text/plain", b"one"))
        .await
        .unwrap();
    let err = q
        .add_attachment(jid, &attachment("dup.txt", "text/plain", b"two"))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("already exists"),
        "expected duplicate error, got: {}",
        err.message
    );

    // Same filename under a DIFFERENT job is fine (dedupe is per-job).
    let jid2 = engine.enqueue_job(&job("host2")).await.unwrap().id;
    q.add_attachment(jid2, &attachment("dup.txt", "text/plain", b"three"))
        .await
        .unwrap();
}

/// Attaching to a nonexistent job → NotFound `job N not found`.
async fn contract_attachment_job_not_found(engine: &dyn BrainEngine) {
    let q = MinionQueue::new(engine);
    let err = q
        .add_attachment(999_999, &attachment("x.txt", "text/plain", b"x"))
        .await
        .unwrap_err();
    assert_eq!(err.class, "NotFound");
    assert!(
        err.message.contains("job 999999 not found"),
        "got: {}",
        err.message
    );
}

/// Validation failures surface as a `Validation` error before any INSERT.
async fn contract_attachment_validation_rejected(engine: &dyn BrainEngine) {
    let jid = engine.enqueue_job(&job("host")).await.unwrap().id;

    // Oversize: cap the queue at 4 bytes, feed 5.
    let q_small = MinionQueue::new(engine).with_max_attachment_bytes(4);
    let err = q_small
        .add_attachment(jid, &attachment("big.bin", "application/octet-stream", b"12345"))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("exceeds maxBytes"),
        "got: {}",
        err.message
    );

    // Path-traversal filename rejected.
    let q = MinionQueue::new(engine);
    let err = q
        .add_attachment(jid, &attachment("../evil", "text/plain", b"x"))
        .await
        .unwrap_err();
    assert!(
        err.message.contains("invalid characters"),
        "got: {}",
        err.message
    );

    // Nothing was persisted.
    assert!(q.list_attachments(jid).await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// InMemory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn inmemory_enqueue_defaults() {
    contract_enqueue_defaults(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_idempotency() {
    contract_idempotency(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_delay() {
    contract_delay_sets_delayed(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_claim_priority() {
    contract_claim_priority_and_exclusive(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_claim_filters() {
    contract_claim_filters(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_complete_token_fence() {
    contract_complete_token_fence(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_fail_delayed() {
    contract_fail_delayed_then_retry(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_fail_terminal_retry() {
    contract_fail_terminal_then_retry(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_renew_lock() {
    contract_renew_lock(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_promote_delayed() {
    contract_promote_delayed(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_handle_stalled() {
    contract_handle_stalled(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_handle_timeouts() {
    contract_handle_timeouts(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_handle_wall_clock_timeouts() {
    contract_handle_wall_clock_timeouts(&InMemoryEngine::new()).await;
}

// D-layer (1-1-3-1)
#[tokio::test]
async fn inmemory_spawn_child_blocks_parent() {
    contract_spawn_child_blocks_parent(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_child_complete_resolves_parent() {
    contract_child_complete_resolves_parent(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_child_fail_propagates_to_parent() {
    contract_child_fail_propagates_to_parent(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_cancel_cascades_subtree() {
    contract_cancel_cascades_subtree(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_inbox_send_and_read() {
    contract_inbox_send_and_read(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_update_tokens_fenced() {
    contract_update_tokens_fenced(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_attachment_crud_round_trip() {
    contract_attachment_crud_round_trip(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_attachment_duplicate_rejected() {
    contract_attachment_duplicate_rejected(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_attachment_job_not_found() {
    contract_attachment_job_not_found(&InMemoryEngine::new()).await;
}
#[tokio::test]
async fn inmemory_attachment_validation_rejected() {
    contract_attachment_validation_rejected(&InMemoryEngine::new()).await;
}

// ---------------------------------------------------------------------------
// Libsql
// ---------------------------------------------------------------------------

#[tokio::test]
async fn libsql_enqueue_defaults() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_enqueue_defaults(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_idempotency() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_idempotency(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_delay() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_delay_sets_delayed(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_claim_priority() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_claim_priority_and_exclusive(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_claim_filters() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_claim_filters(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_complete_token_fence() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_complete_token_fence(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_fail_delayed() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_fail_delayed_then_retry(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_fail_terminal_retry() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_fail_terminal_then_retry(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_renew_lock() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_renew_lock(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_promote_delayed() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_promote_delayed(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_handle_stalled() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_handle_stalled(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_handle_timeouts() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_handle_timeouts(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_handle_wall_clock_timeouts() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_handle_wall_clock_timeouts(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

// D-layer (1-1-3-1)
#[tokio::test]
async fn libsql_spawn_child_blocks_parent() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_spawn_child_blocks_parent(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_child_complete_resolves_parent() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_child_complete_resolves_parent(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_child_fail_propagates_to_parent() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_child_fail_propagates_to_parent(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_cancel_cascades_subtree() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_cancel_cascades_subtree(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_inbox_send_and_read() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_inbox_send_and_read(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_update_tokens_fenced() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_update_tokens_fenced(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_attachment_crud_round_trip() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_attachment_crud_round_trip(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_attachment_duplicate_rejected() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_attachment_duplicate_rejected(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_attachment_job_not_found() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_attachment_job_not_found(&engine).await;
    engine.disconnect().await.expect("disconnect");
}
#[tokio::test]
async fn libsql_attachment_validation_rejected() {
    let (engine, _tmp) = init_clean_libsql().await;
    contract_attachment_validation_rejected(&engine).await;
    engine.disconnect().await.expect("disconnect");
}

#[tokio::test]
async fn libsql_job_survives_reconnect() {
    let path = NamedTempFile::new().expect("alloc temp db file");
    let cfg = EngineConfig {
        database_url: None,
        database_path: Some(path.path().to_string_lossy().into_owned()),
    };

    let id = {
        let engine = LibsqlEngine::new();
        engine.connect(&cfg).await.expect("connect");
        engine.init_schema().await.expect("init_schema");
        let j = engine.enqueue_job(&job("persist")).await.unwrap();
        engine.disconnect().await.expect("disconnect");
        j.id
    };

    let engine2 = LibsqlEngine::new();
    engine2.connect(&cfg).await.expect("reconnect");
    engine2.init_schema().await.expect("reinit schema");
    let fetched = engine2.get_job(id).await.unwrap().expect("row persisted");
    assert_eq!(fetched.name, "persist");
    engine2.disconnect().await.expect("disconnect");
}

// ---------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postgres_enqueue_defaults() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_enqueue_defaults(&fix.engine).await;
}
#[tokio::test]
async fn postgres_idempotency() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_idempotency(&fix.engine).await;
}
#[tokio::test]
async fn postgres_delay() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_delay_sets_delayed(&fix.engine).await;
}
#[tokio::test]
async fn postgres_claim_priority() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_claim_priority_and_exclusive(&fix.engine).await;
}
#[tokio::test]
async fn postgres_claim_filters() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_claim_filters(&fix.engine).await;
}
#[tokio::test]
async fn postgres_complete_token_fence() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_complete_token_fence(&fix.engine).await;
}
#[tokio::test]
async fn postgres_fail_delayed() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_fail_delayed_then_retry(&fix.engine).await;
}
#[tokio::test]
async fn postgres_fail_terminal_retry() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_fail_terminal_then_retry(&fix.engine).await;
}
#[tokio::test]
async fn postgres_renew_lock() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_renew_lock(&fix.engine).await;
}
#[tokio::test]
async fn postgres_promote_delayed() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_promote_delayed(&fix.engine).await;
}
#[tokio::test]
async fn postgres_handle_stalled() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_handle_stalled(&fix.engine).await;
}
#[tokio::test]
async fn postgres_handle_timeouts() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_handle_timeouts(&fix.engine).await;
}
#[tokio::test]
async fn postgres_handle_wall_clock_timeouts() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_handle_wall_clock_timeouts(&fix.engine).await;
}

// D-layer (1-1-3-1)
#[tokio::test]
async fn postgres_spawn_child_blocks_parent() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_spawn_child_blocks_parent(&fix.engine).await;
}
#[tokio::test]
async fn postgres_child_complete_resolves_parent() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_child_complete_resolves_parent(&fix.engine).await;
}
#[tokio::test]
async fn postgres_child_fail_propagates_to_parent() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_child_fail_propagates_to_parent(&fix.engine).await;
}
#[tokio::test]
async fn postgres_cancel_cascades_subtree() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_cancel_cascades_subtree(&fix.engine).await;
}
#[tokio::test]
async fn postgres_inbox_send_and_read() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_inbox_send_and_read(&fix.engine).await;
}
#[tokio::test]
async fn postgres_update_tokens_fenced() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_update_tokens_fenced(&fix.engine).await;
}
#[tokio::test]
async fn postgres_attachment_crud_round_trip() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_attachment_crud_round_trip(&fix.engine).await;
}
#[tokio::test]
async fn postgres_attachment_duplicate_rejected() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_attachment_duplicate_rejected(&fix.engine).await;
}
#[tokio::test]
async fn postgres_attachment_job_not_found() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_attachment_job_not_found(&fix.engine).await;
}
#[tokio::test]
async fn postgres_attachment_validation_rejected() {
    let fix = support::pg_fixture::PgFixture::start().await;
    contract_attachment_validation_rejected(&fix.engine).await;
}
