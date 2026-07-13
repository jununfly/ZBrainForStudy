//! Minion job queue — type layer.
//!
//! BullMQ-inspired job queue for ZBrain (Phase 9). This module is the pure
//! data model: the `MinionJob` record, its status/enum types, and the input
//! shape for submission. The actual SQL lives on the backend engines
//! (`postgres.rs` / `libsql.rs`) behind `BrainEngine` trait methods, and the
//! `MinionQueue` facade in `super::queue` is a thin wrapper over them.
//!
//! TS reference: `src/core/minions/types.ts` (MinionJob, rowToMinionJob).
//!
//! ## Time representation (roadmap 1-1-1 decision 5)
//!
//! Two classes of timestamp with different Rust types:
//! - **Record columns** (`created_at`/`updated_at`/`started_at`/`finished_at`):
//!   `String` (RFC-3339 / ISO-8601). PG reads `DateTime<Utc>` -> `to_rfc3339()`;
//!   SQLite reads the TEXT column directly. Never arithmetic'd.
//! - **Scheduling columns** (`lock_until`/`delay_until`/`timeout_at`):
//!   `Option<i64>` Unix epoch **milliseconds**. PG stores TIMESTAMPTZ and does
//!   `now() + interval` in SQL, reading back as epoch-ms; SQLite stores INTEGER
//!   epoch-ms directly and the arithmetic happens in Rust. Both backends
//!   normalize to the same `Option<i64>` so the queue logic is backend-blind.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Job lifecycle status. Mirrors the 9-value TS `MinionJobStatus` union and
/// the `chk_minion_status` CHECK constraint.
///
/// A+B (slice 1-1-1) drives waiting/active/completed/failed/delayed/dead;
/// `waiting-children` (dependency aggregation) and `paused` are reachable only
/// via the D-layer (1-1-3) but are part of the type for schema fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MinionJobStatus {
    Waiting,
    Active,
    Completed,
    Failed,
    Delayed,
    Dead,
    Cancelled,
    WaitingChildren,
    Paused,
}

impl MinionJobStatus {
    /// Wire form matching the DB TEXT value (kebab-case for `waiting-children`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Delayed => "delayed",
            Self::Dead => "dead",
            Self::Cancelled => "cancelled",
            Self::WaitingChildren => "waiting-children",
            Self::Paused => "paused",
        }
    }

    /// Parse the DB TEXT value. Returns `None` for unrecognized input so
    /// callers decode row values without panicking on schema drift.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "waiting" => Self::Waiting,
            "active" => Self::Active,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "delayed" => Self::Delayed,
            "dead" => Self::Dead,
            "cancelled" => Self::Cancelled,
            "waiting-children" => Self::WaitingChildren,
            "paused" => Self::Paused,
            _ => return None,
        })
    }

    /// Terminal statuses: a job here will never transition again.
    /// Matches TS `TERMINAL_STATUSES` (queue.ts).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Dead | Self::Cancelled
        )
    }
}

/// Retry backoff strategy. Matches `chk_minion_backoff_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackoffType {
    Fixed,
    Exponential,
}

impl BackoffType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Exponential => "exponential",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fixed" => Self::Fixed,
            "exponential" => Self::Exponential,
            _ => return None,
        })
    }
}

/// Policy for what happens to a parent when a child fails terminally.
/// Matches `chk_minion_on_child_fail`. The behavior itself is D-layer
/// (1-1-3); this enum exists now for record fidelity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChildFailPolicy {
    FailParent,
    RemoveDep,
    Ignore,
    Continue,
}

impl ChildFailPolicy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FailParent => "fail_parent",
            Self::RemoveDep => "remove_dep",
            Self::Ignore => "ignore",
            Self::Continue => "continue",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "fail_parent" => Self::FailParent,
            "remove_dep" => Self::RemoveDep,
            "ignore" => Self::Ignore,
            "continue" => Self::Continue,
            _ => return None,
        })
    }
}

