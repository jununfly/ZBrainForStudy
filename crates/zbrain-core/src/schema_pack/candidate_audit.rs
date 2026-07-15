//! Privacy-redacted candidate audit log for schema-pack discovery.
//!
//! Port of `src/core/schema-pack/candidate-audit.ts`.
//!
//! When `detect`/`suggest` derive candidate page types from real brain data,
//! we log them for later review — but the type names and slugs are
//! privacy-sensitive. By default we redact the type via a SHA-256 prefix
//! (`sha8`) and keep only the first path segment of the slug. Full values are
//! written only when `ZBRAIN_SCHEMA_AUDIT_VERBOSE=1`.

use std::path::PathBuf;

use chrono::{DateTime, Datelike, IsoWeek, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Env var to force verbose (non-redacted) candidate logging.
pub const ENV_AUDIT_VERBOSE: &str = "ZBRAIN_SCHEMA_AUDIT_VERBOSE";
/// Env var overriding the audit directory (falls back to `~/.zbrain/audit`).
pub const ENV_AUDIT_DIR: &str = "ZBRAIN_AUDIT_DIR";

/// A single candidate audit record written to the JSONL log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateAuditRecord {
    /// ISO-8601 timestamp.
    pub ts: String,
    /// Redacted type (`sha8`) unless verbose; in verbose mode equals `type_name`.
    pub type_or_hash: String,
    /// Whether `type_or_hash` is a redaction (true) or the literal type (verbose).
    pub type_redacted: bool,
    /// First path segment of the slug only (e.g. `people/` from `people/alice`).
    pub slug_prefix: String,
    /// Frontmatter keys observed on the candidate pages.
    pub frontmatter_keys: Vec<String>,
    /// Number of pages that informed this candidate.
    pub count: usize,
    /// Active pack identity the candidate was derived against.
    pub pack_identity: Option<String>,
}

/// Inputs to [`log_candidate`].
#[derive(Debug, Clone)]
pub struct LogCandidateOpts {
    pub type_name: String,
    pub slug: String,
    pub frontmatter_keys: Vec<String>,
    pub pack_identity: Option<String>,
    pub count: Option<usize>,
}

/// True when verbose (non-redacted) logging is requested.
pub fn is_audit_verbose() -> bool {
    std::env::var(ENV_AUDIT_VERBOSE).map(|v| v == "1").unwrap_or(false)
}

/// Resolve the audit directory: `$ZBRAIN_AUDIT_DIR` or `~/.zbrain/audit`.
pub fn resolve_audit_dir() -> PathBuf {
    if let Ok(dir) = std::env::var(ENV_AUDIT_DIR) {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    crate::paths::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("audit")
}

/// ISO-week name `YYYY-Www` for `date`.
pub fn compute_iso_week_name(date: DateTime<Utc>) -> String {
    let week: IsoWeek = date.iso_week();
    format!("{:04}-W{:02}", week.year(), week.week())
}

/// Path of the candidate audit JSONL for the week of `date`.
pub fn compute_candidate_audit_path(date: DateTime<Utc>) -> PathBuf {
    resolve_audit_dir().join(format!("schema-candidates-{}.jsonl", compute_iso_week_name(date)))
}

/// SHA-256 prefix used for type redaction. Despite the TS name "sha8", this
/// is the first 4 bytes (8 hex chars) of the digest.
pub fn sha8(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..4])
}

/// First path segment of a slug (e.g. `people/` from `people/alice.md`).
/// Returns the whole slug if it has no `/`.
pub fn slug_prefix(slug: &str) -> String {
    match slug.split_once('/') {
        Some((head, _)) => format!("{head}/"),
        None => slug.to_string(),
    }
}

/// Best-effort append of a candidate record. Never throws; warnings go to
/// stderr. Respects the verbose flag for type redaction.
pub fn log_candidate(opts: &LogCandidateOpts) -> std::io::Result<()> {
    let now = Utc::now();
    let verbose = is_audit_verbose();
    let (type_or_hash, type_redacted) = if verbose {
        (opts.type_name.clone(), false)
    } else {
        (sha8(&opts.type_name), true)
    };
    let record = CandidateAuditRecord {
        ts: now.to_rfc3339(),
        type_or_hash,
        type_redacted,
        slug_prefix: slug_prefix(&opts.slug),
        frontmatter_keys: opts.frontmatter_keys.clone(),
        count: opts.count.unwrap_or(0),
        pack_identity: opts.pack_identity.clone(),
    };
    let line = serde_json::to_string(&record)?;
    let path = compute_candidate_audit_path(now);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{line}")?;
    Ok(())
}

