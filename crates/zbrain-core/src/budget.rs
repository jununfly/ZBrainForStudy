//! Unified budget tracker for every gateway-routed LLM call (chat / embed /
//! rerank). Ported from the TypeScript `src/core/budget/budget-tracker.ts`.
//!
//! One tracker, one error type ([`BudgetExhausted`]), one audit JSONL schema.
//! A [`BudgetTracker`] is a mutable, single-scope accumulator: `cumulative_usd`
//! grows only in [`BudgetTracker::record`] and is compared against the optional
//! `max_cost_usd` cap. There is **no** cross-process or per-week aggregation —
//! ISO-week rotation applies only to the audit file name, exactly as in
//! [`crate::rerank_audit`].
//!
//! ## Injection (decided 1-4-3)
//! The TS runtime installs the tracker via `AsyncLocalStorage` so that
//! `gateway.{chat,embed,rerank}` auto-record with no per-call seam. Rust has no
//! idiomatic async ambient singleton, so callers pass `Option<&BudgetTracker>`
//! explicitly down the chat/embed/rerank call chain. `None` means "no budget
//! scope active" and every budget operation becomes a no-op — byte-for-byte the
//! same effect as the TS `__budgetStore.getStore() ?? null` miss.
//!
//! ## Locked contracts (mirrors the TS `/plan-eng-review` contracts)
//! - **TX1**: [`BudgetTracker::record`] returns `Err(BudgetExhausted{reason:Cost})`
//!   AFTER updating cumulative spend when `cumulative > max_cost_usd`. The cap is
//!   a real ceiling: a single under-estimated call can still trip it.
//! - **TX2**: when `max_cost_usd` is set AND the model is not priced,
//!   [`BudgetTracker::reserve`] hard-fails with `reason:NoPricing`. When the cap
//!   is unset, missing pricing warns once (per `(model, kind)`) and is allowed.
//! - **Delayed-throw**: the chat call site swallows the `record` TX1 error so the
//!   original provider result/error surfaces first; the budget overflow then
//!   re-surfaces at the *next* `reserve`. See [`BudgetTracker::record`] docs and
//!   the gateway wiring in [`crate::ai::chat`].
//! - Audit is best-effort: a disk-full audit never gates the LLM call, matching
//!   [`crate::rerank_audit`]'s posture.
//!
//! ## BudgetAuditor trait
//! The built-in audit writes JSONL to the filesystem directly. Callers that
//! want to redirect audit output (test harness, metrics sink, multi-tenant log
//! router) can inject a custom [`BudgetAuditor`] via
//! [`BudgetTrackerOpts::auditor`]. The default is [`NoopAuditor`] which
//! discards all audit rows; test helpers use it to avoid disk I/O.
//!
//! ## Pricing source
//! Chat/rerank prices come from the `REGISTRY` touchpoints (chat: per-input +
//! per-output USD/1M-tok; rerank/embed: single USD/1M-tok), NOT a standalone
//! table — same single-source-of-truth rule the embedding path follows in
//! [`crate::ai::lookup_pricing`]. The pricing maps carry **no** cache-token
//! price; `cache_read`/`cache_creation` tokens are surfaced in `ChatUsage` but
//! never billed, exactly as in TS.

use crate::ai::{parse_model_id, resolve_recipe, REGISTRY};
use chrono::{DateTime, Datelike, Utc};
use serde_json::json;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;

// ---- BudgetAuditor ----

/// Sink for budget audit rows. Each row is a JSON object with `schema_version`,
/// `ts`, `event`, and event-specific fields. The default [`NoopAuditor`]
/// discards all rows; the filesystem auditor is the built-in
/// `append_audit_line` path wired inside [`BudgetTracker`].
///
/// Object-safe: callers store `Box<dyn BudgetAuditor>` or `Arc<dyn>`.
/// Implementations MUST be `Send + Sync` (tracker is shared across tasks).
///
/// # Errors
/// The trait returns `std::io::Result` so file-based impls can signal
/// disk-full. BudgetTracker always swallows the error — audit write failure
/// never gates the LLM call, matching the TS posture.
pub trait BudgetAuditor: Send + Sync {
    fn record_audit(&self, row: &serde_json::Value) -> std::io::Result<()>;
}

/// Default auditor that discards all rows. Use in tests or when the caller
/// doesn't care about audit persistence.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopAuditor;

impl BudgetAuditor for NoopAuditor {
    fn record_audit(&self, _row: &serde_json::Value) -> std::io::Result<()> {
        Ok(())
    }
}

// ---- audit types ----
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetKind {
    Chat,
    Embed,
    Rerank,
}