/// A persisted minion job. 1:1 with a `minion_jobs` row and the TS `MinionJob`
/// interface. Field grouping mirrors `types.ts` for cross-referencing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MinionJob {
    pub id: i64,
    pub name: String,
    pub queue: String,
    pub status: MinionJobStatus,
    pub priority: i32,
    pub data: Value,

    // Retry
    pub max_attempts: i32,
    pub attempts_made: i32,
    pub attempts_started: i32,
    pub backoff_type: BackoffType,
    pub backoff_delay: i32,
    pub backoff_jitter: f64,

    // Stall detection
    pub stalled_counter: i32,
    pub max_stalled: i32,
    pub lock_token: Option<String>,
    /// Epoch-ms; see module time-representation note.
    pub lock_until: Option<i64>,

    // Scheduling
    /// Epoch-ms; see module time-representation note.
    pub delay_until: Option<i64>,

    // Dependencies (D-layer wiring lands in 1-1-3)
    pub parent_job_id: Option<i64>,
    pub on_child_fail: ChildFailPolicy,

    // Token accounting
    pub tokens_input: i64,
    pub tokens_output: i64,
    pub tokens_cache_read: i64,

    // v7: subagent + parity
    pub depth: i32,
    pub max_children: Option<i32>,
    pub timeout_ms: Option<i64>,
    /// Epoch-ms; see module time-representation note.
    pub timeout_at: Option<i64>,
    pub remove_on_complete: bool,
    pub remove_on_fail: bool,
    pub idempotency_key: Option<String>,

    // v12: scheduler polish
    pub quiet_hours: Option<Value>,
    pub stagger_key: Option<String>,

    // Results
    pub result: Option<Value>,
    pub progress: Option<Value>,
    pub error_text: Option<String>,
    pub stacktrace: Vec<String>,

    // Timestamps (record columns: RFC-3339 strings)
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub updated_at: String,
}

/// Submission input for `MinionQueue::add`. Mirrors the A+B-relevant subset of
/// the TS `MinionJobInput`.
///
/// Slice scope: A+B (1-1-1) did basic insert + idempotency only. The D-layer
/// (1-1-3-1) adds `parent_job_id` — spawning a child under a parent triggers
/// the depth/max_children validation and flips the parent to `waiting-children`
/// (see `enqueue_job`). Submission backpressure (`max_waiting` /
/// `pg_advisory_xact_lock`) remains a separate, deferred PG-specific design.
#[derive(Debug, Clone, Default)]
pub struct MinionJobInput {
    pub name: String,
    pub data: Option<Value>,
    pub queue: Option<String>,
    pub priority: Option<i32>,
    pub max_attempts: Option<i32>,
    pub backoff_type: Option<BackoffType>,
    pub backoff_delay: Option<i32>,
    pub backoff_jitter: Option<f64>,
    /// Per-job stall tolerance override. Clamped to [1, 100] on insert;
    /// omitted -> schema DEFAULT (5) applies.
    pub max_stalled: Option<i32>,
    /// Delay in ms before the job becomes eligible. Sets status=delayed and
    /// delay_until = now + delay.
    pub delay: Option<i64>,
    /// Parent job to spawn this job under (D-layer / 1-1-3-1). When set,
    /// `enqueue_job` validates spawn depth + max_children against the parent
    /// and flips the parent to `waiting-children`. `depth` is derived
    /// (parent.depth + 1), never caller-supplied.
    pub parent_job_id: Option<i64>,
    pub on_child_fail: Option<ChildFailPolicy>,
    pub max_children: Option<i32>,
    pub timeout_ms: Option<i64>,
    pub remove_on_complete: Option<bool>,
    pub remove_on_fail: Option<bool>,
    /// Global dedup key. Same key returns the existing job, no second row.
    pub idempotency_key: Option<String>,
}

