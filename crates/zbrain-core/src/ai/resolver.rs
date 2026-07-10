//! Model-id resolver — the pure-static validation layer of the AI gateway.
//!
//! Phase 8 slice 2 (Part6). Ported from `src/core/ai/model-resolver.ts` and
//! `src/core/ai/errors.ts` (the `AIConfigError` shape only).
//!
//! **Pure static, zero engine coupling.** This layer parses/validates
//! `provider:model` strings against the [`REGISTRY`](super::REGISTRY) and
//! fails fast with an actionable `fix` hint. It does NOT read `BrainEngine`
//! config, do IO, or resolve tier defaults — that config-coupled tier-routing
//! layer (`model-config.ts` `resolveModel`/`TIER_DEFAULTS` + capability gating)
//! now lives in [`super::model_config`] (sub-node 1-2-1), which stays pure by
//! taking an injected `ConfigLookup` instead of reading the DB directly.
//!
//! The non-throwing helpers in [`super`] (`parse_model_id` / `resolve_recipe`
//! / `resolve_alias`) remain for the pricing path (`Option`-returning, never
//! errors). This module adds the **fail-fast** counterparts that mirror the TS
//! contract: unknown provider / unsupported touchpoint / model-not-listed all
//! throw `AiConfigError` with a recovery hint.

use super::types::{Recipe, TouchpointKind};
use super::{resolve_recipe, REGISTRY};
use std::collections::BTreeSet;

/// Config-level AI error the user must fix (bad model id, unknown provider,
/// unsupported touchpoint). Mirrors the TS `AIConfigError`: a `message` plus
/// an optional `fix` recovery recipe. Distinct from transient/service errors —
/// callers abort and surface the fix rather than retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConfigError {
    /// Human-readable summary of what is wrong.
    pub message: String,
    /// Actionable recovery recipe (`AIConfigError.fix` in TS). `None` when the
    /// message is self-explanatory.
    pub fix: Option<String>,
}

impl AiConfigError {
    pub(crate) fn new(message: impl Into<String>, fix: impl Into<String>) -> Self {
        Self { message: message.into(), fix: Some(fix.into()) }
    }
}

impl std::fmt::Display for AiConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.fix {
            Some(fix) => write!(f, "{} — {}", self.message, fix),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for AiConfigError {}

/// A parsed `provider:model` id. Mirrors the TS `ParsedModelId`.
///
/// `provider_id` is lowercased + trimmed (matching TS); `model_id` is trimmed
/// but case-preserved (model names are case-sensitive upstream).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModelId {
    pub provider_id: String,
    pub model_id: String,
}

/// Parse `"openai:text-embedding-3-large"` into `{provider_id, model_id}`,
/// failing fast with a hint. Mirrors `parseModelId` in `model-resolver.ts`:
/// splits on the first `:`, lowercases+trims the provider, trims the model,
/// and rejects empty halves.
///
/// # Errors
/// Returns [`AiConfigError`] when the input has no `:` separator or either
/// half is empty after trimming.
pub fn parse_model_id_strict(id: &str) -> Result<ParsedModelId, AiConfigError> {
    let Some((raw_provider, raw_model)) = id.split_once(':') else {
        return Err(AiConfigError::new(
            format!("Model id \"{id}\" is missing a provider prefix."),
            "Use format provider:model, e.g. openai:text-embedding-3-large",
        ));
    };
    let provider_id = raw_provider.trim().to_lowercase();
    let model_id = raw_model.trim().to_string();
    if provider_id.is_empty() || model_id.is_empty() {
        return Err(AiConfigError::new(
            format!("Model id \"{id}\" has empty provider or model."),
            "Use format provider:model, e.g. openai:text-embedding-3-large",
        ));
    }
    Ok(ParsedModelId { provider_id, model_id })
}

/// Resolve a `provider:model` string to a recipe + canonical model id,
/// applying `recipe.aliases`. Mirrors `resolveRecipe` in `model-resolver.ts`.
///
/// # Errors
/// Returns [`AiConfigError`] for malformed ids (via [`parse_model_id_strict`])
/// or unknown providers, with a hint listing known providers.
pub fn resolve_recipe_strict(
    model_id: &str,
) -> Result<(ParsedModelId, &'static Recipe), AiConfigError> {
    let parsed = parse_model_id_strict(model_id)?;
    let Some(recipe) = resolve_recipe(&parsed.provider_id) else {
        return Err(AiConfigError::new(
            format!("Unknown provider: \"{}\"", parsed.provider_id),
            format!(
                "Known providers: {}. Add a new recipe at crates/zbrain-core/src/ai/registry.rs.",
                known_provider_ids().join(", ")
            ),
        ));
    };
    // Apply alias if the model matches an alias key. Canonical wins.
    let canonical = super::resolve_alias(recipe, &parsed.model_id);
    if canonical != parsed.model_id {
        return Ok((
            ParsedModelId {
                provider_id: parsed.provider_id,
                model_id: canonical.to_string(),
            },
            recipe,
        ));
    }
    Ok((parsed, recipe))
}

