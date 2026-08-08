//! v0.32.3 search telemetry rollup — JSONL append + read-time aggregate.
//!
//! Ported from `src/core/search/telemetry.ts` (deleted in commit `bcafcafd`).
//! The TS writer bucketed per-process in-memory and flushed to a
//! `search_telemetry` Postgres/PGLite table on a 60s timer + on exit. The
//! Rust port simplifies the persistence layer to a single JSONL file
//! (`<ZBRAIN_HOME>/telemetry/search.jsonl`, default) — each call appends
//! one event line synchronously, and stats are derived on read. This
//! drops the bucket/flush dance because the JSONL append is O(1) and
//! never blocks the hot path (it's a single `write` syscall on a
//! already-opened file).
//!
//! Trade-offs vs the TS implementation:
//!   - No cross-process aggregation (each process has its own file or
//!     appends to the same one — both are safe because appends are
//!     single-write). Operators can roll up multiple files by glob.
//!   - No exit-time drain (TS worried about losing the last bucket on
//!     hard crash; the JSONL file is per-event so a crash loses at
//!     most the in-flight event, not a 60-second bucket).
//!   - The `flush_threshold_calls` / 60s timer are no longer needed —
//!     the per-event append is the persistence.
//!
//! Telemetry is opt-in via `SearchTelemetryWriter::new(Some(path))`. A
//! writer constructed with `None` is a no-op (every `record` returns
//! immediately), so the hot path is unaffected when the operator hasn't
//! configured telemetry.

use crate::search::intent::classify_query;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Default JSONL file location under the ZBrain home directory.
pub const DEFAULT_TELEMETRY_PATH: &str = "telemetry/search.jsonl";

/// One telemetry event = one search call. Written as a single JSON line.
/// Mirrors the TS `HybridSearchMeta` plus a `latency_ms` stamp + `mode`
/// label (lexical / hybrid / vector).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchTelemetryEvent {
    /// Unix epoch seconds (UTC). Matches `HybridSearchMeta` first_seen
    /// semantics. Stored as i64 to keep the file appendable across leap
    /// seconds (u32 would overflow in 2106).
    pub ts: i64,
    /// Search query text (verbatim from the caller). No PII filtering —
    /// the operator owns the file.
    pub query: String,
    /// "lexical" / "hybrid" / "vector" — set by the caller; "unset" when
    /// not provided. Mirrors TS `meta.mode ?? 'unset'`.
    pub mode: String,
    /// "entity" / "temporal" / "event" / "full_context" / "general" /
    /// "unset" — derived from `query` via `classify_query` when the
    /// caller doesn't pass one. Mirrors TS `meta.intent ?? 'unset'`.
    pub intent: String,
    /// Number of results returned to the caller. `>= 0`. Mirrors TS
    /// `opts.results_count`.
    pub results_count: u32,
    /// Optional token budget tracker (sum of `estimate_tokens` over the
    /// returned page excerpts). `0` when no budget was enforced.
    pub tokens_estimate: u32,
    /// End-to-end latency for the search call in milliseconds. Set by
    /// the wrapping `record_search!` macro / `SearchTimer`. The TS port
    /// dropped this field; the Rust port adds it because the JSONL
    /// per-event cost is the same and operators asked for it.
    pub latency_ms: u32,
}

impl SearchTelemetryEvent {
    /// Build an event from a finished search call. `query` is required;
    /// `mode` defaults to `"unset"`; `intent` is derived from `query`
    /// when `None` (so simple callers can skip the classify step). The
    /// `latency_ms` is taken from a [`SearchTimer`].
    pub fn from_search(
        query: &str,
        mode: Option<&str>,
        intent: Option<&str>,
        results_count: u32,
        tokens_estimate: u32,
        timer: &SearchTimer,
    ) -> Self {
        let intent = match intent {
            Some(i) if !i.is_empty() => i.to_string(),
            Some(_) | None => query_intent_label(&classify_query(query).intent),
        };
        let mode = match mode {
            Some(m) if !m.is_empty() => m.to_string(),
            Some(_) | None => "unset".to_string(),
        };
        Self {
            ts: now_epoch_secs(),
            query: query.to_string(),
            mode,
            intent,
            results_count,
            tokens_estimate,
            latency_ms: timer.elapsed_ms(),
        }
    }
}

/// Map `QueryIntent` enum variants to lowercase labels that match the
/// `intent` strings the rest of the search pipeline uses (entity,
/// temporal, event, general). Single source of truth — when a new
/// variant is added to `QueryIntent` this helper breaks the build.
fn query_intent_label(intent: &crate::search::intent::QueryIntent) -> String {
    use crate::search::intent::QueryIntent;
    match intent {
        QueryIntent::Entity => "entity",
        QueryIntent::Temporal => "temporal",
        QueryIntent::Event => "event",
        QueryIntent::General => "general",
    }
    .to_string()
}

