//! `zbrain models` command — port of `src/commands/models.ts`.
//!
//! Two surfaces, split across slices:
//! - slice 1 (this file's read-only half): `build_models_report` + `format_*`
//!   — pure, config-driven routing table. DI'd through `&dyn ConfigLookup`
//!   (mirrors TS `engine.getConfig`), no engine, no IO, fully unit-testable.
//! - slice 2/3: `doctor` probe mode (embedding-config + reranker-config +
//!   chat/expansion/embedding-reachability/reranker-reachability probes,
//!   fail-open on network/AI errors).
//! - slice 4: `Commands::Models` wiring + delete TS `models.ts` + PARITY_GATE.
//!
//! The routing engine itself (`resolve_model` / `tier_default` /
//! `default_alias` / `ModelTier`) was already ported to
//! `zbrain_core::ai::model_config` (Phase 8, 1-2-1); this file only adds the
//! CLI-shaped report + formatting on top of it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use zbrain_core::ai::chat::{instantiate_chat, ChatMessage, ChatOpts, ChatRole};
use crate::config::Config;
use zbrain_core::ai::model_config::{
    default_alias, resolve_model, tier_default, ConfigLookup, ModelTier, ResolveModelOpts,
};
use zbrain_core::rerank_client::{
    DEFAULT_RERANK_MODEL, RerankClient, RerankRequest, ZeroEntropyRerankClient,
};
use zbrain_core::ai::resolver::resolve_recipe_strict;
use zbrain_core::embedding::{EmbeddingClient, EmbeddingConfig};
use crate::ModelsMode;

/// Per-task model keys, in display order. Verbatim from TS `PER_TASK_KEYS`
/// (src/commands/models.ts). This is a config inventory, not an algorithm, so
/// it is copied as-is rather than derived.
const PER_TASK_KEYS: &[(&str, ModelTier, &str)] = &[
    ("models.dream.synthesize", ModelTier::Reasoning, "Dream synthesis (conversation → brain pages)"),
    ("models.dream.synthesize_verdict", ModelTier::Utility, "Dream synthesis verdict (Haiku judge)"),
    ("models.dream.patterns", ModelTier::Reasoning, "Pattern discovery (cross-take themes)"),
    ("models.drift", ModelTier::Reasoning, "Drift LLM judge (v0.29 scaffold)"),
    ("models.auto_think", ModelTier::Deep, "Auto-think question answering"),
    ("models.think", ModelTier::Deep, "`zbrain think` synthesis op"),
    ("models.subagent", ModelTier::Subagent, "`zbrain agent run` subagent loop"),
    ("facts.extraction_model", ModelTier::Reasoning, "Real-time facts extraction during sync"),
    ("models.eval.longmemeval", ModelTier::Reasoning, "LongMemEval benchmark answer-gen"),
    ("models.eval.contradictions_judge", ModelTier::Utility, "Contradiction probe judge (v0.34 temporal-aware)"),
    ("models.expansion", ModelTier::Utility, "Query expansion for hybrid search"),
    ("models.chat", ModelTier::Reasoning, "Default `gateway.chat()` model"),
];

const TIER_ORDER: [ModelTier; 4] = [
    ModelTier::Utility,
    ModelTier::Reasoning,
    ModelTier::Deep,
    ModelTier::Subagent,
];

