//! Provider capability detection for the gateway-native subagent tool loop.
//!
//! Phase 8 sub-node 1-2-1 (Part6). Ported from `src/core/ai/capabilities.ts`.
//!
//! Pre-v0.38 the subagent loop was Anthropic-direct and pinned to Anthropic
//! because crash-replay relied on Anthropic's stable `tool_use_id`s. v0.38
//! moved stable-ID generation zbrain-side (ordinal + uuid v7 persisted in
//! `subagent_tool_executions`), decoupling the loop from any provider's
//! response format. Capability routing therefore asks "can this model run a
//! tool loop?" via recipe-declared fields rather than "is this Anthropic?".
//!
//! This module reads capabilities from the recipe registry
//! (`crate::ai::REGISTRY`, via [`resolve_recipe_strict`]) and surfaces them via
//! a normalized [`ProviderCapabilities`] shape that the tier-routing gate
//! ([`crate::ai::model_config::enforce_subagent_capable`]) consumes to decide:
//!   - REFUSE (fall back) at submit when tool-calling is unsupported
//!   - WARN at submit when prompt caching is unavailable (cost regression)
//!   - INFO at submit when parallel tools unsupported (just slower)
//!
//! The capability shape is intentionally narrow. Per-call cost is already in
//! `ChatTouchpoint.cost_per_1m_*`; routing decisions don't depend on it, so it
//! is not re-exported here.

use super::resolver::resolve_recipe_strict;

/// Normalized capability set for a `provider:model`. Mirrors the TS
/// `ProviderCapabilities` interface.
// Four bool capability flags faithfully mirror the TS shape; each is an
// independent provider signal, so a bitflags/enum collapse would obscure the
// 1:1 port and the field-level docs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderCapabilities {
    /// Provider returns native function/tool calling. Required for the
    /// subagent loop.
    pub supports_tool_calling: bool,
    /// Anthropic-style ephemeral prompt cache markers honored. When false, the
    /// loop runs hot (no `cache_control` injection) and per-turn costs scale
    /// linearly with conversation length. Doesn't break the loop; just costs
    /// more.
    pub supports_prompt_caching: bool,
    /// Provider can return multiple `tool_use` blocks in a single assistant
    /// turn. When false, the loop falls back to serial dispatch — a perf hit,
    /// not a correctness issue.
    ///
    /// NOTE: faithfully mirrors TS — this reads from `chat.supports_tools`
    /// because no recipe exposes a separate parallel-tools field today. The
    /// Rust registry has a distinct `supports_subagent_loop` field, but that is
    /// a stricter loop-safety signal for a different purpose; parallel dispatch
    /// still gates on `supports_tools` to match the TS contract. Treat as
    /// "best-effort capability hint".
    pub supports_parallel_tools: bool,
    /// Provider supports an extended-thinking / reasoning block in responses.
    /// Not load-bearing for the loop.
    ///
    /// Faithfully mirrors TS: hardcoded `false` because no `ChatTouchpoint`
    /// field exposes it today. Do NOT derive this from `supports_subagent_loop`
    /// — that would be semantic drift. A future recipe field can flip this
    /// without changing the helper's shape.
    pub supports_thinking: bool,
    /// Max input+output tokens the provider/model accepts per turn. Drives the
    /// gateway's pre-flight context check.
    pub max_context: usize,
}

/// Default context ceiling when a recipe declares no `max_context_tokens`
/// (e.g. openrouter, whose catalog spans too wide a range for one safe value).
/// Mirrors the TS `?? 128_000` fallback.
const DEFAULT_MAX_CONTEXT: usize = 128_000;

/// Resolve a `provider:model` string and return its capability set.
///
/// # Errors
/// Returns [`super::resolver::AiConfigError`] when the provider/model is
/// unknown OR when the provider lacks a `chat` touchpoint (e.g. embedding-only
/// providers like Voyage). Callers wanting a soft check wrap in a match and
/// degrade — see [`classify_capabilities`].
pub fn get_provider_capabilities(
    model_string: &str,
) -> Result<ProviderCapabilities, super::resolver::AiConfigError> {
    let (_parsed, recipe) = resolve_recipe_strict(model_string)?;
    let Some(chat) = recipe.touchpoints.chat else {
        return Err(super::resolver::AiConfigError::new(
            format!("Provider \"{}\" does not offer a chat touchpoint.", recipe.id),
            "Pick a provider with a chat touchpoint for models.tier.subagent \
             (e.g. openai, anthropic, google, openrouter, deepseek, groq, \
             together, ollama, llama-server).",
        ));
    };

    Ok(ProviderCapabilities {
        supports_tool_calling: chat.supports_tools,
        supports_prompt_caching: chat.supports_prompt_cache == Some(true),
        // No recipe exposes parallel-tools specifically yet; gate on
        // supports_tools (faithful to TS).
        supports_parallel_tools: chat.supports_tools,
        // Not exposed by ChatTouchpoint today — faithful TS `false`.
        supports_thinking: false,
        max_context: chat.max_context_tokens.unwrap_or(DEFAULT_MAX_CONTEXT),
    })
}

