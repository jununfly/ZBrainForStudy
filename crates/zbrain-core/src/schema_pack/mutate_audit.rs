//! Schema pack mutation audit — JSONL append-only log with privacy redaction.
//!
//! Ported from TS `src/core/schema-pack/mutate-audit.ts`.
//!
//! Records are appended to `~/.zbrain/audit/schema-mutations-YYYY-Www.jsonl`
//! (ISO week naming). All I/O is best-effort: failures are logged to stderr
//! and never propagated, so audit never blocks a mutation.
//!
//! Privacy (D20 + codex C10):
//! - Type names are SHA-8 redacted by default (first 4 bytes of SHA-256 = 8 hex).
//! - Path prefixes are truncated to the first segment.
//! - Pack names are NOT redacted (user-chosen, not PII).
//! - `ZBRAIN_SCHEMA_AUDIT_VERBOSE=1` disables redaction.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{Datelike, DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// The 11 mutation operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationOp {
    AddType,
    RemoveType,
    UpdateType,
    AddAlias,
    RemoveAlias,
    AddPrefix,
    RemovePrefix,
    AddLinkType,
    RemoveLinkType,
    SetExtractable,
    SetExpertRouting,
}

impl MutationOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AddType => "add_type",
            Self::RemoveType => "remove_type",
            Self::UpdateType => "update_type",
            Self::AddAlias => "add_alias",
            Self::RemoveAlias => "remove_alias",
            Self::AddPrefix => "add_prefix",
            Self::RemovePrefix => "remove_prefix",
            Self::AddLinkType => "add_link_type",
            Self::RemoveLinkType => "remove_link_type",
            Self::SetExtractable => "set_extractable",
            Self::SetExpertRouting => "set_expert_routing",
        }
    }
}

/// Who triggered the mutation.
pub type MutationActor = String;

/// Success or failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MutationOutcome {
    Success,
    Failure,
}

/// A single audit record (one JSONL line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationAuditRecord {
    pub ts: String,
    pub op: MutationOp,
    pub pack: String,
    pub type_or_hash: Option<String>,
    pub type_redacted: bool,
    pub prefix_first_seg: Option<String>,
    pub actor: MutationActor,
    pub outcome: MutationOutcome,
    pub reason: Option<String>,
    pub prev_sha8: Option<String>,
    pub new_sha8: Option<String>,
    pub batch_id: Option<String>,
}

/// Options for logging a successful mutation.
#[derive(Debug, Clone)]
pub struct LogMutationOpts {
    pub op: MutationOp,
    pub pack: String,
    pub type_name: Option<String>,
    pub prefix: Option<String>,
    pub actor: MutationActor,
    pub prev_sha8: Option<String>,
    pub new_sha8: Option<String>,
    pub batch_id: Option<String>,
}

/// Options for logging a failed mutation.
#[derive(Debug, Clone)]
pub struct LogMutationFailureOpts {
    pub base: LogMutationOpts,
    pub reason: String,
}

/// Aggregated summary of mutation records.
#[derive(Debug, Clone, Default)]
pub struct MutationSummary {
    pub total: usize,
    pub by_op: BTreeMap<String, usize>,
    pub by_outcome: BTreeMap<String, usize>,
    pub by_pack: BTreeMap<String, usize>,
    pub by_reason: BTreeMap<String, usize>,
    pub by_actor: BTreeMap<String, usize>,
}

// ---------------------------------------------------------------------------
// Privacy helpers
// ---------------------------------------------------------------------------

/// SHA-8: first 4 bytes of SHA-256 → 8 hex chars.
fn sha8(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..4])
}

