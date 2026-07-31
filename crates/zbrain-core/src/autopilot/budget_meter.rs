//! Cumulative max-cost meter for dream-cycle phases (auto-think + drift).
//!
//! Ported from `src/core/cycle/budget-meter.ts` (v0.37.x). Preserves the public
//! shape `BudgetMeter` / `SubmitEstimate` / `BudgetCheckResult` so the
//! dream-cycle call sites keep working. This is a **pre-call max-cost gate**:
//! [`BudgetMeter::check`] accumulates the *projected* max cost of each planned
//! submit and refuses the next submit once cumulative projected cost would
//! exceed the cycle cap. That is semantically distinct from
//! [`crate::budget::BudgetTracker`], which is a *post-call* actual-usage
//! accumulator (`reserve` only projects; `record` adds the real spend). The two
//! modules deliberately share the pricing + audit-row schema-v1 primitives from
//! [`crate::budget`] but keep separate ledgers.
//!
//! ## Pricing
//! [`estimate_max_cost_usd`] delegates to the registry-backed
//! `crate::budget::cost_for_usage` (chat kind). The TypeScript original sourced
//! prices from a standalone `ANTHROPIC_PRICING` map; Rust has no such map — the
//! registry is the single source of truth, so `ANTHROPIC_PRICING` is not
//! re-exported. A model missing from the registry prices at `None` (unpriced),
//! which bypasses the gate with a one-process warn-once
//! (`BUDGET_METER_NO_PRICING`).
//!
//! ## Ledger
//! `check()` appends one JSONL row per submit to
//! `~/.zbrain/audit/dream-budget-YYYY-Www.jsonl` (ISO-week rotation, matching
//! the TS `isoWeekFilename('dream-budget')`). Best-effort — audit failure never
//! gates the cycle (mirrors the TS `writeLedgerLine` try/catch).

use crate::budget::{cost_for_usage, BudgetKind};
use chrono::{DateTime, Datelike, Utc};
use serde_json::json;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Construction options for [`BudgetMeter`].
///
/// `audit_dir` is the directory the ISO-week `dream-budget-*.jsonl` ledger is
/// written under. It is injected (not read from env here) to keep the type
/// pure/testable — the cycle orchestrator resolves `~/.zbrain/audit` via
/// `crate::skillpack::audit::resolve_audit_dir()` and passes it in.
/// `audit_path` overrides the full ledger file path (tests); when `None` the
/// ISO-week file under `audit_dir` is used.
#[derive(Debug, Clone)]
pub struct BudgetMeterOpts {
    /// USD cap for the whole cycle. `<= 0` disables the gate.
    pub budget_usd: f64,
    /// Phase label for telemetry: `"auto_think"` | `"drift"`.
    pub phase: String,
    /// Directory for the ISO-week `dream-budget-*.jsonl` ledger.
    pub audit_dir: PathBuf,
    /// Optional full-path override for the ledger file (tests).
    pub audit_path: Option<PathBuf>,
}

/// A planned submit, projected against the cap before the LLM call is made.
/// Mirrors the TS `SubmitEstimate`.
#[derive(Debug, Clone)]
pub struct SubmitEstimate {
    /// Resolved model id (e.g. `"claude-opus-4-7"` or `"openai:gpt-5.2"`).
    pub model_id: String,
    /// Best-guess input token count (caller computes from prompt size).
    pub estimated_input_tokens: u64,
    /// Max output tokens passed to the LLM call (upper-bounds output cost).
    pub max_output_tokens: u64,
    /// Logical label for the submit (synthesize / verdict / drift / ...).
    pub label: Option<String>,
}

/// Result of [`BudgetMeter::check`]. Mirrors the TS `BudgetCheckResult`.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetCheckResult {
    pub allowed: bool,
    pub estimated_cost_usd: f64,
    pub cumulative_cost_usd: f64,
    pub budget_usd: f64,
    pub reason: Option<String>,
    /// True when the model wasn't in the pricing map (submit runs unbounded).
    pub unpriced: bool,
}

/// Compute the max USD cost for a chat call, or `None` when the model is
/// unpriced. Faithful re-export of the TS `estimateMaxCostUsd` (which sourced
/// prices from `ANTHROPIC_PRICING`). Rust uses the registry-backed
/// `cost_for_usage` (chat kind) as the single source of truth.
#[must_use]
pub fn estimate_max_cost_usd(
    model_id: &str,
    estimated_input_tokens: u64,
    max_output_tokens: u64,
) -> Option<f64> {
    cost_for_usage(model_id, estimated_input_tokens, max_output_tokens, BudgetKind::Chat)
}

