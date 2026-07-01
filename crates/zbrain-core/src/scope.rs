//! OAuth scope hierarchy and enforcement.
//!
//! Six scopes exist: `read`, `write`, `admin`, `sources_admin`, `users_admin`, `agent`.
//! The IMPLIES table encodes which granted scope covers which required scope:
//!
//! - `admin`         → `sources_admin`, `users_admin`, `write`, `read` (NOT `agent`)
//! - `write`         → `read`
//! - everything else → itself only
//!
//! Design note (D13): `agent` is a sibling of `admin`, not a child. This prevents
//! existing admin-credential clients from silently gaining agent-dispatch capability
//! after an upgrade.

/// All valid scope strings in sorted order (for metadata / registration validation).
pub const ALLOWED_SCOPES: &[&str] = &[
    "admin",
    "agent",
    "read",
    "sources_admin",
    "users_admin",
    "write",
];

/// Returns `true` if the set of `granted_scopes` satisfies `required_scope`
/// according to the scope hierarchy.
///
/// Unknown granted scopes are silently skipped (forward-compat).
pub fn has_scope(granted_scopes: &[impl AsRef<str>], required: &str) -> bool {
    for g in granted_scopes {
        let g = g.as_ref();
        if implied_set(g).contains(&required) {
            return true;
        }
    }
    false
}

/// Returns the set of scope strings that `scope` implies (including itself).
/// Unknown scopes return an empty set (they imply nothing).
fn implied_set(scope: &str) -> &'static [&'static str] {
    match scope {
        "admin" => &["admin", "sources_admin", "users_admin", "write", "read"],
        "write" => &["write", "read"],
        "read" => &["read"],
        "sources_admin" => &["sources_admin"],
        "users_admin" => &["users_admin"],
        "agent" => &["agent"],
        _ => &[],
    }
}

/// Parse a space-separated scope string into a list of individual scopes.
/// Filters empty strings (double spaces, trailing/leading whitespace).
pub fn parse_scope_string(input: &str) -> Vec<String> {
    input
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── hasScope: admin implies everything except agent ────────────────────

    #[test]
    fn admin_covers_write() {
        assert!(has_scope(&["admin"], "write"));
    }

    #[test]
    fn admin_covers_read() {
        assert!(has_scope(&["admin"], "read"));
    }

    #[test]
    fn admin_covers_sources_admin() {
        assert!(has_scope(&["admin"], "sources_admin"));
    }

    #[test]
    fn admin_covers_users_admin() {
        assert!(has_scope(&["admin"], "users_admin"));
    }

    #[test]
    fn admin_does_not_cover_agent() {
        assert!(!has_scope(&["admin"], "agent"), "admin must NOT imply agent (D13)");
    }

    // ── write → read ───────────────────────────────────────────────────────

    #[test]
    fn write_covers_read() {
        assert!(has_scope(&["write"], "read"));
    }

    #[test]
    fn write_does_not_cover_admin() {
        assert!(!has_scope(&["write"], "admin"));
    }

    // ── exact self-match ──────────────────────────────────────────────────

    #[test]
    fn read_covers_read() {
        assert!(has_scope(&["read"], "read"));
    }

    #[test]
    fn agent_covers_agent() {
        assert!(has_scope(&["agent"], "agent"));
    }

    #[test]
    fn sources_admin_covers_sources_admin() {
        assert!(has_scope(&["sources_admin"], "sources_admin"));
    }

    // ── unknown scopes silently skipped ──────────────────────────────────

    #[test]
    fn unknown_granted_scope_is_ignored() {
        assert!(!has_scope(&["superpower"], "read"));
    }

    #[test]
    fn unknown_scope_mixed_with_valid_falls_back_to_valid() {
        assert!(has_scope(&["superpower", "write"], "read"));
    }

    // ── empty ─────────────────────────────────────────────────────────────

    #[test]
    fn empty_granted_returns_false() {
        assert!(!has_scope(&[] as &[&str], "read"));
    }

    // ── multi-scope granted set ───────────────────────────────────────────

    #[test]
    fn multiple_grants_any_match() {
        assert!(has_scope(&["read", "sources_admin"], "sources_admin"));
        assert!(has_scope(&["read", "sources_admin"], "read"));
        assert!(!has_scope(&["read", "sources_admin"], "write"));
    }
}
