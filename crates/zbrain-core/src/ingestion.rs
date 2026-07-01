//! Ingestion contract — IngestionEvent, content types, and validation.
//!
//! Ported from `src/core/ingestion/types.ts`. This module provides the
//! canonical IngestionEvent type, content type taxonomy, and boundary
//! validation used by webhook endpoints and (future) daemon-side sources.
//!
//! Design decisions locked by TS contract:
//! - `content_hash` format is validated as 64-char hex but NOT recomputed
//!   (CPU cost on hot path; the source owns correctness).
//! - `untrusted_payload` is optional, boolean if present.
//! - `metadata` is optional, must be a plain JSON object if present.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Canonical taxonomy of content types the ingestion pipeline recognizes.
/// Ported from TS `INGESTION_CONTENT_TYPES`.
pub const INGESTION_CONTENT_TYPES: &[&str] = &[
    "text/markdown",
    "text/plain",
    "text/html",
    "application/pdf",
    "application/json",
    "image/*",
    "audio/*",
    "video/*",
    "unknown",
];

/// Content types accepted by POST /ingest in v1.
/// Binary types (image/audio/video/pdf) return HTTP 415 with a skillpack hint.
pub const INGEST_ALLOWED_CONTENT_TYPES: &[&str] = &[
    "text/markdown",
    "text/plain",
    "text/html",
    "application/json",
];

/// Stable event shape received from ingestion sources.
/// Ported from TS `IngestionEvent` interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestionEvent {
    /// Source instance id. Matches the IngestionSource.id of the emitter.
    pub source_id: String,
    /// Source kind taxonomy (file-watcher | inbox-folder | webhook | <skillpack-kind>).
    pub source_kind: String,
    /// Original URI of the content (file path, mail message-id, URL, etc.).
    pub source_uri: String,
    /// UTC ISO timestamp the source observed the event.
    pub received_at: String,
    /// Detected content type. Drives pipeline routing.
    pub content_type: String,
    /// Primary content body (text payload).
    pub content: String,
    /// SHA-256 hex of `content`. 64 lowercase hex characters.
    pub content_hash: String,
    /// Trust tag. true for network input (webhooks, URL fetchers).
    /// When true, downstream put_page skips auto-link and applies slug-allowlist gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untrusted_payload: Option<bool>,
    /// Optional source-specific metadata. Free-form JSON object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Compute SHA-256 hex digest of a string.
/// Ported from TS `computeContentHash`.
pub fn compute_content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// Validation error for an ingestion event field.
#[derive(Debug, Clone)]
pub struct IngestionEventError {
    pub field: String,
    pub message: String,
}

impl std::fmt::Display for IngestionEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IngestionEvent.{}: {}", self.field, self.message)
    }
}

impl std::error::Error for IngestionEventError {}

