//! Structured, agent-consumable error envelopes.
//!
//! Mirrors `src/core/errors.ts` from the TypeScript codebase. Agents calling
//! zbrain via CLI+JSON (`OpenClaw` and similar) need to distinguish retryable
//! from fatal, user-config from programmer errors, and get a hint to recover
//! — raw `Error.message` strings lose that signal.
//!
//! Wire shape (keys preserved verbatim from TS for cross-rewrite stability):
//!
//! ```json
//! { "class": "FileTooLarge",
//!   "code":  "file_too_large",
//!   "message": "File exceeds 10 MiB cap.",
//!   "hint":  "Pass --chunk to import in pieces.",
//!   "docs_url": "https://zbrain.dev/errors/file_too_large" }
//! ```

use std::error::Error as StdError;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Structured error envelope for agent-consumable failures.
///
/// Field order on the wire matches the TypeScript shape. `hint` and
/// `docs_url` are skipped on serialization when absent so the JSON output is
/// stable byte-for-byte against the TS implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredError {
    /// Short error class name, e.g. `"ConfirmationRequired"`, `"FileTooLarge"`.
    pub class: String,
    /// Stable machine-readable code, `snake_case`. e.g. `"cost_preview_requires_yes"`.
    pub code: String,
    /// Human-readable message. One sentence.
    pub message: String,
    /// Optional actionable hint. e.g. `"Pass --yes to proceed"`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hint: Option<String>,
    /// Optional link to docs/runbook.
    #[serde(
        rename = "docs_url",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub docs_url: Option<String>,
}

impl StructuredError {
    /// Build a structured error envelope. Mirrors `buildError(input)` in TS.
    #[must_use]
    pub fn new(
        class: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            class: class.into(),
            code: code.into(),
            message: message.into(),
            hint: None,
            docs_url: None,
        }
    }

    /// Attach an actionable hint. Returns `self` for builder-style chaining.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Attach a docs/runbook URL. Returns `self` for builder-style chaining.
    #[must_use]
    pub fn with_docs_url(mut self, docs_url: impl Into<String>) -> Self {
        self.docs_url = Some(docs_url.into());
        self
    }

    /// Convenience constructor for engine-layer faults (DB pool errors,
    /// schema migration failures, lifecycle violations). Used by
    /// `PostgresEngine` and future backends so call sites stay terse.
    ///
    /// Equivalent to `StructuredError::new("Engine", "engine", message)`.
    #[must_use]
    pub fn engine(message: impl Into<String>) -> Self {
        Self::new("Engine", "engine", message)
    }

    /// Convenience constructor for "feature requested but not yet implemented
    /// at this slice boundary" faults. Used when a trait method receives an
    /// option (e.g. `GetPageOpts.include_deleted = true`) that the current
    /// schema cannot honor — surfacing an explicit error beats silently
    /// returning a wrong shape.
    ///
    /// Equivalent to `StructuredError::new("Unsupported", "unsupported", message)`.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new("Unsupported", "unsupported", message)
    }

    /// Convenience constructor for "the targeted page does not exist (or is
    /// soft-deleted) within the given source scope". Used by mutation paths
    /// that demand a live row — e.g. `add_tag` mirrors the TS `addTag` which
    /// throws `addTag failed: page "<slug>" (source=<sid>) not found`.
    ///
    /// `source_id = None` is normalized to `"default"` because that is the
    /// exact TS semantic (`opts?.sourceId ?? 'default'`). The rendered
    /// message therefore always shows a concrete source name and stays
    /// byte-for-byte aligned with the TS error string — no `<unspecified>`
    /// or other synthetic placeholder appears.
    ///
    /// Equivalent to `StructuredError::new("PageNotFound", "page_not_found", message)`.
    #[must_use]
    pub fn page_not_found(slug: &str, source_id: Option<&str>) -> Self {
        let source = source_id.unwrap_or("default");
        Self::new(
            "PageNotFound",
            "page_not_found",
            format!("addTag failed: page \"{slug}\" (source={source}) not found"),
        )
    }
}

/// Crate-wide alias for [`StructuredError`]. Lets call sites write
/// `crate::error::Error` (or imported `Error`) without losing the structured
/// envelope contract.
pub type Error = StructuredError;