/// Filters for `MinionQueue::get_jobs`. All `None` -> list all (newest first),
/// bounded by `limit`/`offset`.
#[derive(Debug, Clone, Default)]
pub struct JobFilters {
    pub status: Option<MinionJobStatus>,
    pub queue: Option<String>,
    pub name: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Terminal-or-retry outcome for the `fail_job` transition. A failed job moves
/// to exactly one of these. Constrains the caller so an arbitrary status can't
/// be passed to `fail_job`.
///
/// - `Delayed`: retry-with-backoff (delay_until = now + backoff_ms).
/// - `Failed`: give up (non-retryable, terminal).
/// - `Dead`: dead-letter (terminal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailOutcome {
    Delayed,
    Failed,
    Dead,
}

impl FailOutcome {
    #[must_use]
    pub fn as_status(self) -> MinionJobStatus {
        match self {
            Self::Delayed => MinionJobStatus::Delayed,
            Self::Failed => MinionJobStatus::Failed,
            Self::Dead => MinionJobStatus::Dead,
        }
    }

    /// Whether this outcome is terminal (Failed/Dead) vs a retry (Delayed).
    /// Terminal outcomes set finished_at and run parent hooks (hooks are
    /// D-layer / 1-1-3).
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Failed | Self::Dead)
    }
}

/// Result of a stall sweep (`handle_stalled`). A stalled active job (lease
/// expired, `lock_until < now`) is split by its stall budget:
///
/// - `requeued`: `stalled_counter + 1 < max_stalled` — bumped and returned to
///   `waiting` for another attempt.
/// - `dead`: `stalled_counter + 1 >= max_stalled` — dead-lettered with
///   `error_text = "max stalled count exceeded"`.
///
/// Mirrors the TS `handleStalled` return shape
/// (`{ requeued: MinionJob[]; dead: MinionJob[] }`). Slice scope (1-1-2,
/// decision 1): this is the pure sweep only — the `child_done` inbox insert and
/// `waiting-children` parent unblock that TS folds into timeout sweeps belong to
/// the D-layer (1-1-3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StalledSweep {
    pub requeued: Vec<MinionJob>,
    pub dead: Vec<MinionJob>,
}

// ============================================================
// D-layer (roadmap 1-1-3-1): inbox / sidechannel messaging
// ============================================================

/// A persisted `minion_inbox` row. 1:1 with the TS `InboxMessage` interface
/// (`src/core/minions/types.ts` L224-231).
///
/// `payload` is an arbitrary JSON envelope; the queue only introspects the
/// `child_done` shape (see [`ChildDoneMessage`]). `sender` is `'admin'`,
/// `'minions'` (automatic child-completion hook), or a parent job id string.
///
/// ## Time representation
/// `sent_at`/`read_at` are `String` (RFC-3339 / ISO-8601) record columns —
/// never arithmetic'd, so both backends store them as text (PG TIMESTAMPTZ read
/// back via `to_rfc3339()`, SQLite TEXT read directly). Contrast the
/// `minion_jobs` scheduling columns which use epoch-ms integers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxMessage {
    pub id: i64,
    pub job_id: i64,
    pub sender: String,
    pub payload: Value,
    pub sent_at: String,
    pub read_at: Option<String>,
}

/// Terminal outcome carried by a [`ChildDoneMessage`]. Mirrors the TS
/// `ChildOutcome` union (`types.ts` L260).
///
/// Serialized as a lowercase string inside the JSONB payload (`"complete"`,
/// `"failed"`, ...), so an aggregator parent can count "N children resolved"
/// regardless of which rail (complete/fail/cancel/timeout) each child took.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChildOutcome {
    Complete,
    Failed,
    Dead,
    Cancelled,
    Timeout,
}

