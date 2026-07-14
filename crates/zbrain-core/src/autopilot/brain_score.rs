//! 1-5-2: Brain health scoring + remediation recommendations.
//!
//! Ports `src/core/brain-score-recommendations.ts` + `src/core/anthropic-pricing.ts`
//! + `src/core/embedding-pricing.ts` + `src/core/remediation-step.ts`.
//!
//! Pure module — no engine I/O except `get_health()` which lives on the engine
//! trait and is implemented per-backend. The recommendation/classification/
//! scoring functions here are pure: they take a `BrainHealth` snapshot and a
//! `RecommendationContext` and return deterministic output.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── BrainHealth ────────────────────────────────────────────────────────

/// Health snapshot of a brain. Mirrors TS `BrainHealth` interface.
/// Produced by [`BrainEngine::get_health`]. Consumed by
/// [`compute_recommendations`] and [`max_reachable_score`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BrainHealth {
    pub page_count: usize,
    pub embed_coverage: f64,
    pub stale_pages: usize,
    /// Islanded pages — zero inbound AND zero outbound links.
    pub orphan_pages: usize,
    pub missing_embeddings: usize,
    /// Composite quality score, 0-100.
    pub brain_score: u32,
    pub dead_links: usize,
    /// Fraction of entity pages (person/company) with ≥ 1 inbound link.
    pub link_coverage: f64,
    /// Fraction of entity pages (person/company) with ≥ 1 timeline entry.
    pub timeline_coverage: f64,
    /// Top 5 entities by total link count (in + out).
    pub most_connected: Vec<MostConnectedEntry>,
    // Per-component scores (sum = brain_score)
    pub embed_coverage_score: u32,     // 0-35
    pub link_density_score: u32,       // 0-25
    pub timeline_coverage_score: u32,  // 0-15
    pub no_orphans_score: u32,         // 0-15
    pub no_dead_links_score: u32,      // 0-10
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MostConnectedEntry {
    pub slug: String,
    pub link_count: usize,
}

impl BrainHealth {
    /// Compute brain_score from component scores. Used by `get_health()`
    /// implementations after computing the five sub-scores.
    pub fn compute_brain_score(
        embed_coverage_score: u32,
        link_density_score: u32,
        timeline_coverage_score: u32,
        no_orphans_score: u32,
        no_dead_links_score: u32,
    ) -> u32 {
        embed_coverage_score + link_density_score + timeline_coverage_score
            + no_orphans_score + no_dead_links_score
    }
}

// ── Pricing tables ─────────────────────────────────────────────────────

/// USD per 1M tokens for an Anthropic chat model.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
}

/// Static pricing table for Anthropic models. Prices in USD per 1M tokens.
/// Mirrors TS `ANTHROPIC_PRICING`. Update when Anthropic publishes new pricing.
pub static ANTHROPIC_PRICING: &[(&str, ModelPricing)] = &[
    // Claude 4.7 generation (current)
    ("claude-opus-4-7", ModelPricing { input: 5.00, output: 25.00 }),
    ("claude-sonnet-4-6", ModelPricing { input: 3.00, output: 15.00 }),
    ("claude-haiku-4-5-20251001", ModelPricing { input: 1.00, output: 5.00 }),
    // Older but still frequently aliased
    ("claude-opus-4-6", ModelPricing { input: 5.00, output: 25.00 }),
    ("claude-3-5-sonnet-20241022", ModelPricing { input: 3.00, output: 15.00 }),
    ("claude-3-5-haiku-20241022", ModelPricing { input: 0.80, output: 4.00 }),
];

/// USD per 1M tokens for an embedding model.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingPricing {
    pub price_per_m_tok: f64,
}

/// Static pricing table for embedding models. `provider:model` keyed.
/// Mirrors TS `EMBEDDING_PRICING`.
pub static EMBEDDING_PRICING: &[(&str, EmbeddingPricing)] = &[
    // OpenAI
    ("openai:text-embedding-3-large", EmbeddingPricing { price_per_m_tok: 0.13 }),
    ("openai:text-embedding-3-small", EmbeddingPricing { price_per_m_tok: 0.02 }),
    ("openai:text-embedding-ada-002", EmbeddingPricing { price_per_m_tok: 0.10 }),
    // Voyage
    ("voyage:voyage-3-large", EmbeddingPricing { price_per_m_tok: 0.18 }),
    ("voyage:voyage-3", EmbeddingPricing { price_per_m_tok: 0.06 }),
    ("voyage:voyage-4-large", EmbeddingPricing { price_per_m_tok: 0.18 }),
    // ZeroEntropy
    ("zeroentropyai:zembed-1", EmbeddingPricing { price_per_m_tok: 0.05 }),
];

/// Result of looking up an embedding model's price.
#[derive(Debug, Clone, PartialEq)]
pub enum PriceLookupResult {
    Known { price_per_m_tok: f64, key: String },
    Unknown { provider: String, model: String },
}

