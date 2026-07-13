//! Minion job queue worker (roadmap 1-2-2: queue consume core).
//!
//! This crate owns the async runtime side of the minion job queue: the consume
//! loop that pulls waiting jobs, dispatches them to registered
//! [`MinionHandler`](zbrain_core::minions::MinionHandler)s, and drives the
//! complete / fail-with-retry transitions. The pure contract layer (the
//! handler trait, job context, worker opts) lives in `zbrain-core`
//! (roadmap 1-2-1); this crate is the first slice that actually *runs* jobs.
//!
//! ## Slice scope (1-2-2 = queue consume core)
//!
//! Deliberately serial (`concurrency = 1`) to prove the end-to-end loop first:
//! `promote_delayed -> claim(1) -> dispatch -> complete | fail`. Deferred to
//! later slices:
//! - concurrency pool + per-job lock renewal + per-job timeout + graceful
//!   SIGTERM/SIGINT drain -> 1-2-3.
//! - RSS / self-health / quiet-hours -> 1-2-4.
//! - supervisor + child-spawn -> 1-2-5.
//! - the rate-lease-full bounce branch (TS `RateLeaseUnavailableError`) is a
//!   no-op stub until `1-3` rate-leases land -> 1-2-3.
//!
//! ## TS reference
//!
//! - Consume loop — `src/core/minions/worker.ts` L430-472.
//! - `executeJob` (dispatch + complete/fail) — `worker.ts` L673-847.
//! - `calculateBackoff` — `src/core/minions/backoff.ts`.

pub mod backoff;
pub mod quiet_hours;
pub mod rss;
pub mod worker;

pub use backoff::calculate_backoff;
pub use worker::{unrecoverable, MinionWorker, ProcessOutcome, UnhealthyReason, UNRECOVERABLE_KIND};
