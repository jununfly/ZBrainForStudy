//! Skillpack lifecycle audit logging (ISO-week rotated JSONL).
//!
//! One line per lifecycle event: scaffold / reference / doctor-run / search.
//! Rotated weekly like the rerank-failure audit that already exists.

use std::fs::{create_dir_all, OpenOptions};
use std::io::{self, Write};
use chrono::{Weekday, Datelike, Utc};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Kind of audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillpackAuditEventKind {
    /// Bundled skill scaffolded by zbrain.
    ScaffoldBundled,
    /// Third-party skill scaffolded.
    ScaffoldThirdParty,
    /// Reference diff applied by user.
    ReferenceApplied,
    /// Doctor check run that included this skillpack.
    DoctorRun,
    /// Search triggered that used this skillpack's routing.
    Search,
    /// Registry refreshed after install.
    RegistryRefresh,
}

/// Single audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillpackAuditEvent {
    /// ISO 8601 timestamp.
    pub ts: String,
    /// What happened.
    pub event: SkillpackAuditEventKind,
    /// Pack name (when applicable).
    pub pack: Option<String>,
    /// Pack version (when applicable).
    pub version: Option<String>,
    /// Source (path/url/git).
    pub source: Option<String>,
    /// Source kind.
    pub source_kind: Option<SourceKind>,
    /// Pinned commit hash (when applicable).
    pub pinned_commit: Option<String>,
    /// Tarball SHA256 (when applicable).
    pub tarball_sha256: Option<String>,
    /// Eligibility tier after scoring.
    pub tier: Option<String>,
    /// Doctor rubric score (0-100) if scored.
    pub score: Option<u8>,
    /// Outcome of the operation.
    pub outcome: AuditOutcome,
    /// Error summary (if outcome != ok).
    pub error: Option<String>,
    /// Additional arbitrary metadata.
    #[serde(flatten)]
    pub meta: Option<serde_json::Value>,
}

/// Kind of source for the skillpack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    Git,
    Tarball,
    Local,
}

/// Outcome of the audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditOutcome {
    Ok,
    Aborted,
    Error,
}

/// Compute the ISO-week filename (matches other audit modules).
/// Returns something like `skillpack-2026-W30.jsonl`.
pub fn compute_iso_week_filename() -> String {
    let now = Utc::now();
    let week = now.iso_week();
    let year = week.year();
    let week = week.week();
    format!("skillpack-{year}-W{week:02}.jsonl")
}

/// Resolve the audit directory from env or default.
pub fn resolve_audit_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("ZBRAIN_AUDIT_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir.trim());
        }
    }
    // Default: ~/.zbrain/audit/
    if let Some(home) = dirs::home_dir() {
        home.join(".zbrain").join("audit")
    } else {
        std::path::PathBuf::from("./audit")
    }
}

/// Append an audit event to the weekly rotated audit log.
/// Best effort: never fails — failure goes to stderr warning.
pub fn log_skillpack_event(event: &SkillpackAuditEvent) {
    let audit_dir = resolve_audit_dir();
    let _ = create_dir_all(&audit_dir);

    let filename = compute_iso_week_filename();
    let path = audit_dir.join(filename);

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("zbrain: failed to open skillpack audit log: {e}");
                return;
            }
        };

    let line = match serde_json::to_string(event) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("zbrain: failed to serialize audit event: {e}");
            return;
        }
    };

    writeln!(file, "{}", line).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filename() {
        // Just check it produces a non-empty string.
        let name = compute_iso_week_filename();
        assert!(!name.is_empty());
        assert!(name.ends_with(".jsonl"));
    }
}