/// Auto-posted into a parent's inbox when a child reaches a terminal state.
/// Mirrors the TS `ChildDoneMessage` interface (`types.ts` L262-275).
///
/// This is the only inbox payload shape the queue itself introspects (via the
/// `idx_minion_inbox_child_done` partial index and `read_child_completions`).
///
/// ## Compatibility
/// Pre-v0.15 writers only emitted this on the success path (complete) and did
/// not set `outcome`. When `outcome` is absent on read, consumers treat the
/// message as [`ChildOutcome::Complete`] (see [`ChildDoneMessage::effective_outcome`]).
/// v0.15+ fail/cancel/timeout rails also emit it with the appropriate outcome,
/// so aggregator handlers wait for N children regardless of individual result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildDoneMessage {
    /// Discriminator. Always `"child_done"`; drives the partial index predicate.
    #[serde(rename = "type")]
    pub kind: ChildDoneKind,
    pub child_id: i64,
    pub job_name: String,
    /// Child result payload; non-null on the success path, null otherwise.
    pub result: Value,
    /// Terminal outcome. `None` only when read from a pre-v0.15 writer that
    /// didn't set it — treat as `Complete` via [`Self::effective_outcome`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ChildOutcome>,
    /// Set when `outcome != Complete`. Mirrors `minion_jobs.error_text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The single-variant discriminator tag for [`ChildDoneMessage::kind`].
/// A dedicated enum (rather than a bare `String`) makes the `"child_done"`
/// literal a compile-time constant and lets serde enforce it on deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildDoneKind {
    ChildDone,
}

impl ChildDoneMessage {
    /// Build a `child_done` envelope for the given child transition. `outcome`
    /// and `error` are always set (v0.15+ writer); `error` is `None` for the
    /// success path.
    #[must_use]
    pub fn new(
        child_id: i64,
        job_name: impl Into<String>,
        result: Value,
        outcome: ChildOutcome,
        error: Option<String>,
    ) -> Self {
        Self {
            kind: ChildDoneKind::ChildDone,
            child_id,
            job_name: job_name.into(),
            result,
            outcome: Some(outcome),
            error,
        }
    }

    /// Outcome with the pre-v0.15 legacy fallback applied: a missing `outcome`
    /// means the message came from an old success-only writer, so it counts as
    /// `Complete`.
    #[must_use]
    pub fn effective_outcome(&self) -> ChildOutcome {
        self.outcome.unwrap_or(ChildOutcome::Complete)
    }
}

/// Token-count delta applied by `update_tokens`. Mirrors the TS `TokenUpdate`
/// interface (`types.ts` L314-318). All fields optional — a caller bumps only
/// the counters it has. Missing fields add zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<i64>,
}

// ============================================================
// D-layer (roadmap 1-1-3-2): per-job attachment storage
// ============================================================

/// Caller-supplied attachment payload. Mirrors the TS `AttachmentInput`
/// interface (`src/core/minions/types.ts` L280-285). `content_base64` is
/// base64-encoded file bytes, validated + decoded server-side by
/// [`validate_attachment`](crate::minions::attachments::validate_attachment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentInput {
    pub filename: String,
    pub content_type: String,
    /// Base64-encoded file bytes. Validated server-side.
    pub content_base64: String,
}

/// Validated + decoded attachment, ready to persist. Produced by
/// [`validate_attachment`](crate::minions::attachments::validate_attachment).
/// Mirrors the TS `NormalizedAttachment` (`src/core/minions/attachments.ts`
/// L20-26), except `bytes` is an owned `Vec<u8>` (the decoded payload) rather
/// than a Node `Buffer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedAttachment {
    pub filename: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub size_bytes: i64,
    pub sha256: String,
}

/// A persisted `minion_attachments` row *without* inline bytes — metadata only.
/// Mirrors the TS `Attachment` interface (`src/core/minions/types.ts` L288-297).
/// Fetch the bytes separately with
/// [`get_attachment`](crate::engine::BrainEngine::get_attachment).
///
/// `storage_uri` is always `None` for the current port: attachments only take
/// the inline `content` channel (faithful to the TS behavior). External-storage
/// routing is a reserved capability registered in docs/plans/KNOWN-GAPS.md.
///
/// ## Time representation
/// `created_at` is `String` (RFC-3339 / ISO-8601) — a record column never used
/// in interval arithmetic, matching [`InboxMessage::sent_at`] (PG TIMESTAMPTZ
/// read back via `to_rfc3339()`, SQLite TEXT read directly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: i64,
    pub job_id: i64,
    pub filename: String,
    pub content_type: String,
    pub storage_uri: Option<String>,
    pub size_bytes: i64,
    pub sha256: String,
    pub created_at: String,
}

