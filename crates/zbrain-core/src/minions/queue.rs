//! [`MinionQueue`] — thin facade over the backend job-queue trait methods.
//!
//! Each method delegates directly to a [`BrainEngine`](crate::engine::BrainEngine)
//! trait method. The facade exists to (a) give the queue a stable, TS-shaped
//! API surface independent of the trait's naming, and (b) be the single place
//! the future `jobs` CLI and `MinionWorker` (1-2) depend on.
//!
//! No business logic lives here in A+B — every method is a one-line
//! delegation. Logic that spans multiple trait calls (dependency resolution,
//! sweeps) will be added in later slices as facade methods that orchestrate
//! several trait primitives.

use crate::engine::BrainEngine;
use crate::minions::attachments::{
    validate_attachment, AttachmentValidationOpts, DEFAULT_MAX_ATTACHMENT_BYTES,
};
use crate::minions::types::{
    Attachment, AttachmentInput, FailOutcome, JobFilters, MinionJob, MinionJobInput, StalledSweep,
};
use crate::Result;

use std::collections::HashSet;

use serde_json::Value;

/// Facade over a brain engine's job-queue operations. Borrows the engine, so
/// it is cheap to construct per-call (mirrors TS `new MinionQueue(engine)`).
pub struct MinionQueue<'a> {
    engine: &'a dyn BrainEngine,
    /// Attachment size cap for `add_attachment` validation. Defaults to
    /// [`DEFAULT_MAX_ATTACHMENT_BYTES`]; override with
    /// [`MinionQueue::with_max_attachment_bytes`] (mirrors the TS
    /// `maxAttachmentBytes` constructor option).
    max_attachment_bytes: i64,
}

impl<'a> MinionQueue<'a> {
    /// Wrap an engine. The engine must already be connected.
    #[must_use]
    pub fn new(engine: &'a dyn BrainEngine) -> Self {
        Self {
            engine,
            max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
        }
    }

    /// Override the attachment size cap (bytes). Mirrors the TS
    /// `maxAttachmentBytes` constructor option.
    #[must_use]
    pub fn with_max_attachment_bytes(mut self, max_bytes: i64) -> Self {
        self.max_attachment_bytes = max_bytes;
        self
    }

    /// Submit a job. Basic insert + idempotency (decision 6): if
    /// `input.idempotency_key` matches an existing row, that row is returned
    /// and no second row is created. Parent/child and backpressure are not
    /// handled in A+B.
    pub async fn add(&self, input: &MinionJobInput) -> Result<MinionJob> {
        self.engine.enqueue_job(input).await
    }

    /// Fetch a job by id. `None` if not found.
    pub async fn get_job(&self, id: i64) -> Result<Option<MinionJob>> {
        self.engine.get_job(id).await
    }

    /// List jobs newest-first, filtered/bounded by `filters`.
    pub async fn get_jobs(&self, filters: &JobFilters) -> Result<Vec<MinionJob>> {
        self.engine.get_jobs(filters).await
    }

    /// Atomically claim the next eligible waiting job for a worker. Returns
    /// `None` when the queue has no matching waiting job. Token-fenced: the
    /// returned job carries `lock_token` so later `complete`/`fail`/`renew`
    /// calls can prove ownership.
    ///
    /// `registered_names` filters to job types this worker can handle; an
    /// empty slice claims nothing (matches TS early return).
    pub async fn claim(
        &self,
        lock_token: &str,
        lock_duration_ms: i64,
        queue: &str,
        registered_names: &[String],
    ) -> Result<Option<MinionJob>> {
        if registered_names.is_empty() {
            return Ok(None);
        }
        self.engine
            .claim_job(lock_token, lock_duration_ms, queue, registered_names)
            .await
    }

    /// Mark a claimed job completed (token-fenced). Returns `None` if the job
    /// is not active or the token does not match (lost race / stale worker).
    pub async fn complete_job(
        &self,
        id: i64,
        lock_token: &str,
        result: Option<&Value>,
    ) -> Result<Option<MinionJob>> {
        self.engine.complete_job(id, lock_token, result).await
    }

    /// Fail a claimed job (token-fenced) into one of delayed/failed/dead.
    /// `backoff_ms` sets `delay_until = now + backoff_ms` when `outcome` is
    /// [`FailOutcome::Delayed`]; ignored otherwise. Returns `None` on
    /// token/status mismatch.
    pub async fn fail_job(
        &self,
        id: i64,
        lock_token: &str,
        error_text: &str,
        outcome: FailOutcome,
        backoff_ms: i64,
    ) -> Result<Option<MinionJob>> {
        self.engine
            .fail_job(id, lock_token, error_text, outcome, backoff_ms)
            .await
    }