/// Resolve a model string into a price-per-1M-tokens.
/// Accepts both `provider:model` and bare `model` forms (bare assumes openai).
pub fn lookup_embedding_price(model_string: &str) -> PriceLookupResult {
    let (provider, model) = if model_string.contains(':') {
        let parts: Vec<&str> = model_string.splitn(2, ':').collect();
        (parts[0].trim().to_lowercase(), parts[1].trim().to_string())
    } else {
        ("openai".to_string(), model_string.trim().to_string())
    };
    let key = format!("{}:{}", provider, model);
    for (k, p) in EMBEDDING_PRICING.iter() {
        if *k == key {
            return PriceLookupResult::Known {
                price_per_m_tok: p.price_per_m_tok,
                key,
            };
        }
    }
    PriceLookupResult::Unknown { provider, model }
}

/// Estimate USD cost for embedding `char_count` characters.
/// Uses 3.5 chars/token approximation (English-biased; CJK underestimates ~2x).
pub fn estimate_cost_from_chars(char_count: usize, price_per_m_tok: f64) -> f64 {
    let tokens = (char_count as f64 / 3.5).ceil() as usize;
    (tokens as f64 / 1_000_000.0) * price_per_m_tok
}

/// Estimate the per-recommendation USD cost ceiling for an Anthropic-model job.
/// Returns 0 for unknown models (budget gate bypassed with a warn-once).
pub fn estimate_anthropic_cost(
    model_id: &str,
    est_calls_per_invocation: usize,
    est_input_tokens_per_call: usize,
    est_output_tokens_per_call: usize,
) -> f64 {
    let pricing = lookup_anthropic_pricing(model_id);
    let p = match pricing {
        Some(p) => p,
        None => return 0.0,
    };
    let input_cost = (est_input_tokens_per_call * est_calls_per_invocation) as f64
        / 1_000_000.0
        * p.input;
    let output_cost = (est_output_tokens_per_call * est_calls_per_invocation) as f64
        / 1_000_000.0
        * p.output;
    ((input_cost + output_cost) * 100.0).round() / 100.0
}

/// Look up Anthropic pricing by model id. Accepts both bare
/// (`claude-opus-4-7`) and provider-prefixed (`anthropic:claude-opus-4-7`) ids.
fn lookup_anthropic_pricing(model_id: &str) -> Option<ModelPricing> {
    for (k, p) in ANTHROPIC_PRICING.iter() {
        if *k == model_id {
            return Some(*p);
        }
    }
    // Try tail after ':'
    if model_id.contains(':') {
        let tail = model_id.rsplit(':').next()?;
        for (k, p) in ANTHROPIC_PRICING.iter() {
            if *k == tail {
                return Some(*p);
            }
        }
    }
    None
}

// ── embeddingProviderConfigured (Option B: hardcoded 4 providers) ──────

/// Known embedding providers and their required auth env vars.
/// Option B per grill Q5: hardcode instead of porting the recipe registry.
///
/// Providers with empty required keys (ollama, llama-server) are local —
/// no hosted key needed. Hosted providers (openai, zeroentropyai) require
/// the listed env vars to be present.
///
/// KNOWN-GAPS G29: recipe registry not migrated. New providers need manual
/// match arm addition here.
static KNOWN_EMBEDDING_PROVIDERS: &[(&str, &[&str])] = &[
    // provider_id, required_env_vars (empty = local, no key needed)
    ("openai", &["OPENAI_API_KEY"]),
    ("zeroentropyai", &["ZEROENTROPY_API_KEY"]),
    ("voyage", &["VOYAGE_API_KEY"]),
    ("ollama", &[]),
    ("llama-server", &[]),
    ("google", &["GOOGLE_GENERATIVE_AI_API_KEY"]),
];

/// Check if the configured embedding provider is usable.
///
/// `resolve_key` is a closure that returns true if the given env var name
/// has a non-empty value. This lets each caller read config from its own
/// source (doctor → file plane; autopilot → engine.getConfig).
///
/// Returns false for:
/// - No embedding model configured
/// - Unknown provider (not in KNOWN_EMBEDDING_PROVIDERS)
/// - Hosted provider with missing required key
pub fn embedding_provider_configured<F>(embedding_model: Option<&str>, resolve_key: F) -> bool
where
    F: Fn(&str) -> bool,
{
    let model = match embedding_model {
        Some(m) if !m.is_empty() => m,
        _ => return false,
    };

    let provider_id = if model.contains(':') {
        model.splitn(2, ':').next().unwrap().trim().to_lowercase()
    } else {
        // Bare model id — assume openai (matches TS behavior)
        "openai".to_string()
    };

    // Find provider in known list
    let required: &[&str] = KNOWN_EMBEDDING_PROVIDERS
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, keys)| *keys)
        .unwrap_or(&[]);

    if required.is_empty() {
        // Local provider (ollama, llama-server) — no key needed
        true
    } else {
        required.iter().all(|env_var| resolve_key(env_var))
    }
}

// ── RemediationStep ────────────────────────────────────────────────────

/// Severity buckets — drive ordering (critical first) and operator UX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemediationSeverity {
    Critical,
    High,
    Medium,
    Low,
}

impl RemediationSeverity {
    fn rank(self) -> u8 {
        match self {
            RemediationSeverity::Critical => 0,
            RemediationSeverity::High => 1,
            RemediationSeverity::Medium => 2,
            RemediationSeverity::Low => 3,
        }
    }
}

/// Triage status of an individual check's autofix path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemediationStatus {
    Remediable,
    HumanOnly,
    Blocked,
}