/// The model list a recipe declares for a touchpoint, or `None` if the recipe
/// does not offer that touchpoint at all.
fn touchpoint_models(recipe: &Recipe, kind: TouchpointKind) -> Option<&'static [&'static str]> {
    match kind {
        TouchpointKind::Embedding => recipe.touchpoints.embedding.map(|t| t.models),
        TouchpointKind::Expansion => recipe.touchpoints.expansion.map(|t| t.models),
        TouchpointKind::Chat => recipe.touchpoints.chat.map(|t| t.models),
        TouchpointKind::Reranker => recipe.touchpoints.reranker.map(|t| t.models),
    }
}

fn touchpoint_name(kind: TouchpointKind) -> &'static str {
    match kind {
        TouchpointKind::Embedding => "embedding",
        TouchpointKind::Expansion => "expansion",
        TouchpointKind::Chat => "chat",
        TouchpointKind::Reranker => "reranker",
    }
}

/// Assert the resolved recipe actually offers the requested touchpoint and
/// (for native providers) that the model is listed. Mirrors `assertTouchpoint`
/// in `model-resolver.ts`.
///
/// `extended_models` is a per-caller allow-list of models the user opted into
/// via config. When the model is in this set, the native-recipe allowlist
/// check is skipped (provider rejection then surfaces at HTTP-call time). The
/// resolver itself never reads engine config — the caller constructs the set,
/// keeping this layer pure.
///
/// Non-native providers (ollama/litellm/openrouter/...) accept arbitrary model
/// ids, so the allowlist check only fires for `Tier::Native` recipes — exactly
/// as the TS validator does.
///
/// # Errors
/// Returns [`AiConfigError`] when the touchpoint is absent, or when a native
/// provider's model is neither listed nor in `extended_models`.
pub fn assert_touchpoint(
    recipe: &Recipe,
    kind: TouchpointKind,
    model_id: &str,
    extended_models: Option<&BTreeSet<String>>,
) -> Result<(), AiConfigError> {
    let Some(models) = touchpoint_models(recipe, kind) else {
        let tp = touchpoint_name(kind);
        // Targeted hints for the common misconfigurations, matching TS.
        let fix = if matches!(kind, TouchpointKind::Embedding) && recipe.id == "anthropic" {
            "Anthropic has no embedding model. Use openai or google for embeddings.".to_string()
        } else if matches!(kind, TouchpointKind::Chat)
            && (recipe.id == "voyage" || recipe.id == "ollama")
        {
            format!(
                "{} is configured here only for embeddings. Use openai/anthropic/google/deepseek/groq/together for chat.",
                recipe.name
            )
        } else {
            format!("Provider \"{}\" offers no \"{tp}\" touchpoint.", recipe.id)
        };
        return Err(AiConfigError {
            message: format!(
                "Provider \"{}\" does not support touchpoint \"{tp}\".",
                recipe.id
            ),
            fix: Some(fix),
        });
    };

    // Empty model list (user_provided_models recipes) => accept anything.
    if models.is_empty() || models.contains(&model_id) {
        return Ok(());
    }

    // Model not listed. Only native providers fail fast; openai-compat
    // providers accept arbitrary ids (provider 404 surfaces at call time).
    if matches!(recipe.tier, super::types::Tier::Native) {
        if let Some(set) = extended_models {
            if set.contains(model_id) {
                return Ok(());
            }
        }
        return Err(AiConfigError::new(
            format!(
                "Model \"{model_id}\" is not listed for {} {}.",
                recipe.name,
                touchpoint_name(kind)
            ),
            format!(
                "Known models: {}. Use one of these or add it to the recipe (or add an alias).",
                models.join(", ")
            ),
        ));
    }
    Ok(())
}

