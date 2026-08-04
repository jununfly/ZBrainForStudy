//! Phantom-redirect audit trail (writer + reader).
//!
//! Port of `src/core/facts/phantom-audit.ts` (v0.35.5 / v0.40.4.0). Writes
//! one JSONL row per phantom-redirect decision to
//! `<audit_dir>/phantoms-YYYY-Www.jsonl` (ISO-week rotated, mirroring
//! `rerank_audit.rs`). Records BOTH success (`redirected`) and informational
//! skip outcomes (`ambiguous`, `drift`, `no_canonical`, `not_phantom_has_residue`,
//! `pass_skipped_lock_busy`) so operators can triage what the cycle saw
//! without re-running it.
//!
//! Sister surface of `src/core/facts/stub-guard-audit.ts` — kept separate so
//! each file has a stable schema and the doctor checks don't need a
//! discriminator. Best-effort writes: failures emit a stderr line but never
//! throw. Failure-ONLY is NOT the policy here (unlike rerank) — the
//! phantom pass logs every decision so a later `zbrain doctor` can surface
//! pending redirects.

use crate::schema_pack::candidate_audit::compute_iso_week_name;
use crate::time::current_utc_iso8601;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Stable 6-outcome union (mirrors TS `PhantomOutcome`). Serialized as
/// `snake_case` strings on the wire so rows round-trip with the TS writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhantomOutcome {
    /// Default variant — never emitted as-is (callers always set `outcome`
    /// explicitly on `PhantomEventInput`), present only so `PhantomEventInput`
    /// can `#[derive(Default)]`.
    #[default]
    Redirected,
    Ambiguous,
    Drift,
    NoCanonical,
    NotPhantomHasResidue,
    PassSkippedLockBusy,
}

/// A prefix-expansion candidate row attached to an `ambiguous` event. Mirrors
/// the TS `PhantomAuditEvent.candidates` shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhantomCandidate {
    pub slug: String,
    pub connection_count: i64,
}

/// A single phantom-redirect audit row. Field names + `snake_case` outcome
/// match the TS `PhantomAuditEvent` interface exactly, so during the
/// TS→Rust migration either runtime can read rows the other wrote from the
/// same `~/.zbrain/audit/` directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhantomAuditEvent {
    /// ISO-8601 UTC timestamp of when the decision was recorded.
    pub ts: String,
    /// The phantom slug being considered. Absent for `pass_skipped_lock_busy`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phantom_slug: Option<String>,
    /// Resolved canonical slug (present on `redirected` + as context on
    /// `ambiguous`/`drift`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_slug: Option<String>,
    /// Tagged decision outcome.
    pub outcome: PhantomOutcome,
    /// Number of fact rows migrated to the canonical (redirected only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fact_count: Option<i64>,
    /// Source id the phantom belongs to.
    pub source_id: String,
    /// Optional human-readable reason (exception summary on `drift`, etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Prefix candidates that made a phantom ambiguous.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<PhantomCandidate>>,
}

/// ISO-week-rotated filename: `phantoms-YYYY-Www.jsonl`.
#[must_use]
pub fn compute_phantom_audit_filename(now: DateTime<Utc>) -> String {
    format!("phantoms-{}.jsonl", compute_iso_week_name(now))
}

/// Absent-fields (Option::None) are dropped on the wire so the serialized row
/// matches TS `phantom-audit.ts`'s spread-with-conditional shape (which omits
/// absent fields rather than emitting `field: null`).
#[derive(Debug, Clone, Default)]
pub struct PhantomEventInput {
    pub phantom_slug: Option<String>,
    pub canonical_slug: Option<String>,
    pub outcome: PhantomOutcome,
    pub fact_count: Option<i64>,
    pub source_id: String,
    pub reason: Option<String>,
    pub candidates: Option<Vec<PhantomCandidate>>,
}

/// Append one phantom-redirect event to the current ISO week's JSONL file
/// under `audit_dir`. Best-effort: on any I/O/serialize error a warning is
/// written to stderr and `()` is returned — the caller's cycle continues.
pub fn log_phantom_event(audit_dir: &Path, input: PhantomEventInput) {
    log_phantom_event_at(audit_dir, input, Utc::now());
}

/// [`log_phantom_event`] with an injectable clock, for tests that need to pin
/// the ISO week / timestamp deterministically.
pub fn log_phantom_event_at(audit_dir: &Path, input: PhantomEventInput, now: DateTime<Utc>) {
    let event = PhantomAuditEvent {
        ts: current_utc_iso8601(),
        phantom_slug: input.phantom_slug,
        canonical_slug: input.canonical_slug,
        outcome: input.outcome,
        fact_count: input.fact_count,
        source_id: input.source_id,
        reason: input.reason,
        candidates: input.candidates,
    };
    if let Err(e) = append_event(audit_dir, &event, now) {
        eprintln!("[zbrain] phantom audit write failed ({e}); cycle continues");
    }
}

fn audit_file_path(audit_dir: &Path, now: DateTime<Utc>) -> std::path::PathBuf {
    audit_dir.join(compute_phantom_audit_filename(now))
}