/// Compute the ISO-week-rotated dream-budget ledger filename
/// `dream-budget-YYYY-Www.jsonl`, mirroring the TS `isoWeekFilename('dream-budget')`.
#[must_use]
pub fn dream_budget_audit_filename(now: DateTime<Utc>) -> String {
    let iso = now.iso_week();
    format!("dream-budget-{:04}-W{:02}.jsonl", iso.year(), iso.week())
}

// ---- warn-once memo (one process; per model id) ----

/// One-process warn-once memo for missing pricing, keyed per `model_id` to
/// match the TS module-level `_unpricedWarnings` set.
static UNPRICED_WARNINGS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Reset the warn-once memo. Test seam mirroring the TS
/// `_resetBudgetMeterWarningsForTest`.
pub fn reset_budget_meter_warnings_for_test() {
    if let Ok(mut guard) = UNPRICED_WARNINGS.lock() {
        *guard = Some(HashSet::new());
    }
}

/// Register a warn-once key; returns `true` the first time the key is seen.
fn warn_once(key: &str) -> bool {
    let Ok(mut guard) = UNPRICED_WARNINGS.lock() else {
        return false;
    };
    let set = guard.get_or_insert_with(HashSet::new);
    set.insert(key.to_string())
}

/// A single-cycle cumulative max-cost meter for dream-cycle phases.
///
/// Interior mutability (`Mutex<f64>` + `AtomicU64`) lets the meter be shared
/// across the async submit loop while `check` still updates cumulative spend.
pub struct BudgetMeter {
    opts: BudgetMeterOpts,
    cumulative_usd: Mutex<f64>,
    unpriced_submits: AtomicU64,
}

impl BudgetMeter {
    /// Create a meter. `audit_dir` (or `audit_path` override) controls where
    /// the ISO-week `dream-budget-*.jsonl` ledger is written.
    #[must_use]
    pub fn new(opts: BudgetMeterOpts) -> Self {
        Self {
            opts,
            cumulative_usd: Mutex::new(0.0),
            unpriced_submits: AtomicU64::new(0),
        }
    }

    /// Resolve the ledger file path: `audit_path` override, else ISO-week file
    /// under `audit_dir`.
    fn ledger_path(&self, now: DateTime<Utc>) -> PathBuf {
        match &self.opts.audit_path {
            Some(p) => p.clone(),
            None => self.opts.audit_dir.join(dream_budget_audit_filename(now)),
        }
    }

    /// Best-effort append of one JSON audit row. Failure writes a stderr
    /// warning and returns — must never gate the cycle.
    fn write_ledger(&self, now: DateTime<Utc>, entry: &serde_json::Value) {
        use std::io::Write;
        let write = || -> std::io::Result<()> {
            let path = self.ledger_path(now);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut line = serde_json::to_string(entry)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            line.push('\n');
            let mut file =
                std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
            file.write_all(line.as_bytes())?;
            Ok(())
        };
        if let Err(e) = write() {
            eprintln!("[zbrain] dream-budget audit write failed ({e}); cycle continues");
        }
    }