// ── Report types (mirror TS `ModelsReport`) ────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TierEntry {
    pub tier: String,
    pub resolved: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PerTaskEntry {
    pub key: String,
    pub tier: String,
    pub resolved: String,
    pub source: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Aliases {
    pub defaults: HashMap<String, String>,
    pub user: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalDefault {
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelsReport {
    pub schema_version: u8,
    pub global_default: GlobalDefault,
    /// Keyed by tier name (`utility`/`reasoning`/`deep`/`subagent`) to mirror
    /// the TS object shape exactly.
    pub tiers: BTreeMap<String, TierEntry>,
    pub per_task: Vec<PerTaskEntry>,
    pub aliases: Aliases,
}

// ── Source attribution (mirror TS `buildReport` + `probeSource`) ────────────

/// Mirror TS `buildReport`'s per-tier `source` logic: global `models.default`
/// wins for every tier; else that tier's `models.tier.<t>` override; else the
/// hardcoded `default` sentinel.
fn attribute_tier_source(lookup: &dyn ConfigLookup, tier: ModelTier) -> String {
    if let Some(v) = lookup.get("models.default") {
        if !v.trim().is_empty() {
            return "config: models.default".to_string();
        }
    }
    let tier_key = format!("models.tier.{}", tier.as_str());
    if let Some(v) = lookup.get(&tier_key) {
        if !v.trim().is_empty() {
            return format!("config: {tier_key}");
        }
    }
    "default".to_string()
}

/// Mirror TS `probeSource(engine, key, 'ZBRAIN_MODEL')`: config key wins, then
/// the `env_var` env var, else the `tier.<tier>` fallback label. `env_var` is
/// parameterised (default `"ZBRAIN_MODEL"` in production) so unit tests can
/// pass an unset sentinel and stay isolated from the host environment.
fn attribute_per_task_source(
    lookup: &dyn ConfigLookup,
    key: &str,
    tier: ModelTier,
    env_var: &str,
) -> String {
    if let Some(v) = lookup.get(key) {
        if !v.trim().is_empty() {
            return format!("config: {key}");
        }
    }
    if let Ok(env) = std::env::var(env_var) {
        if !env.trim().is_empty() {
            return format!("env: {env_var}");
        }
    }
    format!("tier.{}", tier.as_str())
}

fn default_aliases_map() -> HashMap<String, String> {
    let mut m = HashMap::new();
    for name in ["opus", "sonnet", "haiku", "gemini", "gpt"] {
        if let Some(v) = default_alias(name) {
            m.insert(name.to_string(), v.to_string());
        }
    }
    m
}

// ── Pure builder ────────────────────────────────────────────────────────────

/// Build the read-only routing-table report. Pure & synchronous: config comes
/// from the injected `lookup`, so this never touches a DB or async runtime.
/// Mirrors TS `buildReport(engine)`. Production entry point — reads the
/// `ZBRAIN_MODEL` env var (same as TS).
pub fn build_models_report(lookup: &dyn ConfigLookup) -> ModelsReport {
    build_models_report_inner(lookup, "ZBRAIN_MODEL")
}

/// Test seam: same as [`build_models_report`] but with an injectable `env_var`
/// so unit tests can pass an unset sentinel and stay isolated from the host's
/// `ZBRAIN_MODEL`.
fn build_models_report_inner(lookup: &dyn ConfigLookup, env_var: &str) -> ModelsReport {
    let global_default = lookup
        .get("models.default")
        .filter(|s| !s.trim().is_empty());

    let mut tiers = BTreeMap::new();
    for t in TIER_ORDER {
        let resolved = resolve_model(
            lookup,
            &ResolveModelOpts {
                tier: Some(t),
                fallback: tier_default(t).to_string(),
                env_var: Some(env_var.to_string()),
                ..Default::default()
            },
        );
        let source = attribute_tier_source(lookup, t);
        tiers.insert(
            t.as_str().to_string(),
            TierEntry {
                tier: t.as_str().to_string(),
                resolved,
                source,
            },
        );
    }

    let mut per_task = Vec::with_capacity(PER_TASK_KEYS.len());
    for (key, tier, description) in PER_TASK_KEYS {
        let resolved = resolve_model(
            lookup,
            &ResolveModelOpts {
                config_key: Some(key.to_string()),
                tier: Some(*tier),
                fallback: tier_default(*tier).to_string(),
                env_var: Some(env_var.to_string()),
                ..Default::default()
            },
        );
        let source = attribute_per_task_source(lookup, key, *tier, env_var);
        per_task.push(PerTaskEntry {
            key: key.to_string(),
            tier: tier.as_str().to_string(),
            resolved,
            source,
            description: description.to_string(),
        });
    }

    let mut user_aliases = HashMap::new();
    for name in ["opus", "sonnet", "haiku", "gemini", "gpt"] {
        if let Some(v) = lookup.get(&format!("models.aliases.{name}")) {
            let v = v.trim();
            if !v.is_empty() {
                user_aliases.insert(name.to_string(), v.to_string());
            }
        }
    }

    ModelsReport {
        schema_version: 1,
        global_default: GlobalDefault {
            value: global_default,
        },
        tiers,
        per_task,
        aliases: Aliases {
            defaults: default_aliases_map(),
            user: user_aliases,
        },
    }
}

// ── Formatters (mirror TS `formatText` / JSON.stringify) ────────────────────

/// Human-readable routing table. Mirrors TS `formatText(report)`.
pub fn format_models_text(report: &ModelsReport) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("Tier routing:".to_string());
    for t in TIER_ORDER {
        let name = t.as_str();
        let e = &report.tiers[name];
        lines.push(format!(
            "  tier.{:<10} {:<45} [{}]",
            name, e.resolved, e.source
        ));
    }
    lines.push(String::new());
    lines.push("Global default:".to_string());
    lines.push(format!(
        "  models.default  {}",
        report.global_default.value.as_deref().unwrap_or("(unset)")
    ));
    lines.push(String::new());
    lines.push("Per-task overrides:".to_string());
    for t in &report.per_task {
        lines.push(format!(
            "  {:<34} → {:<45} [{}]",
            t.key, t.resolved, t.source
        ));
    }
    lines.push(String::new());
    lines.push("Aliases:".to_string());
    for (k, v) in &report.aliases.defaults {
        if let Some(user_override) = report.aliases.user.get(k) {
            lines.push(format!(
                "  {:<8} → {}  (user override; default: {})",
                k, user_override, v
            ));
        } else {
            lines.push(format!("  {:<8} → {}", k, v));
        }
    }
    for (k, v) in &report.aliases.user {
        if !report.aliases.defaults.contains_key(k) {
            lines.push(format!("  {:<8} → {}  (user)", k, v));
        }
    }
    lines.push(String::new());
    lines.push(
        "Tip: probe reachability with `zbrain models doctor` (opt-in; spends a minimal request per configured chat/embed/rerank surface)."
            .to_string(),
    );
    lines.join("\n")
}

/// JSON wire output (pretty). Mirrors TS `JSON.stringify(report, null, 2)`.
pub fn format_models_json(report: &ModelsReport) -> String {
    serde_json::to_string_pretty(report).expect("ModelsReport is serializable")
}

// ── Doctor pure functions (slice 2) ────────────────────────────────────────

/// Probe verdict for a single model surface. snake_case wire (mirrors TS
/// `ProbeStatus` union).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Ok,
    ModelNotFound,
    Auth,
    RateLimit,
    Network,
    Config,
    Unknown,
}

impl std::fmt::Display for ProbeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ProbeStatus::Ok => "ok",
            ProbeStatus::ModelNotFound => "model_not_found",
            ProbeStatus::Auth => "auth",
            ProbeStatus::RateLimit => "rate_limit",
            ProbeStatus::Network => "network",
            ProbeStatus::Config => "config",
            ProbeStatus::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

/// One probe row. Mirrors TS `ProbeResult`.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub model: String,
    pub touchpoint: String,
    pub status: ProbeStatus,
    pub message: String,
    pub elapsed_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

// ── Embedding dimension allowlists (verbatim from TS `src/core/ai/dims.ts`) ──

/// Voyage hosted models that accept `output_dimension` (256/512/1024/2048).
pub const VOYAGE_OUTPUT_DIMENSION_MODELS: &[&str] = &[
    "voyage-4-large",
    "voyage-4",
    "voyage-4-lite",
    "voyage-3-large",
    "voyage-3.5",
    "voyage-3.5-lite",
    "voyage-code-3",
];

pub const VOYAGE_VALID_OUTPUT_DIMS: &[u32] = &[256, 512, 1024, 2048];

pub const ZEROENTROPY_DIM_MODELS: &[&str] = &["zembed-1"];

pub const ZEROENTROPY_VALID_DIMS: &[u32] = &[2560, 1280, 640, 320, 160, 80, 40];

#[must_use]
pub fn supports_voyage_output_dimension(model_id: &str) -> bool {
    VOYAGE_OUTPUT_DIMENSION_MODELS.contains(&model_id)
}

#[must_use]
pub fn is_valid_voyage_output_dim(dims: u32) -> bool {
    VOYAGE_VALID_OUTPUT_DIMS.contains(&dims)
}

#[must_use]
pub fn supports_zeroentropy_dimension(model_id: &str) -> bool {
    ZEROENTROPY_DIM_MODELS.contains(&model_id)
}

#[must_use]
pub fn is_valid_zeroentropy_dim(dims: u32) -> bool {
    ZEROENTROPY_VALID_DIMS.contains(&dims)
}

/// Split `provider:model` into `(provider_id, model_id)` (lowercased provider).
/// Mirrors TS `parseModelId` (non-strict: `None` on malformed input so callers
/// can emit a `config` probe result instead of throwing).
fn parse_provider_model(model_str: &str) -> Option<(String, String)> {
    let colon = model_str.find(':')?;
    if colon == 0 {
        return None;
    }
    let provider_id = model_str[..colon].trim().to_lowercase();
    let model_id = model_str[colon + 1..].trim().to_string();
    if model_id.is_empty() {
        return None;
    }
    Some((provider_id, model_id))
}

/// Pure embedding-config validator (mirrors TS `probeEmbeddingConfig` core).
/// Returns `None` when the configured dims are valid for the model, or
/// `Some(ProbeResult)` describing the config error with a paste-ready fix.
/// `model_str` is `provider:model`; `dims` is the configured embedding
/// dimension. Mirrors TS behaviour (Voyage/ZeroEntropy flexible-dim checks).
#[must_use]
pub fn validate_embedding_dims(model_str: &str, dims: u32) -> Option<ProbeResult> {
    let (provider_id, model_id) = match parse_provider_model(model_str) {
        Some(p) => p,
        // No `provider:model` prefix → a local/default embedding model (e.g.
        // `all-minilm-l6-v2`). TS `probeEmbeddingConfig` falls through to `ok`
        // for any non-Voyage/non-ZeroEntropy model, so we don't flag it as a
        // config error here (dims validation only applies to those two
        // providers). Mirrors TS behaviour.
        None => return None,
    };

    if provider_id == "voyage" && supports_voyage_output_dimension(&model_id) {
        if !is_valid_voyage_output_dim(dims) {
            return Some(ProbeResult {
                model: model_str.to_string(),
                touchpoint: "embedding_config".to_string(),
                status: ProbeStatus::Config,
                message: format!(
                    "embedding_dimensions={} is not a valid Voyage output_dimension for \"{}\" (allowed: {})",
                    dims,
                    model_id,
                    VOYAGE_VALID_OUTPUT_DIMS
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                ),
                elapsed_ms: 0,
                fix: Some(format!(
                    "zbrain config set embedding_dimensions <{}>, or switch to a fixed-dim Voyage model (e.g. voyage-3, voyage-3-lite)",
                    VOYAGE_VALID_OUTPUT_DIMS
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("|")
                )),
            });
        }
    }

    if provider_id == "zeroentropyai" && supports_zeroentropy_dimension(&model_id) {
        if !is_valid_zeroentropy_dim(dims) {
            return Some(ProbeResult {
                model: model_str.to_string(),
                touchpoint: "embedding_config".to_string(),
                status: ProbeStatus::Config,
                message: format!(
                    "embedding_dimensions={} is not a valid ZeroEntropy dimensions for \"{}\" (allowed: {})",
                    dims,
                    model_id,
                    ZEROENTROPY_VALID_DIMS
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                ),
                elapsed_ms: 0,
                fix: Some(format!(
                    "zbrain config set embedding_dimensions <{}>.",
                    ZEROENTROPY_VALID_DIMS
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("|")
                )),
            });
        }
    }

    None
}

/// Classify an error message into a probe status + human message. Mirrors TS
/// `classifyError`. Pure: input is the (already-lowered-or-not) error string.
#[must_use]
pub fn classify_probe_error(msg: &str) -> (ProbeStatus, String) {
    let lower = msg.to_lowercase();
    let status = if lower.contains("not_found")
        || lower.contains("does not exist")
        || lower.contains("invalid_model")
        || (lower.contains("model") && lower.contains("invalid"))
        || lower.contains("404")
    {
        ProbeStatus::ModelNotFound
    } else if lower.contains("auth")
        || lower.contains("unauthor")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("api_key")
    {
        ProbeStatus::Auth
    } else if (lower.contains("rate") && lower.contains("limit"))
        || lower.contains("429")
        || lower.contains("too many")
    {
        ProbeStatus::RateLimit
    } else if lower.contains("timeout")
        || lower.contains("network")
        || lower.contains("econn")
        || lower.contains("fetch failed")
        || lower.contains("enotfound")
    {
        ProbeStatus::Network
    } else {
        ProbeStatus::Unknown
    };
    (status, msg.to_string())
}

/// Whether a model string's provider is in the `--skip` list. Mirrors TS
/// `shouldSkipProvider`.
#[must_use]
pub fn should_skip_provider(model_str: &str, skip: &[String]) -> bool {
    if skip.is_empty() {
        return false;
    }
    let provider = match model_str.find(':') {
        Some(i) => model_str[..i].to_lowercase(),
        None => String::new(),
    };
    skip.iter().any(|s| s.to_lowercase() == provider)
}

// ── Doctor probe orchestration (slice 3) ──────────────────────────────────
//
// `zbrain models doctor` fires a minimal reachability probe against each
// configured chat / expansion / embedding / reranker surface. Mirrors the TS
// `runModels` doctor branch: zero-token config probes first (embedding dims +
// reranker allowlist), then 1-token AI probes (chat/expansion/embedding
// reachability), then reranker reachability. Everything is fail-open: a probe
// error is classified into a `ProbeStatus` row rather than aborting.

/// Probe timeout, mirroring the TS `AbortController` 5s deadline.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default embedding dimensions used by the config probe when the operator
/// left `embedding_dimensions` unset. Mirrors TS `DEFAULT_EMBEDDING_DIMENSIONS`
/// (src/core/ai/defaults.ts = 1280).
const DEFAULT_EMBEDDING_DIMENSIONS: u32 = 1280;

/// Network-touching probes, abstracted so the orchestration stays fail-open
/// and unit-testable. Mirrors the DI style of `features` (1-6-4-3) and the
/// doctor runner (1-5): the live impl builds real clients + spends a minimal
/// request; the test impl returns canned errors.
#[async_trait::async_trait]
pub trait AiProbeExecutor: Send + Sync {
    /// `Ok(())` = reachable; `Err(raw_msg)` = the raw error string, classified
    /// by the orchestration via [`classify_probe_error`].
    async fn probe_chat(&self, model: &str) -> Result<(), String>;
    async fn probe_embedding(&self, model: &str) -> Result<(), String>;
    async fn probe_reranker(&self, model: &str, timeout_ms: u64) -> Result<(), String>;
}

/// Resolved model inputs the doctor needs that come from the on-disk `Config`
/// (not the engine `ConfigLookup`). The CLI builds this from
/// `config::load_config()`; tests construct it directly.
#[derive(Debug, Clone)]
pub struct DoctorModelInputs {
    pub embedding_model: String,
    pub embedding_dimensions: Option<u32>,
    pub reranker_enabled: bool,
    /// Resolved reranker model. Rust has no `resolveSearchMode` (TS
    /// `search/mode.ts`), so this is always [`DEFAULT_RERANK_MODEL`] for now
    /// — the `search.reranker.model` config key is not yet implemented.
    pub reranker_model: String,
    pub skip: Vec<String>,
}

impl Default for DoctorModelInputs {
    fn default() -> Self {
        Self {
            embedding_model: String::new(),
            embedding_dimensions: None,
            reranker_enabled: false,
            reranker_model: DEFAULT_RERANK_MODEL.to_string(),
            skip: Vec::new(),
        }
    }
}

/// Zero-network reranker config probe (mirrors TS `probeRerankerConfig`).
/// Validates that the configured reranker model resolves through the recipe
/// registry, declares a reranker touchpoint, and is in the touchpoint's
/// `models[]` allowlist. Returns `ok` when reranker is disabled (the default
/// opt-in-off state) so the row is informational, not an error.
#[must_use]
pub fn probe_reranker_config(inputs: &DoctorModelInputs) -> ProbeResult {
    let start = Instant::now();
    if !inputs.reranker_enabled {
        return ProbeResult {
            model: "(none)".to_string(),
            touchpoint: "reranker_config".to_string(),
            status: ProbeStatus::Ok,
            message:
                "reranker not configured (set `zbrain config set search.reranker.enabled true` and `search.reranker.model <provider:model>`)"
                    .to_string(),
            elapsed_ms: start.elapsed().as_millis(),
            fix: None,
        };
    }
    let model_str = &inputs.reranker_model;
    match resolve_recipe_strict(model_str) {
        Ok((parsed, recipe)) => {
            let Some(tp) = recipe.touchpoints.reranker else {
                return ProbeResult {
                    model: model_str.clone(),
                    touchpoint: "reranker_config".to_string(),
                    status: ProbeStatus::Config,
                    message: format!(
                        "Provider \"{}\" does not declare a reranker touchpoint.",
                        recipe.id
                    ),
                    elapsed_ms: start.elapsed().as_millis(),
                    fix: Some(
                        "Switch to a provider that does (e.g. zeroentropyai:zerank-2)."
                            .to_string(),
                    ),
                };
            };
            if !tp.models.is_empty() && !tp.models.contains(&parsed.model_id.as_str()) {
                return ProbeResult {
                    model: model_str.clone(),
                    touchpoint: "reranker_config".to_string(),
                    status: ProbeStatus::Config,
                    message: format!(
                        "Model \"{}\" is not in {}'s reranker allowlist.",
                        parsed.model_id, recipe.name
                    ),
                    fix: Some(format!(
                        "zbrain config set search.reranker.model {}:<one of {}>",
                        recipe.id,
                        tp.models.join("|")
                    )),
                    elapsed_ms: start.elapsed().as_millis(),
                };
            }
            ProbeResult {
                model: model_str.clone(),
                touchpoint: "reranker_config".to_string(),
                status: ProbeStatus::Ok,
                message: format!("reranker configured: {model_str}"),
                elapsed_ms: start.elapsed().as_millis(),
                fix: None,
            }
        }
        Err(e) => ProbeResult {
            model: model_str.clone(),
            touchpoint: "reranker_config".to_string(),
            status: ProbeStatus::Config,
            message: e.message,
            elapsed_ms: start.elapsed().as_millis(),
            fix: None,
        },
    }
}

/// Which AI touchpoint a probe targets (internal to the orchestration).
enum AiProbeKind {
    Chat,
    Embedding,
    Reranker,
}

/// Run a single AI probe via `executor`, mapping success → `ok` / reachable
/// and failure → classified `ProbeStatus`. Mirrors the TS per-probe
/// try/catch + `classifyError` shape.
async fn run_ai_probe(
    kind: AiProbeKind,
    model: &str,
    touchpoint: &str,
    executor: &dyn AiProbeExecutor,
) -> ProbeResult {
    let start = Instant::now();
    let res = match kind {
        AiProbeKind::Chat => executor.probe_chat(model).await,
        AiProbeKind::Embedding => executor.probe_embedding(model).await,
        AiProbeKind::Reranker => {
            executor
                .probe_reranker(model, PROBE_TIMEOUT.as_millis() as u64)
                .await
        }
    };
    match res {
        Ok(()) => ProbeResult {
            model: model.to_string(),
            touchpoint: touchpoint.to_string(),
            status: ProbeStatus::Ok,
            message: "reachable".to_string(),
            elapsed_ms: start.elapsed().as_millis(),
            fix: None,
        },
        Err(msg) => {
            let (status, message) = classify_probe_error(&msg);
            ProbeResult {
                model: model.to_string(),
                touchpoint: touchpoint.to_string(),
                status,
                message,
                elapsed_ms: start.elapsed().as_millis(),
                fix: None,
            }
        }
    }
}

/// Doctor report (mirrors the TS `runModels` doctor-branch output shape).
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub schema_version: u8,
    pub probes: Vec<ProbeResult>,
    pub summary: DoctorSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorSummary {
    pub total: usize,
    pub ok: usize,
    pub failed: usize,
}

/// Run the `zbrain models doctor` probe set. Config probes run synchronously;
/// AI probes delegate to `executor` (fail-open). Mirrors the TS ordering:
/// embedding_config + reranker_config first (zero tokens), then chat/expansion
/// reachability, then embedding reachability (only if embedding config passed),
/// then reranker reachability (only if enabled).
pub async fn run_models_doctor(
    lookup: &dyn ConfigLookup,
    inputs: &DoctorModelInputs,
    executor: &dyn AiProbeExecutor,
) -> DoctorReport {
    let mut probes: Vec<ProbeResult> = Vec::new();

    let dims = inputs.embedding_dimensions.unwrap_or(DEFAULT_EMBEDDING_DIMENSIONS);
    let embedding_config = match validate_embedding_dims(&inputs.embedding_model, dims) {
        Some(r) => r,
        None => ProbeResult {
            model: inputs.embedding_model.clone(),
            touchpoint: "embedding_config".to_string(),
            status: ProbeStatus::Ok,
            message: format!("embedding_dimensions={dims} ok for {}", inputs.embedding_model),
            elapsed_ms: 0,
            fix: None,
        },
    };
    probes.push(embedding_config.clone());
    probes.push(probe_reranker_config(inputs));

    let chat_model = resolve_model(
        lookup,
        &ResolveModelOpts {
            tier: Some(ModelTier::Reasoning),
            fallback: tier_default(ModelTier::Reasoning).to_string(),
            ..Default::default()
        },
    );
    let expansion_model = resolve_model(
        lookup,
        &ResolveModelOpts {
            config_key: Some("models.expansion".to_string()),
            tier: Some(ModelTier::Utility),
            fallback: tier_default(ModelTier::Utility).to_string(),
            ..Default::default()
        },
    );

    for (model_str, touchpoint) in [(chat_model, "chat"), (expansion_model, "expansion")] {
        if should_skip_provider(&model_str, &inputs.skip) {
            continue;
        }
        probes.push(run_ai_probe(AiProbeKind::Chat, &model_str, touchpoint, executor).await);
    }

    if embedding_config.status == ProbeStatus::Ok
        && !should_skip_provider(&inputs.embedding_model, &inputs.skip)
    {
        probes.push(
            run_ai_probe(
                AiProbeKind::Embedding,
                &inputs.embedding_model,
                "embedding_reachability",
                executor,
            )
            .await,
        );
    }

    if inputs.reranker_enabled && !should_skip_provider(&inputs.reranker_model, &inputs.skip) {
        probes.push(
            run_ai_probe(
                AiProbeKind::Reranker,
                &inputs.reranker_model,
                "reranker_config",
                executor,
            )
            .await,
        );
    }

    let total = probes.len();
    let ok = probes.iter().filter(|p| p.status == ProbeStatus::Ok).count();
    DoctorReport {
        schema_version: 1,
        probes,
        summary: DoctorSummary {
            total,
            ok,
            failed: total - ok,
        },
    }
}

/// Live probe impl: builds real chat/embedding/rerank clients and spends a
/// minimal request per surface. Fails open — any client-build or network error
/// is returned as `Err(msg)` and classified by the orchestration.
pub struct LiveProbeExecutor {
    pub embedding_dimensions: Option<u32>,
}

#[async_trait::async_trait]
impl AiProbeExecutor for LiveProbeExecutor {
    async fn probe_chat(&self, model: &str) -> Result<(), String> {
        let (_parsed, recipe) = resolve_recipe_strict(model).map_err(|e| e.message)?;
        let provider = instantiate_chat(recipe, &_parsed.model_id, |k| std::env::var(k).ok())
            .map_err(|e| e.to_string())?;
        let opts = ChatOpts {
            model: Some(model.to_string()),
            messages: vec![ChatMessage::text(ChatRole::User, "ping")],
            max_tokens: Some(1),
            ..Default::default()
        };
        match tokio::time::timeout(PROBE_TIMEOUT, provider.chat(opts)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("probe timed out after {}ms", PROBE_TIMEOUT.as_millis())),
        }
    }

    async fn probe_embedding(&self, model: &str) -> Result<(), String> {
        let (_parsed, recipe) = resolve_recipe_strict(model).map_err(|e| e.message)?;
        let key_var = recipe
            .auth_env
            .and_then(|a| a.required.first())
            .ok_or_else(|| "provider declares no auth env".to_string())?;
        let api_key = std::env::var(key_var).map_err(|_| format!("{key_var} not set"))?;
        let mut builder = EmbeddingConfig::builder().model(model).api_key(api_key);
        if let Some(d) = self.embedding_dimensions {
            builder = builder.dimensions(d as usize);
        }
        let config = builder.build().map_err(|e| e.to_string())?;
        let client = EmbeddingClient::new(config).map_err(|e| e.to_string())?;
        match tokio::time::timeout(PROBE_TIMEOUT, client.embed("probe")).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("probe timed out after {}ms", PROBE_TIMEOUT.as_millis())),
        }
    }

    async fn probe_reranker(&self, model: &str, timeout_ms: u64) -> Result<(), String> {
        let client = ZeroEntropyRerankClient::from_env(None)
            .ok_or_else(|| "ZEROENTROPY_API_KEY not set".to_string())?;
        let req = RerankRequest {
            query: "probe".to_string(),
            documents: vec!["probe document".to_string()],
            model: Some(model.to_string()),
            timeout_ms: Some(timeout_ms),
        };
        match tokio::time::timeout(PROBE_TIMEOUT, client.rerank(&req)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("probe timed out after {}ms", PROBE_TIMEOUT.as_millis())),
        }
    }
}

