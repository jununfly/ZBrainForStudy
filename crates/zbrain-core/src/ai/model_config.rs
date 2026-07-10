//! Unified model tier-routing — the async, config-coupled resolver layer.
//!
//! Phase 8 sub-node 1-2-1 (Part6). Ported from `src/core/model-config.ts`.
//! Companion to the pure-static [`super::resolver`] (validation) and
//! [`super::capabilities`] (capability classification) modules.
//!
//! ## What this layer does
//! One resolver replaces every hardcoded `claude-*` string. Given a
//! [`ModelTier`] plus a CLI flag, config values, and env, it walks a fixed
//! precedence chain and returns a concrete `provider:model` id, then applies
//! capability gating for the `subagent` tier.
//!
//! ## Config injection (Q0/Q1 — 1-2-1)
//! TS reads config via `engine.getConfig(key)` (async, DB `config` table). The
//! Rust `BrainEngine` trait exposes **no** config kv method, and the CLI's
//! `get_config_value` reads `zbrain.yml` (startup YAML), not the runtime DB
//! table — different semantics. Rather than grow the 90-method trait + a new
//! `config` table migration + 3 backend impls (which would also violate
//! [`super::resolver`]'s "pure-static, zero-engine-coupling" charter), this
//! layer takes an injected [`ConfigLookup`] (`key -> Option<String>`). The
//! caller (CLI / gateway / Phase 9 consumer) does the DB/YAML read at the
//! boundary and hands in a snapshot. [`resolve_model`] stays **synchronous and
//! pure** — unit-testable with a `HashMap` and no tokio runtime.
//!
//! ## Precedence (highest first) — mirrors TS `resolveModel`
//! 1. CLI flag (`--model`)
//! 2. New-key config (`opts.config_key`)
//! 3. Global default (`models.default`)
//! 4. **Tier override (`models.tier.<tier>`)** — NOTE: intentionally *below*
//!    `models.default`, not above. Faithful to TS v0.31.12; an easy footgun to
//!    "fix" the wrong way.
//! 5. Env var (`opts.env_var` or `ZBRAIN_MODEL`)
//! 6. Tier default ([`TIER_DEFAULTS`])
//! 7. Hardcoded caller fallback (`opts.fallback`)
//!
//! Steps 3/4/5 pass through [`enforce_subagent_capable`]; all results run
//! through [`resolve_model_alias`] (user `models.aliases.<name>` →
//! [`DEFAULT_ALIASES`] → pass-through, depth-2 cycle break).
//!
//! ## Legacy deliberately NOT ported (Q4 — 1-2-1)
//! Per AGENTS.md (no online users; break freely, no compat aliases), these
//! v0.38-dead TS surfaces are intentionally dropped, not migrated:
//! - `deprecatedConfigKey` chain + `emitDeprecationWarning` — served the TS
//!   old-version *upgrade* path; a fresh Rust impl has no prior version.
//! - `isAnthropicProvider` — the pre-v0.38 subagent gate, superseded by
//!   [`super::capabilities::classify_capabilities`] (dead code in TS too).
//! - `enforceSubagentAnthropic` — a `@deprecated` shim for external TS plugins;
//!   Rust has no such consumer.

use super::capabilities::{classify_capabilities, CapabilityVerdict};
use std::collections::HashSet;
use std::sync::Mutex;

/// Semantic routing tier. Distinct from [`super::types::Tier`]
/// (`native`/`openai-compat`, the recipe SDK split) — this is the
/// downstream-cost / capability grouping. Mirrors TS `ModelTier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelTier {
    /// haiku-class: classification + expansion + verdict.
    Utility,
    /// sonnet-class: default chat + synthesis + fact extraction.
    Reasoning,
    /// opus-class: expensive reasoning.
    Deep,
    /// Anthropic-shape multi-turn tool loop. Never inherits a tool-less
    /// `models.default` — falls back to `TIER_DEFAULTS.subagent` with a
    /// one-shot warn (see [`enforce_subagent_capable`]).
    Subagent,
}