    /// Check whether a planned submit fits within the remaining budget.
    ///
    /// Records the attempt to the ledger regardless of allow/deny. The caller
    /// is responsible for skipping the actual LLM call when `allowed == false`.
    ///
    /// # Semantics (mirrors TS `BudgetMeter.check`)
    /// - Unpriced model → gate bypassed (`allowed: true`, `unpriced: true`),
    ///   `cumulative` unchanged, warn-once per model id.
    /// - `budget_usd <= 0` → gate disabled, cost accumulated, `allowed: true`.
    /// - Projected (`cumulative + cost`) `> budget_usd` → `allowed: false`,
    ///   `cumulative` unchanged, `reason: "BUDGET_EXHAUSTED"`.
    /// - Otherwise → cost accumulated, `allowed: true`.
    pub fn check(&self, estimate: &SubmitEstimate) -> BudgetCheckResult {
        let now = Utc::now();
        let budget = self.opts.budget_usd;

        // Unpriced model → bypass the gate (warn-once per model id).
        let Some(cost) =
            estimate_max_cost_usd(&estimate.model_id, estimate.estimated_input_tokens, estimate.max_output_tokens)
        else {
            self.unpriced_submits.fetch_add(1, Ordering::Relaxed);
            if warn_once(&estimate.model_id) {
                eprintln!(
                    "[budget] BUDGET_METER_NO_PRICING: model \"{}\" not in registry pricing. \
                     Budget gate disabled for this submit. (Per-provider pricing modules: TODO.)",
                    estimate.model_id,
                );
            }
            let cumulative = self.total_spent();
            self.write_ledger(
                now,
                &json!({
                    "schema_version": 1,
                    "phase": self.opts.phase,
                    "ts": now.to_rfc3339(),
                    "event": "submit_unpriced",
                    "model": estimate.model_id,
                    "label": estimate.label,
                    "estimated_input_tokens": estimate.estimated_input_tokens,
                    "max_output_tokens": estimate.max_output_tokens,
                }),
            );
            return BudgetCheckResult {
                allowed: true,
                estimated_cost_usd: 0.0,
                cumulative_cost_usd: cumulative,
                budget_usd: budget,
                reason: None,
                unpriced: true,
            };
        };

        // Gate disabled (<= 0): accumulate and allow.
        if budget <= 0.0 {
            let cumulative = {
                let mut c = self
                    .cumulative_usd
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                *c += cost;
                *c
            };
            self.write_ledger(
                now,
                &json!({
                    "schema_version": 1,
                    "phase": self.opts.phase,
                    "ts": now.to_rfc3339(),
                    "event": "submit",
                    "model": estimate.model_id,
                    "label": estimate.label,
                    "estimated_cost_usd": cost,
                    "cumulative_cost_usd": cumulative,
                    "budget_usd": budget,
                }),
            );
            return BudgetCheckResult {
                allowed: true,
                estimated_cost_usd: cost,
                cumulative_cost_usd: cumulative,
                budget_usd: budget,
                reason: None,
                unpriced: false,
            };
        }

        // Gated path: compute without holding the lock across the ledger write.
        let (cumulative, projected, new_cumulative) = {
            let mut c = self
                .cumulative_usd
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let cumulative = *c;
            let projected = cumulative + cost;
            if projected > budget {
                (cumulative, projected, cumulative)
            } else {
                *c += cost;
                (cumulative, projected, *c)
            }
        };

        if projected > budget {
            self.write_ledger(
                now,
                &json!({
                    "schema_version": 1,
                    "phase": self.opts.phase,
                    "ts": now.to_rfc3339(),
                    "event": "submit_denied",
                    "model": estimate.model_id,
                    "label": estimate.label,
                    "estimated_cost_usd": cost,
                    "cumulative_cost_usd": cumulative,
                    "budget_usd": budget,
                }),
            );
            return BudgetCheckResult {
                allowed: false,
                estimated_cost_usd: cost,
                cumulative_cost_usd: cumulative,
                budget_usd: budget,
                reason: Some(format!(
                    "BUDGET_EXHAUSTED: projected ${:.4} > cap ${:.2}",
                    projected, budget
                )),
                unpriced: false,
            };
        }

        self.write_ledger(
            now,
            &json!({
                "schema_version": 1,
                "phase": self.opts.phase,
                "ts": now.to_rfc3339(),
                "event": "submit",
                "model": estimate.model_id,
                "label": estimate.label,
                "estimated_cost_usd": cost,
                "cumulative_cost_usd": new_cumulative,
                "budget_usd": budget,
            }),
        );
        BudgetCheckResult {
            allowed: true,
            estimated_cost_usd: cost,
            cumulative_cost_usd: new_cumulative,
            budget_usd: budget,
            reason: None,
            unpriced: false,
        }
    }

