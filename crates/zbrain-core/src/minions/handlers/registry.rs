//! Minion handler registry — the map that resolves job names to handler
//! implementations, plus the built-in handler factory.
//!
//! ## TS reference
//!
//! The TS worker stores handlers as `Map<string, MinionHandler>` keyed by job
//! name. Subagent delegates are registered under `"subagent"` in
//! `src/core/minions/handlers/subagent.ts`.
//!
//! ## Design (grill Q6)
//!
//! [`MinionHandlerRegistry`] is a newtype around `HashMap<String, Arc<dyn
//! MinionHandler>>`. [`register_builtin_handlers`] is the factory that wires
//! every built-in handler into the map. Callers inject dependencies (chat
//! provider, etc.) once at startup; the registry map is then immutable at
//! runtime.

use std::collections::HashMap;
use std::sync::Arc;

use crate::ai::chat::ChatProvider;
use crate::engine::BrainEngine;
use crate::minions::handler::MinionHandler;
use crate::minions::handlers::backlinks::BacklinksHandler;
use crate::minions::handlers::embed::EmbedHandler;
use crate::minions::handlers::extract::ExtractHandler;
use crate::minions::handlers::import::ImportHandler;
use crate::minions::handlers::integrity::IntegrityHandler;
use crate::minions::handlers::integrity_auto::IntegrityAutoHandler;
use crate::minions::handlers::lint::LintHandler;
use crate::minions::handlers::lint_fix::LintFixHandler;
use crate::minions::handlers::orphans::OrphansHandler;
use crate::minions::handlers::purge::PurgeHandler;
use crate::minions::handlers::reindex::ReindexHandler;
use crate::minions::handlers::repair_jsonb::RepairJsonbHandler;
use crate::minions::handlers::subagent::SubagentHandler;
use crate::minions::handlers::subagent_aggregator::SubagentAggregatorHandler;
use crate::minions::handlers::sync::SyncHandler;
use crate::minions::handlers::sync_retry_failed::SyncRetryFailedHandler;
// 1-4-3
use crate::minions::handlers::autopilot_cycle::AutopilotCycleHandler;
use crate::minions::handlers::consolidate::ConsolidateHandler;
use crate::minions::handlers::extract_facts::ExtractFactsHandler;
use crate::minions::handlers::patterns::PatternsHandler;
use crate::minions::handlers::recompute_emotional_weight::RecomputeEmotionalWeightHandler;
use crate::minions::handlers::resolve_symbol_edges::ResolveSymbolEdgesHandler;
use crate::minions::handlers::synthesize::SynthesizeHandler;
// 1-4-5
use crate::minions::handlers::contextual_reindex::ContextualReindexHandler;
use crate::minions::handlers::embed_backfill::EmbedBackfillHandler;
use crate::minions::handlers::extract_conversation_facts::ExtractConversationFactsHandler;
use crate::minions::handlers::ingest_capture::IngestCaptureHandler;
use crate::minions::handlers::shell::ShellHandler;

/// A named collection of job handlers. Wraps a `HashMap` so the worker can
/// resolve a handler by job name in O(1).
///
/// Immutable after construction: handlers are registered once at startup.
/// The inner map is exposed via [`Deref`](std::ops::Deref) for ergonomic
/// `registry.get(name)` lookups.
#[derive(Clone)]
pub struct MinionHandlerRegistry {
    handlers: HashMap<String, Arc<dyn MinionHandler>>,
}

impl MinionHandlerRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register a single handler under a job name. Returns `true` if no
    /// previous handler was registered under that name; `false` if a handler
    /// was overwritten.
    pub fn register(&mut self, name: impl Into<String>, handler: Arc<dyn MinionHandler>) -> bool {
        self.handlers.insert(name.into(), handler).is_none()
    }

    /// Look up a handler by job name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn MinionHandler>> {
        self.handlers.get(name)
    }

    /// Number of registered handlers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Whether the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// Iterate over (name, handler) pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Arc<dyn MinionHandler>)> {
        self.handlers.iter()
    }
}

