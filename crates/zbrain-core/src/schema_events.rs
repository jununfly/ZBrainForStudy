//! Schema CLI / cycle event audit (v0.39 T15).
//!
//! Port of `src/core/schema-events.ts` (removed from the TS tree in the
//! part11 minions teardown; recovered from git history for this port).
//!
//! JSONL at `~/.zbrain/audit/schema-events-YYYY-Www.jsonl`, ISO-week rotation
//! matching the candidate audit (`schema_pack::candidate_audit`). Best-effort
//! writes — stderr warn on disk failure, NEVER panics or errors.
//!
//! Feeds `zbrain schema usage --since 30d` for the experimental-tier
//! telemetry gate: v0.40+ retro reads this data to decide which cathedral
//! commands are demand-proven vs candidates for deprecation.
//!
//! Privacy: records ONLY verb names + timestamps + outcome. No pack content,
//! no slug names, no user data.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::schema_pack::candidate_audit::{compute_iso_week_name, resolve_audit_dir};

/// Outcome of an audited schema verb.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchemaEventOutcome {
    Success,
    Error,
    Unknown,
}

/// A single audited schema event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaEventRecord {
    /// ISO-8601 timestamp.
    pub ts: String,
    /// Verb name, e.g. `cycle:schema-suggest`.
    pub verb: String,
    pub outcome: SchemaEventOutcome,
    /// Optional flags — e.g. `source=default`. No values with user data,
    /// just flag names / counts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<Vec<String>>,
}

/// Input to [`log_schema_event`] (the `ts` is stamped at write time).
#[derive(Debug, Clone)]
pub struct SchemaEventInput {
    pub verb: String,
    pub outcome: SchemaEventOutcome,
    pub flags: Option<Vec<String>>,
}

/// Path of the schema-events JSONL for the week of `date`.
pub fn compute_schema_event_path(date: DateTime<Utc>) -> PathBuf {
    resolve_audit_dir().join(format!("schema-events-{}.jsonl", compute_iso_week_name(date)))
}

/// Best-effort append of a schema event. Mirrors the TS contract: on any
/// failure it warns to stderr and returns — it never propagates errors.
pub fn log_schema_event(input: &SchemaEventInput) {
    let now = Utc::now();
    let record = SchemaEventRecord {
        ts: now.to_rfc3339(),
        verb: input.verb.clone(),
        outcome: input.outcome,
        flags: input.flags.clone(),
    };
    if let Err(e) = try_append(&record, now) {
        eprintln!("[schema-events] audit write failed: {e}");
    }
}

fn try_append(record: &SchemaEventRecord, now: DateTime<Utc>) -> std::io::Result<()> {
    let path = compute_schema_event_path(now);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(record)?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Read schema events from the last `days` days across all weekly JSONL
/// files in the audit dir. Used by `zbrain schema usage --since 30d`.
/// Malformed lines and unreadable files are skipped silently (TS parity).
pub fn read_recent_schema_events(days: u32) -> Vec<SchemaEventRecord> {
    let mut out = Vec::new();
    let dir = resolve_audit_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let cutoff = Utc::now() - chrono::Duration::days(days as i64);
    let files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("schema-events-") && n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(rec) = serde_json::from_str::<SchemaEventRecord>(line) else {
                continue;
            };
            let Ok(ts) = DateTime::parse_from_rfc3339(&rec.ts) else {
                continue;
            };
            if ts.with_timezone(&Utc) >= cutoff {
                out.push(rec);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&SchemaEventOutcome::Success).unwrap(), "\"success\"");
        assert_eq!(serde_json::to_string(&SchemaEventOutcome::Error).unwrap(), "\"error\"");
        assert_eq!(serde_json::to_string(&SchemaEventOutcome::Unknown).unwrap(), "\"unknown\"");
    }

    #[test]
    fn record_round_trips_without_flags() {
        let rec = SchemaEventRecord {
            ts: "2026-07-29T00:00:00Z".into(),
            verb: "cycle:schema-suggest".into(),
            outcome: SchemaEventOutcome::Success,
            flags: None,
        };
        let json = serde_json::to_string(&rec).unwrap();
        // `flags` omitted when None (TS parity: optional field).
        assert!(!json.contains("flags"));
        let back: SchemaEventRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }

    #[test]
    fn log_and_read_recent_round_trip() {
        // Isolate ~/.zbrain on this thread so the audit file lands in a
        // private temp home (no process-global env mutation).
        let _home = crate::paths::ScopedTestHome::new();

        log_schema_event(&SchemaEventInput {
            verb: "cycle:schema-suggest".into(),
            outcome: SchemaEventOutcome::Success,
            flags: Some(vec!["source=default".into(), "count=3".into()]),
        });
        log_schema_event(&SchemaEventInput {
            verb: "schema:detect".into(),
            outcome: SchemaEventOutcome::Error,
            flags: None,
        });

        let events = read_recent_schema_events(7);
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|e| e.verb == "cycle:schema-suggest"
            && e.outcome == SchemaEventOutcome::Success
            && e.flags.as_deref() == Some(&["source=default".to_string(), "count=3".to_string()][..])));
        assert!(events
            .iter()
            .any(|e| e.verb == "schema:detect" && e.outcome == SchemaEventOutcome::Error && e.flags.is_none()));
    }

    #[test]
    fn read_recent_filters_by_cutoff() {
        let _home = crate::paths::ScopedTestHome::new();

        // Write one fresh event via the public API…
        log_schema_event(&SchemaEventInput {
            verb: "fresh".into(),
            outcome: SchemaEventOutcome::Success,
            flags: None,
        });
        // …and one stale record (40 days old) directly into the same file.
        let stale = SchemaEventRecord {
            ts: (Utc::now() - chrono::Duration::days(40)).to_rfc3339(),
            verb: "stale".into(),
            outcome: SchemaEventOutcome::Success,
            flags: None,
        };
        let path = compute_schema_event_path(Utc::now());
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", serde_json::to_string(&stale).unwrap()).unwrap();

        let events = read_recent_schema_events(30);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verb, "fresh");
    }

    #[test]
    fn read_recent_skips_malformed_lines() {
        let _home = crate::paths::ScopedTestHome::new();

        log_schema_event(&SchemaEventInput {
            verb: "good".into(),
            outcome: SchemaEventOutcome::Success,
            flags: None,
        });
        let path = compute_schema_event_path(Utc::now());
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{not json").unwrap();
        writeln!(f, "{{\"ts\":\"not-a-date\",\"verb\":\"x\",\"outcome\":\"success\"}}").unwrap();

        let events = read_recent_schema_events(7);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].verb, "good");
    }

    #[test]
    fn read_recent_empty_dir_returns_empty() {
        let _home = crate::paths::ScopedTestHome::new();
        assert!(read_recent_schema_events(30).is_empty());
    }
}