/// Read candidate records from the last `days_back` days across all weekly
/// JSONL files in the audit dir. Newer files are read first; records are
/// filtered by `ts >= cutoff`.
pub fn read_recent_candidates(days_back: u32) -> Vec<CandidateAuditRecord> {
    let cutoff = Utc::now() - chrono::Duration::days(days_back as i64);
    let dir = resolve_audit_dir();
    let mut records = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return records;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("schema-candidates-") && n.ends_with(".jsonl"))
                .unwrap_or(false)
        })
        .collect();
    // Newest first so callers see recent candidates first.
    files.sort_by(|a, b| b.cmp(a));
    for file in files {
        let Ok(content) = std::fs::read_to_string(&file) else {
            continue;
        };
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let Ok(rec) = serde_json::from_str::<CandidateAuditRecord>(line) else {
                continue;
            };
            let Ok(ts) = DateTime::parse_from_rfc3339(&rec.ts) else {
                continue;
            };
            if ts.with_timezone(&Utc) >= cutoff {
                records.push(rec);
            }
        }
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_week_format() {
        // 2024-01-01 is ISO week 2024-W01.
        let d = Utc.with_ymd_and_hms(2024, 1, 1, 12, 0, 0).unwrap();
        assert_eq!(compute_iso_week_name(d), "2024-W01");
        // 2024-12-30 is ISO week 2025-W01.
        let d2 = Utc.with_ymd_and_hms(2024, 12, 30, 0, 0, 0).unwrap();
        assert_eq!(compute_iso_week_name(d2), "2025-W01");
    }

    #[test]
    fn sha8_is_4_bytes_hex() {
        let h = sha8("person");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        // stable
        assert_eq!(h, sha8("person"));
        assert_ne!(h, sha8("company"));
    }

    #[test]
    fn slug_prefix_extraction() {
        assert_eq!(slug_prefix("people/alice.md"), "people/");
        assert_eq!(slug_prefix("notes/ideas.md"), "notes/");
        assert_eq!(slug_prefix("standalone"), "standalone");
    }

    #[test]
    fn verbose_flag_defaults_false() {
        std::env::remove_var(ENV_AUDIT_VERBOSE);
        assert!(!is_audit_verbose());
    }

    #[test]
    fn log_and_read_round_trip() {
        let tmp = std::env::temp_dir().join(format!("zbrain-audit-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var(ENV_AUDIT_DIR, &tmp);
        std::env::remove_var(ENV_AUDIT_VERBOSE); // redacted mode

        let opts = LogCandidateOpts {
            type_name: "person".to_string(),
            slug: "people/alice.md".to_string(),
            frontmatter_keys: vec!["name".to_string(), "employer".to_string()],
            pack_identity: Some("zbrain-base".to_string()),
            count: Some(42),
        };
        log_candidate(&opts).unwrap();

        let records = read_recent_candidates(30);
        assert_eq!(records.len(), 1);
        let rec = &records[0];
        assert!(rec.type_redacted);
        assert_ne!(rec.type_or_hash, "person");
        assert_eq!(rec.type_or_hash, sha8("person"));
        assert_eq!(rec.slug_prefix, "people/");
        assert_eq!(rec.frontmatter_keys, vec!["name", "employer"]);
        assert_eq!(rec.count, 42);
        assert_eq!(rec.pack_identity.as_deref(), Some("zbrain-base"));

        // Verbose mode writes the literal type in a second record.
        std::env::set_var(ENV_AUDIT_VERBOSE, "1");
        log_candidate(&opts).unwrap();
        let records2 = read_recent_candidates(30);
        assert_eq!(records2.len(), 2);
        let verbose_rec = records2
            .iter()
            .find(|r| !r.type_redacted)
            .expect("verbose record present");
        assert_eq!(verbose_rec.type_or_hash, "person");

        // cleanup
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::remove_var(ENV_AUDIT_DIR);
        std::env::remove_var(ENV_AUDIT_VERBOSE);
    }
}
