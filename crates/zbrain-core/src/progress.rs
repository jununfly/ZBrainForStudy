//! Progress reporter — per-item progress for long-running operations.
//!
//! Ported from `src/core/progress.ts` (495 lines). This is a minimal slice:
//! `start` / `tick` / `finish`, three modes (human-plain / json / quiet),
//! dual-gate throttling (time interval + item count + final-tick force),
//! NDJSON `start`/`tick`/`finish` events, writes to the caller-provided
//! writer (stderr in production).
//!
//! Cut from this slice (registered in KNOWN-GAPS):
//!   - Signal coordinator (SIGINT/SIGTERM) → G14
//!   - EPIPE defense (safeWrite + brokenStreams) → G15
//!   - `child()` factory (phase path composition) → G16
//!   - `heartbeat(note)` + `startHeartbeat` timer → G17
//!   - TTY `\r\x1b[2K` rewrite mode (human-tty) → G18
//!   - Source-prefix injection (`getSourcePrefix()`) → G19
//!   - `abort` events → G20

use std::io::Write;
use std::time::Instant;

// ──────────────────────────────────────────────────────────────────────────
// ProgressMode
// ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    /// No output.
    Quiet,
    /// Human-readable one-line-per-event (plain text, no TTY rewrite).
    Human,
    /// Newline-delimited JSON events (NDJSON).
    Json,
}

// ──────────────────────────────────────────────────────────────────────────
// PhaseState — internal per-phase tracking (mirrors TS `PhaseState`)
// ──────────────────────────────────────────────────────────────────────────

struct PhaseState {
    phase: String,
    total: Option<usize>,
    done: usize,
    started_at: Instant,
    last_emit: Instant,
    last_done_emitted: usize,
}

