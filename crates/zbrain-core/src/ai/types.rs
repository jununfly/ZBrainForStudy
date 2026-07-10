//! AI provider recipe types — the pure-data half of the AI gateway.
//!
//! Ported from `src/core/ai/types.ts`. Recipes are **pure data**: no auth
//! resolution, no HTTP, no probe hooks. Those behavioral hooks
//! (`resolveAuth` / `resolveOpenAICompatConfig` / `probe` /
//! `resolveDefaultHeaders` in the TS recipes) are intentionally NOT ported
//! here — they land in the ChatProvider/embedding trait layer (Phase 8 slice
//! 3). Keeping the registry pure-data means it is a `static` table with zero
//! IO, trivially testable and const-constructible.
//!
//! Shape is **provider-centric / nested**: one `Recipe` per provider, with a
//! `Touchpoints` struct holding optional per-capability configs. This mirrors
//! the TS `Recipe { touchpoints: { embedding?, expansion?, chat?, reranker? } }`
//! so the by-provider resolve path (the primary query route, driven by
//! `provider:model` strings) is an O(1) struct lookup.

/// Distinguishes native-package providers from openai-compatible endpoints.
///
/// Maps to the TS gateway's `implementation` switch that selects which
/// statically-imported factory to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Implementation {
    NativeOpenai,
    NativeGoogle,
    NativeAnthropic,
    OpenaiCompatible,
}

impl Implementation {
    /// Stable wire string, matching the TS `Implementation` union literals.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Implementation::NativeOpenai => "native-openai",
            Implementation::NativeGoogle => "native-google",
            Implementation::NativeAnthropic => "native-anthropic",
            Implementation::OpenaiCompatible => "openai-compatible",
        }
    }
}

/// Recipe tier: `native` (first-party SDK) vs `openai-compat` (generic
/// OpenAI-compatible endpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Native,
    OpenaiCompat,
}

impl Tier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::Native => "native",
            Tier::OpenaiCompat => "openai-compat",
        }
    }
}

/// Env var names for auth. `required` must all be present; `optional` may be
/// absent. `setup_url` points at the provider's key-issuance page for error
/// hints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthEnv {
    /// Required env vars; first is the primary API key. Empty for local/no-auth
    /// recipes (ollama, llama-server, litellm without key).
    pub required: &'static [&'static str],
    /// Optional env vars (org ids, base-url overrides, secondary keys).
    pub optional: &'static [&'static str],
    /// Provider key-issuance / setup docs URL. `None` when the recipe has no
    /// single canonical setup page.
    pub setup_url: Option<&'static str>,
}

/// Embedding touchpoint. Pure-data mirror of TS `EmbeddingTouchpoint`.
///
/// `models` empty is only valid together with `user_provided_models = true`
/// (litellm-proxy / llama-server "bring your own backend" recipes); the
/// contract test enforces this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmbeddingTouchpoint {
    pub models: &'static [&'static str],
    pub default_dims: usize,
    /// Matryoshka-aware providers advertise selectable output dims.
    pub dims_options: Option<&'static [usize]>,
    pub cost_per_1m_tokens_usd: Option<f64>,
    /// ISO date the price was last verified.
    pub price_last_verified: Option<&'static str>,
    /// Max tokens per batch for this provider's embedding endpoint. `None`
    /// means single-call, no pre-split (OpenAI fast path).
    pub max_batch_tokens: Option<usize>,
    /// Chars-per-token density for pre-split budgeting. Defaults to 4 when
    /// unset (only consulted with `max_batch_tokens`).
    pub chars_per_token: Option<f64>,
    /// Budget-utilization ceiling in (0, 1]. Defaults to 0.8 when unset.
    pub safety_factor: Option<f64>,
    /// At least one model accepts image inputs via a multimodal endpoint.
    pub supports_multimodal: Option<bool>,
    /// Explicit allow-list of multimodal-capable models when the recipe mixes
    /// text-only and multimodal models under one touchpoint.
    pub multimodal_models: Option<&'static [&'static str]>,
    /// Recipe ships without a fixed model list; user must pass model + dims.
    pub user_provided_models: Option<bool>,
    /// Explicit opt-out of the missing-`max_batch_tokens` startup warning.
    pub no_batch_cap: Option<bool>,
}