impl fmt::Display for StructuredError {
    /// Renders the same way `StructuredAgentError.message` does in TS:
    /// `"<class>: <message>"` plus `" (<hint>)"` when present.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.class, self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl StdError for StructuredError {}

/// Crate-wide alias.
pub type Result<T> = std::result::Result<T, StructuredError>;

/// Coerce an arbitrary [`StdError`] into a [`StructuredError`].
///
/// Mirrors `serializeError(value)` from the TS implementation: opaque errors
/// collapse to `class = "Error", code = "unknown"` while preserving the
/// original message.
pub fn from_std_error(err: &(dyn StdError + 'static)) -> StructuredError {
    StructuredError::new("Error", "unknown", err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_only_required_fields() {
        let e = StructuredError::new("FileTooLarge", "file_too_large", "File exceeds cap.");
        assert_eq!(e.class, "FileTooLarge");
        assert_eq!(e.code, "file_too_large");
        assert_eq!(e.message, "File exceeds cap.");
        assert!(e.hint.is_none());
        assert!(e.docs_url.is_none());
    }

    #[test]
    fn builder_with_optional_fields() {
        let e = StructuredError::new("X", "x", "msg")
            .with_hint("try --yes")
            .with_docs_url("https://docs/x");
        assert_eq!(e.hint.as_deref(), Some("try --yes"));
        assert_eq!(e.docs_url.as_deref(), Some("https://docs/x"));
    }

    #[test]
    fn display_matches_ts_format_no_hint() {
        let e = StructuredError::new("Boom", "boom", "kaboom");
        assert_eq!(format!("{e}"), "Boom: kaboom");
    }

    #[test]
    fn display_matches_ts_format_with_hint() {
        let e = StructuredError::new("Boom", "boom", "kaboom").with_hint("retry later");
        assert_eq!(format!("{e}"), "Boom: kaboom (retry later)");
    }

    #[test]
    fn json_roundtrip_minimal_omits_optionals() {
        let e = StructuredError::new("X", "x_code", "the message");
        let json = serde_json::to_string(&e).unwrap();
        // hint / docs_url must not appear when absent (matches TS buildError).
        assert!(!json.contains("hint"), "json={json}");
        assert!(!json.contains("docs_url"), "json={json}");
        let back: StructuredError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn json_roundtrip_full_preserves_all_fields() {
        let e = StructuredError::new("FileTooLarge", "file_too_large", "Too big.")
            .with_hint("--chunk")
            .with_docs_url("https://zbrain.dev/errors/file_too_large");
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"hint\":\"--chunk\""), "json={json}");
        assert!(
            json.contains("\"docs_url\":\"https://zbrain.dev/errors/file_too_large\""),
            "json={json}"
        );
        let back: StructuredError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn from_std_error_collapses_to_unknown() {
        let io = std::io::Error::other("disk on fire");
        let e = from_std_error(&io);
        assert_eq!(e.class, "Error");
        assert_eq!(e.code, "unknown");
        assert_eq!(e.message, "disk on fire");
    }

    #[test]
    fn structured_error_implements_std_error() {
        // Compile-time check: must be usable as `Box<dyn Error>`.
        let e = StructuredError::new("X", "x", "y");
        let boxed: Box<dyn StdError> = Box::new(e);
        assert!(boxed.to_string().starts_with("X: "));
    }

    #[test]
    fn page_not_found_with_source_renders_ts_message_shape() {
        let e = StructuredError::page_not_found("alpha", Some("docs"));
        assert_eq!(e.class, "PageNotFound");
        assert_eq!(e.code, "page_not_found");
        assert_eq!(
            e.message,
            "addTag failed: page \"alpha\" (source=docs) not found"
        );
    }

    #[test]
    fn page_not_found_without_source_defaults_to_default() {
        // TS: `opts?.sourceId ?? 'default'`. Rust mirrors this exactly — None
        // is normalized to the literal "default", NOT to a synthetic
        // placeholder. Keeps the error message byte-aligned with TS.
        let e = StructuredError::page_not_found("ghost", None);
        assert_eq!(e.class, "PageNotFound");
        assert_eq!(e.code, "page_not_found");
        assert_eq!(
            e.message,
            "addTag failed: page \"ghost\" (source=default) not found"
        );
    }
}
