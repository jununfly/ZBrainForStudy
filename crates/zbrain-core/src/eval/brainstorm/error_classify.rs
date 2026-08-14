//! v0.39.3.0 WARN-10 + CV11 + T4 — brainstorm timeout classifier.
//!
//! Faithful port of `src/core/error-classify.ts`. The orchestrator wraps its
//! entire body in one fallible scope (covers every 57014 source — prefix
//! enumeration, hybrid search, domain-bank fetch, embedding fetch, save
//! phase). On a classifier-positive match the caller swaps in a
//! `StructuredError` with `code = "brainstorm_timeout"` and a hint covering
//! all three PG cancel sub-causes (statement timeout, lock timeout,
//! user-cancel). Non-57014 errors pass through unchanged.

use crate::error::StructuredError;
use regex::Regex;
use std::sync::LazyLock;

/// Matches the three PG cancel sub-causes regardless of driver phrasing.
static CANCEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)canceling statement due to|query.*canceled|sqlstate[\s:]+57014",
    )
    .expect("brainstorm error-classify regex must compile")
});

/// Detect Postgres SQLSTATE 57014 (query_canceled) on an arbitrary error.
///
/// Mirrors TS `isQueryCanceledError`: some drivers attach `57014` to `.code`
/// / `.sqlState`, others only surface it in the message text. We check the
/// rendered message (which Rust's `Display` for `sqlx::Error` / `libsql::Error`
/// includes the SQLSTATE) and fall back to a regex over the cancellation
/// phrasing.
#[must_use]
pub fn is_query_canceled_error(err: &(impl std::error::Error + ?Sized)) -> bool {
    let msg = err.to_string();
    if msg.contains("57014") {
        return true;
    }
    CANCEL_RE.is_match(&msg)
}

/// Convert any 57014 error into a `brainstorm_timeout` [`StructuredError`].
/// Returns `None` for non-57014 errors so the caller can rethrow them
/// unchanged (preserving OAuth / network / embedding-provider shapes).
#[must_use]
pub fn classify_brainstorm_error(
    err: &(impl std::error::Error + ?Sized),
) -> Option<StructuredError> {
    if !is_query_canceled_error(err) {
        return None;
    }
    Some(
        StructuredError::new(
            "BrainstormError",
            "brainstorm_timeout",
            "Brainstorm query was canceled by Postgres",
        )
        .with_hint(
            "Causes: statement_timeout (often PgBouncer transaction-mode), lock_timeout, or \
             user-cancel. Workarounds: try a smaller --limit, retry once, or ask your brain \
             admin about statement_timeout / PgBouncer settings. The orchestrator entry-point \
             wrap covers every internal SQL site.",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_err(msg: &str) -> crate::Error {
        crate::Error::engine(msg)
    }

    #[test]
    fn detects_57014_in_message() {
        let e = fake_err("error: canceling statement due to statement timeout (sqlstate 57014)");
        assert!(is_query_canceled_error(&e));
    }

    #[test]
    fn detects_lowercase_query_canceled() {
        let e = fake_err("db error: query was canceled by user");
        assert!(is_query_canceled_error(&e));
    }

    #[test]
    fn ignores_unrelated_errors() {
        let e = fake_err("connection refused: OAuth token invalid");
        assert!(!is_query_canceled_error(&e));
    }

    #[test]
    fn classify_returns_some_for_cancel() {
        let e = fake_err("canceling statement due to lock timeout");
        let se = classify_brainstorm_error(&e).expect("should classify");
        assert_eq!(se.code, "brainstorm_timeout");
        assert_eq!(se.class, "BrainstormError");
        assert!(se.hint.is_some());
    }

    #[test]
    fn classify_passthrough_for_unrelated() {
        let e = fake_err("embedding provider 503");
        assert!(classify_brainstorm_error(&e).is_none());
    }
}
