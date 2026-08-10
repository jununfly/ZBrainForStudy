//! Resume bookkeeping, JSONL emission, and the `by_type_summary` aggregate.
//!
//! All pure-ish helpers (file I/O only, no engine, no LLM) extracted from the
//! TS command module `src/commands/eval-longmemeval.ts` (v0.35.1.0 resume,
//! v0.40.1.0 Track D by-type summary).

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use serde::Serialize;

/// Per-question-type retrieval recall counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecallBucket {
    pub hit: u64,
    pub total: u64,
}

/// `BTreeMap` (not `HashMap`) so key order is deterministic — this replaces
/// the TS `Object.keys(...).sort()` step at summary-build time.
pub type RecallByType = BTreeMap<String, RecallBucket>;

/// One `recall_by_type` entry in the emitted summary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct RecallByTypeEntry {
    pub hit: u64,
    pub total: u64,
    pub rate: f64,
}

/// Aggregate across all question types. `rate` is `null` (not NaN) when no
/// bucket was populated, so downstream JSON consumers don't trip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct AggregateRecall {
    pub hit: u64,
    pub total: u64,
    pub rate: Option<f64>,
}

/// The final `kind: "by_type_summary"` line of a run's output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ByTypeSummary {
    pub schema_version: u32,
    pub kind: &'static str,
    pub recall_by_type: BTreeMap<String, RecallByTypeEntry>,
    pub aggregate: AggregateRecall,
}

/// Load the set of `question_id`s already present in `resume_path`.
///
/// One row per line; only the `question_id` field matters. Rows whose
/// `hypothesis` is empty AND that carry an `error` field are NOT skipped —
/// those are previous-run failures that should be retried, not preserved. A
/// row with a non-empty `hypothesis` counts as "done".
///
/// Returns an empty set when the file doesn't exist, so a first run with the
/// flag behaves identically to no flag. Corrupt lines are logged to stderr and
/// skipped — a partial JSONL from a killed writer is the normal recovery case.
#[must_use]
pub fn load_resume_set(resume_path: &Path) -> HashSet<String> {
    let mut done: HashSet<String> = HashSet::new();
    let Ok(raw) = fs::read_to_string(resume_path) else {
        return done;
    };
    for (idx, line) in raw.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            eprintln!("[longmemeval] resume: skipping corrupt line {}", idx + 1);
            continue;
        };
        let Some(qid) = row.get("question_id").and_then(|v| v.as_str()) else {
            continue;
        };
        let hypothesis = row.get("hypothesis").and_then(|v| v.as_str());
        let has_error = row.get("error").is_some_and(|v| !v.is_null());
        // Retry rows that only recorded an error.
        if has_error && hypothesis.is_none_or(str::is_empty) {
            continue;
        }
        done.insert(qid.to_string());
    }
    done
}

/// Seed `bucket` from an existing output file so the summary is cumulative
/// across resume runs (not just "this run's questions").
///
/// Rows missing `recall_hit` are skipped (the dataset had no ground truth for
/// them) and prior `by_type_summary` rows are skipped (they're aggregates, not
/// source data). Best-effort: corrupt lines are silently ignored —
/// [`load_resume_set`] already logs them.
pub fn seed_recall_by_type_from_file(output_path: &Path, bucket: &mut RecallByType) {
    let Ok(raw) = fs::read_to_string(output_path) else {
        return;
    };
    for line in raw.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if row.get("kind").and_then(|v| v.as_str()) == Some("by_type_summary") {
            continue;
        }
        let Some(qtype) = row.get("question_type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(hit) = row.get("recall_hit").and_then(serde_json::Value::as_bool) else {
            continue;
        };
        let entry = bucket.entry(qtype.to_string()).or_default();
        entry.total += 1;
        if hit {
            entry.hit += 1;
        }
    }
}

/// Build the `by_type_summary` payload from the per-type bucket.
///
/// Pure and deterministic (`BTreeMap` gives sorted key order). A zero-total
/// bucket yields `rate: 0.0`; a fully empty map yields `aggregate.rate: null`.
#[must_use]
pub fn build_by_type_summary(recall_by_type: &RecallByType) -> ByTypeSummary {
    let mut recall: BTreeMap<String, RecallByTypeEntry> = BTreeMap::new();
    let mut agg_hit = 0u64;
    let mut agg_total = 0u64;
    for (key, value) in recall_by_type {
        #[allow(clippy::cast_precision_loss)]
        let rate = if value.total == 0 {
            0.0
        } else {
            value.hit as f64 / value.total as f64
        };
        recall.insert(
            key.clone(),
            RecallByTypeEntry {
                hit: value.hit,
                total: value.total,
                rate,
            },
        );
        agg_hit += value.hit;
        agg_total += value.total;
    }
    #[allow(clippy::cast_precision_loss)]
    let agg_rate = if agg_total == 0 {
        None
    } else {
        Some(agg_hit as f64 / agg_total as f64)
    };
    ByTypeSummary {
        schema_version: 1,
        kind: "by_type_summary",
        recall_by_type: recall,
        aggregate: AggregateRecall {
            hit: agg_hit,
            total: agg_total,
            rate: agg_rate,
        },
    }
}