impl Default for MinionHandlerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Register every built-in minion handler into `registry`. Called once at
/// startup. `engine` is the primary wiring point — handlers that need
/// engine calls get access at handle time via `ctx.engine()`. `chat_provider`
/// is injected for handlers that call the LLM (subagent).
///
/// As more handlers are built (1-4-2, 1-4-5), their registration calls
/// are added here.
pub fn register_builtin_handlers(
    registry: &mut MinionHandlerRegistry,
    engine: Arc<dyn BrainEngine>,
    chat_provider: Arc<dyn ChatProvider>,
) {
    let _ = engine; // engine is available for handler construction (1-4-2 / 1-4-5)

    // Subagent handler v1 — gateway path (1-4-4).
    registry.register("subagent", Arc::new(SubagentHandler::new(Arc::clone(&chat_provider))));

    // ── 1-4-2 low-complexity handlers ─────────────────────────────────────
    registry.register("backlinks", Arc::new(BacklinksHandler));
    registry.register("embed", Arc::new(EmbedHandler));
    registry.register("extract", Arc::new(ExtractHandler));
    registry.register("import", Arc::new(ImportHandler));
    registry.register("integrity", Arc::new(IntegrityHandler));
    registry.register("integrity-auto", Arc::new(IntegrityAutoHandler));
    registry.register("lint", Arc::new(LintHandler));
    registry.register("lint-fix", Arc::new(LintFixHandler));
    registry.register("orphans", Arc::new(OrphansHandler));
    registry.register("purge", Arc::new(PurgeHandler));
    registry.register("reindex", Arc::new(ReindexHandler));
    registry.register("repair-jsonb", Arc::new(RepairJsonbHandler));
    registry.register("subagent_aggregator", Arc::new(SubagentAggregatorHandler));
    registry.register("sync", Arc::new(SyncHandler));
    registry.register("sync-retry-failed", Arc::new(SyncRetryFailedHandler));

    // ── 1-4-3 autopilot + phase handlers (smoke, runCycle pending) ────────
    registry.register("autopilot-cycle", Arc::new(AutopilotCycleHandler));
    registry.register("consolidate", Arc::new(ConsolidateHandler));
    registry.register("extract_facts", Arc::new(ExtractFactsHandler));
    registry.register("patterns", Arc::new(PatternsHandler));
    registry.register("recompute_emotional_weight", Arc::new(RecomputeEmotionalWeightHandler));
    registry.register("resolve_symbol_edges", Arc::new(ResolveSymbolEdgesHandler));
    registry.register("synthesize", Arc::new(SynthesizeHandler));

    // ── 1-4-5 medium-complexity handlers ──────────────────────────────────
    registry.register(
        "contextual_reindex_per_chunk",
        Arc::new(ContextualReindexHandler::new(Arc::clone(&chat_provider))),
    );
    registry.register("embed-backfill", Arc::new(EmbedBackfillHandler));
    registry.register("extract-conversation-facts", Arc::new(ExtractConversationFactsHandler));
    registry.register("ingest_capture", Arc::new(IngestCaptureHandler));
    registry.register("shell", Arc::new(ShellHandler));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;
    use crate::InMemoryEngine;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    fn provider() -> Arc<dyn ChatProvider> {
        Arc::new(MockChatProvider::new("test"))
    }

    #[test]
    fn registry_starts_empty() {
        let r = MinionHandlerRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn register_and_lookup_handler() {
        let mut r = MinionHandlerRegistry::new();
        register_builtin_handlers(&mut r, engine(), provider());

        assert_eq!(r.len(), 28);
        assert!(!r.is_empty());
        assert!(r.get("subagent").is_some());
        assert!(r.get("orphans").is_some());
        assert!(r.get("nonexistent").is_none());
    }

    #[test]
    fn register_overwrite_returns_false() {
        let mut r = MinionHandlerRegistry::new();
        let p1 = Arc::new(MockChatProvider::new("first"));
        let p2 = Arc::new(MockChatProvider::new("second"));

        let first = r.register("job", Arc::new(SubagentHandler::new(p1)));
        assert!(first);

        let second = r.register("job", Arc::new(SubagentHandler::new(p2)));
        assert!(!second);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn iter_yields_all_entries() {
        let mut r = MinionHandlerRegistry::new();
        register_builtin_handlers(&mut r, engine(), provider());

        let entries: Vec<_> = r.iter().collect();
        assert_eq!(entries.len(), 28);
    }
}