impl BudgetKind {
    /// Stable wire string used in audit rows.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BudgetKind::Chat => "chat",
            BudgetKind::Embed => "embed",
            BudgetKind::Rerank => "rerank",
        }
    }
}

/// Why the budget was exhausted. Mirrors the TS `BudgetReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetReason {
    /// Projected/cumulative USD cost exceeded `max_cost_usd`.
    Cost,
    /// Wall-clock elapsed exceeded `max_runtime_ms`.
    Runtime,
    /// A cap is set but the model has no pricing entry (TX2 hard-fail).
    NoPricing,
}

impl BudgetReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BudgetReason::Cost => "cost",
            BudgetReason::Runtime => "runtime",
            BudgetReason::NoPricing => "no_pricing",
        }
    }
}

/// The budget-cap error. A gate/quota signal, deliberately distinct from the
/// transport-layer `ChatError` (a budget module must not depend on chat).
/// Mirrors the TS `BudgetExhausted` (message + reason + spent + cap + model).
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetExhausted {
    /// Human-readable summary (operator-facing, mirrors the TS message text).
    pub message: String,
    /// Machine-readable cause.
    pub reason: BudgetReason,
    /// For cost: cumulative USD spent so far. For runtime: elapsed ms.
    pub spent: f64,
    /// For cost: `max_cost_usd`. For runtime: `max_runtime_ms`.
    pub cap: f64,
    /// The model id in flight when the budget tripped, if known.
    pub model_id: Option<String>,
}

impl std::fmt::Display for BudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BudgetExhausted {}

/// A planned call, projected against the cap before the provider is hit.
/// Mirrors the TS `BudgetEstimate`.
#[derive(Debug, Clone)]
pub struct BudgetEstimate {
    pub model_id: String,
    pub estimated_input_tokens: u64,
    pub max_output_tokens: u64,
    pub kind: BudgetKind,
    /// Optional telemetry sub-label (e.g. `"brainstorm.cross"`).
    pub label: Option<String>,
}

/// Actual usage recorded after the provider returned (or threw). Mirrors the
/// TS `BudgetActualUsage`.
#[derive(Debug, Clone)]
pub struct BudgetActualUsage {
    pub model_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Embedding dimension count — audit-only metadata.
    pub embedding_dims: Option<u64>,
    pub kind: BudgetKind,
    pub label: Option<String>,
}

/// A point-in-time view of tracker state. Mirrors the TS `BudgetSnapshot`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetSnapshot {
    pub cumulative_cost_usd: f64,
    pub elapsed_ms: u64,
    pub max_cost_usd: Option<f64>,
    pub max_runtime_ms: Option<u64>,
    pub calls_recorded: u64,
}

/// Construction options. `label` is required (audit provenance); the two caps
/// and the audit-dir override are optional. Mirrors the TS `BudgetTrackerOpts`.
#[derive(Debug, Clone)]
pub struct BudgetTrackerOpts {
    /// USD cap. `None` disables the cost gate (pricing misses warn once).
    pub max_cost_usd: Option<f64>,
    /// Wall-clock cap in ms. `None` disables the runtime gate.
    pub max_runtime_ms: Option<u64>,
    /// Phase/command label stamped into every audit row.
    pub label: String,
}

// ---- pricing (chat/rerank per-1M-tok tuple; embed single price) ----

/// A per-1M-token price pair. `output` is 0 for embed/rerank.
#[derive(Debug, Clone, Copy, PartialEq)]
struct ModelPricing {
    input: f64,
    output: f64,
}

/// Provider ids whose rerank runs on local inference (electricity, not tokens)
/// and so price at $0. Matches the TS `FREE_LOCAL_RERANK_PROVIDERS`.
const FREE_LOCAL_RERANK_PROVIDERS: &[&str] = &["llama-server-reranker"];

/// Provider ids whose embeddings run on local inference and so price at $0.
/// Matches the TS `FREE_LOCAL_EMBED_PROVIDERS` (lmstudio/litellm deliberately
/// excluded — see the TS note).
const FREE_LOCAL_EMBED_PROVIDERS: &[&str] = &["ollama", "llama-server"];

/// Look up chat pricing for a `provider:model` (or bare model) string by
/// scanning the registry's chat touchpoints. The scan mirrors
/// [`crate::ai::lookup_pricing`] (embedding): a known provider prefix scopes the
/// search, otherwise the first chat touchpoint listing the model wins. Returns
/// `None` when the model is unlisted or its recipe declares no chat cost.
fn lookup_chat_pricing(model: &str) -> Option<ModelPricing> {
    let (scoped_provider, bare_model) = match parse_model_id(model) {
        Some((p, m)) if resolve_recipe(p).is_some() => (Some(p), m),
        _ => (None, model),
    };
    for recipe in REGISTRY {
        if let Some(p) = scoped_provider {
            if recipe.id != p {
                continue;
            }
        }
        let Some(chat) = recipe.touchpoints.chat else {
            continue;
        };
        let listed = chat.models.iter().any(|&m| m == bare_model || m == model);
        if listed {
            let input = chat.cost_per_1m_input_usd?;
            let output = chat.cost_per_1m_output_usd?;
            return Some(ModelPricing { input, output });
        }
    }
    None
}