impl ModelTier {
    /// Lowercase wire name, used to build the `models.tier.<tier>` config key
    /// and the warn `source` label. Mirrors the TS string-union values.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ModelTier::Utility => "utility",
            ModelTier::Reasoning => "reasoning",
            ModelTier::Deep => "deep",
            ModelTier::Subagent => "subagent",
        }
    }
}

/// Default model for each tier — the hardcoded fallback when no
/// `models.tier.<tier>` and no `models.default` is set. Mirrors TS
/// `TIER_DEFAULTS`. Subagent gets Sonnet (Anthropic Messages tool-loop shape);
/// reasoning gets Sonnet (workhorse); deep gets Opus; utility gets Haiku.
#[must_use]
pub fn tier_default(tier: ModelTier) -> &'static str {
    // Reasoning and Subagent both default to Sonnet today, but they are
    // conceptually independent tiers (Subagent asserts a tool-loop shape) and
    // may diverge — keep the arms separate rather than collapsing them.
    #[allow(clippy::match_same_arms)]
    match tier {
        ModelTier::Utility => "anthropic:claude-haiku-4-5-20251001",
        ModelTier::Reasoning => "anthropic:claude-sonnet-4-6",
        ModelTier::Deep => "anthropic:claude-opus-4-7",
        ModelTier::Subagent => "anthropic:claude-sonnet-4-6",
    }
}

/// Built-in short-name aliases. Users override via `models.aliases.<name>`
/// config. Values carry the `provider:` prefix so resolved model strings always
/// name an explicit provider (bare ids fail `resolve_recipe_strict`). Mirrors
/// TS `DEFAULT_ALIASES`.
#[must_use]
pub fn default_alias(name: &str) -> Option<&'static str> {
    match name {
        "opus" => Some("anthropic:claude-opus-4-7"),
        "sonnet" => Some("anthropic:claude-sonnet-4-6"),
        "haiku" => Some("anthropic:claude-haiku-4-5-20251001"),
        "gemini" => Some("google:gemini-3-pro"),
        "gpt" => Some("openai:gpt-5"),
        _ => None,
    }
}

/// Injected config reader. The tier-routing layer walks the precedence chain
/// against this instead of coupling to `BrainEngine` (see module docs, Q0/Q1).
///
/// Implementations return the raw config value for a dotted key
/// (`models.default`, `models.tier.subagent`, `models.aliases.opus`, …) or
/// `None` if unset. Synchronous by contract: a DB-backed impl must pre-fetch or
/// hold a connected snapshot so [`resolve_model`] never blocks on IO.
pub trait ConfigLookup {
    /// Return the configured value for `key`, or `None` if unset.
    fn get(&self, key: &str) -> Option<String>;
}

/// A [`ConfigLookup`] backed by an in-memory map — the test/no-config default,
/// and a convenient snapshot type for callers that pre-fetch config. `None`
/// entries and missing keys are equivalent.
// Impl is deliberately fixed to the default `RandomState` hasher: this is a
// convenience/snapshot type, not a hot path, so generalizing over hashers adds
// noise without benefit.
#[allow(clippy::implicit_hasher)]
impl ConfigLookup for std::collections::HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        std::collections::HashMap::get(self, key).cloned()
    }
}

/// Options controlling one tier resolution. Mirrors TS `ResolveModelOpts`
/// minus the deliberately-dropped `deprecated_config_key` (see module docs).
#[derive(Debug, Clone, Default)]
pub struct ResolveModelOpts {
    /// CLI flag value (e.g. `--model opus` → `"opus"`). Highest precedence.
    pub cli_flag: Option<String>,
    /// New-key config name (e.g. `"models.dream.synthesize"`).
    pub config_key: Option<String>,
    /// Env var consulted after the tier override. Defaults to `ZBRAIN_MODEL`.
    pub env_var: Option<String>,
    /// Tier classification. Looked up after `models.default` and before the
    /// env var; also drives capability gating and the tier default.
    pub tier: Option<ModelTier>,
    /// Hardcoded last-resort fallback.
    pub fallback: String,
}

