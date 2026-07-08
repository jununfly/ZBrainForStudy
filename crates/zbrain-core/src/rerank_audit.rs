//! Rerank-failure audit trail (writer + reader + health classifier).
//!
//! Mirrors the TypeScript runtime at `src/core/rerank-audit.ts` (which
//! delegates file I/O to `src/core/audit/audit-writer.ts`). Rows are written
//! as JSONL to `<audit_dir>/rerank-failures-YYYY-Www.jsonl`, one
//! [`RerankFailureEvent`] per line, rotated per ISO-8601 week. The event
//! shape (field names + `snake_case` `reason` values + `severity: "warn"`) is
//! kept byte-compatible with the TS writer so that during the TS→Rust
//! migration either runtime can read rows the other wrote from the same
//! `~/.zbrain/audit/` directory.
//!
//! Deliberately failure-ONLY: success events are never logged. Writing once
//! per token-max search would be hot-path I/O churn, and success rows would
//! leak query volume + timing into a file that otherwise holds only
//! failures. The [`classify_reranker_health`] reader interprets "no events in
//! window" via the `enabled` flag (enabled + no events = healthy; disabled =
//! no failures expected).
//!
//! Best-effort writes: append failures go to stderr but never propagate as
//! errors — search must fail open to RRF order regardless. This matches
//! `sync/failures.rs`'s JSONL-append convention; the per-week rotation +
//! `~/.zbrain/audit/` layout is this module's own (it does NOT reuse the
//! per-source model of `sync/failures.rs`).

use crate::time::current_utc_iso8601;
use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Stable error-classification union; mirrors `RerankError.reason` and the
/// TS `RerankFailureReason`. Serialized as `snake_case` strings on the wire so
/// rows round-trip with the TS writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RerankFailureReason {
    Auth,
    RateLimit,
    Network,
    Timeout,
    PayloadTooLarge,
    Unknown,
}

/// A single rerank-failure row. Field names + JSON representation match the
/// TS `RerankFailureEvent` interface exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RerankFailureEvent {
    /// ISO-8601 UTC timestamp of when the failure was recorded.
    pub ts: String,
    /// `provider:model` — e.g. `"zeroentropyai:zerank-2"`.
    pub model: String,
    /// Classified failure mode.
    pub reason: RerankFailureReason,
    /// SHA-256 prefix of the rerank query (8 hex chars). Privacy: never log
    /// query text; this lets doctor dedupe repeat failures on one query.
    pub query_hash: String,
    /// Number of documents being reranked when the failure fired.
    pub doc_count: u32,
    /// Truncated upstream error message (first 200 chars). Query text is
    /// hashed separately so this never carries PII.
    pub error_summary: String,
    /// Always `"warn"` — every rerank failure degrades UX identically.
    pub severity: String,
}

/// Max `error_summary` length, matching the TS writer's 200-char cut.
const ERROR_SUMMARY_MAX: usize = 200;

/// Doctor's failure-window width, in days. Hardcoded 7 to match
/// `readRecentRerankFailures(7)` in the TS runtime.
pub const HEALTH_WINDOW_DAYS: i64 = 7;

/// Transient-failure warn threshold: >= 5 transient failures (network +
/// timeout + `rate_limit`) in the window. Below this they are noise — rerank
/// fails open to RRF order anyway. Mirrors the TS threshold.
pub const TRANSIENT_WARN_THRESHOLD: usize = 5;

/// Truncate an error message to [`ERROR_SUMMARY_MAX`] chars, appending an
/// ellipsis when cut. Char-boundary safe (the TS side cuts UTF-16 code units;
/// we cut on Rust char boundaries, which is the correct behavior for a
/// human-readable summary).
fn truncate_error_summary(msg: &str) -> String {
    if msg.chars().count() <= ERROR_SUMMARY_MAX {
        return msg.to_string();
    }
    let truncated: String = msg.chars().take(ERROR_SUMMARY_MAX - 1).collect();
    format!("{truncated}…")
}

/// Compute the ISO-week-rotated filename `rerank-failures-YYYY-Www.jsonl`.
///
/// Uses chrono's ISO-8601 week (`%G-W%V`): `%G` is the ISO week-numbering
/// year and `%V` the zero-padded week (01..=53). The year-boundary rule
/// (a late-December date can belong to week 52/53 of the ISO year, and
/// 2027-01-01 belongs to ISO week 53 of 2026) is handled by chrono.
#[must_use]
pub fn rerank_audit_filename(now: DateTime<Utc>) -> String {
    let iso = now.iso_week();
    format!(
        "rerank-failures-{:04}-W{:02}.jsonl",
        iso.year(),
        iso.week()
    )
}

