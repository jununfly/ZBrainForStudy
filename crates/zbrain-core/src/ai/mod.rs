//! AI provider registry — pure-data model/provider/pricing catalog.
//!
//! Phase 8 slice 1 (Part6). Ported from `src/core/ai/` (recipes + types +
//! model-resolver's data-only parts + dims). Behavioral hooks
//! (`resolveAuth`/`probe`/`resolveDefaultHeaders`) are intentionally deferred
//! to the provider trait layer (slice 3); this module is IO-free static data
//! plus lookup helpers.
//!
//! This module **absorbs** the former standalone `embedding_pricing.rs`: its
//! model->price/dims table is now derived from `REGISTRY` embedding touchpoints
//! via [`lookup_pricing`] / [`estimate_cost_usd`], so there is a single source
//! of truth for model data and no drift between two tables.
//!
//! ## Query paths
//! - by-provider (primary): [`resolve_recipe`] — O(n) scan over 17 recipes,
//!   the same route `model-resolver.ts` takes for `provider:model` strings.
//! - by-model (pricing): [`lookup_pricing`] — linear scan across embedding
//!   touchpoints. 17-provider scale; no index needed.

pub mod capabilities;
pub mod chat;
pub mod expand;
pub mod model_config;
pub mod registry;
pub mod resolver;
pub mod tool_loop;
pub mod types;

pub use capabilities::{
    classify_capabilities, get_provider_capabilities, CapabilityVerdict, ProviderCapabilities,
};
pub use chat::{
    instantiate_chat, parse_anthropic_response, parse_gemini_response, parse_openai_response,
    serialize_messages_anthropic, serialize_messages_gemini, serialize_messages_openai,
    serialize_tools_anthropic, serialize_tools_gemini, serialize_tools_openai, ChatBlock,
    ChatContent, ChatError, ChatMessage, ChatOpts, ChatProvider, ChatResult, ChatRole, ChatToolDef,
    ChatUsage, MockChatProvider, StopReason,
};
pub use expand::{
    expand_query, sanitize_expansion_output, sanitize_query_for_prompt, ChatExpansionProvider,
    ExpansionError, ExpansionProvider,
};
pub use model_config::{
    default_alias, enforce_subagent_capable, reset_subagent_warnings_for_test, resolve_model,
    resolve_model_alias, tier_default, ConfigLookup, ModelTier, ResolveModelOpts,
};
pub use registry::REGISTRY;
pub use resolver::{
    assert_touchpoint, known_provider_ids, parse_model_id_strict, resolve_recipe_strict,
    AiConfigError, ParsedModelId,
};
pub use tool_loop::{
    tool_loop, NoopHooks, PriorToolStatus, ToolHandler, ToolLoopHooks, ToolLoopOpts,
    ToolLoopReplayState, ToolLoopResult, ToolLoopStopReason, ZbrainToolUseId,
};
pub use types::{
    AuthEnv, ChatTouchpoint, EmbeddingTouchpoint, ExpansionTouchpoint, Implementation, Recipe,
    RerankerTouchpoint, Tier, TouchpointKind, Touchpoints,
};

/// Look up a recipe by its provider id (the left side of `provider:model`).
///
/// This is the primary query route. Returns `None` for unknown providers.
#[must_use]
pub fn resolve_recipe(provider_id: &str) -> Option<&'static Recipe> {
    REGISTRY.iter().find(|r| r.id == provider_id)
}

/// Parse a `provider:model` string into `(provider, model)`. Mirrors
/// `parseModelId` in `model-resolver.ts` (split on the first `:` only, so
/// model ids containing `:` survive in the model half).
///
/// Returns `None` when there is no `:` separator or the provider half is empty.
#[must_use]
pub fn parse_model_id(spec: &str) -> Option<(&str, &str)> {
    let (provider, model) = spec.split_once(':')?;
    if provider.is_empty() {
        return None;
    }
    Some((provider, model))
}

/// Resolve a possibly-aliased model name against a recipe's alias map.
/// Returns the canonical model name (the alias target) or the input unchanged
/// when no alias matches.
#[must_use]
pub fn resolve_alias<'a>(recipe: &'a Recipe, model: &'a str) -> &'a str {
    match recipe.aliases {
        Some(aliases) => aliases
            .iter()
            .find(|(from, _)| *from == model)
            .map(|(_, to)| *to)
            .unwrap_or(model),
        None => model,
    }
}