/// Capability verdict consumed by the subagent tier gate. Mirrors the TS
/// `CapabilityVerdict` string-union.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityVerdict {
    /// Provider has tool-calling, prompt caching, and parallel tools. Loop runs
    /// at full speed.
    Ok,
    /// Provider supports tools but lacks prompt caching. Loop runs but per-turn
    /// cost is higher. Warn once per (source, model).
    DegradedNoCaching,
    /// Provider supports tools + caching but the loop dispatches serially.
    /// Info-log; no warn.
    DegradedNoParallel,
    /// Provider lacks tool calling entirely. Refuse at submit; the loop cannot
    /// execute brain ops.
    UnusableNoTools,
    /// The provider/model isn't in any recipe. Refuse at submit (defensive:
    /// don't spend money on an unrecognized provider).
    Unknown,
}

/// Tier-1 gate consumed by
/// [`crate::ai::model_config::enforce_subagent_capable`]. Pure function; no
/// side effects. The caller decides what to do with each verdict (warn / info /
/// fall back). Mirrors the TS `classifyCapabilities`.
///
/// Precedence (first match wins): unknown provider → `Unknown`; no tools →
/// `UnusableNoTools`; no caching → `DegradedNoCaching`; no parallel →
/// `DegradedNoParallel`; else `Ok`.
#[must_use]
pub fn classify_capabilities(model_string: &str) -> CapabilityVerdict {
    let Ok(caps) = get_provider_capabilities(model_string) else {
        return CapabilityVerdict::Unknown;
    };
    if !caps.supports_tool_calling {
        return CapabilityVerdict::UnusableNoTools;
    }
    if !caps.supports_prompt_caching {
        return CapabilityVerdict::DegradedNoCaching;
    }
    if !caps.supports_parallel_tools {
        return CapabilityVerdict::DegradedNoParallel;
    }
    CapabilityVerdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- get_provider_capabilities ----

    #[test]
    fn caps_anthropic_full() {
        // Anthropic declares tools + prompt cache + 200k context.
        let caps = get_provider_capabilities("anthropic:claude-sonnet-4-6").unwrap();
        assert!(caps.supports_tool_calling);
        assert!(caps.supports_prompt_caching);
        assert!(caps.supports_parallel_tools); // faithfully mirrors supports_tools
        assert!(!caps.supports_thinking); // faithful TS hardcoded false
        assert_eq!(caps.max_context, 200_000);
    }

    #[test]
    fn caps_openai_no_caching() {
        // OpenAI declares tools but supports_prompt_cache = Some(false).
        let caps = get_provider_capabilities("openai:gpt-5.2").unwrap();
        assert!(caps.supports_tool_calling);
        assert!(!caps.supports_prompt_caching);
        assert_eq!(caps.max_context, 200_000);
    }

    #[test]
    fn caps_embedding_only_provider_errors() {
        // Voyage is embedding-only — no chat touchpoint → AiConfigError.
        let err = get_provider_capabilities("voyage:voyage-3").unwrap_err();
        assert!(err.message.contains("does not offer a chat touchpoint"));
    }

    #[test]
    fn caps_unknown_provider_errors() {
        assert!(get_provider_capabilities("nope:model").is_err());
    }

    #[test]
    fn caps_bare_model_no_provider_errors() {
        // No provider prefix → parse error surfaces as AiConfigError.
        assert!(get_provider_capabilities("claude-sonnet-4-6").is_err());
    }

    // ---- classify_capabilities ----

    #[test]
    fn classify_ok_for_anthropic() {
        // tools + caching + (parallel==tools) → Ok.
        assert_eq!(
            classify_capabilities("anthropic:claude-sonnet-4-6"),
            CapabilityVerdict::Ok
        );
    }

    #[test]
    fn classify_degraded_no_caching_for_openai() {
        // tools but no prompt cache → DegradedNoCaching.
        assert_eq!(
            classify_capabilities("openai:gpt-5.2"),
            CapabilityVerdict::DegradedNoCaching
        );
    }

    #[test]
    fn classify_unknown_for_embedding_only() {
        // No chat touchpoint → get_provider_capabilities errs → Unknown.
        assert_eq!(
            classify_capabilities("voyage:voyage-3"),
            CapabilityVerdict::Unknown
        );
    }

    #[test]
    fn classify_unknown_for_unrecognized_provider() {
        assert_eq!(
            classify_capabilities("nope:model"),
            CapabilityVerdict::Unknown
        );
    }

    // NOTE: `UnusableNoTools` and `DegradedNoParallel` are unreachable with the
    // current registry — no recipe declares `supports_tools: false`, and
    // `supports_parallel_tools` faithfully mirrors `supports_tools` (so the
    // no-parallel branch only fires when tools is false, which already returns
    // UnusableNoTools first). Both branches are preserved to mirror the TS
    // `CapabilityVerdict` union and will activate the moment a tool-less chat
    // recipe (or a distinct parallel-tools field) is added. Their branch logic
    // is verified by code review, not a data-driven test, because there is no
    // registry input that reaches them.
}