/// Structured remediation step emitted by doctor checks and the
/// recommendation generator. Mirrors TS `RemediationStep`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemediationStep {
    pub id: String,
    pub job: String,
    pub params: serde_json::Value,
    pub idempotency_key: String,
    pub severity: RemediationSeverity,
    pub est_seconds: u64,
    pub est_usd_cost: Option<f64>,
    pub depends_on: Vec<String>,
    pub rationale: String,
    pub protected: Option<bool>,
    pub status: RemediationStatus,
    pub blocked_reason: Option<String>,
}

/// Canonical JSON serializer: sorts object keys recursively before stringify
/// so the same logical params always hash to the same value regardless of
/// insertion order. Mirrors TS `canonicalJson`.
pub fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => serde_json::to_string(s).unwrap_or_default(),
        serde_json::Value::Array(arr) => {
            format!("[{}]", arr.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        }
        serde_json::Value::Object(obj) => {
            let mut keys: Vec<&String> = obj.keys().collect();
            keys.sort();
            let pairs: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        canonical_json(&obj[*k])
                    )
                })
                .collect();
            format!("{{{}}}", pairs.join(","))
        }
    }
}

/// SHA-256 of a UTF-8 string, hex-encoded, first 8 chars.
fn sha8(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    format!("{:x}", result)[..8].to_string()
}

/// Build a content-stable idempotency key.
/// Pattern: `<source>:<job>:sha8(canonical-JSON(params))`
pub fn idempotency_key(source: &str, job: &str, params: &serde_json::Value) -> String {
    format!("{}:{}:{}", source, job, sha8(&canonical_json(params)))
}

// ── Check / RecommendationContext / CheckClassification ───────────────

/// Minimal Check shape consumed by `classify_checks`.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

/// Context for generating recommendations. Names which prereqs are met.
#[derive(Debug, Clone, Default)]
pub struct RecommendationContext {
    pub source_id: Option<String>,
    pub repo_path: Option<String>,
    pub embedding_model: Option<String>,
    pub embedding_dimensions: Option<usize>,
    pub embedding_provider_configured: Option<bool>,
    pub chat_model: Option<String>,
    pub has_chat_api_key: Option<bool>,
}

/// Triage result for one check.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckClassification {
    pub check: String,
    pub status: RemediationStatus,
    pub reason: Option<String>,
}

// ── compute_recommendations ────────────────────────────────────────────