/// Absolute path to the audit file for `now`'s ISO week under `audit_dir`.
fn audit_file_path(audit_dir: &Path, now: DateTime<Utc>) -> std::path::PathBuf {
    audit_dir.join(rerank_audit_filename(now))
}

/// Fields a caller supplies; `ts` and `severity` are stamped by the writer.
pub struct RerankFailureInput {
    pub model: String,
    pub reason: RerankFailureReason,
    pub query_hash: String,
    pub doc_count: u32,
    pub error_summary: String,
}

/// Append one rerank-failure event to the current ISO week's JSONL file
/// under `audit_dir`. Best-effort: on any I/O/serialize error a warning is
/// written to stderr and `()` is returned — the caller's search continues.
///
/// The sole production caller is the rerank HTTP client's fail-open branch
/// (the `reqwest` cross-encoder POST path), which catches a classified
/// rerank error, logs it here, and returns results in RRF order. That
/// call-site does not exist yet; it arrives with the rerank client work.
/// Until then this writer is exercised end-to-end by the reader round-trip
/// test and consumed by the doctor `reranker_health` check, so it is not
/// dead code.
pub fn log_rerank_failure(audit_dir: &Path, input: RerankFailureInput) {
    log_rerank_failure_at(audit_dir, input, Utc::now());
}

/// [`log_rerank_failure`] with an injectable clock, for tests that need to
/// pin the ISO week / timestamp deterministically.
pub fn log_rerank_failure_at(audit_dir: &Path, input: RerankFailureInput, now: DateTime<Utc>) {
    let event = RerankFailureEvent {
        ts: current_utc_iso8601(),
        model: input.model,
        reason: input.reason,
        query_hash: input.query_hash,
        doc_count: input.doc_count,
        error_summary: truncate_error_summary(&input.error_summary),
        severity: "warn".to_string(),
    };
    if let Err(e) = append_event(audit_dir, &event, now) {
        eprintln!("[zbrain] rerank-failure audit write failed ({e}); search continues");
    }
}

fn append_event(
    audit_dir: &Path,
    event: &RerankFailureEvent,
    now: DateTime<Utc>,
) -> std::io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(audit_dir)?;
    let path = audit_file_path(audit_dir, now);
    let mut line = serde_json::to_string(event)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Read rerank-failure events from the last `days` window under `audit_dir`.
///
/// Walks the current + previous ISO-week files (so a 7-day window straddling
/// Monday-midnight stays covered), then keeps only rows whose `ts` parses and
/// is >= `now - days`. Missing files / corrupt rows are skipped silently —
/// the audit trail is informational and must never block the doctor check.
/// Mirrors `readRecentRerankFailures` in the TS runtime.
#[must_use]
pub fn read_recent_rerank_failures(audit_dir: &Path, days: i64) -> Vec<RerankFailureEvent> {
    read_recent_rerank_failures_at(audit_dir, days, Utc::now())
}

/// [`read_recent_rerank_failures`] with an injectable clock, for tests.
#[must_use]
pub fn read_recent_rerank_failures_at(
    audit_dir: &Path,
    days: i64,
    now: DateTime<Utc>,
) -> Vec<RerankFailureEvent> {
    let cutoff = now - Duration::days(days);
    let filenames = [
        rerank_audit_filename(now),
        rerank_audit_filename(now - Duration::days(7)),
    ];
    let mut out = Vec::new();
    for filename in filenames {
        let path = audit_dir.join(&filename);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue; // missing file — skip
        };
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<RerankFailureEvent>(line) else {
                continue; // corrupt row — skip
            };
            // Keep rows whose ts parses to a time within the window.
            if let Ok(ts) = DateTime::parse_from_rfc3339(&event.ts) {
                if ts.with_timezone(&Utc) >= cutoff {
                    out.push(event);
                }
            }
        }
    }
    out
}

/// Status of the `reranker_health` doctor check. Only `Ok` / `Warn` are ever
/// produced — a rerank failure degrades UX but never hard-fails doctor (the
/// search path fails open). Mirrors the TS `Check.status` for this check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankerHealthStatus {
    Ok,
    Warn,
}