    /// Cumulative max-cost spent so far this cycle.
    #[must_use]
    pub fn total_spent(&self) -> f64 {
        *self
            .cumulative_usd
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Count of submits that bypassed the gate due to missing pricing.
    #[must_use]
    pub fn unpriced_submits(&self) -> u64 {
        self.unpriced_submits.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn opts(budget_usd: f64, audit_dir: &std::path::Path, audit_path: Option<std::path::PathBuf>) -> BudgetMeterOpts {
        BudgetMeterOpts {
            budget_usd,
            phase: "auto_think".to_string(),
            audit_dir: audit_dir.to_path_buf(),
            audit_path,
        }
    }

    fn estimate(model: &str, inp: u64, out: u64) -> SubmitEstimate {
        SubmitEstimate {
            model_id: model.to_string(),
            estimated_input_tokens: inp,
            max_output_tokens: out,
            label: Some("synthesize".to_string()),
        }
    }

    // ---- pricing ----

    #[test]
    fn estimate_max_cost_usd_priced() {
        // openai:gpt-5.2 = input 1.25 + output 10.0 per 1M (registry).
        let c = estimate_max_cost_usd("openai:gpt-5.2", 1_000_000, 1_000_000).unwrap();
        assert!((c - 11.25).abs() < 1e-6);
    }

    #[test]
    fn estimate_max_cost_usd_unpriced_is_none() {
        assert_eq!(estimate_max_cost_usd("nope:unpriced", 100, 100), None);
    }

    // ---- check ----

    #[test]
    fn check_unpriced_bypasses_gate() {
        reset_budget_meter_warnings_for_test();
        let dir = TempDir::new().unwrap();
        let m = BudgetMeter::new(opts(10.0, dir.path(), None));
        let r = m.check(&estimate("nope:unpriced", 100, 100));
        assert!(r.allowed);
        assert!(r.unpriced);
        assert_eq!(r.estimated_cost_usd, 0.0);
        assert_eq!(r.cumulative_cost_usd, 0.0);
        assert_eq!(m.unpriced_submits(), 1);
        assert_eq!(m.total_spent(), 0.0);
    }

    #[test]
    fn check_disabled_when_budget_le_zero() {
        let dir = TempDir::new().unwrap();
        let m = BudgetMeter::new(opts(0.0, dir.path(), None));
        let r = m.check(&estimate("openai:gpt-5.2", 1_000_000, 0)); // $1.25
        assert!(r.allowed);
        assert!(!r.unpriced);
        assert!((r.estimated_cost_usd - 1.25).abs() < 1e-6);
        assert!((m.total_spent() - 1.25).abs() < 1e-6);
    }

    #[test]
    fn check_allows_under_cap() {
        let dir = TempDir::new().unwrap();
        let m = BudgetMeter::new(opts(100.0, dir.path(), None));
        let r = m.check(&estimate("openai:gpt-5.2", 1000, 1000));
        assert!(r.allowed);
        assert!((m.total_spent() - r.estimated_cost_usd).abs() < 1e-9);
    }

    #[test]
    fn check_denies_when_exhausted() {
        let dir = TempDir::new().unwrap();
        let m = BudgetMeter::new(opts(0.001, dir.path(), None));
        // 1M input @1.25 → $1.25 projected, far over $0.001 cap.
        let r = m.check(&estimate("openai:gpt-5.2", 1_000_000, 0));
        assert!(!r.allowed);
        assert!(r.reason.unwrap().contains("BUDGET_EXHAUSTED"));
        // Denied submits do NOT accumulate.
        assert_eq!(m.total_spent(), 0.0);
    }

    #[test]
    fn check_accumulates_cumulative() {
        let dir = TempDir::new().unwrap();
        let m = BudgetMeter::new(opts(100.0, dir.path(), None));
        m.check(&estimate("openai:gpt-5.2", 1_000_000, 0)); // $1.25
        m.check(&estimate("openai:gpt-5.2", 1_000_000, 0)); // +$1.25
        assert!((m.total_spent() - 2.50).abs() < 1e-6);
    }

    // ---- ledger ----

    #[test]
    fn dream_budget_audit_filename_iso_week() {
        let now = Utc.with_ymd_and_hms(2026, 7, 6, 12, 0, 0).unwrap();
        assert_eq!(dream_budget_audit_filename(now), "dream-budget-2026-W28.jsonl");
    }

    #[test]
    fn check_writes_ledger_row() {
        let dir = TempDir::new().unwrap();
        let ledger = dir.path().join("dream-budget-test.jsonl");
        let m = BudgetMeter::new(opts(100.0, dir.path(), Some(ledger.clone())));
        m.check(&estimate("openai:gpt-5.2", 1_000_000, 0)); // $1.25
        let content = std::fs::read_to_string(&ledger).unwrap();
        let row: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(row["event"], "submit");
        assert_eq!(row["schema_version"], 1);
        assert_eq!(row["model"], "openai:gpt-5.2");
        assert_eq!(row["phase"], "auto_think");
        assert!((row["estimated_cost_usd"].as_f64().unwrap() - 1.25).abs() < 1e-6);
        assert!((row["cumulative_cost_usd"].as_f64().unwrap() - 1.25).abs() < 1e-6);
    }

    #[test]
    fn check_denied_writes_denied_row_not_submit() {
        let dir = TempDir::new().unwrap();
        let ledger = dir.path().join("dream-budget-denied.jsonl");
        let m = BudgetMeter::new(opts(0.001, dir.path(), Some(ledger.clone())));
        m.check(&estimate("openai:gpt-5.2", 1_000_000, 0));
        let content = std::fs::read_to_string(&ledger).unwrap();
        let row: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(row["event"], "submit_denied");
        assert_eq!(row["cumulative_cost_usd"].as_f64().unwrap(), 0.0);
    }
}