/// Embedding pricing/dims for a model, derived from the registry (absorbs the
/// former `embedding_pricing.rs` table).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EmbeddingPricing {
    pub model: &'static str,
    pub provider: &'static str,
    pub price_per_mtok_usd: f64,
    pub dimensions: usize,
}

/// Find embedding pricing for a model. Accepts either a bare model name
/// (`text-embedding-3-small`) or a `provider:model` string. When a known
/// provider prefix is present it scopes the search; otherwise the first
/// embedding touchpoint whose model list contains the name wins. Returns
/// `None` for unknown models or models whose recipe declares no cost.
#[must_use]
pub fn lookup_pricing(model: &str) -> Option<EmbeddingPricing> {
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
        let Some(emb) = recipe.touchpoints.embedding else {
            continue;
        };
        let matched = emb.models.iter().find(|&&m| m == bare_model || m == model);
        if let Some(&matched_model) = matched {
            let price = emb.cost_per_1m_tokens_usd?;
            return Some(EmbeddingPricing {
                model: matched_model,
                provider: recipe.id,
                price_per_mtok_usd: price,
                dimensions: emb.default_dims,
            });
        }
    }
    None
}

/// Estimate embedding cost in USD for `num_chunks × avg_tokens_per_chunk`
/// tokens of the given model. Unknown/free models return 0.0. Ported from the
/// former `embedding_pricing::estimate_cost_usd`.
#[must_use]
pub fn estimate_cost_usd(model: &str, num_chunks: usize, avg_tokens_per_chunk: usize) -> f64 {
    match lookup_pricing(model) {
        Some(pricing) => {
            let total_tokens = num_chunks * avg_tokens_per_chunk;
            let mtok = total_tokens as f64 / 1_000_000.0;
            mtok * pricing.price_per_mtok_usd
        }
        None => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- registry contract tests (mirror recipes-contract.test.ts) ----

    #[test]
    fn registry_has_seventeen_recipes() {
        assert_eq!(REGISTRY.len(), 17, "expected 17 provider recipes");
    }

    #[test]
    fn recipe_ids_are_unique() {
        let mut ids: Vec<&str> = REGISTRY.iter().map(|r| r.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate recipe id found");
    }

    #[test]
    fn recipe_ids_are_lowercase_and_nonempty() {
        for r in REGISTRY {
            assert!(!r.id.is_empty(), "empty recipe id");
            assert_eq!(r.id, r.id.to_lowercase(), "recipe id must be lowercase: {}", r.id);
        }
    }

    #[test]
    fn every_recipe_has_at_least_one_touchpoint() {
        for r in REGISTRY {
            let t = &r.touchpoints;
            let any = t.embedding.is_some()
                || t.expansion.is_some()
                || t.chat.is_some()
                || t.reranker.is_some();
            assert!(any, "recipe {} has no touchpoints", r.id);
        }
    }

    #[test]
    fn touchpoint_models_nonempty_unless_user_provided() {
        for r in REGISTRY {
            if let Some(emb) = r.touchpoints.embedding {
                let user_provided = emb.user_provided_models.unwrap_or(false);
                assert!(
                    !emb.models.is_empty() || user_provided,
                    "recipe {} embedding models empty without user_provided_models",
                    r.id
                );
            }
            if let Some(exp) = r.touchpoints.expansion {
                assert!(!exp.models.is_empty(), "recipe {} expansion models empty", r.id);
            }
            if let Some(chat) = r.touchpoints.chat {
                assert!(!chat.models.is_empty(), "recipe {} chat models empty", r.id);
            }
            // reranker: llama-server-reranker ships empty models[] + a
            // default_model (user aliases the served model), so we only assert
            // default_model presence, matching the TS recipe.
            if let Some(rr) = r.touchpoints.reranker {
                assert!(
                    !rr.default_model.is_empty(),
                    "recipe {} reranker default_model empty",
                    r.id
                );
            }
        }
    }

    #[test]
    fn tier_matches_implementation() {
        for r in REGISTRY {
            match r.tier {
                Tier::Native => assert!(
                    matches!(
                        r.implementation,
                        Implementation::NativeOpenai
                            | Implementation::NativeGoogle
                            | Implementation::NativeAnthropic
                    ),
                    "recipe {} tier=native but implementation not native",
                    r.id
                ),
                Tier::OpenaiCompat => assert!(
                    matches!(r.implementation, Implementation::OpenaiCompatible),
                    "recipe {} tier=openai-compat but implementation not openai-compatible",
                    r.id
                ),
            }
        }
    }

    #[test]
    fn openai_compat_recipes_declare_base_url_except_azure() {
        for r in REGISTRY {
            if matches!(r.implementation, Implementation::OpenaiCompatible)
                && r.id != "azure-openai"
            {
                assert!(
                    r.base_url_default.is_some(),
                    "openai-compat recipe {} missing base_url_default",
                    r.id
                );
            }
        }
    }

    #[test]
    fn dims_options_include_default_dims() {
        for r in REGISTRY {
            if let Some(emb) = r.touchpoints.embedding {
                if emb.user_provided_models.unwrap_or(false) {
                    continue; // default_dims=0 placeholder
                }
                if let Some(opts) = emb.dims_options {
                    assert!(
                        opts.contains(&emb.default_dims),
                        "recipe {} default_dims {} not in dims_options {:?}",
                        r.id,
                        emb.default_dims,
                        opts
                    );
                }
            }
        }
    }

    // ---- query helper tests ----

    #[test]
    fn resolve_recipe_known_and_unknown() {
        assert!(resolve_recipe("openai").is_some());
        assert!(resolve_recipe("anthropic").is_some());
        assert!(resolve_recipe("nonexistent").is_none());
    }

    #[test]
    fn parse_model_id_splits_on_first_colon() {
        assert_eq!(parse_model_id("openai:gpt-5.2"), Some(("openai", "gpt-5.2")));
        assert_eq!(
            parse_model_id("provider:model:with:colons"),
            Some(("provider", "model:with:colons"))
        );
        assert_eq!(parse_model_id("nocolon"), None);
        assert_eq!(parse_model_id(":noprovider"), None);
    }

    #[test]
    fn resolve_alias_hits_and_misses() {
        let anthropic = resolve_recipe("anthropic").unwrap();
        assert_eq!(
            resolve_alias(anthropic, "claude-haiku-4-5"),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(resolve_alias(anthropic, "claude-opus-4-7"), "claude-opus-4-7");
        let openai = resolve_recipe("openai").unwrap();
        assert_eq!(resolve_alias(openai, "gpt-5.2"), "gpt-5.2");
    }

    // ---- pricing tests (absorbed from embedding_pricing.rs) ----

    #[test]
    fn lookup_pricing_known_model() {
        let result = lookup_pricing("text-embedding-3-small");
        assert!(result.is_some());
        let pricing = result.unwrap();
        assert_eq!(pricing.provider, "openai");
        assert_eq!(pricing.dimensions, 1536);
        assert_eq!(pricing.price_per_mtok_usd, 0.13);
    }

    #[test]
    fn lookup_pricing_unknown_model() {
        assert!(lookup_pricing("unknown-model-x").is_none());
    }

    #[test]
    fn lookup_pricing_provider_scoped() {
        let result = lookup_pricing("voyage:voyage-4").unwrap();
        assert_eq!(result.provider, "voyage");
        assert_eq!(result.price_per_mtok_usd, 0.18);
        assert_eq!(result.dimensions, 1024);
    }

    #[test]
    fn estimate_cost_usd_known_model() {
        // 1000 chunks * 500 tokens = 500K tokens = 0.5 MTok
        // text-embedding-3-small: $0.13/MTok -> 0.5 * 0.13 = 0.065
        let cost = estimate_cost_usd("text-embedding-3-small", 1000, 500);
        assert!((cost - 0.065).abs() < 0.0001, "got {cost}");
    }

    #[test]
    fn estimate_cost_usd_unknown_model_is_free() {
        assert_eq!(estimate_cost_usd("unknown-model", 1000, 500), 0.0);
    }
}