/// Emit the `by_type_summary` as the final line of output.
///
/// Resume-safe: any prior `kind:"by_type_summary"` line in the file is REMOVED
/// before the new summary is appended, so repeated `--resume-from`
/// invocations can't stack duplicate summaries.
///
/// When `output_path` is `None` (stdout mode) the line is just written —
/// resume-replace is impossible for stdout and not meaningful (resume always
/// uses a file).
///
/// # Errors
///
/// Returns an error when the summary contains a carriage return (which would
/// corrupt the JSONL contract) or when the file rewrite fails.
pub fn emit_by_type_summary(
    output_path: Option<&Path>,
    summary: &ByTypeSummary,
) -> Result<(), String> {
    let json = serde_json::to_string(summary).map_err(|e| format!("serialize summary: {e}"))?;
    if json.contains('\r') {
        return Err("CRLF in by_type_summary emit (corrupt input)".to_string());
    }
    let Some(path) = output_path else {
        println!("{json}");
        return Ok(());
    };
    // Read existing (if present), strip any prior summary lines, append the new
    // one. Sync I/O is fine — output files stay under 1MB even on full
    // 500-question runs.
    let existing = fs::read_to_string(path).unwrap_or_default();
    let mut kept: Vec<&str> = Vec::new();
    for line in existing.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<serde_json::Value>(line) {
            if row.get("kind").and_then(|v| v.as_str()) == Some("by_type_summary") {
                continue; // drop prior summary
            }
        }
        // Corrupt line — keep as-is; the resume loader has its own skip logic.
        kept.push(line);
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(&json);
    out.push('\n');
    fs::write(path, out).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Sink for the per-question JSONL rows.
///
/// Mirrors the TS `JsonlEmitter`: stdout when no `--output`, otherwise a file
/// handle opened in truncate or append mode.
pub enum JsonlEmitter {
    Stdout,
    File(File),
}

impl JsonlEmitter {
    /// Open an emitter.
    ///
    /// `append` is used by `--resume-from` when the output path overlaps the
    /// resume file — truncating would erase the already-answered questions
    /// that were just loaded into the resume set.
    ///
    /// # Errors
    ///
    /// Returns an error when the output file cannot be opened.
    pub fn open(output_path: Option<&Path>, append: bool) -> Result<Self, String> {
        let Some(path) = output_path else {
            return Ok(Self::Stdout);
        };
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        Ok(Self::File(file))
    }

    /// Write one JSON value as a line.
    ///
    /// # Errors
    ///
    /// Returns an error when the row serializes to something containing a
    /// carriage return (corrupt input would break the JSONL contract) or when
    /// the write fails.
    pub fn emit(&mut self, value: &serde_json::Value) -> Result<(), String> {
        let json = serde_json::to_string(value).map_err(|e| format!("serialize row: {e}"))?;
        if json.contains('\r') {
            return Err("CRLF in JSONL emit (corrupt input)".to_string());
        }
        match self {
            Self::Stdout => {
                println!("{json}");
                Ok(())
            }
            Self::File(f) => writeln!(f, "{json}").map_err(|e| format!("write row: {e}")),
        }
    }

    /// Flush the underlying handle. Stdout stays open.
    ///
    /// # Errors
    ///
    /// Returns an error when the flush fails.
    pub fn close(&mut self) -> Result<(), String> {
        match self {
            Self::Stdout => Ok(()),
            Self::File(f) => f.flush().map_err(|e| format!("flush output: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static SEQ: AtomicU32 = AtomicU32::new(0);

    /// Unique temp path per call — pid-only naming collides when tests run in
    /// parallel across cores.
    fn tmp_path(tag: &str) -> std::path::PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "zbrain-lme-summary-{tag}-{}-{n}.jsonl",
            std::process::id()
        ))
    }

    fn write_lines(path: &Path, lines: &[&str]) {
        fs::write(path, format!("{}\n", lines.join("\n"))).expect("fixture write");
    }

    #[test]
    fn resume_set_is_empty_for_missing_file() {
        let p = tmp_path("missing");
        assert!(load_resume_set(&p).is_empty());
    }

    #[test]
    fn resume_set_skips_error_rows_but_keeps_answered() {
        let p = tmp_path("resume");
        write_lines(
            &p,
            &[
                r#"{"question_id":"a","hypothesis":"answered"}"#,
                r#"{"question_id":"b","hypothesis":"","error":"rate limit"}"#,
                r#"{"question_id":"c","hypothesis":"also answered","error":null}"#,
                "not json at all",
                r#"{"no_question_id":true}"#,
            ],
        );
        let set = load_resume_set(&p);
        assert!(set.contains("a"), "answered row must count as done");
        assert!(!set.contains("b"), "error row must be retried");
        assert!(set.contains("c"), "null error must not disqualify");
        assert_eq!(set.len(), 2);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn seed_skips_summary_rows_and_rows_without_ground_truth() {
        let p = tmp_path("seed");
        write_lines(
            &p,
            &[
                r#"{"question_id":"a","question_type":"temporal","recall_hit":true}"#,
                r#"{"question_id":"b","question_type":"temporal","recall_hit":false}"#,
                r#"{"question_id":"c","question_type":"multi"}"#,
                r#"{"kind":"by_type_summary","recall_by_type":{}}"#,
            ],
        );
        let mut bucket = RecallByType::new();
        seed_recall_by_type_from_file(&p, &mut bucket);
        assert_eq!(bucket.len(), 1);
        assert_eq!(bucket["temporal"], RecallBucket { hit: 1, total: 2 });
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn summary_rates_and_aggregate() {
        let mut bucket = RecallByType::new();
        bucket.insert("temporal".to_string(), RecallBucket { hit: 3, total: 4 });
        bucket.insert("multi".to_string(), RecallBucket { hit: 1, total: 4 });
        let s = build_by_type_summary(&bucket);
        assert_eq!(s.schema_version, 1);
        assert_eq!(s.kind, "by_type_summary");
        assert!((s.recall_by_type["temporal"].rate - 0.75).abs() < 1e-9);
        assert!((s.recall_by_type["multi"].rate - 0.25).abs() < 1e-9);
        assert_eq!(s.aggregate.hit, 4);
        assert_eq!(s.aggregate.total, 8);
        assert!((s.aggregate.rate.expect("rate") - 0.5).abs() < 1e-9);
        // Deterministic sorted key order in the serialized form.
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.find("\"multi\"") < json.find("\"temporal\""));
    }

    #[test]
    fn empty_bucket_yields_null_aggregate_rate() {
        let s = build_by_type_summary(&RecallByType::new());
        assert_eq!(s.aggregate.rate, None);
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains(r#""rate":null"#));
    }

    #[test]
    fn zero_total_bucket_rate_is_zero_not_nan() {
        let mut bucket = RecallByType::new();
        bucket.insert("empty".to_string(), RecallBucket { hit: 0, total: 0 });
        let s = build_by_type_summary(&bucket);
        assert!((s.recall_by_type["empty"].rate - 0.0).abs() < f64::EPSILON);
        assert_eq!(s.aggregate.rate, None);
    }

    #[test]
    fn emit_replaces_prior_summary_instead_of_appending() {
        let p = tmp_path("emit");
        write_lines(
            &p,
            &[
                r#"{"question_id":"a","hypothesis":"x"}"#,
                r#"{"schema_version":1,"kind":"by_type_summary","recall_by_type":{},"aggregate":{"hit":0,"total":0,"rate":null}}"#,
            ],
        );
        let mut bucket = RecallByType::new();
        bucket.insert("temporal".to_string(), RecallBucket { hit: 1, total: 1 });
        emit_by_type_summary(Some(&p), &build_by_type_summary(&bucket)).expect("emit");

        let raw = fs::read_to_string(&p).expect("read back");
        let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "prior summary must be replaced, not stacked");
        assert!(lines[0].contains(r#""question_id":"a""#));
        assert!(lines[1].contains(r#""kind":"by_type_summary""#));
        assert!(lines[1].contains(r#""hit":1"#));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn emit_to_new_file_writes_single_line() {
        let p = tmp_path("emit-new");
        emit_by_type_summary(Some(&p), &build_by_type_summary(&RecallByType::new()))
            .expect("emit");
        let raw = fs::read_to_string(&p).expect("read back");
        assert_eq!(raw.lines().count(), 1);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn emitter_append_preserves_prior_rows() {
        let p = tmp_path("append");
        write_lines(&p, &[r#"{"question_id":"old"}"#]);
        let mut em = JsonlEmitter::open(Some(&p), true).expect("open append");
        em.emit(&serde_json::json!({"question_id": "new"}))
            .expect("emit");
        em.close().expect("close");
        drop(em);
        let raw = fs::read_to_string(&p).expect("read back");
        assert!(raw.contains("old"));
        assert!(raw.contains("new"));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn emitter_truncate_drops_prior_rows() {
        let p = tmp_path("truncate");
        write_lines(&p, &[r#"{"question_id":"old"}"#]);
        let mut em = JsonlEmitter::open(Some(&p), false).expect("open truncate");
        em.emit(&serde_json::json!({"question_id": "new"}))
            .expect("emit");
        em.close().expect("close");
        drop(em);
        let raw = fs::read_to_string(&p).expect("read back");
        assert!(!raw.contains("old"));
        assert!(raw.contains("new"));
        let _ = fs::remove_file(&p);
    }
}
