//! `zbrain-core` — core engine, types and operation contracts.
//!
//! This crate is the heart of the zbrain Rust rewrite. Slice 1 only sets up
//! the scaffold; richer modules (types, error, engine, singleton, ...) land in
//! later slices per `docs/plans/20260526/04-plan.md`.

/// Static crate name. Used by callers (CLI, web, mcp) for diagnostics.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-core"
}

/// Static crate version, sourced from Cargo at compile time.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_is_zbrain_core() {
        assert_eq!(crate_name(), "zbrain-core");
    }

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty(), "CARGO_PKG_VERSION must not be empty");
    }
}