/// Look up embedding pricing for a `provider:model` string, returning an
/// input-only tuple. Local-inference embed providers price at $0 so a
/// `--max-cost`-bounded reindex does not TX2 hard-fail. Mirrors the embed arm
/// of the TS `lookupPricing`.
fn lookup_embed_pricing(model: &str) -> Option<ModelPricing> {
    if let Some(p) = crate::ai::lookup_pricing(model) {
        return Some(ModelPricing {
            input: p.price_per_mtok_usd,
            output: 0.0,
        });
    }
    // Local-inference embed providers cost electricity, not tokens → $0.
    if let Some((provider, _)) = parse_model_id(model) {
        if FREE_LOCAL_EMBED_PROVIDERS.contains(&provider) {
            return Some(ModelPricing {
                input: 0.0,
                output: 0.0,
            });
        }
    }
    None
}

/// Look up rerank pricing. Tries chat-style registry pricing first (a Claude-
/// priced rerank would live there), then zero-prices local-inference rerank
/// providers so `--max-cost` callers don't TX2 hard-fail. Mirrors the rerank
/// arm of the TS `lookupPricing`.
fn lookup_rerank_pricing(model: &str) -> Option<ModelPricing> {
    // Registry rerank touchpoints carry a single per-1M-tok price.
    let (scoped_provider, bare_model) = match parse_model_id(model) {
        Some((p, m)) if resolve_recipe(p).is_some() => (Some(p), m),
        _ => (None, model),
    };
    for recipe in REGISTRY {
        if let Some(p) = scoped_provider {
            if recipe.id != p {
                continue;
            }
        }
        let Some(rr) = recipe.touchpoints.reranker else {
            continue;
        };
        let listed = rr.models.iter().any(|&m| m == bare_model || m == model)
            || rr.default_model == bare_model
            || rr.default_model == model;
        if listed {
            if let Some(price) = rr.cost_per_1m_tokens_usd {
                return Some(ModelPricing {
                    input: price,
                    output: 0.0,
                });
            }
        }
    }
    // Local-inference rerank providers price at $0.
    if let Some((provider, _)) = parse_model_id(model) {
        if FREE_LOCAL_RERANK_PROVIDERS.contains(&provider) {
            return Some(ModelPricing {
                input: 0.0,
                output: 0.0,
            });
        }
    }
    None
}

/// Dispatch pricing lookup by kind. Mirrors the TS `lookupPricing(modelId, kind)`.
fn lookup_pricing_for_kind(model: &str, kind: BudgetKind) -> Option<ModelPricing> {
    match kind {
        BudgetKind::Chat => lookup_chat_pricing(model),
        BudgetKind::Embed => lookup_embed_pricing(model),
        BudgetKind::Rerank => lookup_rerank_pricing(model),
    }
}

/// Compute USD cost for a usage tuple, or `None` when the model is unpriced.
/// Mirrors the TS `costForUsage`. `pub(crate)` so the dream-cycle
/// `autopilot::budget_meter` can reuse the registry-backed pricing as the
/// single source of truth (the TS original used a standalone `ANTHROPIC_PRICING`
/// map, which Rust has no equivalent of).
pub(crate) fn cost_for_usage(
    model: &str,
    input_tokens: u64,
    output_tokens: u64,
    kind: BudgetKind,
) -> Option<f64> {
    let p = lookup_pricing_for_kind(model, kind)?;
    let cost = (input_tokens as f64 / 1_000_000.0) * p.input
        + (output_tokens as f64 / 1_000_000.0) * p.output;
    Some(cost)
}

// ---- audit (ISO-week JSONL, mirrors crate::rerank_audit) ----

/// Compute the ISO-week-rotated budget audit filename `budget-YYYY-Www.jsonl`.
/// Same `%G-W%V` ISO-week rule as [`crate::rerank_audit::rerank_audit_filename`].
#[must_use]
pub fn budget_audit_filename(now: DateTime<Utc>) -> String {
    let iso = now.iso_week();
    format!("budget-{:04}-W{:02}.jsonl", iso.year(), iso.week())
}