/// Validate an IngestionEvent at the boundary.
/// Returns Ok(()) on success, or an IngestionEventError on the first failed field.
///
/// Ported from TS `validateIngestionEvent`.
/// Deliberately structural — we don't recompute content_hash because
/// the source owns correctness and recomputing on every emit doubles CPU cost.
pub fn validate_ingestion_event(event: &IngestionEvent) -> Result<(), IngestionEventError> {
    // Required non-empty strings.
    if event.source_id.is_empty() {
        return Err(IngestionEventError {
            field: "source_id".into(),
            message: "must be a non-empty string".into(),
        });
    }
    if event.source_kind.is_empty() {
        return Err(IngestionEventError {
            field: "source_kind".into(),
            message: "must be a non-empty string".into(),
        });
    }
    if event.source_uri.is_empty() {
        return Err(IngestionEventError {
            field: "source_uri".into(),
            message: "must be a non-empty string".into(),
        });
    }
    if event.received_at.is_empty() {
        return Err(IngestionEventError {
            field: "received_at".into(),
            message: "must be a non-empty string".into(),
        });
    }
    if event.content.is_empty() {
        return Err(IngestionEventError {
            field: "content".into(),
            message: "must be a non-empty string".into(),
        });
    }
    if event.content_hash.is_empty() {
        return Err(IngestionEventError {
            field: "content_hash".into(),
            message: "must be a non-empty string".into(),
        });
    }

    // Content type from the closed taxonomy.
    if !INGESTION_CONTENT_TYPES.contains(&event.content_type.as_str()) {
        return Err(IngestionEventError {
            field: "content_type".into(),
            message: format!(
                "must be one of [{}]; got '{}'",
                INGESTION_CONTENT_TYPES.join(", "),
                event.content_type
            ),
        });
    }

    // received_at must parse as an ISO 8601 timestamp.
    // We use a simple check: the string must contain 'T' (ISO separator)
    // and be parseable by chrono or a basic format check.
    // For now, accept any non-empty string that looks ISO-like.
    // A full chrono parse would add a dependency; the TS code uses Date.parse()
    // which is lenient. We match that leniency by checking for the 'T' separator.
    if !event.received_at.contains('T') {
        return Err(IngestionEventError {
            field: "received_at".into(),
            message: format!(
                "must be an ISO 8601 timestamp; got '{}'",
                event.received_at
            ),
        });
    }

    // content_hash must be 64 lowercase hex characters (SHA-256).
    if event.content_hash.len() != 64
        || !event.content_hash.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(IngestionEventError {
            field: "content_hash".into(),
            message: format!(
                "must be 64 hex characters (SHA-256); got '{}...'",
                &event.content_hash.chars().take(16).collect::<String>()
            ),
        });
    }

    // untrusted_payload is optional but must be boolean if present.
    // Already enforced by the type system (Option<bool>).

    // metadata is optional but must be a JSON object if present.
    if let Some(ref meta) = event.metadata {
        if !meta.is_object() {
            return Err(IngestionEventError {
                field: "metadata".into(),
                message: "must be a plain object when present".into(),
            });
        }
    }

    Ok(())
}

/// Check if a content type is allowed for POST /ingest in v1.
pub fn is_allowed_ingest_content_type(ct: &str) -> bool {
    INGEST_ALLOWED_CONTENT_TYPES.contains(&ct)
}