/// All known provider ids, sorted. Mirrors `knownProviderIds`.
#[must_use]
pub fn known_provider_ids() -> Vec<&'static str> {
    let mut ids: Vec<&'static str> = REGISTRY.iter().map(|r| r.id).collect();
    ids.sort_unstable();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_model_id_strict ----

    #[test]
    fn parse_ok_lowercases_provider_trims_model() {
        let p = parse_model_id_strict("  OpenAI : gpt-5.2 ").unwrap();
        assert_eq!(p.provider_id, "openai");
        assert_eq!(p.model_id, "gpt-5.2");
    }

    #[test]
    fn parse_keeps_model_case_and_inner_colons() {
        let p = parse_model_id_strict("zeroentropyai:zerank-2").unwrap();
        assert_eq!(p.provider_id, "zeroentropyai");
        assert_eq!(p.model_id, "zerank-2");
        // model half keeps inner colons
        let p2 = parse_model_id_strict("prov:a:b:c").unwrap();
        assert_eq!(p2.model_id, "a:b:c");
    }

    #[test]
    fn parse_missing_colon_errors_with_hint() {
        let e = parse_model_id_strict("nocolon").unwrap_err();
        assert!(e.message.contains("missing a provider prefix"));
        assert!(e.fix.is_some());
    }

    #[test]
    fn parse_empty_halves_error() {
        assert!(parse_model_id_strict(":model").is_err());
        assert!(parse_model_id_strict("provider:").is_err());
        assert!(parse_model_id_strict("  :  ").is_err());
    }

    // ---- resolve_recipe_strict ----

    #[test]
    fn resolve_known_provider() {
        let (parsed, recipe) = resolve_recipe_strict("openai:gpt-5.2").unwrap();
        assert_eq!(recipe.id, "openai");
        assert_eq!(parsed.model_id, "gpt-5.2");
    }

    #[test]
    fn resolve_applies_alias_canonical() {
        let (parsed, recipe) = resolve_recipe_strict("anthropic:claude-haiku-4-5").unwrap();
        assert_eq!(recipe.id, "anthropic");
        assert_eq!(parsed.model_id, "claude-haiku-4-5-20251001");
    }

    #[test]
    fn resolve_unknown_provider_lists_known() {
        let e = resolve_recipe_strict("nope:model").unwrap_err();
        assert!(e.message.contains("Unknown provider"));
        let fix = e.fix.unwrap();
        assert!(fix.contains("openai"));
        assert!(fix.contains("anthropic"));
    }

    // ---- assert_touchpoint ----

    #[test]
    fn assert_ok_for_listed_native_model() {
        let (_, recipe) = resolve_recipe_strict("openai:gpt-5.2").unwrap();
        assert!(assert_touchpoint(recipe, TouchpointKind::Chat, "gpt-5.2", None).is_ok());
    }

    #[test]
    fn assert_missing_touchpoint_anthropic_embedding_hint() {
        let recipe = resolve_recipe("anthropic").unwrap();
        let e = assert_touchpoint(recipe, TouchpointKind::Embedding, "whatever", None).unwrap_err();
        assert!(e.message.contains("does not support touchpoint"));
        assert!(e.fix.unwrap().contains("no embedding model"));
    }

    #[test]
    fn assert_native_unlisted_model_fails_fast() {
        let recipe = resolve_recipe("openai").unwrap();
        let e = assert_touchpoint(recipe, TouchpointKind::Chat, "gpt-imaginary", None).unwrap_err();
        assert!(e.message.contains("not listed"));
    }

    #[test]
    fn assert_native_unlisted_model_ok_when_extended() {
        let recipe = resolve_recipe("openai").unwrap();
        let mut ext = std::collections::BTreeSet::new();
        ext.insert("gpt-imaginary".to_string());
        assert!(assert_touchpoint(recipe, TouchpointKind::Chat, "gpt-imaginary", Some(&ext)).is_ok());
    }

    #[test]
    fn assert_openai_compat_accepts_arbitrary_model() {
        // openrouter is openai-compat; arbitrary chat model id must pass even
        // though it is not in the curated list.
        let recipe = resolve_recipe("openrouter").unwrap();
        assert!(
            assert_touchpoint(recipe, TouchpointKind::Chat, "some/unknown-model", None).is_ok()
        );
    }

    #[test]
    fn assert_user_provided_empty_list_accepts_anything() {
        // llama-server embedding has empty models[] + user_provided_models.
        let recipe = resolve_recipe("llama-server").unwrap();
        assert!(
            assert_touchpoint(recipe, TouchpointKind::Embedding, "whatever-gguf", None).is_ok()
        );
    }

    // ---- known_provider_ids ----

    #[test]
    fn known_provider_ids_sorted_and_complete() {
        let ids = known_provider_ids();
        assert_eq!(ids.len(), 17);
        // sorted
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        assert!(ids.contains(&"openai"));
        assert!(ids.contains(&"litellm"));
    }

    #[test]
    fn error_display_joins_message_and_fix() {
        let e = AiConfigError::new("bad thing", "do this instead");
        assert_eq!(format!("{e}"), "bad thing — do this instead");
    }
}