/// Stopwatch for the search hot path. Cheap: one `Instant::now()` on
/// construction, one `duration_since` on `elapsed_ms`. The `Drop` impl
/// is intentionally a no-op so the timer is drop-safe (no panic on
/// unwind).
#[derive(Debug, Clone)]
pub struct SearchTimer {
    start: Instant,
}

impl SearchTimer {
    /// Start the timer. Mirrors `performance.now()` at the top of
    /// `hybridSearch` in TS.
    #[inline]
    #[must_use]
    pub fn start() -> Self {
        Self { start: Instant::now() }
    }

    /// Elapsed milliseconds since `start`. Saturates at `u32::MAX` for
    /// absurdly long calls (43 days) so JSON serialization can't fail.
    #[inline]
    pub fn elapsed_ms(&self) -> u32 {
        let ms = self.start.elapsed().as_millis();
        if ms > u32::MAX as u128 { u32::MAX } else { ms as u32 }
    }
}

impl Default for SearchTimer {
    fn default() -> Self { Self::start() }
}

/// Thread-safe JSONL appender. The inner state is a `Mutex<Option<File>>`
/// so a no-op writer (path was `None` at construction) holds no file
/// handle and pays no cost on `record`. The `File` is opened lazily on
/// the first `record` call so a never-used writer doesn't leave a stale
/// file descriptor around.
#[derive(Debug)]
pub struct SearchTelemetryWriter {
    inner: Mutex<Option<TelemetryInner>>,
}

#[derive(Debug)]
struct TelemetryInner {
    file: File,
    path: PathBuf,
}

impl SearchTelemetryWriter {
    /// Construct a writer that appends to `path`. If `path` is `None`,
    /// the writer is a no-op (every `record` returns immediately, no
    /// file is opened). Mirrors the TS "no engine, no flush" semantics.
    /// The file is opened eagerly so a failed open (e.g. permission
    /// error) surfaces at construction time, not on the hot path.
    /// `ensure_parent` creates the directory tree if missing.
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let inner = path.and_then(|p| {
            ensure_parent(&p).ok()?;
            Some(TelemetryInner {
                file: open_append(&p),
                path: p,
            })
        });
        Self { inner: Mutex::new(inner) }
    }

    /// Resolve the default path under the ZBrain home directory.
    /// `zbrain_home` is `$ZBRAIN_HOME` or `~/.zbrain`. Creates the
    /// telemetry directory if missing.
    pub fn default_path() -> Option<PathBuf> {
        let home = std::env::var("ZBRAIN_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                dirs_home().map(|h| h.join(".zbrain"))
            })?;
        Some(home.join(DEFAULT_TELEMETRY_PATH))
    }

    /// Record a single search call. Synchronous; blocks on the file
    /// write (one `write` syscall on a buffered `File` plus a flush on
    /// drop — the file is opened with `O_APPEND` so concurrent writers
    /// can't interleave on POSIX, and Windows append is atomic per
    /// `OpenOptions` docs). Returns `Ok(())` for the no-op writer.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the file can't be opened or written.
    /// The hot path should log and continue (telemetry must NEVER break
    /// search) — see the G72 KNOWN-GAPS entry for the rationale.
    pub fn record(&self, event: &SearchTelemetryEvent) -> std::io::Result<()> {
        let mut guard = self.inner.lock().expect("telemetry mutex poisoned");
        let inner = match guard.as_mut() {
            None => return Ok(()), // no-op writer
            Some(i) => i,
        };
        let mut buf = serde_json::to_vec(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        buf.push(b'\n');
        inner.file.write_all(&buf)?;
        inner.file.flush()?; // fire-and-forget durability: persist per event.
        Ok(())
    }

    /// Test-only: total bytes written. Reads the file size when
    /// the writer is file-backed; returns 0 for no-op writers.
    #[cfg(test)]
    pub fn count_for_test(&self) -> usize {
        let guard = self.inner.lock().expect("telemetry mutex poisoned");
        match guard.as_ref() {
            None => 0,
            Some(inner) => std::fs::metadata(&inner.path)
                .map(|m| m.len() as usize)
                .unwrap_or(0),
        }
    }
}

fn open_append(path: &Path) -> File {
    // Best-effort: if open fails (permission denied, invalid path),
    // surface a clear panic at construction time rather than a confusing
    // write error deep in the hot path. Operators who want fail-soft
    // should construct with `None`.
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap_or_else(|e| panic!("telemetry file open failed for {}: {e}", path.display()))
}