// ---- warn-once memo (Q3: mirrors budget.rs UNPRICED_WARNINGS) --------------

/// Process-wide dedup for subagent-tier capability warnings, keyed by
/// `"<source>:<resolved>"`. Mirrors the TS `_subagentTierWarningsEmitted` Set.
static SUBAGENT_TIER_WARNINGS: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// Reset the warn-once memo. Test seam mirroring TS
/// `_resetDeprecationWarningsForTest` (subagent half).
pub fn reset_subagent_warnings_for_test() {
    if let Ok(mut guard) = SUBAGENT_TIER_WARNINGS.lock() {
        *guard = Some(HashSet::new());
    }
}

/// Register a warn-once key; returns `true` the first time `key` is seen (i.e.
/// the caller should emit the warning). Mirrors budget.rs `warn_once`.
fn warn_once(key: &str) -> bool {
    let Ok(mut guard) = SUBAGENT_TIER_WARNINGS.lock() else {
        return false;
    };
    let set = guard.get_or_insert_with(HashSet::new);
    set.insert(key.to_string())
}

// ---- alias resolution ------------------------------------------------------

/// Resolve a name (possibly an alias) to its full `provider:model` id. Order:
///   1. User-defined alias via `models.aliases.<name>` config
///   2. [`DEFAULT_ALIASES`] map
///   3. Pass-through (treat as already-full model id)
///
/// Cycles in user aliases are broken at depth 2 — if `opus` → `super-opus` →
/// `opus`, we return `super-opus` and stop. Mirrors TS `resolveAlias`.
#[must_use]
pub fn resolve_model_alias(lookup: &dyn ConfigLookup, name: &str) -> String {
    resolve_model_alias_inner(lookup, name, 0)
}

fn resolve_model_alias_inner(lookup: &dyn ConfigLookup, name: &str, depth: u8) -> String {
    if depth > 2 {
        return name.to_string(); // cycle break
    }
    // 1. User-defined alias.
    if let Some(user) = lookup.get(&format!("models.aliases.{name}")) {
        let user = user.trim();
        if !user.is_empty() && user != name {
            return resolve_model_alias_inner(lookup, user, depth + 1);
        }
    }
    // 2. Built-in alias.
    if let Some(next) = default_alias(name) {
        if next != name {
            return resolve_model_alias_inner(lookup, next, depth + 1);
        }
    }
    // 3. Pass-through.
    name.to_string()
}

// ---- resolve_model ---------------------------------------------------------

/// Resolve a model name through the 7-step precedence chain (see module docs).
///
/// Pure and synchronous: config comes from the injected `lookup`, so this never
/// touches a DB or an async runtime. Steps that read config for `models.default`
/// / `models.tier.<tier>` / env pass through [`enforce_subagent_capable`]; every
/// return runs through [`resolve_model_alias`].
#[must_use]
pub fn resolve_model(lookup: &dyn ConfigLookup, opts: &ResolveModelOpts) -> String {
    let env_var = opts.env_var.as_deref().unwrap_or("ZBRAIN_MODEL");

    // 1. CLI flag wins.
    if let Some(flag) = &opts.cli_flag {
        let flag = flag.trim();
        if !flag.is_empty() {
            return resolve_model_alias(lookup, flag);
        }
    }

    // 2. New-key config.
    if let Some(key) = &opts.config_key {
        if let Some(v) = lookup.get(key) {
            let v = v.trim();
            if !v.is_empty() {
                return resolve_model_alias(lookup, v);
            }
        }
    }

    // 3. Global default.
    if let Some(def) = lookup.get("models.default") {
        let def = def.trim();
        if !def.is_empty() {
            let resolved = resolve_model_alias(lookup, def);
            return enforce_subagent_capable(&resolved, opts.tier, "models.default");
        }
    }

    // 4. Tier override (intentionally below models.default — see module docs).
    if let Some(tier) = opts.tier {
        if let Some(v) = lookup.get(&format!("models.tier.{}", tier.as_str())) {
            let v = v.trim();
            if !v.is_empty() {
                let resolved = resolve_model_alias(lookup, v);
                let source = format!("models.tier.{}", tier.as_str());
                return enforce_subagent_capable(&resolved, opts.tier, &source);
            }
        }
    }

    // 5. Env var.
    if let Ok(env) = std::env::var(env_var) {
        let env = env.trim();
        if !env.is_empty() {
            let resolved = resolve_model_alias(lookup, env);
            let source = format!("env:{env_var}");
            return enforce_subagent_capable(&resolved, opts.tier, &source);
        }
    }

    // 6. Tier default — the tier's canonical model beats the caller fallback.
    if let Some(tier) = opts.tier {
        return resolve_model_alias(lookup, tier_default(tier));
    }

    // 7. Hardcoded caller fallback.
    resolve_model_alias(lookup, &opts.fallback)
}