fn append_event(
    audit_dir: &Path,
    event: &PhantomAuditEvent,
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

/// Read phantom-redirect events from the last `days` window under `audit_dir`.
///
/// Walks the current + previous ISO-week files (so a 7-day window straddling
/// Monday-midnight stays covered), then keeps only rows whose `ts` parses and
/// is >= `now - days`. Missing files / corrupt rows are skipped silently —
/// the audit trail is informational and must never block a consumer.
#[must_use]
pub fn read_recent_phantom_events(audit_dir: &Path, days: i64) -> Vec<PhantomAuditEvent> {
    read_recent_phantom_events_at(audit_dir, days, Utc::now())
}

/// [`read_recent_phantom_events`] with an injectable clock, for tests.
#[must_use]
pub fn read_recent_phantom_events_at(
    audit_dir: &Path,
    days: i64,
    now: DateTime<Utc>,
) -> Vec<PhantomAuditEvent> {
    let cutoff = now - Duration::days(days);
    let filenames = [
        compute_phantom_audit_filename(now),
        compute_phantom_audit_filename(now - Duration::days(7)),
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
            let Ok(event) = serde_json::from_str::<PhantomAuditEvent>(line) else {
                continue; // corrupt row — skip
            };
            if let Ok(ts) = DateTime::parse_from_rfc3339(&event.ts) {
                if ts.with_timezone(&Utc) >= cutoff {
                    out.push(event);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn ymd_hms(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, mi, s).unwrap()
    }

    #[test]
    fn filename_uses_iso_week() {
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);
        assert_eq!(compute_phantom_audit_filename(now), "phantoms-2026-W28.jsonl");
    }

    #[test]
    fn filename_iso_year_boundary() {
        let now = ymd_hms(2027, 1, 1, 8, 0, 0);
        assert_eq!(compute_phantom_audit_filename(now), "phantoms-2026-W53.jsonl");
    }

    #[test]
    fn outcome_wire_values_snake_case() {
        // Ensure the 6 variants serialize to the TS wire strings.
        let cases = [
            (PhantomOutcome::Redirected, "redirected"),
            (PhantomOutcome::Ambiguous, "ambiguous"),
            (PhantomOutcome::Drift, "drift"),
            (PhantomOutcome::NoCanonical, "no_canonical"),
            (PhantomOutcome::NotPhantomHasResidue, "not_phantom_has_residue"),
            (PhantomOutcome::PassSkippedLockBusy, "pass_skipped_lock_busy"),
        ];
        for (variant, want) in cases {
            let v = serde_json::to_value(variant).unwrap();
            assert_eq!(v, serde_json::Value::String(want.to_string()), "{variant:?}");
        }
    }

    #[test]
    fn log_then_read_round_trip_absent_fields_omitted() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path();
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);

        log_phantom_event_at(
            audit_dir,
            PhantomEventInput {
                phantom_slug: Some("alice".to_string()),
                canonical_slug: Some("people/alice-example".to_string()),
                outcome: PhantomOutcome::Redirected,
                fact_count: Some(3),
                source_id: "default".to_string(),
                reason: None,
                candidates: None,
            },
            now,
        );

        let expected = audit_dir.join("phantoms-2026-W28.jsonl");
        assert!(expected.exists(), "audit file created for current ISO week");

        let read = read_recent_phantom_events_at(audit_dir, 7, now);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].outcome, PhantomOutcome::Redirected);
        assert_eq!(read[0].phantom_slug.as_deref(), Some("alice"));
        assert_eq!(read[0].canonical_slug.as_deref(), Some("people/alice-example"));
        assert_eq!(read[0].fact_count, Some(3));
        assert_eq!(read[0].source_id, "default");
        assert!(read[0].reason.is_none());
        assert!(read[0].candidates.is_none());
    }

    #[test]
    fn ambiguous_event_carries_candidates() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path();
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);

        log_phantom_event_at(
            audit_dir,
            PhantomEventInput {
                phantom_slug: Some("alice".to_string()),
                canonical_slug: Some("people/alice-example".to_string()),
                outcome: PhantomOutcome::Ambiguous,
                fact_count: None,
                source_id: "default".to_string(),
                reason: None,
                candidates: Some(vec![
                    PhantomCandidate { slug: "people/alice-a".to_string(), connection_count: 5 },
                    PhantomCandidate { slug: "people/alice-b".to_string(), connection_count: 2 },
                ]),
            },
            now,
        );

        let read = read_recent_phantom_events_at(audit_dir, 7, now);
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].outcome, PhantomOutcome::Ambiguous);
        let cands = read[0].candidates.as_ref().unwrap();
        assert_eq!(cands.len(), 2);
        assert_eq!(cands[0].slug, "people/alice-a");
        assert_eq!(cands[0].connection_count, 5);
    }

    #[test]
    fn read_empty_for_missing_dir() {
        let dir = TempDir::new().unwrap();
        let read = read_recent_phantom_events_at(dir.path(), 7, ymd_hms(2026, 7, 6, 12, 0, 0));
        assert!(read.is_empty());
    }

    #[test]
    fn read_skips_rows_outside_window() {
        let dir = TempDir::new().unwrap();
        let audit_dir = dir.path();
        let now = ymd_hms(2026, 7, 6, 12, 0, 0);
        // Old row (well before the 7-day window) — should be filtered out.
        log_phantom_event_at(
            audit_dir,
            PhantomEventInput {
                phantom_slug: Some("old".to_string()),
                outcome: PhantomOutcome::NoCanonical,
                source_id: "default".to_string(),
                ..Default::default()
            },
            now - Duration::days(20),
        );
        // Fresh row — should survive.
        log_phantom_event_at(
            audit_dir,
            PhantomEventInput {
                phantom_slug: Some("fresh".to_string()),
                outcome: PhantomOutcome::NoCanonical,
                source_id: "default".to_string(),
                ..Default::default()
            },
            now,
        );

        let read = read_recent_phantom_events_at(audit_dir, 7, now);
        assert_eq!(read.len(), 1, "only the in-window row should survive");
        assert_eq!(read[0].phantom_slug.as_deref(), Some("fresh"));
    }
}