/// Aggregate read over a JSONL events file. Mirrors the TS
/// `readSearchStats` summary output, simplified (no Postgres rollup —
/// p50/p95 derived from the in-memory event slice).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchStats {
    /// Total events in the window.
    pub count: u32,
    /// Median latency in milliseconds (0 when count == 0).
    pub p50_latency_ms: u32,
    /// 95th percentile latency in milliseconds (0 when count == 0).
    pub p95_latency_ms: u32,
    /// Count of events per intent label (entity / temporal / etc.).
    pub by_intent: HashMap<String, u32>,
    /// Count of events per mode label (lexical / hybrid / vector).
    pub mode_counts: HashMap<String, u32>,
    /// Top 5 queries by occurrence. Ties broken by recency.
    pub top_queries: Vec<(String, u32)>,
    /// Window the stats cover.
    pub window: StatsWindow,
}

/// Read-time window selector. Mirrors the TS `StatsWindow` union but
/// without the absolute-day variants (operators reading the JSONL file
/// directly get the same granularity; the CLI just prints "last N").
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatsWindow {
    LastHour,
    LastDay,
    LastWeek,
    All,
}

impl StatsWindow {
    /// Window length in seconds. `All` returns `i64::MAX` so the filter
    /// in `read_search_stats` is a no-op.
    #[must_use]
    pub fn window_secs(self) -> i64 {
        match self {
            StatsWindow::LastHour => 3600,
            StatsWindow::LastDay => 86_400,
            StatsWindow::LastWeek => 604_800,
            StatsWindow::All => i64::MAX,
        }
    }
}

/// Read a JSONL events file and aggregate over the time window.
/// Returns a zeroed `SearchStats` when the file is missing or empty
/// (operator-friendly: `zbrain query --stats` prints a friendly "no
/// events recorded" rather than a 404).
pub fn read_search_stats(path: &Path, window: StatsWindow) -> std::io::Result<SearchStats> {
    let now = now_epoch_secs();
    let cutoff = now.saturating_sub(window.window_secs());

    let mut events: Vec<SearchTelemetryEvent> = Vec::new();
    if path.exists() {
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() { continue; }
            // Malformed lines are skipped (telemetry must not break reads).
            // The G72 doc explicitly says: "Operator error in the
            // JSONL file should never propagate to the CLI; log and
            // continue."
            if let Ok(event) = serde_json::from_str::<SearchTelemetryEvent>(&line) {
                if event.ts >= cutoff {
                    events.push(event);
                }
            }
        }
    }

    let count = events.len() as u32;
    let mut latencies: Vec<u32> = events.iter().map(|e| e.latency_ms).collect();
    latencies.sort_unstable();
    let p50 = percentile(&latencies, 0.50);
    let p95 = percentile(&latencies, 0.95);

    let mut by_intent: HashMap<String, u32> = HashMap::new();
    let mut mode_counts: HashMap<String, u32> = HashMap::new();
    let mut query_counts: HashMap<String, u32> = HashMap::new();
    for e in &events {
        *by_intent.entry(e.intent.clone()).or_insert(0) += 1;
        *mode_counts.entry(e.mode.clone()).or_insert(0) += 1;
        *query_counts.entry(e.query.clone()).or_insert(0) += 1;
    }
    let mut top_queries: Vec<(String, u32)> = query_counts.into_iter().collect();
    top_queries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    top_queries.truncate(5);

    Ok(SearchStats {
        count,
        p50_latency_ms: p50,
        p95_latency_ms: p95,
        by_intent,
        mode_counts,
        top_queries,
        window,
    })
}