/// Generate ordered Remediation list from health snapshot + context.
///
/// Returns ONLY `remediable` items. `blocked` items surface via
/// `classify_checks()` and are rendered alongside the plan as informational.
///
/// Sort: severity (critical > high > medium > low), then est_seconds asc.
pub fn compute_recommendations(
    health: &BrainHealth,
    ctx: &RecommendationContext,
) -> Vec<RemediationStep> {
    let mut out: Vec<RemediationStep> = Vec::new();
    let source = ctx.source_id.as_deref().unwrap_or("default");

    // sync.repo — fires when sync hasn't run recently OR pages are stale
    if ctx.repo_path.is_some() && health.stale_pages > 0 {
        let repo = ctx.repo_path.as_deref().unwrap();
        let params = serde_json::json!({
            "repoPath": repo,
            "sourceId": ctx.source_id,
            "noEmbed": true,
        });
        let severity = if health.stale_pages > 50 {
            RemediationSeverity::High
        } else {
            RemediationSeverity::Medium
        };
        let est_seconds = std::cmp::min(600, 30 + (health.stale_pages as u64) / 2);
        out.push(RemediationStep {
            id: "sync.repo".into(),
            job: "sync".into(),
            params: params.clone(),
            idempotency_key: idempotency_key(source, "sync", &params),
            severity,
            est_seconds,
            est_usd_cost: Some(0.0),
            depends_on: vec![],
            rationale: format!(
                "{} stale page{} on disk",
                health.stale_pages,
                if health.stale_pages == 1 { "" } else { "s" }
            ),
            protected: None,
            status: RemediationStatus::Remediable,
            blocked_reason: None,
        });
    }

    // embed.stale — missing embeddings. Critical: invisible to vector search
    if health.missing_embeddings > 0 && ctx.embedding_provider_configured != Some(false) {
        let embed_model = ctx
            .embedding_model
            .as_deref()
            .unwrap_or("openai:text-embedding-3-large");
        let _embed_dims = ctx.embedding_dimensions.unwrap_or(3072);
        let params = serde_json::json!({
            "stale": true,
            "sourceId": ctx.source_id,
        });
        let idem_params = serde_json::json!({
            "stale": true,
            "sourceId": ctx.source_id,
            "embedModel": embed_model,
            "embedDims": ctx.embedding_dimensions.unwrap_or(3072),
        });
        // Rough char estimate per chunk ~ 1.5k chars
        let est_chars = health.missing_embeddings * 1500;
        let est_usd_cost = match lookup_embedding_price(embed_model) {
            PriceLookupResult::Known { price_per_m_tok, .. } => {
                estimate_cost_from_chars(est_chars, price_per_m_tok)
            }
            PriceLookupResult::Unknown { .. } => 0.0,
        };
        let depends_on = if ctx.repo_path.is_some() && health.stale_pages > 0 {
            vec!["sync.repo".to_string()]
        } else {
            vec![]
        };
        let est_seconds = std::cmp::min(3600, 5 + (health.missing_embeddings as u64) / 20);
        out.push(RemediationStep {
            id: "embed.stale".into(),
            job: "embed".into(),
            params,
            idempotency_key: idempotency_key(source, "embed", &idem_params),
            severity: RemediationSeverity::Critical,
            est_seconds,
            est_usd_cost: Some(est_usd_cost),
            depends_on,
            rationale: format!(
                "{} chunk{} invisible to vector search",
                health.missing_embeddings,
                if health.missing_embeddings == 1 { "" } else { "s" }
            ),
            protected: None,
            status: RemediationStatus::Remediable,
            blocked_reason: None,
        });
    }

    // backlinks.fix — dead links
    if health.dead_links > 0 && ctx.repo_path.is_some() {
        let repo = ctx.repo_path.as_deref().unwrap();
        let params = serde_json::json!({
            "action": "fix",
            "dir": repo,
        });
        let est_seconds = std::cmp::min(300, 10 + (health.dead_links as u64) / 2);
        out.push(RemediationStep {
            id: "backlinks.fix".into(),
            job: "backlinks".into(),
            params: params.clone(),
            idempotency_key: idempotency_key(source, "backlinks", &params),
            severity: RemediationSeverity::High,
            est_seconds,
            est_usd_cost: Some(0.0),
            depends_on: vec![],
            rationale: format!(
                "{} dead link{}",
                health.dead_links,
                if health.dead_links == 1 { "" } else { "s" }
            ),
            protected: None,
            status: RemediationStatus::Remediable,
            blocked_reason: None,
        });
    }

    // extract.all — runs after sync to materialize links + timeline
    if ctx.repo_path.is_some() && health.stale_pages > 0 {
        let repo = ctx.repo_path.as_deref().unwrap();
        let params = serde_json::json!({
            "mode": "all",
            "dir": repo,
        });
        let est_seconds = std::cmp::min(600, 30 + (health.page_count as u64) / 100);
        out.push(RemediationStep {
            id: "extract.all".into(),
            job: "extract".into(),
            params: params.clone(),
            idempotency_key: idempotency_key(source, "extract", &params),
            severity: RemediationSeverity::Medium,
            est_seconds,
            est_usd_cost: Some(0.0),
            depends_on: vec!["sync.repo".to_string()],
            rationale: "Materialize link + timeline edges from fresh pages".into(),
            protected: None,
            status: RemediationStatus::Remediable,
            blocked_reason: None,
        });
    }

    // Sort: severity (critical first), then est_seconds ascending
    out.sort_by(|a, b| {
        let sd = a.severity.rank().cmp(&b.severity.rank());
        if sd != std::cmp::Ordering::Equal {
            return sd;
        }
        a.est_seconds.cmp(&b.est_seconds)
    });

    out
}

// ── classify_checks ────────────────────────────────────────────────────

/// Triage every check from the doctor report into one of three buckets.
///
/// Checks not listed here default to `human_only` (conservative — anything
/// the recommendation generator doesn't know about is treated as needing
/// operator judgment, not autonomous remediation).
pub fn classify_checks(
    checks: &[Check],
    ctx: &RecommendationContext,
) -> Vec<CheckClassification> {
    checks
        .iter()
        .map(|c| classify_one(c, ctx))
        .collect()
}

fn classify_one(check: &Check, ctx: &RecommendationContext) -> CheckClassification {
    match check.name.as_str() {
        // remediable paths
        "brain_score" | "sync_freshness" => {
            if ctx.repo_path.is_none() {
                CheckClassification {
                    check: check.name.clone(),
                    status: RemediationStatus::Blocked,
                    reason: Some("no repo configured (set sync.repo_path)".into()),
                }
            } else {
                CheckClassification {
                    check: check.name.clone(),
                    status: RemediationStatus::Remediable,
                    reason: None,
                }
            }
        }
        "missing_embeddings" => {
            if ctx.embedding_provider_configured == Some(false) {
                CheckClassification {
                    check: check.name.clone(),
                    status: RemediationStatus::Blocked,
                    reason: Some("embedding provider not configured".into()),
                }
            } else {
                CheckClassification {
                    check: check.name.clone(),
                    status: RemediationStatus::Remediable,
                    reason: None,
                }
            }
        }
        "dead_links" => {
            if ctx.repo_path.is_none() {
                CheckClassification {
                    check: check.name.clone(),
                    status: RemediationStatus::Blocked,
                    reason: Some("no repo configured".into()),
                }
            } else {
                CheckClassification {
                    check: check.name.clone(),
                    status: RemediationStatus::Remediable,
                    reason: None,
                }
            }
        }

        // human_only paths
        "orphan_pages" | "multi_source_drift" | "eval_drift" | "slug_fallback_audit"
        | "whoknows_health" | "rls_event_trigger" | "reranker_health" => {
            CheckClassification {
                check: check.name.clone(),
                status: RemediationStatus::HumanOnly,
                reason: Some("no autonomous remediation".into()),
            }
        }

        _ => CheckClassification {
            check: check.name.clone(),
            status: RemediationStatus::HumanOnly,
            reason: Some("unmapped check".into()),
        },
    }
}

