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

/// Returns `true` if `scope` is one of the six canonical scopes.
pub fn is_allowed_scope(scope: &str) -> bool {
    ALLOWED_SCOPES.contains(&scope)
}

/// Error returned when an unknown scope is encountered.
#[derive(Debug, Clone)]
pub struct InvalidScopeError {
    pub invalid_scope: String,
    pub all_scopes: Vec<String>,
}

impl std::fmt::Display for InvalidScopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Unknown scope \"{}\". Allowed: {}.",
            self.invalid_scope,
            ALLOWED_SCOPES.join(", ")
        )
    }
}

impl std::error::Error for InvalidScopeError {}

/// Validate that every scope in the input is allowed. Returns `Err(InvalidScopeError)`
/// on the first unknown scope. Used at OAuth client registration time.
pub fn assert_allowed_scopes(scopes: &[impl AsRef<str>]) -> Result<(), InvalidScopeError> {
    for s in scopes {
        let s = s.as_ref();
        if !is_allowed_scope(s) {
            return Err(InvalidScopeError {
                invalid_scope: s.to_string(),
                all_scopes: ALLOWED_SCOPES.iter().map(|&x| x.to_string()).collect(),
            });
        }
    }
    Ok(())
}

/// Normalize scopes input from admin SPA / OAuth wire format into a
/// deterministic, validated space-separated scope string.
///
/// Valid input shapes:
///   - `None` or missing → defaults to `"read"`
///   - `Some("read write")` (space-separated string)
///   - `Some(["read", "write"])` (string array)
///
/// Rejection cases:
///   - array element with internal whitespace (the `["read write"]` bug)
///   - array element that is empty string
///   - any element not in ALLOWED_SCOPES → InvalidScopeError
///   - empty after normalization → Error
///
/// Returns sorted, deduplicated space-separated scope string.
pub fn normalize_scopes_input(raw: Option<&serde_json::Value>) -> Result<String, String> {
    // Default: no scopes → "read"
    let raw = match raw {
        Some(v) if v.is_null() => return Ok("read".to_string()),
        Some(v) => v,
        None => return Ok("read".to_string()),
    };

    let candidates: Vec<String> = if let Some(s) = raw.as_str() {
        // String input: split on whitespace
        s.split_whitespace().map(|x| x.to_string()).collect()
    } else if let Some(arr) = raw.as_array() {
        // Array input: validate each element
        let mut out = Vec::with_capacity(arr.len());
        for el in arr {
            match el.as_str() {
                Some(s) if s.is_empty() => {
                    return Err("scopes array must not contain empty strings".to_string());
                }
                Some(s) if s.contains(char::is_whitespace) => {
                    return Err(format!(
                        "scopes array element \"{}\" contains whitespace. Each element must be a single scope name; use ['read', 'write'] not ['read write'].",
                        s
                    ));
                }
                Some(s) => out.push(s.to_string()),
                None => {
                    return Err(format!(
                        "scopes array must contain only strings, got {}",
                        if el.is_null() { "null" } else { "non-string" }
                    ));
                }
            }
        }
        out
    } else {
        return Err(format!(
            "scopes must be a string or array of strings, got {}",
            if raw.is_number() {
                "number"
            } else if raw.is_boolean() {
                "boolean"
            } else if raw.is_object() {
                "object"
            } else {
                "unknown"
            }
        ));
    };

    if candidates.is_empty() {
        return Err("scopes is empty after normalization".to_string());
    }

    // Dedupe via BTreeSet + sort for stable output
    use std::collections::BTreeSet;
    let deduped: Vec<String> = candidates
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    // Validate against ALLOWED_SCOPES
    assert_allowed_scopes(&deduped).map_err(|e| e.to_string())?;

    Ok(deduped.join(" "))
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

    // ── is_allowed_scope ──────────────────────────────────────────────────

    #[test]
    fn known_scopes_pass_is_allowed() {
        for &s in ALLOWED_SCOPES {
            assert!(is_allowed_scope(s), "expected {} to be allowed", s);
        }
    }

    #[test]
    fn unknown_scopes_fail_is_allowed() {
        assert!(!is_allowed_scope("superpower"));
        assert!(!is_allowed_scope("flying-unicorn"));
        assert!(!is_allowed_scope(""));
    }

    // ── assert_allowed_scopes ─────────────────────────────────────────────

    #[test]
    fn valid_scopes_pass_assert() {
        assert!(assert_allowed_scopes(&["read", "write"]).is_ok());
        assert!(assert_allowed_scopes(&["admin"]).is_ok());
        assert!(assert_allowed_scopes(&["agent"]).is_ok());
    }

    #[test]
    fn invalid_scopes_fail_assert() {
        let err = assert_allowed_scopes(&["read", "superpower"]).unwrap_err();
        assert_eq!(err.invalid_scope, "superpower");
        assert!(err.to_string().contains("Unknown scope"));
    }

    #[test]
    fn assert_stops_at_first_invalid() {
        // "flying-unicorn" comes first, so it should be the one reported
        let err = assert_allowed_scopes(&["flying-unicorn", "superpower", "read"]).unwrap_err();
        assert_eq!(err.invalid_scope, "flying-unicorn");
    }

    // ── normalize_scopes_input ────────────────────────────────────────────

    #[test]
    fn normalize_none_defaults_to_read() {
        let result = normalize_scopes_input(None).unwrap();
        assert_eq!(result, "read");
    }

    #[test]
    fn normalize_null_defaults_to_read() {
        let v = serde_json::Value::Null;
        let result = normalize_scopes_input(Some(&v)).unwrap();
        assert_eq!(result, "read");
    }

    #[test]
    fn normalize_string_input() {
        let v = serde_json::Value::String("read write".to_string());
        let result = normalize_scopes_input(Some(&v)).unwrap();
        assert_eq!(result, "read write");
    }

    #[test]
    fn normalize_array_input() {
        let v = serde_json::json!(["read", "write"]);
        let result = normalize_scopes_input(Some(&v)).unwrap();
        assert_eq!(result, "read write");
    }

    #[test]
    fn normalize_dedupes_and_sorts() {
        // "write read read" → "read write" (sorted, deduplicated)
        let v = serde_json::Value::String("write read read".to_string());
        let result = normalize_scopes_input(Some(&v)).unwrap();
        assert_eq!(result, "read write");
    }

    #[test]
    fn normalize_rejects_unknown_scope() {
        let v = serde_json::Value::String("read superpower".to_string());
        let err = normalize_scopes_input(Some(&v)).unwrap_err();
        assert!(err.contains("Unknown scope"), "got: {}", err);
    }

    #[test]
    fn normalize_rejects_array_with_whitespace() {
        let v = serde_json::json!(["read write"]);
        let err = normalize_scopes_input(Some(&v)).unwrap_err();
        assert!(err.contains("contains whitespace"), "got: {}", err);
    }

    #[test]
    fn normalize_rejects_empty_array() {
        let v = serde_json::json!([]);
        let err = normalize_scopes_input(Some(&v)).unwrap_err();
        assert!(err.contains("empty"), "got: {}", err);
    }

    #[test]
    fn normalize_rejects_array_with_empty_string() {
        let v = serde_json::json!([""]);
        let err = normalize_scopes_input(Some(&v)).unwrap_err();
        assert!(err.contains("empty strings"), "got: {}", err);
    }

    #[test]
    fn normalize_rejects_non_string_non_array() {
        let v = serde_json::json!(42);
        let err = normalize_scopes_input(Some(&v)).unwrap_err();
        assert!(err.contains("must be a string or array"), "got: {}", err);
    }

    #[test]
    fn normalize_handles_extra_whitespace() {
        let v = serde_json::Value::String("  read   write  ".to_string());
        let result = normalize_scopes_input(Some(&v)).unwrap();
        assert_eq!(result, "read write");
    }
}