    /// Extend the lease on an active job (worker heartbeat). Returns `true` if
    /// the lock was renewed, `false` if the token/status no longer matches.
    pub async fn renew_lock(
        &self,
        id: i64,
        lock_token: &str,
        lock_duration_ms: i64,
    ) -> Result<bool> {
        self.engine
            .renew_job_lock(id, lock_token, lock_duration_ms)
            .await
    }

    /// Requeue a failed/dead job back to waiting, clearing error/lock/delay.
    /// Returns `None` if the job is not in a failed/dead state.
    pub async fn retry_job(&self, id: i64) -> Result<Option<MinionJob>> {
        self.engine.retry_job(id).await
    }

    // ─── Background sweeps (1-1-2) ──────────────────────────────────────────
    //
    // Time-driven transitions the worker/supervisor loop calls periodically.
    // Pure C-layer sweeps: their D-layer side effects (child_done inbox,
    // waiting-children parent unblock) land in 1-1-3.

    /// Promote delayed jobs whose `delay_until` has passed back to `waiting`.
    /// Returns the promoted jobs.
    pub async fn promote_delayed(&self) -> Result<Vec<MinionJob>> {
        self.engine.promote_delayed().await
    }

    /// Sweep stalled active jobs (lease expired). Requeues those under their
    /// stall budget and dead-letters the rest; see [`StalledSweep`].
    pub async fn handle_stalled(&self) -> Result<StalledSweep> {
        self.engine.handle_stalled().await
    }

    /// Dead-letter active jobs whose per-job `timeout_at` has passed while the
    /// lease is still held. Returns the timed-out jobs.
    pub async fn handle_timeouts(&self) -> Result<Vec<MinionJob>> {
        self.engine.handle_timeouts().await
    }

    /// Dead-letter active jobs that exceed a wall-clock runtime threshold
    /// regardless of lease state. `lock_duration_ms` feeds the fallback
    /// threshold for jobs without an explicit `timeout_ms`.
    pub async fn handle_wall_clock_timeouts(
        &self,
        lock_duration_ms: i64,
    ) -> Result<Vec<MinionJob>> {
        self.engine
            .handle_wall_clock_timeouts(lock_duration_ms)
            .await
    }

    // ─── Attachments (1-1-3-2) ──────────────────────────────────────────────
    //
    // Per-job blob CRUD. `add_attachment` orchestrates the backend-agnostic
    // validation (pure function) around the backend INSERT; the other three are
    // one-line delegations. Not token-fenced (mirrors the TS surface).

    /// Attach a file to a job. Validates filename safety, content-type, base64,
    /// size cap, and duplicate filename, then persists the decoded bytes and
    /// returns the metadata row (not the bytes — use [`get_attachment`] to
    /// fetch). Mirrors TS `addAttachment` (`queue.ts` L1272-1306).
    ///
    /// The DB `UNIQUE (job_id, filename)` constraint is the authoritative
    /// duplicate fence; the `existing_filenames` early-out here just gives a
    /// faster, clearer error before the round-trip.
    ///
    /// [`get_attachment`]: MinionQueue::get_attachment
    pub async fn add_attachment(
        &self,
        job_id: i64,
        input: &AttachmentInput,
    ) -> Result<Attachment> {
        let existing: HashSet<String> = self
            .engine
            .list_attachment_filenames(job_id)
            .await?
            .into_iter()
            .collect();

        let normalized = validate_attachment(
            input,
            &AttachmentValidationOpts {
                max_bytes: self.max_attachment_bytes,
                existing_filenames: Some(&existing),
            },
        )
        .map_err(|e| {
            crate::error::StructuredError::new(
                "Validation",
                "validation",
                format!("attachment validation failed: {e}"),
            )
        })?;

        self.engine.insert_attachment(job_id, &normalized).await
    }

    /// List attachments for a job (metadata only, no bytes), ordered
    /// `created_at ASC, id ASC`. Mirrors TS `listAttachments`.
    pub async fn list_attachments(&self, job_id: i64) -> Result<Vec<Attachment>> {
        self.engine.list_attachments(job_id).await
    }

    /// Fetch a single attachment with its bytes by (job_id, filename). `None` if
    /// absent. Mirrors TS `getAttachment`.
    pub async fn get_attachment(
        &self,
        job_id: i64,
        filename: &str,
    ) -> Result<Option<(Attachment, Vec<u8>)>> {
        self.engine.get_attachment(job_id, filename).await
    }

    /// Delete an attachment by (job_id, filename). `true` if a row was removed.
    /// Mirrors TS `deleteAttachment`.
    pub async fn delete_attachment(&self, job_id: i64, filename: &str) -> Result<bool> {
        self.engine.delete_attachment(job_id, filename).await
    }
}