/// Best-effort append of one JSON audit row to the current ISO week's file
/// under `audit_dir`. Failure writes a stderr warning and returns — an audit
/// write MUST never gate the LLM call. Mirrors the TS `appendAuditLine`.
fn append_audit_line(audit_dir: &Path, now: DateTime<Utc>, entry: &serde_json::Value) {
    use std::io::Write;
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(audit_dir)?;
        let path = audit_dir.join(budget_audit_filename(now));
        let mut line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        file.write_all(line.as_bytes())?;
        Ok(())
    };
    if let Err(e) = write() {
        eprintln!("[zbrain] budget audit write failed ({e}); LLM call continues");
    }
}
// ---- warn-once memo (one process; per (model, kind)) ----

/// One-process warn-once memo for missing pricing per `(model, kind)`, matching
/// the TS module-level `_unpricedWarnings` set. Guarded by a `Mutex` since a
/// tracker may be shared across async tasks.
static UNPRICED_WARNINGS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Reset the warn-once memo. Test seam mirroring the TS
/// `_resetBudgetTrackerWarningsForTest`.
pub fn reset_budget_warnings_for_test() {
    if let Ok(mut guard) = UNPRICED_WARNINGS.lock() {
        *guard = Some(HashSet::new());
    }
}

/// Register a warn-once key; returns `true` when this is the first time the key
/// is seen (i.e. the caller should emit the warning).
fn warn_once(key: &str) -> bool {
    let Ok(mut guard) = UNPRICED_WARNINGS.lock() else {
        return false;
    };
    let set = guard.get_or_insert_with(HashSet::new);
    set.insert(key.to_string())
}

// ---- BudgetTracker ----

/// A single-scope USD + wall-clock budget accumulator. See the module docs for
/// the injection model and the locked TX1/TX2/delayed-throw contracts.
///
/// Interior mutability (`Mutex`) lets callers hold a shared `&BudgetTracker`
/// across the chat/embed/rerank call chain (the `Option<&BudgetTracker>`
/// injection seam) while `record` still mutates cumulative spend.
#[derive(Debug)]
pub struct BudgetTracker {
    opts: BudgetTrackerOpts,
    audit_dir: std::path::PathBuf,
    started_at: std::time::Instant,
    state: Mutex<TrackerState>,
}

#[derive(Debug, Default)]
struct TrackerState {
    cumulative_usd: f64,
    calls_recorded: u64,
    exhausted_fired: bool,
}

