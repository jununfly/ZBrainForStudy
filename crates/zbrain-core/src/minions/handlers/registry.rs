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
use crate::minions::handler::MinionHandler;
use crate::minions::handlers::subagent::SubagentHandler;

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
/// startup. The `chat_provider` is injected for handlers that need to call
/// the LLM (currently only the subagent handler).
///
/// As more handlers are built (1-4-2, 1-4-3, 1-4-5), their registration calls
/// are added here.
pub fn register_builtin_handlers(
    registry: &mut MinionHandlerRegistry,
    chat_provider: Arc<dyn ChatProvider>,
) {
    // Subagent handler v1 — gateway path (1-4-4).
    registry.register("subagent", Arc::new(SubagentHandler::new(chat_provider)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::MockChatProvider;

    #[test]
    fn registry_starts_empty() {
        let r = MinionHandlerRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }

    #[test]
    fn register_and_lookup_handler() {
        let mut r = MinionHandlerRegistry::new();
        // Use a mock chat provider so we can register the subagent handler
        let provider = Arc::new(MockChatProvider::new("test"));
        register_builtin_handlers(&mut r, provider);

        assert_eq!(r.len(), 1);
        assert!(!r.is_empty());
        assert!(r.get("subagent").is_some());
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
        let p = Arc::new(MockChatProvider::new("test"));
        register_builtin_handlers(&mut r, p);

        let entries: Vec<_> = r.iter().collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "subagent");
    }
}