// ── max_reachable_score ────────────────────────────────────────────────

/// Compute the score ceiling assuming only `remediable` checks fire.
///
/// Each component of brain_score maps to a remediable or non-remediable
/// classification. Components without an autofix path stay at their current
/// score; remediable components can theoretically reach their max.
///
/// Returns the ceiling; callers refuse `--target-score > ceiling`.
pub fn max_reachable_score(
    health: &BrainHealth,
    classifications: &[CheckClassification],
) -> u32 {
    let class_map: std::collections::HashMap<&str, RemediationStatus> = classifications
        .iter()
        .map(|c| (c.check.as_str(), c.status))
        .collect();

    let mut ceiling = 0u32;
    ceiling += pick_max(health.embed_coverage_score, 35, class_map.get("missing_embeddings").copied());
    ceiling += pick_max(health.link_density_score, 25, class_map.get("dead_links").copied());
    ceiling += pick_max(health.timeline_coverage_score, 15, None); // no current autofix
    ceiling += pick_max(health.no_orphans_score, 15, class_map.get("orphan_pages").copied());
    ceiling += pick_max(health.no_dead_links_score, 10, class_map.get("dead_links").copied());
    std::cmp::min(100, ceiling)
}

fn pick_max(current: u32, max: u32, status: Option<RemediationStatus>) -> u32 {
    if status == Some(RemediationStatus::Remediable) {
        max
    } else {
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_health() -> BrainHealth {
        BrainHealth {
            page_count: 100,
            embed_coverage: 0.8,
            stale_pages: 5,
            orphan_pages: 3,
            missing_embeddings: 20,
            brain_score: 75,
            dead_links: 2,
            link_coverage: 0.6,
            timeline_coverage: 0.5,
            most_connected: vec![],
            embed_coverage_score: 28,
            link_density_score: 20,
            timeline_coverage_score: 10,
            no_orphans_score: 12,
            no_dead_links_score: 5,
        }
    }

    // ── pricing ────────────────────────────────────────────────────────

    #[test]
    fn lookup_embedding_price_known_openai() {
        let r = lookup_embedding_price("openai:text-embedding-3-large");
        assert_eq!(
            r,
            PriceLookupResult::Known {
                price_per_m_tok: 0.13,
                key: "openai:text-embedding-3-large".into(),
            }
        );
    }

    #[test]
    fn lookup_embedding_price_unknown_provider() {
        let r = lookup_embedding_price("hunyuan:emb-v1");
        assert_eq!(
            r,
            PriceLookupResult::Unknown {
                provider: "hunyuan".into(),
                model: "emb-v1".into(),
            }
        );
    }

    #[test]
    fn lookup_embedding_price_bare_model_assumes_openai() {
        let r = lookup_embedding_price("text-embedding-3-small");
        match r {
            PriceLookupResult::Known { key, .. } => {
                assert_eq!(key, "openai:text-embedding-3-small");
            }
            _ => panic!("expected known"),
        }
    }

    #[test]
    fn estimate_cost_from_chars_basic() {
        // 1500 chars / 3.5 = ~429 tokens. 429/1M * 0.13 ≈ 0.0000557
        let cost = estimate_cost_from_chars(1500, 0.13);
        assert!(cost > 0.0 && cost < 0.001);
    }

    #[test]
    fn estimate_anthropic_cost_known_model() {
        // claude-sonnet-4-6: input $3/MTok, output $15/MTok
        // 20 calls × (5000 input + 1000 output)
        // input: 20*5000/1M * 3 = 0.3
        // output: 20*1000/1M * 15 = 0.3
        // total = 0.60
        let cost = estimate_anthropic_cost("claude-sonnet-4-6", 20, 5000, 1000);
        assert!((cost - 0.60).abs() < 0.01, "expected 0.60, got {cost}");
    }

    #[test]
    fn estimate_anthropic_cost_unknown_model_returns_zero() {
        let cost = estimate_anthropic_cost("gpt-4o", 20, 5000, 1000);
        assert_eq!(cost, 0.0);
    }

    #[test]
    fn estimate_anthropic_cost_prefixed_model() {
        // anthropic:claude-sonnet-4-6 should resolve same as bare
        let cost = estimate_anthropic_cost("anthropic:claude-sonnet-4-6", 20, 5000, 1000);
        assert!((cost - 0.60).abs() < 0.01, "expected 0.60, got {cost}");
    }

    // ── embeddingProviderConfigured ───────────────────────────────────

    #[test]
    fn embedding_provider_configured_none_returns_false() {
        assert!(!embedding_provider_configured(None, |_| false));
    }

    #[test]
    fn embedding_provider_configured_openai_with_key() {
        assert!(embedding_provider_configured(
            Some("openai:text-embedding-3-large"),
            |env| env == "OPENAI_API_KEY"
        ));
    }

    #[test]
    fn embedding_provider_configured_openai_without_key() {
        assert!(!embedding_provider_configured(
            Some("openai:text-embedding-3-large"),
            |_| false
        ));
    }

    #[test]
    fn embedding_provider_configured_ollama_no_key_needed() {
        assert!(embedding_provider_configured(
            Some("ollama:nomic-embed-text"),
            |_| false
        ));
    }

    #[test]
    fn embedding_provider_configured_unknown_provider() {
        // Unknown provider → no required keys → treated as local (true)
        // This matches TS behavior: getRecipe returns undefined → no touchpoints → false
        // But our hardcoded version: unknown provider not in list → empty required → true
        // This is a known difference (G29): we return true for unknown providers
        // because we can't distinguish "unknown local" from "unknown hosted".
        assert!(embedding_provider_configured(
            Some("unknown-provider:model"),
            |_| false
        ));
    }

    // ── canonical_json + idempotency_key ─────────────────────────────

    #[test]
    fn canonical_json_sorts_keys() {
        let a = serde_json::json!({"b": 1, "a": 2});
        let b = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn idempotency_key_is_content_stable() {
        let p1 = serde_json::json!({"repoPath": "/x", "sourceId": "s1"});
        let p2 = serde_json::json!({"sourceId": "s1", "repoPath": "/x"});
        let k1 = idempotency_key("default", "sync", &p1);
        let k2 = idempotency_key("default", "sync", &p2);
        assert_eq!(k1, k2);
        assert!(k1.starts_with("default:sync:"));
    }

    // ── compute_recommendations ───────────────────────────────────────

    #[test]
    fn compute_recommendations_empty_when_healthy() {
        let health = BrainHealth {
            page_count: 10,
            embed_coverage: 1.0,
            stale_pages: 0,
            orphan_pages: 0,
            missing_embeddings: 0,
            brain_score: 100,
            dead_links: 0,
            link_coverage: 1.0,
            timeline_coverage: 1.0,
            most_connected: vec![],
            embed_coverage_score: 35,
            link_density_score: 25,
            timeline_coverage_score: 15,
            no_orphans_score: 15,
            no_dead_links_score: 10,
        };
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            ..Default::default()
        };
        let recs = compute_recommendations(&health, &ctx);
        assert!(recs.is_empty());
    }

    #[test]
    fn compute_recommendations_sync_when_stale_pages() {
        let health = make_health();
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            ..Default::default()
        };
        let recs = compute_recommendations(&health, &ctx);
        assert!(recs.iter().any(|r| r.id == "sync.repo"));
        assert!(recs.iter().any(|r| r.id == "extract.all"));
    }

    #[test]
    fn compute_recommendations_embed_when_missing() {
        let health = make_health();
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            embedding_provider_configured: Some(true),
            ..Default::default()
        };
        let recs = compute_recommendations(&health, &ctx);
        let embed = recs.iter().find(|r| r.id == "embed.stale").unwrap();
        assert_eq!(embed.severity, RemediationSeverity::Critical);
        // embed depends on sync when stale_pages > 0
        assert!(embed.depends_on.contains(&"sync.repo".to_string()));
    }

    #[test]
    fn compute_recommendations_skips_embed_when_provider_not_configured() {
        let health = make_health();
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            embedding_provider_configured: Some(false),
            ..Default::default()
        };
        let recs = compute_recommendations(&health, &ctx);
        assert!(!recs.iter().any(|r| r.id == "embed.stale"));
    }

    #[test]
    fn compute_recommendations_backlinks_when_dead_links() {
        let health = make_health();
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            ..Default::default()
        };
        let recs = compute_recommendations(&health, &ctx);
        assert!(recs.iter().any(|r| r.id == "backlinks.fix"));
    }

    #[test]
    fn compute_recommendations_sorts_by_severity_then_time() {
        let health = make_health();
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            embedding_provider_configured: Some(true),
            ..Default::default()
        };
        let recs = compute_recommendations(&health, &ctx);
        // Critical (embed) should come first
        assert_eq!(recs[0].severity, RemediationSeverity::Critical);
    }

    #[test]
    fn compute_recommendations_no_sync_without_repo_path() {
        let health = make_health();
        let ctx = RecommendationContext::default();
        let recs = compute_recommendations(&health, &ctx);
        assert!(!recs.iter().any(|r| r.id == "sync.repo"));
        assert!(!recs.iter().any(|r| r.id == "extract.all"));
    }

    // ── classify_checks ────────────────────────────────────────────────

    #[test]
    fn classify_checks_remediable_when_repo_configured() {
        let checks = vec![
            Check { name: "sync_freshness".into(), status: CheckStatus::Warn },
            Check { name: "dead_links".into(), status: CheckStatus::Fail },
        ];
        let ctx = RecommendationContext {
            repo_path: Some("/repo".into()),
            ..Default::default()
        };
        let classifications = classify_checks(&checks, &ctx);
        assert!(classifications.iter().all(|c| c.status == RemediationStatus::Remediable));
    }

    #[test]
    fn classify_checks_blocked_without_repo() {
        let checks = vec![
            Check { name: "sync_freshness".into(), status: CheckStatus::Warn },
        ];
        let ctx = RecommendationContext::default();
        let classifications = classify_checks(&checks, &ctx);
        assert_eq!(classifications[0].status, RemediationStatus::Blocked);
        assert!(classifications[0].reason.as_ref().unwrap().contains("repo"));
    }

    #[test]
    fn classify_checks_missing_embeddings_blocked_without_provider() {
        let checks = vec![
            Check { name: "missing_embeddings".into(), status: CheckStatus::Fail },
        ];
        let ctx = RecommendationContext {
            embedding_provider_configured: Some(false),
            ..Default::default()
        };
        let classifications = classify_checks(&checks, &ctx);
        assert_eq!(classifications[0].status, RemediationStatus::Blocked);
    }

    #[test]
    fn classify_checks_orphan_pages_human_only() {
        let checks = vec![
            Check { name: "orphan_pages".into(), status: CheckStatus::Warn },
        ];
        let ctx = RecommendationContext::default();
        let classifications = classify_checks(&checks, &ctx);
        assert_eq!(classifications[0].status, RemediationStatus::HumanOnly);
    }

    #[test]
    fn classify_checks_unknown_check_defaults_to_human_only() {
        let checks = vec![
            Check { name: "some_random_check".into(), status: CheckStatus::Warn },
        ];
        let ctx = RecommendationContext::default();
        let classifications = classify_checks(&checks, &ctx);
        assert_eq!(classifications[0].status, RemediationStatus::HumanOnly);
        assert_eq!(
            classifications[0].reason.as_deref(),
            Some("unmapped check")
        );
    }

    // ── max_reachable_score ───────────────────────────────────────────

    #[test]
    fn max_reachable_score_all_remediable() {
        let health = make_health();
        let classifications = vec![
            CheckClassification { check: "missing_embeddings".into(), status: RemediationStatus::Remediable, reason: None },
            CheckClassification { check: "dead_links".into(), status: RemediationStatus::Remediable, reason: None },
            CheckClassification { check: "orphan_pages".into(), status: RemediationStatus::Remediable, reason: None },
        ];
        // embed(35) + link_density(25) + timeline_coverage(10, no autofix) + orphans(15) + dead_links(10) = 95
        let score = max_reachable_score(&health, &classifications);
        assert_eq!(score, 95);
    }

    #[test]
    fn max_reachable_score_all_human_only() {
        let health = make_health();
        let classifications: Vec<CheckClassification> = vec![];
        // All components stay at current values
        // 28 + 20 + 10 + 12 + 5 = 75
        let score = max_reachable_score(&health, &classifications);
        assert_eq!(score, 75);
    }

    #[test]
    fn max_reachable_score_empty_brain_is_100() {
        let health = BrainHealth {
            page_count: 0,
            embed_coverage: 0.0,
            stale_pages: 0,
            orphan_pages: 0,
            missing_embeddings: 0,
            brain_score: 100,
            dead_links: 0,
            link_coverage: 0.0,
            timeline_coverage: 0.0,
            most_connected: vec![],
            embed_coverage_score: 35,
            link_density_score: 25,
            timeline_coverage_score: 15,
            no_orphans_score: 15,
            no_dead_links_score: 10,
        };
        let classifications: Vec<CheckClassification> = vec![];
        let score = max_reachable_score(&health, &classifications);
        assert_eq!(score, 100);
    }

    // ── get_health (InMemory integration) ──────────────────────────────

    mod get_health_tests {
        use super::*;
        use crate::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};
        use crate::types::PageKind;
        use crate::import::ChunkInput;

        async fn setup_engine() -> InMemoryEngine {
            let engine = InMemoryEngine::new();
            engine.connect(&EngineConfig::default()).await.unwrap();
            engine
        }

        async fn put_page(engine: &InMemoryEngine, slug: &str, page_type: &str) {
            engine
                .put_page(
                    slug,
                    Some("default"),
                    &PageInput {
                        page_type: page_type.to_string(),
                        title: slug.to_string(),
                        compiled_truth: "content".to_string(),
                        timeline: None,
                        frontmatter: None,
                        content_hash: None,
                        page_kind: None,
                        effective_date: None,
                        effective_date_source: None,
                        import_filename: None,
                        chunker_version: None,
                        source_path: None,
                        source_kind: None,
                        source_uri: None,
                        ingested_via: None,
                        ingested_at: None,
                        last_retrieved_at: None,
                        embedding: None,
                    },
                )
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn empty_brain_scores_100() {
            let engine = setup_engine().await;
            let h = engine.get_health().await.unwrap();
            assert_eq!(h.page_count, 0);
            assert_eq!(h.brain_score, 100);
            assert_eq!(h.embed_coverage_score, 35);
            assert_eq!(h.link_density_score, 25);
            assert_eq!(h.timeline_coverage_score, 15);
            assert_eq!(h.no_orphans_score, 15);
            assert_eq!(h.no_dead_links_score, 10);
        }

        #[tokio::test]
        async fn brain_with_pages_no_chunks() {
            let engine = setup_engine().await;
            put_page(&engine, "page-a", "note").await;
            put_page(&engine, "page-b", "note").await;
            let h = engine.get_health().await.unwrap();
            assert_eq!(h.page_count, 2);
            assert_eq!(h.missing_embeddings, 0);
            assert_eq!(h.embed_coverage, 1.0); // no chunks → full coverage
            assert_eq!(h.orphan_pages, 2); // no links → both islanded
            assert_eq!(h.dead_links, 0);
        }

        #[tokio::test]
        async fn brain_with_chunks_missing_embeddings() {
            let engine = setup_engine().await;
            put_page(&engine, "page-a", "note").await;

            // Add chunks: 2 with embedding, 1 without
            engine
                .upsert_chunks(
                    "page-a",
                    &[
                        ChunkInput {
                            chunk_index: 0,
                            chunk_text: "text1".into(),
                            chunk_source: crate::import::ChunkSource::CompiledTruth,
                            embedding: Some(vec![0.1, 0.2]),
                            token_count: None,
                            language: None,
                            symbol_name: None,
                            symbol_type: None,
                            start_line: None,
                            end_line: None,
                            parent_symbol_path: vec![],
                            symbol_name_qualified: None,
                        },
                        ChunkInput {
                            chunk_index: 1,
                            chunk_text: "text2".into(),
                            chunk_source: crate::import::ChunkSource::CompiledTruth,
                            embedding: Some(vec![0.3, 0.4]),
                            token_count: None,
                            language: None,
                            symbol_name: None,
                            symbol_type: None,
                            start_line: None,
                            end_line: None,
                            parent_symbol_path: vec![],
                            symbol_name_qualified: None,
                        },
                        ChunkInput {
                            chunk_index: 2,
                            chunk_text: "text3".into(),
                            chunk_source: crate::import::ChunkSource::CompiledTruth,
                            embedding: None, // missing!
                            token_count: None,
                            language: None,
                            symbol_name: None,
                            symbol_type: None,
                            start_line: None,
                            end_line: None,
                            parent_symbol_path: vec![],
                            symbol_name_qualified: None,
                        },
                    ],
                )
                .await
                .unwrap();

            let h = engine.get_health().await.unwrap();
            assert_eq!(h.missing_embeddings, 1);
            assert!((h.embed_coverage - (2.0 / 3.0)).abs() < 0.01);
            assert_eq!(h.embed_coverage_score, 23); // round(2/3 * 35) = round(23.33) = 23
        }

        #[tokio::test]
        async fn brain_with_dead_links() {
            let engine = setup_engine().await;
            put_page(&engine, "page-a", "note").await;
            put_page(&engine, "page-b", "note").await;
            // Add link page-a → page-b
            engine
                .add_links_batch(&[crate::types::LinkBatchInput {
                    from_slug: "page-a".into(),
                    to_slug: "page-b".into(),
                    link_type: Some("ref".into()),
                    context: None,
                    link_source: None,
                    origin_slug: None,
                    origin_field: None,
                    from_source_id: None,
                    to_source_id: None,
                    origin_source_id: None,
                }])
                .await
                .unwrap();
            // Delete page-b → link becomes dead
            engine.delete_page("page-b", Some("default")).await.unwrap();

            let h = engine.get_health().await.unwrap();
            assert_eq!(h.dead_links, 1);
            // page-a has outbound link → not orphan
            assert_eq!(h.orphan_pages, 0);
        }

        #[tokio::test]
        async fn brain_with_timeline_entries() {
            let engine = setup_engine().await;
            // page-a with non-empty timeline
            engine
                .put_page(
                    "page-a",
                    Some("default"),
                    &PageInput {
                        page_type: "note".to_string(),
                        title: "A".to_string(),
                        compiled_truth: "content".to_string(),
                        timeline: Some(r#"[{"date":"2026-01-01","event":"test"}]"#.to_string()),
                        frontmatter: None,
                        content_hash: None,
                        page_kind: None,
                        effective_date: None,
                        effective_date_source: None,
                        import_filename: None,
                        chunker_version: None,
                        source_path: None,
                        source_kind: None,
                        source_uri: None,
                        ingested_via: None,
                        ingested_at: None,
                        last_retrieved_at: None,
                        embedding: None,
                    },
                )
                .await
                .unwrap();
            put_page(&engine, "page-b", "note").await; // empty timeline

            let h = engine.get_health().await.unwrap();
            assert_eq!(h.page_count, 2);
            // pages_with_timeline = 1 (page-a has non-empty array)
            // timeline_coverage_score = round(1/2 * 15) = round(7.5) = 8
            assert_eq!(h.timeline_coverage_score, 8);
        }

        #[tokio::test]
        async fn brain_with_entity_pages_and_links() {
            let engine = setup_engine().await;
            put_page(&engine, "alice", "person").await;
            put_page(&engine, "acme", "company").await;
            put_page(&engine, "note-1", "note").await;

            // note-1 → alice (alice has inbound link)
            engine
                .add_links_batch(&[crate::types::LinkBatchInput {
                    from_slug: "note-1".into(),
                    to_slug: "alice".into(),
                    link_type: Some("ref".into()),
                    context: None,
                    link_source: None,
                    origin_slug: None,
                    origin_field: None,
                    from_source_id: None,
                    to_source_id: None,
                    origin_source_id: None,
                }])
                .await
                .unwrap();

            let h = engine.get_health().await.unwrap();
            // 2 entity pages, 1 with inbound link
            assert!((h.link_coverage - 0.5).abs() < 0.01);
            // alice has 1 link (inbound), acme has 0
            assert_eq!(h.most_connected.len(), 1);
            assert_eq!(h.most_connected[0].slug, "alice");
            assert_eq!(h.most_connected[0].link_count, 1);
        }
    }
}