impl BudgetTracker {
    /// Create a tracker writing audit rows under `audit_dir`. The audit dir is
    /// injected (not read from env here) to keep the type pure/testable — the
    /// CLI resolves `~/.zbrain/audit` (or `ZBRAIN_AUDIT_DIR`) and passes it in,
    /// mirroring how [`crate::rerank_audit`] takes an `audit_dir: &Path`.
    #[must_use]
    pub fn new(opts: BudgetTrackerOpts, audit_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            opts,
            audit_dir: audit_dir.into(),
            started_at: std::time::Instant::now(),
            state: Mutex::new(TrackerState::default()),
        }
    }

    /// Total USD recorded so far.
    #[must_use]
    pub fn total_spent(&self) -> f64 {
        self.state.lock().map(|s| s.cumulative_usd).unwrap_or(0.0)
    }

    /// Point-in-time snapshot. Mirrors the TS `snapshot()`.
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        let (cumulative, calls) = self
            .state
            .lock()
            .map(|s| (s.cumulative_usd, s.calls_recorded))
            .unwrap_or((0.0, 0));
        BudgetSnapshot {
            cumulative_cost_usd: cumulative,
            elapsed_ms: self.elapsed_ms(),
            max_cost_usd: self.opts.max_cost_usd,
            max_runtime_ms: self.opts.max_runtime_ms,
            calls_recorded: calls,
        }
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// Fire the one-shot exhausted flag; returns `true` the first time only.
    /// The TS side runs `onExhausted` callbacks here; the Rust consumer wiring
    /// (checkpoint persistence) is a Phase 9 concern, so this just latches.
    fn fire_exhausted(&self) {
        if let Ok(mut s) = self.state.lock() {
            if !s.exhausted_fired {
                s.exhausted_fired = true;
            }
        }
    }

    /// Project a planned call against the cap BEFORE the provider is hit.
    ///
    /// # Errors
    /// - `reason:Runtime` when wall-clock exceeds `max_runtime_ms`.
    /// - `reason:NoPricing` when `max_cost_usd` is set but the model is unpriced
    ///   (TX2). When the cap is unset, missing pricing warns once and is allowed.
    /// - `reason:Cost` when `cumulative + projected > max_cost_usd`.
    pub fn reserve(&self, estimate: &BudgetEstimate) -> Result<(), BudgetExhausted> {
        let now = Utc::now();
        self.assert_runtime(&estimate.model_id, now)?;

        let projected = cost_for_usage(
            &estimate.model_id,
            estimate.estimated_input_tokens,
            estimate.max_output_tokens,
            estimate.kind,
        );

        let cumulative = self.total_spent();

        let Some(projected) = projected else {
            // Unpriced model.
            if let Some(cap) = self.opts.max_cost_usd {
                // TX2: hard-fail — cannot enforce a cap without pricing.
                self.fire_exhausted();
                let kind = estimate.kind;
                let pricing_file = if matches!(kind, BudgetKind::Embed) {
                    "embedding pricing"
                } else {
                    "chat/rerank pricing"
                };
                let msg = format!(
                    "{}: no pricing entry for model \"{}\" (kind={}). Add it to the registry {} or drop --max-cost.",
                    self.opts.label,
                    estimate.model_id,
                    kind.as_str(),
                    pricing_file,
                );
                return Err(BudgetExhausted {
                    message: msg,
                    reason: BudgetReason::NoPricing,
                    spent: cumulative,
                    cap,
                    model_id: Some(estimate.model_id.clone()),
                });
            }
            // Legacy warn-once path — cap unset.
            let memo_key = format!("{}:{}", estimate.model_id, estimate.kind.as_str());
            if warn_once(&memo_key) {
                eprintln!(
                    "[budget] BUDGET_TRACKER_NO_PRICING: model \"{}\" (kind={}) not in pricing maps. Cost gate disabled for this call.",
                    estimate.model_id,
                    estimate.kind.as_str(),
                );
            }
            append_audit_line(
                &self.audit_dir,
                now,
                &json!({
                    "schema_version": 1,
                    "ts": now.to_rfc3339(),
                    "event": "reserve_unpriced",
                    "label": self.opts.label,
                    "kind": estimate.kind.as_str(),
                    "model": estimate.model_id,
                    "sub_label": estimate.label,
                    "estimated_input_tokens": estimate.estimated_input_tokens,
                    "max_output_tokens": estimate.max_output_tokens,
                }),
            );
            return Ok(());
        };

        if let Some(cap) = self.opts.max_cost_usd {
            let after = cumulative + projected;
            if after > cap {
                append_audit_line(
                    &self.audit_dir,
                    now,
                    &json!({
                        "schema_version": 1,
                        "ts": now.to_rfc3339(),
                        "event": "reserve_denied",
                        "label": self.opts.label,
                        "kind": estimate.kind.as_str(),
                        "model": estimate.model_id,
                        "sub_label": estimate.label,
                        "projected_cost_usd": projected,
                        "cumulative_cost_usd": cumulative,
                        "max_cost_usd": cap,
                    }),
                );
                self.fire_exhausted();
                let msg = format!(
                    "{}: projected cost ${:.4} exceeds --max-cost ${:.2} (cumulative ${:.4} + this call ${:.4})",
                    self.opts.label, after, cap, cumulative, projected,
                );
                return Err(BudgetExhausted {
                    message: msg,
                    reason: BudgetReason::Cost,
                    spent: cumulative,
                    cap,
                    model_id: Some(estimate.model_id.clone()),
                });
            }
        }

        append_audit_line(
            &self.audit_dir,
            now,
            &json!({
                "schema_version": 1,
                "ts": now.to_rfc3339(),
                "event": "reserve",
                "label": self.opts.label,
                "kind": estimate.kind.as_str(),
                "model": estimate.model_id,
                "sub_label": estimate.label,
                "projected_cost_usd": projected,
                "cumulative_cost_usd": cumulative,
                "max_cost_usd": self.opts.max_cost_usd,
            }),
        );
        Ok(())
    }

    /// Record actual usage AFTER the provider returned (or threw), updating
    /// cumulative spend.
    ///
    /// # Errors
    /// TX1: returns `Err(reason:Cost)` AFTER the update when `cumulative >
    /// max_cost_usd`. Per the delayed-throw contract the chat call site
    /// swallows this so the provider result surfaces first; the overflow
    /// re-surfaces at the next `reserve`. Unpriced models record an audit row
    /// and never accumulate (cannot trip the cap).
    pub fn record(&self, actual: &BudgetActualUsage) -> Result<(), BudgetExhausted> {
        let now = Utc::now();
        {
            if let Ok(mut s) = self.state.lock() {
                s.calls_recorded += 1;
            }
        }
        let cost = cost_for_usage(
            &actual.model_id,
            actual.input_tokens,
            actual.output_tokens,
            actual.kind,
        );

        let Some(cost) = cost else {
            // Unpriced: audit only, no cumulative math.
            append_audit_line(
                &self.audit_dir,
                now,
                &json!({
                    "schema_version": 1,
                    "ts": now.to_rfc3339(),
                    "event": "record_unpriced",
                    "label": self.opts.label,
                    "kind": actual.kind.as_str(),
                    "model": actual.model_id,
                    "sub_label": actual.label,
                    "input_tokens": actual.input_tokens,
                    "output_tokens": actual.output_tokens,
                    "embedding_dims": actual.embedding_dims,
                }),
            );
            return Ok(());
        };

        let cumulative = {
            let mut s = self.state.lock().expect("budget state mutex poisoned");
            s.cumulative_usd += cost;
            s.cumulative_usd
        };
        append_audit_line(
            &self.audit_dir,
            now,
            &json!({
                "schema_version": 1,
                "ts": now.to_rfc3339(),
                "event": "record",
                "label": self.opts.label,
                "kind": actual.kind.as_str(),
                "model": actual.model_id,
                "sub_label": actual.label,
                "input_tokens": actual.input_tokens,
                "output_tokens": actual.output_tokens,
                "embedding_dims": actual.embedding_dims,
                "actual_cost_usd": cost,
                "cumulative_cost_usd": cumulative,
                "max_cost_usd": self.opts.max_cost_usd,
            }),
        );

        if let Some(cap) = self.opts.max_cost_usd {
            if cumulative > cap {
                // TX1: hard-fail — a single under-estimated call blew the cap.
                self.fire_exhausted();
                let msg = format!(
                    "{}: cumulative cost ${:.4} exceeded --max-cost ${:.2} after recording {} call to {}",
                    self.opts.label, cumulative, cap, actual.kind.as_str(), actual.model_id,
                );
                return Err(BudgetExhausted {
                    message: msg,
                    reason: BudgetReason::Cost,
                    spent: cumulative,
                    cap,
                    model_id: Some(actual.model_id.clone()),
                });
            }
        }
        Ok(())
    }

    /// Throw `reason:Runtime` when the wall-clock cap fires. Mirrors the TS
    /// `assertRuntime`.
    fn assert_runtime(&self, model_id: &str, now: DateTime<Utc>) -> Result<(), BudgetExhausted> {
        let Some(cap_ms) = self.opts.max_runtime_ms else {
            return Ok(());
        };
        let elapsed = self.elapsed_ms();
        if elapsed > cap_ms {
            append_audit_line(
                &self.audit_dir,
                now,
                &json!({
                    "schema_version": 1,
                    "ts": now.to_rfc3339(),
                    "event": "runtime_denied",
                    "label": self.opts.label,
                    "elapsed_ms": elapsed,
                    "max_runtime_ms": cap_ms,
                    "model": model_id,
                }),
            );
            self.fire_exhausted();
            let msg = format!(
                "{}: wall-clock {:.1}s exceeded --max-runtime {:.1}s",
                self.opts.label,
                elapsed as f64 / 1000.0,
                cap_ms as f64 / 1000.0,
            );
            return Err(BudgetExhausted {
                message: msg,
                reason: BudgetReason::Runtime,
                spent: elapsed as f64,
                cap: cap_ms as f64,
                model_id: Some(model_id.to_string()),
            });
        }
        Ok(())
    }
}