/// JSON wire output for the doctor report (pretty).
#[must_use]
pub fn format_doctor_json(report: &DoctorReport) -> String {
    serde_json::to_string_pretty(report).expect("DoctorReport is serializable")
}

/// Human-readable doctor report. Mirrors TS `runModels` doctor branch text.
#[must_use]
pub fn format_doctor_text(report: &DoctorReport) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("Model reachability probe:".to_string());
    for r in &report.probes {
        let icon = if r.status == ProbeStatus::Ok { "✔" } else { "✘" };
        lines.push(format!(
            "  {} {:<17} {:<50} {} ({}ms)",
            icon, r.touchpoint, r.model, r.status, r.elapsed_ms
        ));
        if r.status != ProbeStatus::Ok {
            lines.push(format!("      {}", r.message));
            if let Some(fix) = &r.fix {
                lines.push(format!("      fix: {fix}"));
            }
        }
    }
    lines.push(String::new());
    lines.push(format!(
        "Summary: {}/{} reachable.",
        report.summary.ok, report.summary.total
    ));
    lines.join("\n")
}

// ── CLI wiring (slice 4) ─────────────────────────────────────────────────────

/// Flatten a `serde_yaml::Value` into dotted keys (no leading dot). Sequences
/// are joined with commas; scalars become their string form.
fn flatten_yaml(prefix: &str, v: &serde_yaml::Value, out: &mut HashMap<String, String>) {
    match v {
        serde_yaml::Value::Mapping(m) => {
            for (k, val) in m {
                let key = k.as_str().unwrap_or_default().to_string();
                let next = if prefix.is_empty() {
                    key
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_yaml(&next, val, out);
            }
        }
        serde_yaml::Value::Sequence(seq) => {
            let joined = seq
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect::<Vec<_>>()
                .join(",");
            if !joined.is_empty() {
                out.insert(prefix.to_string(), joined);
            }
        }
        serde_yaml::Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        serde_yaml::Value::Bool(b) => {
            out.insert(prefix.to_string(), b.to_string());
        }
        serde_yaml::Value::Number(n) => {
            out.insert(prefix.to_string(), n.to_string());
        }
        _ => {}
    }
}

/// Build a `ConfigLookup` from the on-disk `Config`. Rust's typed `Config`
/// carries no `models.*` namespace, but its `#[serde(flatten)] extra` field
/// captures every unknown key (including `models.default`, `models.tier.*`,
/// `models.aliases.*`, and per-task keys) as raw `serde_yaml::Value`. Flatten
/// that subtree so `build_models_report` / `run_models_doctor` read the same
/// overrides TS read from `engine.getConfig()`.
pub fn config_to_lookup(config: &Config) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (k, v) in &config.extra {
        flatten_yaml(k, v, &mut map);
    }
    map
}