/// Expansion touchpoint (query expansion LLMs). Pure-data mirror of TS
/// `ExpansionTouchpoint`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpansionTouchpoint {
    pub models: &'static [&'static str],
    pub cost_per_1m_tokens_usd: Option<f64>,
    pub price_last_verified: Option<&'static str>,
}

/// Chat touchpoint: tool-using conversational LLMs. Pure-data mirror of TS
/// `ChatTouchpoint`.
///
/// `supports_tools` and `supports_subagent_loop` are intentionally separate:
/// some chat-capable models have flaky tool-calling; `supports_subagent_loop`
/// is the stricter signal the subagent loop asserts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChatTouchpoint {
    pub models: &'static [&'static str],
    pub supports_tools: bool,
    pub supports_subagent_loop: bool,
    pub supports_prompt_cache: Option<bool>,
    /// Recipe-wide context ceiling. `None` when the catalog spans too wide a
    /// range for a single safe value (openrouter).
    pub max_context_tokens: Option<usize>,
    pub cost_per_1m_input_usd: Option<f64>,
    pub cost_per_1m_output_usd: Option<f64>,
    pub price_last_verified: Option<&'static str>,
}

/// Reranker touchpoint: cross-encoder rerankers. Pure-data mirror of TS
/// `RerankerTouchpoint`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RerankerTouchpoint {
    pub models: &'static [&'static str],
    pub default_model: &'static str,
    pub cost_per_1m_tokens_usd: Option<f64>,
    pub price_last_verified: Option<&'static str>,
    pub max_payload_bytes: usize,
    /// Override the rerank URL path (defaults to `/models/rerank`).
    pub path: Option<&'static str>,
    /// Recipe-level timeout fallback for rerank calls.
    pub default_timeout_ms: Option<u64>,
}

/// The four capability slots a provider may expose. Absent (`None`) means the
/// provider does not offer that capability.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touchpoints {
    pub embedding: Option<EmbeddingTouchpoint>,
    pub expansion: Option<ExpansionTouchpoint>,
    pub chat: Option<ChatTouchpoint>,
    pub reranker: Option<RerankerTouchpoint>,
}

/// A provider recipe: pure static data describing how to reach one provider's
/// models across capability touchpoints. Behavioral hooks are NOT here (see
/// module docs).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recipe {
    /// Stable lowercase id used in `provider:model` strings. Unique across
    /// recipes.
    pub id: &'static str,
    /// Human-readable display name.
    pub name: &'static str,
    pub tier: Tier,
    pub implementation: Implementation,
    /// Default base URL for openai-compatible tier. `None` for native
    /// providers and recipes whose URL is env-template-only (azure).
    pub base_url_default: Option<&'static str>,
    pub auth_env: Option<AuthEnv>,
    pub touchpoints: Touchpoints,
    /// Optional alias map (`&[(from, to)]`) for friendlier `provider:model`
    /// strings, resolved at lookup time.
    pub aliases: Option<&'static [(&'static str, &'static str)]>,
    /// One-line setup description (shown in wizard + env subcommand).
    pub setup_hint: Option<&'static str>,
}

impl Recipe {
    /// True when this recipe exposes the given touchpoint kind.
    #[must_use]
    pub const fn has_touchpoint(&self, kind: TouchpointKind) -> bool {
        match kind {
            TouchpointKind::Embedding => self.touchpoints.embedding.is_some(),
            TouchpointKind::Expansion => self.touchpoints.expansion.is_some(),
            TouchpointKind::Chat => self.touchpoints.chat.is_some(),
            TouchpointKind::Reranker => self.touchpoints.reranker.is_some(),
        }
    }
}

/// The touchpoint kinds the registry models. A subset of the TS `TouchpointKind`
/// union — only the four with concrete recipe configs are represented as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TouchpointKind {
    Embedding,
    Expansion,
    Chat,
    Reranker,
}