/// Pull a usage tuple out of an SDK error's JSON envelope, falling back to the
/// pessimistic ceiling (NOT the optimistic pre-call estimate) when none is
/// found. Callers pass `fallback = (estimated_input, max_output)` so the
/// worst-case budget is consumed on failure. Mirrors the TS
/// `extractUsageFromError`: usage may sit at the top level (Anthropic) or under
/// `response.usage` (OpenAI), with either snake_case or camelCase token keys.
#[must_use]
pub fn extract_usage_from_error(
    err: Option<&serde_json::Value>,
    fallback: (u64, u64),
) -> (u64, u64) {
    let read_tokens = |usage: &serde_json::Value| -> (Option<u64>, Option<u64>) {
        let get = |a: &str, b: &str| {
            usage
                .get(a)
                .or_else(|| usage.get(b))
                .and_then(serde_json::Value::as_u64)
        };
        (
            get("input_tokens", "inputTokens"),
            get("output_tokens", "outputTokens"),
        )
    };
    if let Some(err) = err {
        let candidate = err.get("usage").filter(|v| v.is_object()).or_else(|| {
            err.get("response")
                .and_then(|r| r.get("usage"))
                .filter(|v| v.is_object())
        });
        if let Some(usage) = candidate {
            let (input, output) = read_tokens(usage);
            if input.is_some() || output.is_some() {
                return (input.unwrap_or(fallback.0), output.unwrap_or(fallback.1));
            }
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn opts(max_cost: Option<f64>) -> BudgetTrackerOpts {
        BudgetTrackerOpts {
            max_cost_usd: max_cost,
            max_runtime_ms: None,
            label: "test".to_string(),
        }
    }

    fn chat_estimate(model: &str, inp: u64, out: u64) -> BudgetEstimate {
        BudgetEstimate {
            model_id: model.to_string(),
            estimated_input_tokens: inp,
            max_output_tokens: out,
            kind: BudgetKind::Chat,
            label: None,
        }
    }

    fn chat_actual(model: &str, inp: u64, out: u64) -> BudgetActualUsage {
        BudgetActualUsage {
            model_id: model.to_string(),
            input_tokens: inp,
            output_tokens: out,
            embedding_dims: None,
            kind: BudgetKind::Chat,
            label: None,
        }
    }

    // ---- pricing lookup ----

    #[test]
    fn chat_pricing_from_registry_input_output() {
        // openai:gpt-5.2 = input 1.25, output 10.0 per 1M (registry line 60-61).
        let p = lookup_chat_pricing("openai:gpt-5.2").expect("gpt-5.2 priced");
        assert!((p.input - 1.25).abs() < 1e-9);
        assert!((p.output - 10.0).abs() < 1e-9);
    }

    #[test]
    fn chat_pricing_bare_model_resolves() {
        // Bare model name (no provider prefix) still resolves via the scan.
        assert!(lookup_chat_pricing("gpt-5.2").is_some());
    }

    #[test]
    fn chat_pricing_unknown_is_none() {
        assert!(lookup_chat_pricing("openai:gpt-imaginary").is_none());
        assert!(lookup_chat_pricing("nope:model").is_none());
    }

    #[test]
    fn embed_pricing_free_local_is_zero() {
        // ollama embeddings run on local inference → $0, never None (so a
        // --max-cost reindex doesn't TX2 hard-fail).
        let p = lookup_embed_pricing("ollama:nomic-embed-text").expect("free-local embed priced 0");
        assert_eq!(p.input, 0.0);
        assert_eq!(p.output, 0.0);
    }

    #[test]
    fn cost_for_usage_math() {
        // 1M input @1.25 + 1M output @10.0 = 11.25.
        let c = cost_for_usage("openai:gpt-5.2", 1_000_000, 1_000_000, BudgetKind::Chat).unwrap();
        assert!((c - 11.25).abs() < 1e-6);
    }

    // ---- reserve ----

    #[test]
    fn reserve_ok_under_cap() {
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(100.0)), dir.path());
        // tiny call, well under $100.
        assert!(t
            .reserve(&chat_estimate("openai:gpt-5.2", 1000, 1000))
            .is_ok());
    }

    #[test]
    fn reserve_denies_when_projected_exceeds_cap() {
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(0.001)), dir.path());
        // 1M input @1.25 → $1.25 projected, way over $0.001 cap.
        let e = t
            .reserve(&chat_estimate("openai:gpt-5.2", 1_000_000, 0))
            .unwrap_err();
        assert_eq!(e.reason, BudgetReason::Cost);
        assert!(e.message.contains("exceeds --max-cost"));
    }

    #[test]
    fn reserve_tx2_no_pricing_hard_fails_with_cap() {
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(10.0)), dir.path());
        let e = t
            .reserve(&chat_estimate("nope:unpriced", 100, 100))
            .unwrap_err();
        assert_eq!(e.reason, BudgetReason::NoPricing);
        assert!(e.message.contains("no pricing entry"));
    }

    #[test]
    fn reserve_unpriced_allowed_when_no_cap() {
        reset_budget_warnings_for_test();
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(None), dir.path());
        // No cap → unpriced model warns-once but reserve succeeds.
        assert!(t.reserve(&chat_estimate("nope:unpriced", 100, 100)).is_ok());
    }

    #[test]
    fn reserve_runtime_denied() {
        let dir = TempDir::new().unwrap();
        let mut o = opts(None);
        o.max_runtime_ms = Some(0); // any elapsed > 0 trips it
        let t = BudgetTracker::new(o, dir.path());
        std::thread::sleep(std::time::Duration::from_millis(2));
        let e = t
            .reserve(&chat_estimate("openai:gpt-5.2", 10, 10))
            .unwrap_err();
        assert_eq!(e.reason, BudgetReason::Runtime);
    }

    // ---- record ----

    #[test]
    fn record_accumulates_cumulative() {
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(100.0)), dir.path());
        t.record(&chat_actual("openai:gpt-5.2", 1_000_000, 0))
            .unwrap(); // $1.25
        assert!((t.total_spent() - 1.25).abs() < 1e-6);
        t.record(&chat_actual("openai:gpt-5.2", 1_000_000, 0))
            .unwrap(); // +$1.25
        assert!((t.total_spent() - 2.50).abs() < 1e-6);
        assert_eq!(t.snapshot().calls_recorded, 2);
    }

    #[test]
    fn record_tx1_hard_fails_over_cap() {
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(1.0)), dir.path());
        // 1M input @1.25 = $1.25 > $1.00 cap → TX1.
        let e = t
            .record(&chat_actual("openai:gpt-5.2", 1_000_000, 0))
            .unwrap_err();
        assert_eq!(e.reason, BudgetReason::Cost);
        assert!(e.message.contains("exceeded --max-cost"));
        // The spend is still recorded even though it threw.
        assert!((t.total_spent() - 1.25).abs() < 1e-6);
    }

    #[test]
    fn record_unpriced_does_not_accumulate() {
        reset_budget_warnings_for_test();
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(None), dir.path());
        t.record(&chat_actual("nope:unpriced", 1_000_000, 0))
            .unwrap();
        assert_eq!(t.total_spent(), 0.0);
        assert_eq!(t.snapshot().calls_recorded, 1);
    }

    #[test]
    fn delayed_throw_scenario() {
        // Byte-for-byte the delayed-throw contract: record blows the cap and
        // returns Err (caller swallows it so the provider result surfaces),
        // then the NEXT reserve re-surfaces the overflow.
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(1.0)), dir.path());
        let rec = t.record(&chat_actual("openai:gpt-5.2", 1_000_000, 0)); // $1.25 > $1
        assert!(
            rec.is_err(),
            "record over cap returns Err (caller swallows)"
        );
        // Next reserve sees cumulative 1.25 already > cap → denies.
        let e = t
            .reserve(&chat_estimate("openai:gpt-5.2", 1000, 1000))
            .unwrap_err();
        assert_eq!(e.reason, BudgetReason::Cost);
    }

    // ---- audit JSONL ----

    #[test]
    fn audit_filename_iso_week() {
        use chrono::TimeZone;
        let now = Utc.with_ymd_and_hms(2026, 7, 6, 12, 0, 0).unwrap();
        assert_eq!(budget_audit_filename(now), "budget-2026-W28.jsonl");
    }

    #[test]
    fn record_writes_audit_row() {
        let dir = TempDir::new().unwrap();
        let t = BudgetTracker::new(opts(Some(100.0)), dir.path());
        t.record(&chat_actual("openai:gpt-5.2", 1_000_000, 0))
            .unwrap();
        // Find whichever ISO-week file was written.
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("budget-"))
            .collect();
        assert_eq!(files.len(), 1, "one budget audit file created");
        let content = std::fs::read_to_string(files[0].path()).unwrap();
        let row: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(row["event"], "record");
        assert_eq!(row["schema_version"], 1);
        assert_eq!(row["model"], "openai:gpt-5.2");
        assert!((row["actual_cost_usd"].as_f64().unwrap() - 1.25).abs() < 1e-6);
    }

    // ---- extract_usage_from_error ----

    #[test]
    fn extract_usage_top_level_snake_case() {
        let err = json!({ "usage": { "input_tokens": 100, "output_tokens": 50 } });
        assert_eq!(extract_usage_from_error(Some(&err), (1, 2)), (100, 50));
    }

    #[test]
    fn extract_usage_nested_camel_case() {
        let err = json!({ "response": { "usage": { "inputTokens": 7, "outputTokens": 8 } } });
        assert_eq!(extract_usage_from_error(Some(&err), (1, 2)), (7, 8));
    }

    #[test]
    fn extract_usage_falls_back_when_absent() {
        let err = json!({ "message": "boom" });
        assert_eq!(extract_usage_from_error(Some(&err), (11, 22)), (11, 22));
        assert_eq!(extract_usage_from_error(None, (11, 22)), (11, 22));
    }

    #[test]
    fn extract_usage_partial_uses_fallback_for_missing_half() {
        let err = json!({ "usage": { "input_tokens": 100 } });
        assert_eq!(extract_usage_from_error(Some(&err), (1, 999)), (100, 999));
    }

    // ---- BudgetAuditor ----

    #[test]
    fn noop_auditor_discards_all_rows() {
        let auditor = NoopAuditor;
        // All calls succeed and return Ok.
        assert!(auditor.record_audit(&json!({"event": "reserve"})).is_ok());
        assert!(auditor.record_audit(&json!({"event": "record"})).is_ok());
        assert!(auditor.record_audit(&serde_json::Value::Null).is_ok());
    }

    #[test]
    fn noop_auditor_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<NoopAuditor>();
    }
}