/// CLI entry for `zbrain models [read|doctor] [--json] [--skip=<provider>]`.
/// Mirrors TS `runModels`. Read mode prints the routing table; doctor mode
/// fires reachability probes against chat / expansion / embedding / reranker.
pub async fn run_models_command(
    mode: ModelsMode,
    json: bool,
    skip: Vec<String>,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = crate::config::load_config(config_path)?;
    let lookup = config_to_lookup(&config);

    match mode {
        ModelsMode::Read => {
            let report = build_models_report(&lookup);
            if json {
                println!("{}", format_models_json(&report));
            } else {
                println!("{}", format_models_text(&report));
            }
        }
        ModelsMode::Doctor => {
            let mut skip_lc = skip;
            for s in skip_lc.iter_mut() {
                *s = s.to_lowercase();
            }
            let inputs = DoctorModelInputs {
                embedding_model: config.embedding.model.clone(),
                embedding_dimensions: config.embedding.dimensions.map(|d| d as u32),
                reranker_enabled: config.search.reranker_enabled,
                reranker_model: DEFAULT_RERANK_MODEL.to_string(),
                skip: skip_lc,
            };
            let executor = LiveProbeExecutor {
                embedding_dimensions: inputs.embedding_dimensions,
            };
            let report = run_models_doctor(&lookup, &inputs, &executor).await;
            if json {
                println!("{}", format_doctor_json(&report));
            } else {
                println!("{}", format_doctor_text(&report));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn report_all_defaults() {
        let c = cfg(&[]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.global_default.value, None);
        // Every tier resolves to its hardcoded default; no config → "default".
        assert_eq!(r.tiers["utility"].resolved, "anthropic:claude-haiku-4-5-20251001");
        assert_eq!(r.tiers["reasoning"].resolved, "anthropic:claude-sonnet-4-6");
        assert_eq!(r.tiers["deep"].resolved, "anthropic:claude-opus-4-7");
        assert_eq!(r.tiers["subagent"].resolved, "anthropic:claude-sonnet-4-6");
        for t in TIER_ORDER {
            assert_eq!(r.tiers[t.as_str()].source, "default");
        }
        // per_task with no config → source is the tier fallback.
        assert_eq!(r.per_task[0].source, "tier.reasoning");
        assert_eq!(r.per_task.len(), PER_TASK_KEYS.len());
        // aliases.defaults populated, user empty.
        assert_eq!(r.aliases.defaults.get("opus").unwrap(), "anthropic:claude-opus-4-7");
        assert!(r.aliases.user.is_empty());
    }

    #[test]
    fn report_global_default_attribution() {
        let c = cfg(&[("models.default", "openai:gpt-5.2")]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        assert_eq!(r.global_default.value.as_deref(), Some("openai:gpt-5.2"));
        // All tiers attribute to models.default (it beats tier overrides).
        for t in TIER_ORDER {
            assert_eq!(r.tiers[t.as_str()].source, "config: models.default");
        }
        // Resolved value routes through the alias/override chain.
        assert_eq!(r.tiers["utility"].resolved, "openai:gpt-5.2");
    }

    #[test]
    fn report_tier_override_below_default() {
        // No global default; reasoning tier override applies only to reasoning.
        let c = cfg(&[("models.tier.reasoning", "anthropic:claude-opus-4-7")]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        assert_eq!(r.tiers["reasoning"].source, "config: models.tier.reasoning");
        assert_eq!(r.tiers["reasoning"].resolved, "anthropic:claude-opus-4-7");
        // Other tiers with no override → default sentinel.
        assert_eq!(r.tiers["utility"].source, "default");
        assert_eq!(r.tiers["utility"].resolved, "anthropic:claude-haiku-4-5-20251001");
    }

    #[test]
    fn report_per_task_config_source() {
        let c = cfg(&[("models.dream.synthesize", "openai:gpt-4o-mini")]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        let pt = r.per_task.iter().find(|p| p.key == "models.dream.synthesize").unwrap();
        assert_eq!(pt.source, "config: models.dream.synthesize");
        assert_eq!(pt.resolved, "openai:gpt-4o-mini");
    }

    #[test]
    fn report_per_task_env_source() {
        std::env::set_var("ZBRAIN_MODEL_TEST_ENV_PROBE", "openai:gpt-5.2");
        let c = cfg(&[]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_ENV_PROBE");
        // No config key set for any per-task → env attribution.
        let pt = r.per_task.iter().find(|p| p.key == "models.chat").unwrap();
        assert_eq!(pt.source, "env: ZBRAIN_MODEL_TEST_ENV_PROBE");
        std::env::remove_var("ZBRAIN_MODEL_TEST_ENV_PROBE");
    }

    #[test]
    fn report_user_alias_collected() {
        let c = cfg(&[("models.aliases.opus", "anthropic:claude-opus-custom")]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        assert_eq!(
            r.aliases.user.get("opus").unwrap(),
            "anthropic:claude-opus-custom"
        );
        // defaults still present alongside the override.
        assert_eq!(r.aliases.defaults.get("opus").unwrap(), "anthropic:claude-opus-4-7");
    }

    #[test]
    fn format_text_contains_sections() {
        let c = cfg(&[]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        let text = format_models_text(&r);
        assert!(text.contains("Tier routing:"));
        assert!(text.contains("Global default:"));
        assert!(text.contains("Per-task overrides:"));
        assert!(text.contains("Aliases:"));
        assert!(text.contains("models.default  (unset)"));
        assert!(text.contains("Tip: probe reachability with `zbrain models doctor`"));
    }

    #[test]
    fn format_json_roundtrips_structure() {
        let c = cfg(&[("models.default", "openai:gpt-5.2")]);
        let r = build_models_report_inner(&c, "ZBRAIN_MODEL_TEST_NEVER_SET_XYZ");
        let json = format_models_json(&r);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["global_default"]["value"], "openai:gpt-5.2");
        assert!(v["tiers"]["utility"]["resolved"].is_string());
        assert!(v["per_task"].is_array());
        assert_eq!(v["per_task"].as_array().unwrap().len(), PER_TASK_KEYS.len());
        assert!(v["aliases"]["defaults"]["opus"].is_string());
    }

    // ── Doctor pure-function tests (slice 2) ──

    #[test]
    fn validate_embedding_dims_voyage_bad() {
        let r = validate_embedding_dims("voyage:voyage-4-large", 1536).unwrap();
        assert_eq!(r.status, ProbeStatus::Config);
        assert!(r.message.contains("not a valid Voyage output_dimension"));
        assert!(r.fix.as_ref().unwrap().contains("embedding_dimensions"));
    }

    #[test]
    fn validate_embedding_dims_voyage_ok() {
        assert!(validate_embedding_dims("voyage:voyage-4-large", 1024).is_none());
    }

    #[test]
    fn validate_embedding_dims_zeroentropy_bad() {
        let r = validate_embedding_dims("zeroentropyai:zembed-1", 1536).unwrap();
        assert_eq!(r.status, ProbeStatus::Config);
        assert!(r.message.contains("not a valid ZeroEntropy dimensions"));
    }

    #[test]
    fn validate_embedding_dims_zeroentropy_ok() {
        assert!(validate_embedding_dims("zeroentropyai:zembed-1", 2560).is_none());
    }

    #[test]
    fn validate_embedding_dims_non_flexible_passes() {
        // OpenAI text-embedding-3-small is not in either flexible-dim set, so
        // no dim validation fires regardless of value.
        assert!(validate_embedding_dims("openai:text-embedding-3-small", 1536).is_none());
    }

    #[test]
    fn validate_embedding_dims_local_model_not_config_error() {
        // A model id without a `provider:` prefix (e.g. a local default like
        // `all-minilm-l6-v2`, or `not-a-model`) is treated as ok — TS
        // `probeEmbeddingConfig` only validates dims for Voyage/ZeroEntropy.
        assert!(validate_embedding_dims("not-a-model", 1024).is_none());
        assert!(validate_embedding_dims("all-minilm-l6-v2", 1024).is_none());
    }

    #[test]
    fn flatten_yaml_produces_dotted_keys() {
        // Mirrors how `config_to_lookup` turns `config.extra["models"]`
        // (captured by `#[serde(flatten)] extra`) into the dotted keys the
        // read report / doctor resolution read via `ConfigLookup`.
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "default: foo\ntier:\n  reasoning: bar\naliases:\n  opus: baz\n",
        )
        .unwrap();
        let mut map = HashMap::new();
        flatten_yaml("models", &yaml, &mut map);
        assert_eq!(map.get("models.default").unwrap(), "foo");
        assert_eq!(map.get("models.tier.reasoning").unwrap(), "bar");
        assert_eq!(map.get("models.aliases.opus").unwrap(), "baz");
    }

    #[test]
    fn classify_probe_error_buckets() {
        assert_eq!(classify_probe_error("model not_found").0, ProbeStatus::ModelNotFound);
        assert_eq!(classify_probe_error("401 Unauthorized").0, ProbeStatus::Auth);
        assert_eq!(classify_probe_error("429 rate limit").0, ProbeStatus::RateLimit);
        assert_eq!(classify_probe_error("fetch failed ENOTFOUND").0, ProbeStatus::Network);
        assert_eq!(classify_probe_error("weird boom").0, ProbeStatus::Unknown);
    }

    #[test]
    fn should_skip_provider_matches() {
        let skip: Vec<String> = vec!["openai".to_string()];
        assert!(should_skip_provider("openai:gpt-5", &skip));
        assert!(!should_skip_provider("anthropic:claude", &skip));
        assert!(!should_skip_provider("openai:gpt-5", &[]));
    }

    #[test]
    fn probe_result_json_shape() {
        let r = ProbeResult {
            model: "anthropic:claude-opus-4-7".to_string(),
            touchpoint: "chat".to_string(),
            status: ProbeStatus::Ok,
            message: "reachable".to_string(),
            elapsed_ms: 42,
            fix: None,
        };
        let json = serde_json::to_string(&r).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["status"], "ok");
        assert!(v.get("fix").is_none());
    }

    // ── Doctor probe orchestration tests (slice 3) ──

    struct FakeProbeExecutor {
        chat: Result<(), String>,
        embedding: Result<(), String>,
        reranker: Result<(), String>,
    }

    #[async_trait::async_trait]
    impl AiProbeExecutor for FakeProbeExecutor {
        async fn probe_chat(&self, _m: &str) -> Result<(), String> {
            self.chat.clone()
        }
        async fn probe_embedding(&self, _m: &str) -> Result<(), String> {
            self.embedding.clone()
        }
        async fn probe_reranker(&self, _m: &str, _t: u64) -> Result<(), String> {
            self.reranker.clone()
        }
    }

    fn doctor_inputs() -> DoctorModelInputs {
        DoctorModelInputs {
            embedding_model: "openai:text-embedding-3-small".to_string(),
            embedding_dimensions: None,
            reranker_enabled: false,
            reranker_model: DEFAULT_RERANK_MODEL.to_string(),
            skip: Vec::new(),
        }
    }

    #[tokio::test]
    async fn doctor_runs_chat_and_expansion_probes() {
        let c = cfg(&[]);
        let inputs = doctor_inputs();
        let exec = FakeProbeExecutor {
            chat: Ok(()),
            embedding: Ok(()),
            reranker: Ok(()),
        };
        let report = run_models_doctor(&c, &inputs, &exec).await;
        // chat + expansion + embedding_config + embedding_reachability + reranker_config = 5.
        assert_eq!(report.probes.len(), 5);
        assert!(report
            .probes
            .iter()
            .any(|p| p.touchpoint == "chat" && p.status == ProbeStatus::Ok));
        assert!(report
            .probes
            .iter()
            .any(|p| p.touchpoint == "expansion" && p.status == ProbeStatus::Ok));
        assert_eq!(report.summary.failed, 0);
    }

    #[tokio::test]
    async fn doctor_skip_provider_suppresses_chat() {
        let c = cfg(&[]);
        let mut inputs = doctor_inputs();
        inputs.skip = vec!["anthropic".to_string()];
        let exec = FakeProbeExecutor {
            chat: Ok(()),
            embedding: Ok(()),
            reranker: Ok(()),
        };
        let report = run_models_doctor(&c, &inputs, &exec).await;
        // With anthropic skipped: chat + expansion suppressed (both anthropic).
        assert!(!report.probes.iter().any(|p| p.touchpoint == "chat"));
        assert!(!report.probes.iter().any(|p| p.touchpoint == "expansion"));
    }

    #[tokio::test]
    async fn doctor_embedding_reachability_gated_on_config_ok() {
        let c = cfg(&[]);
        let ok_inputs = doctor_inputs();
        let exec = FakeProbeExecutor {
            chat: Ok(()),
            embedding: Ok(()),
            reranker: Ok(()),
        };
        let report_ok = run_models_doctor(&c, &ok_inputs, &exec).await;
        assert!(report_ok
            .probes
            .iter()
            .any(|p| p.touchpoint == "embedding_reachability"));

        // voyage with bad dims -> config probe fails -> reachability skipped.
        let mut bad_inputs = doctor_inputs();
        bad_inputs.embedding_model = "voyage:voyage-4-large".to_string();
        bad_inputs.embedding_dimensions = Some(1536);
        let report_bad = run_models_doctor(&c, &bad_inputs, &exec).await;
        assert!(!report_bad
            .probes
            .iter()
            .any(|p| p.touchpoint == "embedding_reachability"));
    }

    #[tokio::test]
    async fn doctor_reranker_reachability_gated_on_enabled() {
        let c = cfg(&[]);
        let mut inputs = doctor_inputs();
        inputs.reranker_enabled = true;
        let exec = FakeProbeExecutor {
            chat: Ok(()),
            embedding: Ok(()),
            reranker: Ok(()),
        };
        let report = run_models_doctor(&c, &inputs, &exec).await;
        // reachability probe reports "reachable".
        assert!(report
            .probes
            .iter()
            .any(|p| p.touchpoint == "reranker_config" && p.message == "reachable"));
    }

    #[tokio::test]
    async fn doctor_classifies_network_error() {
        let c = cfg(&[]);
        let inputs = doctor_inputs();
        let exec = FakeProbeExecutor {
            chat: Err("model not_found: 404".to_string()),
            embedding: Ok(()),
            reranker: Ok(()),
        };
        let report = run_models_doctor(&c, &inputs, &exec).await;
        let chat = report.probes.iter().find(|p| p.touchpoint == "chat").unwrap();
        assert_eq!(chat.status, ProbeStatus::ModelNotFound);
        // chat AND expansion both route through probe_chat and both fail.
        assert_eq!(report.summary.failed, 2);
    }

    #[test]
    fn probe_reranker_config_disabled_is_ok() {
        let inputs = DoctorModelInputs {
            reranker_enabled: false,
            ..Default::default()
        };
        let r = probe_reranker_config(&inputs);
        assert_eq!(r.status, ProbeStatus::Ok);
        assert_eq!(r.model, "(none)");
    }

    #[test]
    fn probe_reranker_config_unknown_provider_is_config() {
        let inputs = DoctorModelInputs {
            reranker_enabled: true,
            reranker_model: "bogus:thing".to_string(),
            ..Default::default()
        };
        let r = probe_reranker_config(&inputs);
        assert_eq!(r.status, ProbeStatus::Config);
    }

    #[test]
    fn probe_reranker_config_no_reranker_touchpoint() {
        let inputs = DoctorModelInputs {
            reranker_enabled: true,
            reranker_model: "openai:gpt-5.2".to_string(),
            ..Default::default()
        };
        let r = probe_reranker_config(&inputs);
        assert_eq!(r.status, ProbeStatus::Config);
        assert!(r.message.contains("does not declare a reranker touchpoint"));
    }

    #[test]
    fn probe_reranker_config_valid_default_ok() {
        let inputs = DoctorModelInputs {
            reranker_enabled: true,
            reranker_model: DEFAULT_RERANK_MODEL.to_string(),
            ..Default::default()
        };
        let r = probe_reranker_config(&inputs);
        assert_eq!(r.status, ProbeStatus::Ok);
    }
}