/// Whether verbose (non-redacted) mode is enabled.
fn is_verbose() -> bool {
    std::env::var("ZBRAIN_SCHEMA_AUDIT_VERBOSE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Redact a type name: sha8 unless verbose.
fn redact_type(name: &str) -> (String, bool) {
    if is_verbose() {
        (name.to_string(), false)
    } else {
        (sha8(name), true)
    }
}

/// Redact a prefix: keep only the first path segment.
fn redact_prefix(prefix: &str) -> Option<String> {
    let first = prefix.split('/').next()?;
    if first.is_empty() {
        None
    } else {
        Some(first.to_string())
    }
}

// ---------------------------------------------------------------------------
// Audit path
// ---------------------------------------------------------------------------

/// Compute the audit file path for the given date (ISO week naming).
///
/// Format: `~/.zbrain/audit/schema-mutations-YYYY-Www.jsonl`
pub fn compute_mutate_audit_path(now: Option<DateTime<Utc>>) -> PathBuf {
    let now = now.unwrap_or_else(Utc::now);
    let year = now.iso_week().year();
    let week = now.iso_week().week();
    let filename = format!("schema-mutations-{year}-W{week:02}.jsonl");

    crate::paths::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("audit")
        .join(filename)
}

// ---------------------------------------------------------------------------
// Build record
// ---------------------------------------------------------------------------

fn build_record(
    opts: &LogMutationOpts,
    outcome: MutationOutcome,
    reason: Option<&str>,
) -> MutationAuditRecord {
    let (type_or_hash, type_redacted) = match &opts.type_name {
        Some(name) => {
            let (h, r) = redact_type(name);
            (Some(h), r)
        }
        None => (None, false),
    };

    let prefix_first_seg = opts.prefix.as_deref().and_then(redact_prefix);

    MutationAuditRecord {
        ts: Utc::now().to_rfc3339(),
        op: opts.op,
        pack: opts.pack.clone(),
        type_or_hash,
        type_redacted,
        prefix_first_seg,
        actor: opts.actor.clone(),
        outcome,
        reason: reason.map(|s| s.to_string()),
        prev_sha8: opts.prev_sha8.clone(),
        new_sha8: opts.new_sha8.clone(),
        batch_id: opts.batch_id.clone(),
    }
}

// ---------------------------------------------------------------------------
// Logging (best-effort, never panics)
// ---------------------------------------------------------------------------

/// Append a record as JSONL to the audit file. Best-effort.
fn append_record(record: &MutationAuditRecord) {
    let path = compute_mutate_audit_path(None);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("[schema-audit] cannot create audit dir: {e}");
            return;
        }
    }
    let line = serde_json::to_string(record).unwrap_or_default();
    if let Err(e) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()))
    {
        eprintln!("[schema-audit] cannot write audit record: {e}");
    }
}

/// Log a successful mutation.
pub fn log_mutation_success(opts: &LogMutationOpts) {
    let record = build_record(opts, MutationOutcome::Success, None);
    append_record(&record);
}

/// Log a failed mutation.
pub fn log_mutation_failure(opts: &LogMutationFailureOpts) {
    let record = build_record(&opts.base, MutationOutcome::Failure, Some(&opts.reason));
    append_record(&record);
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read recent mutation records from the audit log.
///
/// Scans all `schema-mutations-*.jsonl` files in the audit directory,
/// filters by `days_back` (default 30), and skips malformed lines.
pub fn read_recent_mutations(days_back: Option<u32>) -> Vec<MutationAuditRecord> {
    let days = days_back.unwrap_or(30);
    let cutoff = Utc::now() - chrono::Duration::days(days as i64);

    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    let audit_dir = PathBuf::from(home).join(".zbrain").join("audit");

    let mut records = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&audit_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("schema-mutations-") || !name.ends_with(".jsonl") {
                continue;
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                for line in content.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(record) = serde_json::from_str::<MutationAuditRecord>(line) {
                        // Filter by timestamp
                        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&record.ts) {
                            if ts.with_timezone(&Utc) >= cutoff {
                                records.push(record);
                            }
                        }
                    }
                    // Skip malformed lines silently
                }
            }
        }
    }
    records
}

// ---------------------------------------------------------------------------
// Summarize
// ---------------------------------------------------------------------------