// ---- enforce_subagent_capable ----------------------------------------------

/// Subagent-tier capability gate (TS v0.38 `enforceSubagentCapable`).
///
/// Only fires when `tier == Some(Subagent)`; every other tier returns `resolved`
/// unchanged. Asks "can this model run a subagent tool loop?" via
/// [`classify_capabilities`] (recipe-driven), not "is this Anthropic?":
///   - `UnusableNoTools` / `Unknown` → fall back to `TIER_DEFAULTS.subagent`,
///     warn once per `(source, resolved)` (the loop cannot dispatch tools, or
///     the provider is unrecognized — don't burn money unverified).
///   - `DegradedNoCaching` → return `resolved`; warn once about cost regression.
///   - `DegradedNoParallel` / `Ok` → return `resolved` unchanged, no warn.
///
/// `source` is the resolution-chain step that produced `resolved`
/// (`"models.default"`, `"models.tier.subagent"`, `"env:ZBRAIN_MODEL"`), so the
/// warning tells the user where to fix it.
#[must_use]
pub fn enforce_subagent_capable(
    resolved: &str,
    tier: Option<ModelTier>,
    source: &str,
) -> String {
    if tier != Some(ModelTier::Subagent) {
        return resolved.to_string();
    }

    let verdict = classify_capabilities(resolved);
    let key = format!("{source}:{resolved}");

    match verdict {
        CapabilityVerdict::UnusableNoTools | CapabilityVerdict::Unknown => {
            if warn_once(&key) {
                let reason = if verdict == CapabilityVerdict::UnusableNoTools {
                    "lacks tool-calling support"
                } else {
                    "is an unrecognized provider"
                };
                eprintln!(
                    "[models] tier.subagent resolved to \"{resolved}\" via \"{source}\", which {reason}. \
                     The subagent tool loop cannot run on this model — falling back to {}. \
                     Fix: zbrain config set models.tier.subagent <provider>:<model-with-tools>",
                    tier_default(ModelTier::Subagent)
                );
            }
            tier_default(ModelTier::Subagent).to_string()
        }
        CapabilityVerdict::DegradedNoCaching => {
            if warn_once(&key) {
                eprintln!(
                    "[models] tier.subagent resolved to \"{resolved}\" via \"{source}\" — provider does not \
                     support prompt caching. The loop will run hot (cost scales linearly with conversation \
                     length). For lower cost on long loops, set models.tier.subagent to an Anthropic model."
                );
            }
            resolved.to_string()
        }
        // DegradedNoParallel and Ok return resolved unchanged (no warn).
        CapabilityVerdict::DegradedNoParallel | CapabilityVerdict::Ok => resolved.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn opts(tier: Option<ModelTier>, fallback: &str) -> ResolveModelOpts {
        ResolveModelOpts { tier, fallback: fallback.to_string(), ..Default::default() }
    }

    // ---- ModelTier ----

    #[test]
    fn tier_as_str_and_defaults() {
        assert_eq!(ModelTier::Utility.as_str(), "utility");
        assert_eq!(ModelTier::Subagent.as_str(), "subagent");
        assert_eq!(tier_default(ModelTier::Deep), "anthropic:claude-opus-4-7");
        assert_eq!(tier_default(ModelTier::Reasoning), "anthropic:claude-sonnet-4-6");
    }

    // ---- resolve_model_alias ----

    #[test]
    fn alias_builtin() {
        let c = cfg(&[]);
        assert_eq!(resolve_model_alias(&c, "opus"), "anthropic:claude-opus-4-7");
        assert_eq!(resolve_model_alias(&c, "gpt"), "openai:gpt-5");
    }

    #[test]
    fn alias_passthrough_for_full_id() {
        let c = cfg(&[]);
        assert_eq!(resolve_model_alias(&c, "openai:gpt-5.2"), "openai:gpt-5.2");
    }

    #[test]
    fn alias_user_override_wins() {
        // User alias for `opus` overrides the built-in.
        let c = cfg(&[("models.aliases.opus", "anthropic:claude-opus-custom")]);
        assert_eq!(resolve_model_alias(&c, "opus"), "anthropic:claude-opus-custom");
    }

    #[test]
    fn alias_user_chains_to_builtin() {
        // `fast` → `haiku` (user) → built-in haiku full id.
        let c = cfg(&[("models.aliases.fast", "haiku")]);
        assert_eq!(
            resolve_model_alias(&c, "fast"),
            "anthropic:claude-haiku-4-5-20251001"
        );
    }

    #[test]
    fn alias_cycle_broken_at_depth_2() {
        // a → b → a … depth-2 break returns the last-resolved value, no hang.
        let c = cfg(&[("models.aliases.a", "b"), ("models.aliases.b", "a")]);
        let out = resolve_model_alias(&c, "a");
        assert!(out == "a" || out == "b", "cycle break returned {out}");
    }

    // ---- resolve_model precedence ----

    #[test]
    fn precedence_cli_flag_wins() {
        let c = cfg(&[("models.default", "openai:gpt-5.2")]);
        let o = ResolveModelOpts {
            cli_flag: Some("opus".to_string()),
            ..opts(Some(ModelTier::Reasoning), "anthropic:claude-sonnet-4-6")
        };
        // CLI flag `opus` resolves via alias, beats models.default.
        assert_eq!(resolve_model(&c, &o), "anthropic:claude-opus-4-7");
    }

    #[test]
    fn precedence_config_key_over_default() {
        let c = cfg(&[
            ("models.dream.synthesize", "openai:gpt-4o-mini"),
            ("models.default", "anthropic:claude-sonnet-4-6"),
        ]);
        let o = ResolveModelOpts {
            config_key: Some("models.dream.synthesize".to_string()),
            ..opts(None, "anthropic:claude-haiku-4-5-20251001")
        };
        assert_eq!(resolve_model(&c, &o), "openai:gpt-4o-mini");
    }

    #[test]
    fn precedence_global_default() {
        let c = cfg(&[("models.default", "openai:gpt-5.2")]);
        let o = opts(Some(ModelTier::Reasoning), "anthropic:claude-haiku-4-5-20251001");
        assert_eq!(resolve_model(&c, &o), "openai:gpt-5.2");
    }

    #[test]
    fn precedence_tier_override_below_default() {
        // FOOTGUN GUARD: models.tier.<tier> is BELOW models.default. With both
        // set, models.default wins (faithful to TS v0.31.12).
        let c = cfg(&[
            ("models.default", "openai:gpt-5.2"),
            ("models.tier.reasoning", "anthropic:claude-opus-4-7"),
        ]);
        let o = opts(Some(ModelTier::Reasoning), "x:y");
        assert_eq!(resolve_model(&c, &o), "openai:gpt-5.2");
    }

    #[test]
    fn precedence_tier_override_when_no_default() {
        // Without models.default, the tier override is consulted.
        let c = cfg(&[("models.tier.reasoning", "anthropic:claude-opus-4-7")]);
        let o = opts(Some(ModelTier::Reasoning), "x:y");
        assert_eq!(resolve_model(&c, &o), "anthropic:claude-opus-4-7");
    }

    #[test]
    fn precedence_tier_default_beats_fallback() {
        // No config, no env, no CLI: the tier default beats opts.fallback.
        let c = cfg(&[]);
        let o = ResolveModelOpts {
            env_var: Some("ZBRAIN_MODEL_TEST_NEVER_SET_XYZ".to_string()),
            ..opts(Some(ModelTier::Utility), "some:fallback")
        };
        assert_eq!(resolve_model(&c, &o), "anthropic:claude-haiku-4-5-20251001");
    }

    #[test]
    fn precedence_hardcoded_fallback_when_no_tier() {
        // No tier + nothing set → caller fallback (run through alias).
        let c = cfg(&[]);
        let o = ResolveModelOpts {
            env_var: Some("ZBRAIN_MODEL_TEST_NEVER_SET_XYZ".to_string()),
            ..opts(None, "openai:gpt-5.2")
        };
        assert_eq!(resolve_model(&c, &o), "openai:gpt-5.2");
    }

    #[test]
    fn precedence_env_var_over_tier_default() {
        // Env var beats the tier default (step 5 vs step 6). Use a unique var
        // name and clean up to avoid cross-test contamination.
        let var = "ZBRAIN_MODEL_TEST_ENV_PRECEDENCE";
        std::env::set_var(var, "openai:gpt-5.2");
        let c = cfg(&[]);
        let o = ResolveModelOpts {
            env_var: Some(var.to_string()),
            ..opts(Some(ModelTier::Utility), "x:y")
        };
        let out = resolve_model(&c, &o);
        std::env::remove_var(var);
        assert_eq!(out, "openai:gpt-5.2");
    }

    // ---- enforce_subagent_capable ----

    #[test]
    fn enforce_noop_for_non_subagent_tier() {
        // Reasoning tier: even a tool-less/unknown model passes through.
        assert_eq!(
            enforce_subagent_capable("nope:model", Some(ModelTier::Reasoning), "models.default"),
            "nope:model"
        );
        assert_eq!(
            enforce_subagent_capable("nope:model", None, "models.default"),
            "nope:model"
        );
    }

    #[test]
    fn enforce_ok_model_passes_through() {
        // Anthropic sonnet is fully capable → returned unchanged.
        reset_subagent_warnings_for_test();
        assert_eq!(
            enforce_subagent_capable(
                "anthropic:claude-sonnet-4-6",
                Some(ModelTier::Subagent),
                "models.tier.subagent"
            ),
            "anthropic:claude-sonnet-4-6"
        );
    }

    #[test]
    fn enforce_degraded_no_caching_returns_resolved() {
        // OpenAI has tools but no prompt cache → DegradedNoCaching: keep the
        // resolved model (just warns about cost).
        reset_subagent_warnings_for_test();
        assert_eq!(
            enforce_subagent_capable(
                "openai:gpt-5.2",
                Some(ModelTier::Subagent),
                "models.tier.subagent"
            ),
            "openai:gpt-5.2"
        );
    }

    #[test]
    fn enforce_unknown_falls_back_to_tier_default() {
        // Unrecognized provider on subagent tier → fall back to the subagent
        // tier default.
        reset_subagent_warnings_for_test();
        assert_eq!(
            enforce_subagent_capable(
                "nope:model",
                Some(ModelTier::Subagent),
                "models.default"
            ),
            "anthropic:claude-sonnet-4-6"
        );
    }

    #[test]
    fn resolve_model_subagent_bad_default_falls_back() {
        // End-to-end: models.default points at an unknown provider, but tier is
        // subagent → enforce gate rewrites it to the subagent tier default.
        reset_subagent_warnings_for_test();
        let c = cfg(&[("models.default", "nope:model")]);
        let o = opts(Some(ModelTier::Subagent), "x:y");
        assert_eq!(resolve_model(&c, &o), "anthropic:claude-sonnet-4-6");
    }
}