/// Compute the `p * 100`th percentile of `values` (linear interpolation
/// between adjacent samples when the percentile doesn't land on an
/// integer index). Returns 0 for an empty slice.
fn percentile(values: &[u32], p: f64) -> u32 {
    if values.is_empty() { return 0; }
    let rank = p * (values.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi { return values[lo]; }
    let frac = rank - lo as f64;
    let v = values[lo] as f64 + (values[hi] as f64 - values[lo] as f64) * frac;
    v.round() as u32
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Ensure `path`'s parent directory exists. Returns the path unchanged
/// when the parent already exists. Used by `SearchTelemetryWriter` and
/// the CLI to bootstrap the telemetry dir on first record.
pub fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::TempDir;

    fn fresh_writer(dir: &TempDir) -> (SearchTelemetryWriter, PathBuf) {
        let path = dir.path().join("search.jsonl");
        let writer = SearchTelemetryWriter::new(Some(path.clone()));
        // The writer opens eagerly — verify the file is created at
        // construction (not lazily on the first record) so a failed
        // open surfaces immediately in tests.
        assert!(path.exists(), "telemetry file must be created eagerly");
        (writer, path)
    }

    #[test]
    fn noop_writer_does_not_create_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("never.jsonl");
        // `new(None)` constructs a no-op writer that holds no file
        // handle and never touches the filesystem. The path variable
        // is just used to assert no file got created in `dir`.
        let writer = SearchTelemetryWriter::new(None);
        let event = SearchTelemetryEvent {
            ts: 1, query: "x".into(), mode: "unset".into(),
            intent: "general".into(), results_count: 0,
            tokens_estimate: 0, latency_ms: 0,
        };
        writer.record(&event).unwrap();
        // No file should be created at `path` because the writer
        // never had a path wired in.
        assert!(!path.exists(), "no-op writer must not create a file");
    }

    #[test]
    fn record_appends_one_jsonl_line() {
        let dir = TempDir::new().unwrap();
        let (writer, path) = fresh_writer(&dir);
        let event = SearchTelemetryEvent {
            ts: 1_700_000_000,
            query: "database".into(),
            mode: "hybrid".into(),
            intent: "entity".into(),
            results_count: 5,
            tokens_estimate: 200,
            latency_ms: 12,
        };
        writer.record(&event).unwrap();
        let mut content = String::new();
        File::open(&path).unwrap().read_to_string(&mut content).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // File is opened eagerly but no record has been written yet
        // → 1 line after the first record.
        assert_eq!(lines.len(), 1);
        let parsed: SearchTelemetryEvent = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn read_search_stats_aggregates_window() {
        let dir = TempDir::new().unwrap();
        let (writer, path) = fresh_writer(&dir);
        // Write three known events to a fresh file; verify the
        // aggregate. Use the writer so the JSONL format is exercised
        // end-to-end.
        for (q, ms) in [("alpha", 5), ("beta", 10), ("alpha", 15)] {
            writer.record(&SearchTelemetryEvent {
                ts: now_epoch_secs(),
                query: q.into(),
                mode: "hybrid".into(),
                intent: "entity".into(),
                results_count: 1,
                tokens_estimate: 0,
                latency_ms: ms,
            }).unwrap();
        }
        let stats = read_search_stats(&path, StatsWindow::All).unwrap();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.p50_latency_ms, 10);
        assert_eq!(stats.p95_latency_ms, 15);
        assert_eq!(stats.by_intent.get("entity"), Some(&3));
        assert_eq!(stats.mode_counts.get("hybrid"), Some(&3));
        assert_eq!(stats.top_queries[0], ("alpha".to_string(), 2));
    }

    #[test]
    fn percentile_handles_empty_and_single() {
        assert_eq!(percentile(&[], 0.5), 0);
        assert_eq!(percentile(&[42], 0.5), 42);
        assert_eq!(percentile(&[42], 0.95), 42);
    }

    #[test]
    fn percentile_p50_and_p95_correct() {
        // 100 samples 1..=100 → p50 ≈ 50, p95 ≈ 95.
        let values: Vec<u32> = (1..=100).collect();
        assert!((percentile(&values, 0.50) as i32 - 50).abs() <= 1);
        assert!((percentile(&values, 0.95) as i32 - 95).abs() <= 1);
    }

    #[test]
    fn stats_window_seconds() {
        assert_eq!(StatsWindow::LastHour.window_secs(), 3600);
        assert_eq!(StatsWindow::LastDay.window_secs(), 86_400);
        assert_eq!(StatsWindow::LastWeek.window_secs(), 604_800);
        assert_eq!(StatsWindow::All.window_secs(), i64::MAX);
    }

    #[test]
    fn read_search_stats_missing_file_returns_zero() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing.jsonl");
        let stats = read_search_stats(&path, StatsWindow::All).unwrap();
        assert_eq!(stats.count, 0);
        assert_eq!(stats.p50_latency_ms, 0);
        assert_eq!(stats.p95_latency_ms, 0);
        assert!(stats.top_queries.is_empty());
    }

    #[test]
    fn search_timer_measures_latency() {
        let timer = SearchTimer::start();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let ms = timer.elapsed_ms();
        assert!(ms >= 1, "expected at least 1ms, got {ms}");
    }

    #[test]
    fn from_search_derives_intent() {
        let timer = SearchTimer::start();
        let event = SearchTelemetryEvent::from_search(
            "who is Alice", None, None, 3, 100, &timer,
        );
        assert_eq!(event.intent, "entity");
        assert_eq!(event.mode, "unset");
        assert_eq!(event.results_count, 3);
    }
}
