//! Minion job queue (Phase 9).
//!
//! A BullMQ-inspired, backend-native job queue. Jobs are submitted with
//! [`MinionQueue::add`], claimed by workers with [`MinionQueue::claim`], and
//! completed / failed with the token-fenced [`MinionQueue::complete_job`] /
//! [`MinionQueue::fail_job`].
//!
//! ## Architecture (roadmap 1-1-1 decision 2)
//!
//! The queue's SQL lives on the backend engines: each job operation is a
//! [`BrainEngine`](crate::engine::BrainEngine) trait method implemented once in
//! `postgres.rs` (using `FOR UPDATE SKIP LOCKED`) and once in `libsql.rs`
//! (using `BEGIN IMMEDIATE`). This keeps the two backends' concurrency
//! primitives isolated — SQLite's single-writer semantics are equivalent to
//! `SKIP LOCKED` for claim, so each backend writes its own optimal SQL rather
//! than sharing a lowest-common-denominator query.
//!
//! [`MinionQueue`] is a thin facade over those trait methods, matching the TS
//! `MinionQueue` class surface so the `jobs` CLI and future worker are 1:1
//! wrappers over it.
//!
//! ## Slice scope (1-1-1 = A+B)
//!
//! - **A (foundation)**: schema, [`MinionJob`] type + status enums,
//!   `add` (insert + idempotency), `get_job`, `get_jobs`.
//! - **B (concurrency core)**: `claim`, `complete_job`, `fail_job`,
//!   `renew_lock`, `retry_job`.
//!
//! Deferred: dependency graph + inbox + attachments + pause/prune/stats ->
//! 1-1-3. Background sweeps (promote-delayed / stall / timeout / wall-clock)
//! land in 1-1-2 as the pure C-layer transitions; their D-layer side effects
//! (child_done inbox inserts, waiting-children parent unblock) stay in 1-1-3.

pub mod attachments;
pub mod handler;
pub mod queue;
pub mod tools;
pub mod types;

pub use handler::{MinionHandler, MinionJobContext, MinionWorkerOpts};
pub use queue::MinionQueue;
pub use types::{
    Attachment, AttachmentInput, BackoffType, ChildDoneKind, ChildDoneMessage, ChildFailPolicy,
    ChildOutcome, FailOutcome, InboxMessage, JobFilters, MinionJob, MinionJobInput,
    MinionJobStatus, NormalizedAttachment, QueueHealth, QueueStats, QueueTypeStat, StalledSweep,
    TokenUpdate,
};