/// Classify reranker health from the reranker-enabled flag and the recent
/// failure window. Pure function (no I/O) so it can be unit-tested against a
/// constructed failure list.
///
/// Threshold order is short-circuit and matches `checkRerankerHealth` in the
/// TS runtime exactly:
///   1. empty window                    → Ok (message differs by `enabled`)
///   2. auth failures >= 1              → Warn (highest priority)
///   3. `payload_too_large` failures >= 1 → Warn
///   4. transient failures >= 5         → Warn (network + timeout + `rate_limit`)
///   5. otherwise (below threshold)     → Ok
///
/// Returns `(status, message)`. The message strings mirror the TS check's
/// operator-facing guidance byte-for-byte.
#[must_use]
pub fn classify_reranker_health(
    enabled: bool,
    failures: &[RerankFailureEvent],
) -> (RerankerHealthStatus, String) {
    if failures.is_empty() {
        let message = if enabled {
            "No rerank failures in last 7 days".to_string()
        } else {
            "Reranker disabled — no failures expected".to_string()
        };
        return (RerankerHealthStatus::Ok, message);
    }

    let count = |r: RerankFailureReason| failures.iter().filter(|f| f.reason == r).count();

    let auth = count(RerankFailureReason::Auth);
    if auth > 0 {
        return (
            RerankerHealthStatus::Warn,
            format!(
                "{auth} reranker auth failure(s) in last 7 days. Fix: verify ZEROENTROPY_API_KEY and run `zbrain models doctor`."
            ),
        );
    }

    let payload = count(RerankFailureReason::PayloadTooLarge);
    if payload > 0 {
        return (
            RerankerHealthStatus::Warn,
            format!(
                "{payload} reranker payload-too-large failure(s) in last 7 days. Fix: lower `search.reranker.top_n_in` (default 30) or split very large documents."
            ),
        );
    }

    let transient = count(RerankFailureReason::Network)
        + count(RerankFailureReason::Timeout)
        + count(RerankFailureReason::RateLimit);
    if transient >= TRANSIENT_WARN_THRESHOLD {
        return (
            RerankerHealthStatus::Warn,
            format!(
                "{transient} transient reranker failure(s) in last 7 days. Search fails open to RRF order; check ZE status if persistent."
            ),
        );
    }

    (
        RerankerHealthStatus::Ok,
        format!("{} reranker failure(s) in last 7 days (below threshold)", failures.len()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn ymd_hms(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    fn event(reason: RerankFailureReason, ts: &str) -> RerankFailureEvent {
        RerankFailureEvent {
            ts: ts.to_string(),
            model: "zeroentropyai:zerank-2".to_string(),
            reason,
            query_hash: "deadbeef".to_string(),
            doc_count: 30,
            error_summary: "boom".to_string(),
            severity: "warn".to_string(),
        }
    }

    // --- filename / ISO-week edge cases -------------------------------------

    #[test]
    fn filename_uses_iso_week() {
        // 2026-07-06 is a Monday in ISO week 28 of 2026.
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);
        assert_eq!(rerank_audit_filename(now), "rerank-failures-2026-W28.jsonl");
    }

    #[test]
    fn filename_iso_year_boundary_belongs_to_previous_year() {
        // 2027-01-01 is a Friday belonging to ISO week 53 of 2026, NOT
        // week 1 of 2027. chrono's %G/%V handles this; a naive calendar-year
        // + week-of-year would mis-file it.
        let now = ymd_hms(2027, 1, 1, 8, 0, 0);
        assert_eq!(rerank_audit_filename(now), "rerank-failures-2026-W53.jsonl");
    }

    // --- writer -> reader round-trip ----------------------------------------

    #[test]
    fn log_then_read_round_trip() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path();
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);

        log_rerank_failure_at(
            audit_dir,
            RerankFailureInput {
                model: "zeroentropyai:zerank-2".to_string(),
                reason: RerankFailureReason::Auth,
                query_hash: "abc12345".to_string(),
                doc_count: 12,
                error_summary: "401 Unauthorized".to_string(),
            },
            now,
        );

        // File is named for `now`'s ISO week.
        let expected = audit_dir.join("rerank-failures-2026-W28.jsonl");
        assert!(expected.exists(), "audit file should be created for the current ISO week");

        let read = read_recent_rerank_failures_at(audit_dir, HEALTH_WINDOW_DAYS, now);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].reason, RerankFailureReason::Auth);
        assert_eq!(read[0].model, "zeroentropyai:zerank-2");
        assert_eq!(read[0].doc_count, 12);
        assert_eq!(read[0].severity, "warn");
    }

    #[test]
    fn read_empty_for_missing_dir() {
        let dir = TempDir::new().unwrap();
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);
        let read = read_recent_rerank_failures_at(dir.path(), HEALTH_WINDOW_DAYS, now);
        assert!(read.is_empty());
    }

    #[test]
    fn read_skips_rows_outside_window() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path();
        // Write a row dated well before the window into this week's file, and
        // a fresh one. Only the fresh one survives the cutoff filter.
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);
        let old = event(RerankFailureReason::Network, "2026-06-01T00:00:00Z");
        let fresh = event(RerankFailureReason::Network, "2026-07-05T00:00:00Z");
        append_event(audit_dir, &old, now).unwrap();
        append_event(audit_dir, &fresh, now).unwrap();

        let read = read_recent_rerank_failures_at(audit_dir, HEALTH_WINDOW_DAYS, now);
        assert_eq!(read.len(), 1, "only the in-window row should be returned");
        assert_eq!(read[0].ts, "2026-07-05T00:00:00Z");
    }

    #[test]
    fn read_skips_corrupt_rows() {
        use std::io::Write;
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path();
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);
        let good = event(RerankFailureReason::Timeout, "2026-07-05T00:00:00Z");
        append_event(audit_dir, &good, now).unwrap();
        // Append a garbage line to the same file.
        let path = audit_dir.join(rerank_audit_filename(now));
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{not valid json\n").unwrap();

        let read = read_recent_rerank_failures_at(audit_dir, HEALTH_WINDOW_DAYS, now);
        assert_eq!(read.len(), 1, "corrupt row is skipped, good row survives");
    }

    // --- classify_reranker_health four-branch ------------------------------

    #[test]
    fn classify_empty_enabled_is_ok() {
        let (status, msg) = classify_reranker_health(true, &[]);
        assert_eq!(status, RerankerHealthStatus::Ok);
        assert_eq!(msg, "No rerank failures in last 7 days");
    }

    #[test]
    fn classify_empty_disabled_is_ok_with_disabled_message() {
        let (status, msg) = classify_reranker_health(false, &[]);
        assert_eq!(status, RerankerHealthStatus::Ok);
        assert_eq!(msg, "Reranker disabled — no failures expected");
    }

    #[test]
    fn classify_auth_warns_at_one() {
        let f = [event(RerankFailureReason::Auth, "2026-07-05T00:00:00Z")];
        let (status, msg) = classify_reranker_health(true, &f);
        assert_eq!(status, RerankerHealthStatus::Warn);
        assert!(msg.contains("auth failure"));
        assert!(msg.contains("ZEROENTROPY_API_KEY"));
    }

    #[test]
    fn classify_payload_warns_at_one() {
        let f = [event(RerankFailureReason::PayloadTooLarge, "2026-07-05T00:00:00Z")];
        let (status, msg) = classify_reranker_health(true, &f);
        assert_eq!(status, RerankerHealthStatus::Warn);
        assert!(msg.contains("payload-too-large"));
        assert!(msg.contains("top_n_in"));
    }

    #[test]
    fn classify_auth_takes_priority_over_payload() {
        // Both present: auth short-circuits first (highest priority).
        let f = [
            event(RerankFailureReason::PayloadTooLarge, "2026-07-05T00:00:00Z"),
            event(RerankFailureReason::Auth, "2026-07-05T00:00:00Z"),
        ];
        let (status, msg) = classify_reranker_health(true, &f);
        assert_eq!(status, RerankerHealthStatus::Warn);
        assert!(msg.contains("auth failure"), "auth must win over payload");
    }

    #[test]
    fn classify_transient_below_threshold_is_ok() {
        // 4 transient failures — below the >=5 threshold, so Ok.
        let f: Vec<_> = (0..4)
            .map(|_| event(RerankFailureReason::Network, "2026-07-05T00:00:00Z"))
            .collect();
        let (status, msg) = classify_reranker_health(true, &f);
        assert_eq!(status, RerankerHealthStatus::Ok);
        assert!(msg.contains("below threshold"));
    }

    #[test]
    fn classify_transient_warns_at_five_mixed_reasons() {
        // network(2) + timeout(2) + rate_limit(1) = 5 transient → Warn.
        let f = [
            event(RerankFailureReason::Network, "2026-07-05T00:00:00Z"),
            event(RerankFailureReason::Network, "2026-07-05T00:00:01Z"),
            event(RerankFailureReason::Timeout, "2026-07-05T00:00:02Z"),
            event(RerankFailureReason::Timeout, "2026-07-05T00:00:03Z"),
            event(RerankFailureReason::RateLimit, "2026-07-05T00:00:04Z"),
        ];
        let (status, msg) = classify_reranker_health(true, &f);
        assert_eq!(status, RerankerHealthStatus::Warn);
        assert!(msg.contains("transient reranker failure"));
    }

    #[test]
    fn classify_unknown_reason_never_warns_alone() {
        // `unknown` is neither auth/payload nor transient; a lone unknown
        // stays Ok (below threshold), matching the TS fall-through.
        let f = [event(RerankFailureReason::Unknown, "2026-07-05T00:00:00Z")];
        let (status, _) = classify_reranker_health(true, &f);
        assert_eq!(status, RerankerHealthStatus::Ok);
    }
}