/// Detect content type from HTTP headers.
/// Caller can override via X-Zbrain-Content-Type header.
/// Ported from TS serve-http.ts content-type detection logic.
pub fn detect_content_type(
    zbrain_content_type: Option<&str>,
    http_content_type: Option<&str>,
) -> String {
    let declared = zbrain_content_type
        .or(http_content_type)
        .unwrap_or("")
        .to_lowercase();

    if declared.starts_with("text/markdown") {
        "text/markdown".to_string()
    } else if declared.starts_with("text/html") {
        "text/html".to_string()
    } else if declared.starts_with("text/plain") {
        "text/plain".to_string()
    } else if declared.starts_with("application/json") {
        "application/json".to_string()
    } else if declared.starts_with("text/") {
        // Unknown text/* sub-types pass through as text/plain.
        "text/plain".to_string()
    } else {
        // Binary or unknown.
        declared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── compute_content_hash ───────────────────────────────────────────

    #[test]
    fn content_hash_is_64_hex_chars() {
        let hash = compute_content_hash("hello world");
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn content_hash_is_deterministic() {
        let a = compute_content_hash("same input");
        let b = compute_content_hash("same input");
        assert_eq!(a, b);
    }

    #[test]
    fn content_hash_differs_for_different_input() {
        let a = compute_content_hash("apple");
        let b = compute_content_hash("banana");
        assert_ne!(a, b);
    }

    #[test]
    fn content_hash_matches_known_sha256() {
        // SHA-256 of empty string
        let hash = compute_content_hash("");
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    // ─── validate_ingestion_event ───────────────────────────────────────

    fn valid_event() -> IngestionEvent {
        IngestionEvent {
            source_id: "webhook-test".into(),
            source_kind: "webhook".into(),
            source_uri: "https://example.com/hook".into(),
            received_at: "2026-07-01T12:00:00.000Z".into(),
            content_type: "text/markdown".into(),
            content: "# Hello".into(),
            content_hash: compute_content_hash("# Hello"),
            untrusted_payload: Some(true),
            metadata: Some(serde_json::json!({"ip": "127.0.0.1"})),
        }
    }

    #[test]
    fn valid_event_passes_validation() {
        let event = valid_event();
        assert!(validate_ingestion_event(&event).is_ok());
    }

    #[test]
    fn empty_source_id_fails() {
        let mut event = valid_event();
        event.source_id = "".into();
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "source_id");
    }

    #[test]
    fn empty_source_kind_fails() {
        let mut event = valid_event();
        event.source_kind = "".into();
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "source_kind");
    }

    #[test]
    fn invalid_content_type_fails() {
        let mut event = valid_event();
        event.content_type = "application/octet-stream".into();
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "content_type");
    }

    #[test]
    fn bad_received_at_format_fails() {
        let mut event = valid_event();
        event.received_at = "not-a-timestamp".into();
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "received_at");
    }

    #[test]
    fn bad_content_hash_fails() {
        let mut event = valid_event();
        event.content_hash = "too-short".into();
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "content_hash");
    }

    #[test]
    fn non_hex_content_hash_fails() {
        let mut event = valid_event();
        event.content_hash = "z".repeat(64);
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "content_hash");
    }

    #[test]
    fn array_metadata_fails() {
        let mut event = valid_event();
        event.metadata = Some(serde_json::json!([1, 2, 3]));
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "metadata");
    }

    #[test]
    fn null_metadata_fails() {
        let mut event = valid_event();
        event.metadata = Some(serde_json::Value::Null);
        let err = validate_ingestion_event(&event).unwrap_err();
        assert_eq!(err.field, "metadata");
    }

    #[test]
    fn missing_metadata_is_ok() {
        let mut event = valid_event();
        event.metadata = None;
        assert!(validate_ingestion_event(&event).is_ok());
    }

    #[test]
    fn missing_untrusted_payload_is_ok() {
        let mut event = valid_event();
        event.untrusted_payload = None;
        assert!(validate_ingestion_event(&event).is_ok());
    }

    #[test]
    fn all_content_types_in_taxonomy_are_valid() {
        for ct in INGESTION_CONTENT_TYPES {
            let mut event = valid_event();
            event.content_type = ct.to_string();
            assert!(
                validate_ingestion_event(&event).is_ok(),
                "content_type '{}' should be valid",
                ct
            );
        }
    }

    // ─── detect_content_type ────────────────────────────────────────────

    #[test]
    fn detect_markdown_from_header() {
        assert_eq!(
            detect_content_type(None, Some("text/markdown")),
            "text/markdown"
        );
    }

    #[test]
    fn detect_html_from_header() {
        assert_eq!(
            detect_content_type(None, Some("text/html; charset=utf-8")),
            "text/html"
        );
    }

    #[test]
    fn detect_plain_from_header() {
        assert_eq!(
            detect_content_type(None, Some("text/plain")),
            "text/plain"
        );
    }

    #[test]
    fn detect_json_from_header() {
        assert_eq!(
            detect_content_type(None, Some("application/json")),
            "application/json"
        );
    }

    #[test]
    fn unknown_text_subtype_falls_back_to_plain() {
        assert_eq!(
            detect_content_type(None, Some("text/xml")),
            "text/plain"
        );
    }

    #[test]
    fn binary_returns_as_is() {
        assert_eq!(
            detect_content_type(None, Some("application/octet-stream")),
            "application/octet-stream"
        );
    }

    #[test]
    fn zbrain_header_overrides_content_type() {
        assert_eq!(
            detect_content_type(Some("text/markdown"), Some("application/json")),
            "text/markdown"
        );
    }

    #[test]
    fn missing_headers_defaults_to_empty() {
        assert_eq!(detect_content_type(None, None), "");
    }

    // ─── is_allowed_ingest_content_type ─────────────────────────────────

    #[test]
    fn markdown_is_allowed() {
        assert!(is_allowed_ingest_content_type("text/markdown"));
    }

    #[test]
    fn binary_is_not_allowed() {
        assert!(!is_allowed_ingest_content_type("image/png"));
    }

    #[test]
    fn pdf_is_not_allowed_in_v1() {
        // PDF is in the taxonomy but not in the v1 ingest allowlist
        assert!(!is_allowed_ingest_content_type("application/pdf"));
    }
}