/// Aggregate mutation records into a summary.
pub fn summarize_mutations(records: &[MutationAuditRecord]) -> MutationSummary {
    let mut summary = MutationSummary::default();
    summary.total = records.len();

    for r in records {
        *summary.by_op.entry(r.op.as_str().to_string()).or_default() += 1;
        let outcome_str = match r.outcome {
            MutationOutcome::Success => "success",
            MutationOutcome::Failure => "failure",
        };
        *summary.by_outcome.entry(outcome_str.to_string()).or_default() += 1;
        *summary.by_pack.entry(r.pack.clone()).or_default() += 1;
        if let Some(ref reason) = r.reason {
            *summary.by_reason.entry(reason.clone()).or_default() += 1;
        }
        // Actor: mcp:* → mcp bucket
        let actor_bucket = if r.actor.starts_with("mcp:") {
            "mcp".to_string()
        } else {
            r.actor.clone()
        };
        *summary.by_actor.entry(actor_bucket).or_default() += 1;
    }

    summary
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha8_produces_8_hex_chars() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let h = sha8("person");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sha8_is_deterministic() {
        let _guard = crate::schema_pack::lock_schema_fs();
        assert_eq!(sha8("person"), sha8("person"));
        assert_ne!(sha8("person"), sha8("company"));
    }

    #[test]
    fn redact_type_returns_hash_when_not_verbose() {
        let _guard = crate::schema_pack::lock_schema_fs();
        // Ensure verbose is off (save and restore)
        let prev = std::env::var("ZBRAIN_SCHEMA_AUDIT_VERBOSE");
        std::env::remove_var("ZBRAIN_SCHEMA_AUDIT_VERBOSE");

        let (h, redacted) = redact_type("person");
        assert!(redacted, "type should be redacted when not verbose");
        assert_eq!(h.len(), 8);
        assert_ne!(h, "person");

        // Restore
        if let Ok(v) = prev {
            std::env::set_var("ZBRAIN_SCHEMA_AUDIT_VERBOSE", v);
        }
    }

    #[test]
    fn redact_type_returns_plain_when_verbose() {
        let _guard = crate::schema_pack::lock_schema_fs();
        std::env::set_var("ZBRAIN_SCHEMA_AUDIT_VERBOSE", "1");
        let (h, redacted) = redact_type("person");
        assert!(!redacted, "type should NOT be redacted when verbose");
        assert_eq!(h, "person");
        std::env::remove_var("ZBRAIN_SCHEMA_AUDIT_VERBOSE");
    }

    #[test]
    fn redact_prefix_keeps_first_segment() {
        let _guard = crate::schema_pack::lock_schema_fs();
        assert_eq!(redact_prefix("people/"), Some("people".to_string()));
        assert_eq!(redact_prefix("wiki/concepts/"), Some("wiki".to_string()));
        assert_eq!(redact_prefix("notes/"), Some("notes".to_string()));
    }

    #[test]
    fn redact_prefix_empty_returns_none() {
        let _guard = crate::schema_pack::lock_schema_fs();
        assert_eq!(redact_prefix(""), None);
        assert_eq!(redact_prefix("/"), None);
    }

    #[test]
    fn compute_audit_path_uses_iso_week() {
        let _guard = crate::schema_pack::lock_schema_fs();
        // 2026-01-01 is Thursday → ISO week 1 of 2026
        let dt = chrono::DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let path = compute_mutate_audit_path(Some(dt));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, "schema-mutations-2026-W01.jsonl");
    }

    #[test]
    fn compute_audit_path_week_27() {
        let _guard = crate::schema_pack::lock_schema_fs();
        // 2026-07-01 is Wednesday → ISO week 27 of 2026
        let dt = chrono::DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let path = compute_mutate_audit_path(Some(dt));
        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, "schema-mutations-2026-W27.jsonl");
    }

    #[test]
    fn build_record_success_has_no_reason() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let opts = LogMutationOpts {
            op: MutationOp::AddType,
            pack: "my-pack".to_string(),
            type_name: Some("person".to_string()),
            prefix: Some("people/".to_string()),
            actor: "cli".to_string(),
            prev_sha8: Some("abc12345".to_string()),
            new_sha8: Some("def67890".to_string()),
            batch_id: None,
        };
        std::env::remove_var("ZBRAIN_SCHEMA_AUDIT_VERBOSE");
        let record = build_record(&opts, MutationOutcome::Success, None);
        assert_eq!(record.pack, "my-pack");
        assert_eq!(record.outcome, MutationOutcome::Success);
        assert!(record.reason.is_none());
        assert!(record.type_redacted, "type should be redacted");
        assert_ne!(record.type_or_hash.as_deref(), Some("person"));
        assert_eq!(record.prefix_first_seg.as_deref(), Some("people"));
    }

    #[test]
    fn build_record_failure_has_reason() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let opts = LogMutationOpts {
            op: MutationOp::RemoveType,
            pack: "my-pack".to_string(),
            type_name: Some("note".to_string()),
            prefix: None,
            actor: "cli".to_string(),
            prev_sha8: Some("abc12345".to_string()),
            new_sha8: None,
            batch_id: None,
        };
        let record = build_record(&opts, MutationOutcome::Failure, Some("STILL_REFERENCED"));
        assert_eq!(record.outcome, MutationOutcome::Failure);
        assert_eq!(record.reason.as_deref(), Some("STILL_REFERENCED"));
        assert!(record.new_sha8.is_none());
    }

    #[test]
    fn build_record_no_type_name() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let opts = LogMutationOpts {
            op: MutationOp::AddLinkType,
            pack: "my-pack".to_string(),
            type_name: None,
            prefix: None,
            actor: "mcp:claude".to_string(),
            prev_sha8: None,
            new_sha8: Some("abc12345".to_string()),
            batch_id: Some("batch-123".to_string()),
        };
        let record = build_record(&opts, MutationOutcome::Success, None);
        assert!(record.type_or_hash.is_none());
        assert!(!record.type_redacted);
        assert_eq!(record.batch_id.as_deref(), Some("batch-123"));
    }

    #[test]
    fn summarize_counts_by_op_and_outcome() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let records = vec![
            MutationAuditRecord {
                ts: Utc::now().to_rfc3339(),
                op: MutationOp::AddType,
                pack: "p1".into(),
                type_or_hash: None,
                type_redacted: false,
                prefix_first_seg: None,
                actor: "cli".into(),
                outcome: MutationOutcome::Success,
                reason: None,
                prev_sha8: None,
                new_sha8: None,
                batch_id: None,
            },
            MutationAuditRecord {
                ts: Utc::now().to_rfc3339(),
                op: MutationOp::AddType,
                pack: "p1".into(),
                type_or_hash: None,
                type_redacted: false,
                prefix_first_seg: None,
                actor: "mcp:claude".into(),
                outcome: MutationOutcome::Failure,
                reason: Some("PACK_READONLY".into()),
                prev_sha8: None,
                new_sha8: None,
                batch_id: None,
            },
            MutationAuditRecord {
                ts: Utc::now().to_rfc3339(),
                op: MutationOp::RemoveType,
                pack: "p2".into(),
                type_or_hash: None,
                type_redacted: false,
                prefix_first_seg: None,
                actor: "cli".into(),
                outcome: MutationOutcome::Success,
                reason: None,
                prev_sha8: None,
                new_sha8: None,
                batch_id: None,
            },
        ];
        let s = summarize_mutations(&records);
        assert_eq!(s.total, 3);
        assert_eq!(s.by_op.get("add_type"), Some(&2));
        assert_eq!(s.by_op.get("remove_type"), Some(&1));
        assert_eq!(s.by_outcome.get("success"), Some(&2));
        assert_eq!(s.by_outcome.get("failure"), Some(&1));
        assert_eq!(s.by_pack.get("p1"), Some(&2));
        assert_eq!(s.by_pack.get("p2"), Some(&1));
        assert_eq!(s.by_reason.get("PACK_READONLY"), Some(&1));
        // mcp:claude → mcp bucket
        assert_eq!(s.by_actor.get("mcp"), Some(&1));
        assert_eq!(s.by_actor.get("cli"), Some(&2));
    }

    #[test]
    fn summarize_empty_records() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let s = summarize_mutations(&[]);
        assert_eq!(s.total, 0);
        assert!(s.by_op.is_empty());
    }

    #[test]
    fn mutation_op_as_str_roundtrip() {
        let _guard = crate::schema_pack::lock_schema_fs();
        for op in [
            MutationOp::AddType,
            MutationOp::RemoveType,
            MutationOp::UpdateType,
            MutationOp::AddAlias,
            MutationOp::RemoveAlias,
            MutationOp::AddPrefix,
            MutationOp::RemovePrefix,
            MutationOp::AddLinkType,
            MutationOp::RemoveLinkType,
            MutationOp::SetExtractable,
            MutationOp::SetExpertRouting,
        ] {
            let s = op.as_str();
            assert!(!s.is_empty());
            // Deserialize back
            let json = serde_json::to_string(&op).unwrap();
            let back: MutationOp = serde_json::from_str(&json).unwrap();
            assert_eq!(op, back);
        }
    }

    #[test]
    fn log_and_read_roundtrip_with_tempdir() {
        let _guard = crate::schema_pack::lock_schema_fs();
        // Use a temp HOME to isolate the test
        let tmp = std::env::temp_dir().join(format!("zbrain-audit-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let prev_home = std::env::var("HOME").ok();
        let prev_profile = std::env::var("USERPROFILE").ok();
        std::env::set_var("HOME", &tmp);
        std::env::set_var("USERPROFILE", &tmp);
        std::env::remove_var("ZBRAIN_SCHEMA_AUDIT_VERBOSE");

        let opts = LogMutationOpts {
            op: MutationOp::AddType,
            pack: "test-pack".to_string(),
            type_name: Some("person".to_string()),
            prefix: Some("people/".to_string()),
            actor: "tests/unit".to_string(),
            prev_sha8: Some("aaaa1111".to_string()),
            new_sha8: Some("bbbb2222".to_string()),
            batch_id: None,
        };
        log_mutation_success(&opts);

        let records = read_recent_mutations(Some(1));
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.pack, "test-pack");
        assert_eq!(r.op, MutationOp::AddType);
        assert_eq!(r.outcome, MutationOutcome::Success);
        assert!(r.type_redacted);
        assert_ne!(r.type_or_hash.as_deref(), Some("person"));
        assert_eq!(r.prefix_first_seg.as_deref(), Some("people"));
        assert_eq!(r.actor, "tests/unit");
        assert_eq!(r.prev_sha8.as_deref(), Some("aaaa1111"));
        assert_eq!(r.new_sha8.as_deref(), Some("bbbb2222"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        }
        if let Some(p) = prev_profile {
            std::env::set_var("USERPROFILE", p);
        }
    }

    #[test]
    fn log_failure_and_read() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let tmp = std::env::temp_dir().join(format!("zbrain-audit-fail-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let prev_home = std::env::var("HOME").ok();
        let prev_profile = std::env::var("USERPROFILE").ok();
        std::env::set_var("HOME", &tmp);
        std::env::set_var("USERPROFILE", &tmp);

        let opts = LogMutationFailureOpts {
            base: LogMutationOpts {
                op: MutationOp::RemoveType,
                pack: "readonly-pack".to_string(),
                type_name: Some("note".to_string()),
                prefix: None,
                actor: "cli".to_string(),
                prev_sha8: None,
                new_sha8: None,
                batch_id: None,
            },
            reason: "PACK_READONLY".to_string(),
        };
        log_mutation_failure(&opts);

        let records = read_recent_mutations(Some(1));
        let failures: Vec<_> = records
            .iter()
            .filter(|r| r.outcome == MutationOutcome::Failure)
            .collect();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].reason.as_deref(), Some("PACK_READONLY"));

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        }
        if let Some(p) = prev_profile {
            std::env::set_var("USERPROFILE", p);
        }
    }

    #[test]
    fn read_malformed_lines_are_skipped() {
        let _guard = crate::schema_pack::lock_schema_fs();
        let tmp = std::env::temp_dir().join(format!("zbrain-audit-malformed-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let prev_home = std::env::var("HOME").ok();
        let prev_profile = std::env::var("USERPROFILE").ok();
        std::env::set_var("HOME", &tmp);
        std::env::set_var("USERPROFILE", &tmp);

        // Write a file with mixed valid/invalid lines
        let path = compute_mutate_audit_path(Some(Utc::now()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let valid = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "op": "add_type",
            "pack": "p",
            "type_or_hash": null,
            "type_redacted": false,
            "prefix_first_seg": null,
            "actor": "cli",
            "outcome": "success",
            "reason": null,
            "prev_sha8": null,
            "new_sha8": null,
            "batch_id": null,
        });
        let content = format!(
            "{}\nNOT JSON\n{{bad json\n{}\n\n",
            valid,
            valid
        );
        std::fs::write(&path, &content).unwrap();

        let records = read_recent_mutations(Some(1));
        assert_eq!(records.len(), 2, "should skip malformed lines, keep valid ones");

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        }
        if let Some(p) = prev_profile {
            std::env::set_var("USERPROFILE", p);
        }
    }
}