impl PhaseState {
    fn new(phase: String, total: Option<usize>) -> Self {
        let now = Instant::now();
        Self {
            phase,
            total,
            done: 0,
            started_at: now,
            last_emit: now,
            last_done_emitted: 0,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// ProgressReporter
// ──────────────────────────────────────────────────────────────────────────

/// Stateful progress reporter, wired to a caller-provided writer.
///
/// Lifecycle: `start(phase, total?)` → zero or more `tick(n?, note?)` →
/// `finish(note?)`. Calling `start` while a prior phase is live will
/// auto-finish it first (same behavior as TS).
pub struct ProgressReporter {
    mode: ProgressMode,
    min_interval_ms: u64,
    writer: Box<dyn Write + Send>,
    state: Option<PhaseState>,
}

impl ProgressReporter {
    /// Create a new reporter.
    ///
    /// `writer` is typically `Box::new(std::io::stderr())` in production,
    /// or a shared buffer in tests.
    #[must_use]
    pub fn new(mode: ProgressMode, min_interval_ms: u64, writer: Box<dyn Write + Send>) -> Self {
        Self {
            mode,
            min_interval_ms,
            writer,
            state: None,
        }
    }

    // ── helpers ──────────────────────────────────────────────────────────

    fn default_min_items(total: Option<usize>) -> usize {
        let base = match total {
            Some(t) if t > 0 => t,
            _ => 1000,
        };
        std::cmp::max(10, base.div_ceil(100))
    }

    fn emit_json(&mut self, obj: &serde_json::Value) {
        let _ = writeln!(self.writer, "{obj}");
    }

    /// Render a human-mode line. Mirrors TS `renderHumanLine`.
    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    fn render_human_line(
        phase: &str,
        done: Option<usize>,
        total: Option<usize>,
        note: Option<&str>,
    ) -> String {
        let mut parts = vec![format!("[{phase}]")];
        if let Some(d) = done {
            if let Some(t) = total.filter(|&t| t > 0) {
                let pct = ((d as f64 / t as f64) * 100.0).floor() as u64;
                parts.push(format!("{d}/{t} ({pct}%)"));
            } else {
                parts.push(format!("{d}"));
            }
        }
        if let Some(n) = note {
            parts.push(n.to_string());
        }
        parts.join(" ")
    }

    fn emit_human_line(&mut self, line: &str) {
        let _ = writeln!(self.writer, "{line}");
    }

    /// ISO 8601 UTC timestamp, matching JS `new Date().toISOString()` format.
    fn now_iso() -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }

    #[allow(clippy::cast_possible_truncation)]
    fn elapsed_ms(started_at: Instant) -> u64 {
        started_at.elapsed().as_millis() as u64
    }

    // ── public API ───────────────────────────────────────────────────────

    /// Begin a new phase. Auto-finishes any prior live phase.
    pub fn start(&mut self, local_phase: &str, total: Option<usize>) {
        // Auto-finish prior phase if caller forgot.
        if self.state.is_some() {
            self.finish(None);
        }

        let phase = local_phase.to_string();
        let s = PhaseState::new(phase.clone(), total);
        self.state = Some(s);

        if self.mode == ProgressMode::Quiet {
            return;
        }

        if self.mode == ProgressMode::Json {
            let mut obj = serde_json::json!({
                "event": "start",
                "phase": phase,
                "ts": Self::now_iso(),
            });
            if let Some(t) = total {
                obj["total"] = serde_json::json!(t);
            }
            self.emit_json(&obj);
        } else {
            let line = Self::render_human_line(&phase, None, total, Some("start"));
            self.emit_human_line(&line);
        }
    }

    /// Register progress. `n` defaults to 1, `note` is optional.
    ///
    /// Emission is throttled by dual gates:
    /// 1. Time gate: at least `min_interval_ms` since last emit.
    /// 2. Item gate: at least `min_items` (= max(10, ceil(total/100))) ticks
    ///    since last emit.
    /// 3. Final tick: if `total` is known and `done >= total`, always force-emit.
    #[allow(clippy::cast_precision_loss, clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    pub fn tick(&mut self, n: usize, note: Option<&str>) {
        // Step 1: mutate `done` (mutable borrow of self.state, scoped).
        {
            let Some(s) = &mut self.state else { return };
            s.done += n;
        }

        if self.mode == ProgressMode::Quiet {
            return;
        }

        // Step 2: read throttle values (immutable borrow of self.state).
        let (phase, done, total, started_at, since_emit, items_since_emit) = {
            let Some(s) = &self.state else { return };
            (
                s.phase.clone(),
                s.done,
                s.total,
                s.started_at,
                s.last_emit.elapsed().as_millis() as u64,
                s.done - s.last_done_emitted,
            )
        };

        let min_items = Self::default_min_items(total);
        let is_final_tick = total.is_some_and(|t| done >= t);

        let should_emit =
            since_emit >= self.min_interval_ms || items_since_emit >= min_items || is_final_tick;
        if !should_emit {
            return;
        }

        // Step 3: update `last_emit` / `last_done_emitted` (mutable borrow again).
        if let Some(s) = &mut self.state {
            s.last_emit = Instant::now();
            s.last_done_emitted = done;
        }

        // Step 4: render (self is free).
        if self.mode == ProgressMode::Json {
            let elapsed_ms = Self::elapsed_ms(started_at);
            let mut obj = serde_json::json!({
                "event": "tick",
                "phase": phase,
                "done": done,
                "elapsed_ms": elapsed_ms,
                "ts": Self::now_iso(),
            });
            if let Some(t) = total.filter(|&t| t > 0) {
                obj["total"] = serde_json::json!(t);
                obj["pct"] = serde_json::json!(((done as f64 / t as f64) * 1000.0_f64).round()
                    / 10.0_f64);
                if done > 0 {
                    let ms_per_item = elapsed_ms as f64 / done as f64;
                    let remaining = t.saturating_sub(done);
                    obj["eta_ms"] = serde_json::json!((ms_per_item * remaining as f64).round() as u64);
                }
            }
            if let Some(n) = note {
                obj["note"] = serde_json::json!(n);
            }
            self.emit_json(&obj);
        } else {
            let line = Self::render_human_line(&phase, Some(done), total, note);
            self.emit_human_line(&line);
        }
    }

    /// End the current phase. `done > 0` is included in the output; if
    /// `done == 0` it is omitted (mirrors TS behavior). Default note is "done".
    pub fn finish(&mut self, note: Option<&str>) {
        let Some(s) = self.state.take() else { return };

        if self.mode == ProgressMode::Quiet {
            return;
        }

        let elapsed_ms = Self::elapsed_ms(s.started_at);
        if self.mode == ProgressMode::Json {
            let mut obj = serde_json::json!({
                "event": "finish",
                "phase": s.phase,
                "elapsed_ms": elapsed_ms,
                "ts": Self::now_iso(),
            });
            if s.done > 0 {
                obj["done"] = serde_json::json!(s.done);
            }
            if let Some(t) = s.total {
                obj["total"] = serde_json::json!(t);
            }
            if let Some(n) = note {
                obj["note"] = serde_json::json!(n);
            }
            self.emit_json(&obj);
        } else {
            let done = if s.done > 0 { Some(s.done) } else { None };
            let default_note = "done";
            let n = note.unwrap_or(default_note);
            let line = Self::render_human_line(&s.phase, done, s.total, Some(n));
            self.emit_human_line(&line);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Arc, Mutex};

    /// A writer backed by a shared buffer, so tests can assert output.
    struct SharedBuf {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Create a reporter + shared buffer for test assertions.
    fn test_reporter(
        mode: ProgressMode,
        min_interval_ms: u64,
    ) -> (ProgressReporter, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedBuf {
            inner: Arc::clone(&buf),
        };
        let r = ProgressReporter::new(mode, min_interval_ms, Box::new(writer));
        (r, buf)
    }

    fn output_str(buf: &Arc<Mutex<Vec<u8>>>) -> String {
        let guard = buf.lock().unwrap();
        String::from_utf8_lossy(&guard).to_string()
    }

    // ── human-plain: start ───────────────────────────────────────────────

    #[test]
    fn human_start_with_total() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 1000);
        r.start("sync.import", Some(100));
        assert_eq!(output_str(&buf), "[sync.import] start\n");
    }

    #[test]
    fn human_start_without_total() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 1000);
        r.start("init", None);
        assert_eq!(output_str(&buf), "[init] start\n");
    }

    #[test]
    fn human_start_with_zero_total_treated_as_none() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 1000);
        // total=0 is treated same as no total (TS: `typeof total === 'number' && total > 0`)
        r.start("phase", Some(0));
        assert_eq!(output_str(&buf), "[phase] start\n");
    }

    // ── human-plain: tick ────────────────────────────────────────────────

    #[test]
    fn human_tick_without_total() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0); // 0ms = emit every tick
        r.start("scan", None);
        r.tick(1, None);
        r.tick(1, None);
        assert!(output_str(&buf).contains("[scan] 1\n"));
        assert!(output_str(&buf).contains("[scan] 2\n"));
    }

    #[test]
    fn human_tick_with_total_shows_fraction() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("import", Some(100));
        r.tick(1, None);
        r.tick(49, None); // 50/100 = 50%
        assert!(output_str(&buf).contains("1/100 (1%)\n"));
        assert!(output_str(&buf).contains("50/100 (50%)\n"));
    }

    #[test]
    fn human_tick_with_note() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("import", Some(100));
        r.tick(42, Some("files processed"));
        assert!(output_str(&buf).contains("[import] 42/100 (42%) files processed\n"));
    }

    #[test]
    fn human_tick_final_tick_forces_emit() {
        // min_interval_ms large, so time gate never fires. But final tick
        // (done == total) must force-emit.
        let (mut r, buf) = test_reporter(ProgressMode::Human, 600_000);
        r.start("phase", Some(3));
        r.tick(1, None); // 1 < min_items(10) AND not final → suppressed
        r.tick(2, None); // done=3 == total → forced emit
        let out = output_str(&buf);
        assert!(out.contains("[phase] 3/3 (100%)\n"), "final tick must emit: {out}");
        assert!(!out.contains("[phase] 1\n"), "intermediate tick should be suppressed: {out}");
    }

    #[test]
    fn human_tick_item_gate_triggers_before_time_gate() {
        // min_interval_ms huge, but min_items=10 → after 10 items, emit
        let (mut r, buf) = test_reporter(ProgressMode::Human, 600_000);
        r.start("phase", Some(100));
        // Ticks 1-9: suppressed (not 10 items yet, not time-gate, not final)
        for _i in 0..9 {
            r.tick(1, None);
        }
        // At tick 10: items_since_emit=10 >= min_items=10 → emit
        r.tick(1, None);
        let out = output_str(&buf);
        assert!(out.contains("[phase] 10/100 (10%)\n"), "item gate must fire: {out}");
    }

    // ── human-plain: finish ──────────────────────────────────────────────

    #[test]
    fn human_finish_default_note() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("import", Some(100));
        r.tick(42, None);
        r.finish(None);
        let out = output_str(&buf);
        assert!(out.contains("[import] 42/100 (42%) done\n"), "finish with done note: {out}");
    }

    #[test]
    fn human_finish_custom_note() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("import", Some(100));
        r.tick(42, None);
        r.finish(Some("complete"));
        let out = output_str(&buf);
        assert!(out.contains("[import] 42/100 (42%) complete\n"), "custom note: {out}");
    }

    #[test]
    fn human_finish_zero_done_omits_count() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("init", None);
        r.finish(None);
        let out = output_str(&buf);
        assert_eq!(out, "[init] start\n[init] done\n", "zero done omitted: {out}");
    }

    #[test]
    fn human_auto_finish_on_second_start() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("phase1", Some(10));
        r.tick(5, None);
        // start phase2 → auto-finishes phase1
        r.start("phase2", Some(20));
        let out = output_str(&buf);
        assert!(out.contains("[phase1] 5/10 (50%) done\n"), "phase1 auto-finished: {out}");
        assert!(out.contains("[phase2] start\n"), "phase2 started: {out}");
    }

    // ── json mode ────────────────────────────────────────────────────────

    #[test]
    fn json_start_event() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 1000);
        r.start("sync.import", Some(100));
        let out = output_str(&buf);
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["event"], "start");
        assert_eq!(v["phase"], "sync.import");
        assert_eq!(v["total"], 100);
        assert!(v["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn json_start_without_total() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 1000);
        r.start("init", None);
        let out = output_str(&buf);
        let v: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(v["event"], "start");
        assert_eq!(v["phase"], "init");
        assert!(v.get("total").is_none());
    }

    #[test]
    fn json_tick_event() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 0);
        r.start("import", Some(100));
        r.tick(50, Some("halfway"));
        let out = output_str(&buf);
        // Second line is the tick (first is start)
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 2, "expected start + tick lines");
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["event"], "tick");
        assert_eq!(v["done"], 50);
        assert_eq!(v["total"], 100);
        assert_eq!(v["pct"], 50.0);
        assert_eq!(v["note"], "halfway");
        assert!(v["elapsed_ms"].as_u64().is_some());
        assert!(v["eta_ms"].as_u64().is_some());
    }

    #[test]
    fn json_tick_without_total_omits_pct_and_eta() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 0);
        r.start("scan", None);
        r.tick(42, None);
        let out = output_str(&buf);
        let lines: Vec<&str> = out.trim().lines().collect();
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["event"], "tick");
        assert_eq!(v["done"], 42);
        assert!(v.get("total").is_none());
        assert!(v.get("pct").is_none());
        assert!(v.get("eta_ms").is_none());
    }

    #[test]
    fn json_finish_event() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 0);
        r.start("import", Some(100));
        r.tick(100, None);
        r.finish(None);
        let out = output_str(&buf);
        let lines: Vec<&str> = out.trim().lines().collect();
        assert_eq!(lines.len(), 3, "expected start + tick + finish");
        let v: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(v["event"], "finish");
        assert_eq!(v["phase"], "import");
        assert_eq!(v["done"], 100);
        assert_eq!(v["total"], 100);
        assert!(v["elapsed_ms"].as_u64().is_some());
    }

    #[test]
    fn json_finish_zero_done_omits_done_field() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 0);
        r.start("phase", Some(10));
        r.finish(None);
        let out = output_str(&buf);
        let lines: Vec<&str> = out.trim().lines().collect();
        let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v["event"], "finish");
        assert!(v.get("done").is_none(), "zero done must be omitted");
    }

    #[test]
    fn json_finish_custom_note() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 0);
        r.start("phase", Some(10));
        r.tick(10, None);
        r.finish(Some("all done"));
        let out = output_str(&buf);
        let lines: Vec<&str> = out.trim().lines().collect();
        let v: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(v["note"], "all done");
    }

    // ── quiet mode ───────────────────────────────────────────────────────

    #[test]
    fn quiet_produces_nothing() {
        let (mut r, buf) = test_reporter(ProgressMode::Quiet, 1000);
        r.start("import", Some(100));
        r.tick(50, None);
        r.finish(None);
        assert!(output_str(&buf).is_empty(), "quiet must produce no output");
    }

    // ── throttling behavior ──────────────────────────────────────────────

    #[test]
    fn throttle_min_interval_zero_emits_every_tick() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("phase", Some(100));
        r.tick(1, None);
        r.tick(1, None);
        r.tick(1, None);
        let out = output_str(&buf);
        // start + 3 ticks
        assert_eq!(out.lines().count(), 4, "min_interval=0 should emit every tick");
    }

    #[test]
    fn tick_when_no_phase_is_noop() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.tick(1, None); // no start → no-op
        assert!(output_str(&buf).is_empty());
    }

    #[test]
    fn finish_when_no_phase_is_noop() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.finish(None); // no start → no-op
        assert!(output_str(&buf).is_empty());
    }

    #[test]
    fn tick_with_note_in_no_total_mode() {
        let (mut r, buf) = test_reporter(ProgressMode::Human, 0);
        r.start("scan", None);
        r.tick(1, Some("processing"));
        assert!(output_str(&buf).contains("[scan] 1 processing\n"));
    }

    #[test]
    fn json_tick_pct_one_decimal() {
        let (mut r, buf) = test_reporter(ProgressMode::Json, 0);
        r.start("phase", Some(1000));
        // 1/1000 = 0.1% → pct should be 0.1, NOT 0 (floor behavior)
        r.tick(1, None);
        r.tick(2, None); // 3/1000 = 0.3%
        let out = output_str(&buf);
        let lines: Vec<&str> = out.trim().lines().collect();
        let v1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v1["pct"], 0.1, "1/1000 should be 0.1%");
        let v2: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(v2["pct"], 0.3, "3/1000 should be 0.3%");
    }
}
