//! Attachment validation for minions (roadmap 1-1-3-2).
//!
//! Faithful port of `src/core/minions/attachments.ts`. Decoupled from the queue
//! facade so it can be unit-tested without a DB — a pure function taking input +
//! opts and returning ok-or-error.
//!
//! The DB `UNIQUE (job_id, filename)` constraint is the authoritative duplicate
//! fence; the in-memory `existing_filenames` check just gives a faster, clearer
//! error before the round-trip.

use std::collections::HashSet;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use regex::Regex;
use sha2::{Digest, Sha256};

use super::types::{AttachmentInput, NormalizedAttachment};

/// Default attachment size cap — mirrors TS `DEFAULT_MAX_ATTACHMENT_BYTES`
/// (`src/core/minions/queue.ts` L32): 5 MiB. Overridable per-queue via
/// [`AttachmentValidationOpts::max_bytes`].
pub const DEFAULT_MAX_ATTACHMENT_BYTES: i64 = 5 * 1024 * 1024;

/// Strict base64 alphabet: only `A-Z a-z 0-9 + /` and trailing `=`. Rejects
/// whitespace and line breaks so callers normalize before sending (no silent
/// corruption). Mirrors TS `BASE64_RE`.
static BASE64_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9+/]*={0,2}$").unwrap());

/// RFC-6838-ish media type grammar. Mirrors TS `CONTENT_TYPE_RE`
/// (`src/core/minions/attachments.ts` L33).
static CONTENT_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^[A-Za-z0-9!#$&^_.+\-]+/[A-Za-z0-9!#$&^_.+\-]+(;\s*[A-Za-z0-9!#$&^_.+\-]+=[A-Za-z0-9!#$&^_.+\-"]+)*$"#,
    )
    .unwrap()
});

/// Options for [`validate_attachment`]. Mirrors TS `AttachmentValidationOpts`.
#[derive(Debug, Clone, Default)]
pub struct AttachmentValidationOpts<'a> {
    pub max_bytes: i64,
    /// Filenames already present for this job — a friendly early-out before the
    /// DB `UNIQUE` constraint would reject the INSERT.
    pub existing_filenames: Option<&'a HashSet<String>>,
}

/// Validate + decode a caller-supplied attachment. Faithful port of TS
/// `validateAttachment` (`src/core/minions/attachments.ts` L35-99): the checks
/// run in the same order and produce the same error messages.
///
/// Returns the decoded [`NormalizedAttachment`] on success, or a human-readable
/// error string on the first failing check.
pub fn validate_attachment(
    input: &AttachmentInput,
    opts: &AttachmentValidationOpts<'_>,
) -> Result<NormalizedAttachment, String> {
    if input.filename.trim().is_empty() {
        return Err("filename is required".to_string());
    }
    let filename = &input.filename;

    // Reject path traversal, separators, null bytes. Filenames are leaves only.
    if filename.contains('/')
        || filename.contains('\\')
        || filename.contains("..")
        || filename.contains('\0')
    {
        return Err(format!(
            "filename contains invalid characters: {}",
            serde_json::to_string(filename).unwrap_or_else(|_| format!("{filename:?}"))
        ));
    }

    if input.content_type.is_empty() || !CONTENT_TYPE_RE.is_match(&input.content_type) {
        return Err("content_type missing or malformed".to_string());
    }

    if input.content_base64.is_empty() {
        return Err("content_base64 is empty".to_string());
    }

    // Strict base64: only A-Z a-z 0-9 + / and trailing =. Reject whitespace and
    // line breaks so callers normalize before sending (no silent corruption).
    if !BASE64_RE.is_match(&input.content_base64) {
        return Err("content_base64 contains invalid characters".to_string());
    }

    let bytes = match BASE64_STANDARD.decode(input.content_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return Err(format!("base64 decode failed: {e}")),
    };

    if bytes.is_empty() {
        return Err("attachment content is empty after base64 decode".to_string());
    }

    let size_bytes = bytes.len() as i64;
    if size_bytes > opts.max_bytes {
        return Err(format!(
            "attachment size {} exceeds maxBytes {}",
            size_bytes, opts.max_bytes
        ));
    }

    if let Some(existing) = opts.existing_filenames {
        if existing.contains(filename) {
            return Err(format!("filename already exists for this job: {filename}"));
        }
    }

    let sha256 = {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    };

    Ok(NormalizedAttachment {
        filename: filename.clone(),
        content_type: input.content_type.clone(),
        bytes,
        size_bytes,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        BASE64_STANDARD.encode(s.as_bytes())
    }

    fn input(filename: &str, content_type: &str, content_base64: &str) -> AttachmentInput {
        AttachmentInput {
            filename: filename.to_string(),
            content_type: content_type.to_string(),
            content_base64: content_base64.to_string(),
        }
    }

    fn opts<'a>() -> AttachmentValidationOpts<'a> {
        AttachmentValidationOpts {
            max_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
            existing_filenames: None,
        }
    }

    #[test]
    fn accepts_valid_attachment_and_computes_sha256() {
        let inp = input("manifest.json", "application/json", &b64("hello world"));
        let n = validate_attachment(&inp, &opts()).expect("should validate");
        assert_eq!(n.filename, "manifest.json");
        assert_eq!(n.content_type, "application/json");
        assert_eq!(n.bytes, b"hello world");
        assert_eq!(n.size_bytes, 11);
        // sha256("hello world")
        assert_eq!(
            n.sha256,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn rejects_empty_filename() {
        let inp = input("   ", "application/json", &b64("x"));
        assert_eq!(
            validate_attachment(&inp, &opts()).unwrap_err(),
            "filename is required"
        );
    }

    #[test]
    fn rejects_path_traversal_and_separators() {
        for bad in ["../etc/passwd", "a/b", "a\\b", "..", "a\0b"] {
            let inp = input(bad, "application/json", &b64("x"));
            let err = validate_attachment(&inp, &opts()).unwrap_err();
            assert!(
                err.starts_with("filename contains invalid characters"),
                "expected invalid-char error for {bad:?}, got: {err}"
            );
        }
    }

    #[test]
    fn rejects_malformed_content_type() {
        let inp = input("f.bin", "not-a-media-type", &b64("x"));
        assert_eq!(
            validate_attachment(&inp, &opts()).unwrap_err(),
            "content_type missing or malformed"
        );
    }

    #[test]
    fn accepts_content_type_with_params() {
        let inp = input("f.txt", "text/plain; charset=utf-8", &b64("x"));
        assert!(validate_attachment(&inp, &opts()).is_ok());
    }

    #[test]
    fn rejects_empty_content() {
        let inp = input("f.bin", "application/octet-stream", "");
        assert_eq!(
            validate_attachment(&inp, &opts()).unwrap_err(),
            "content_base64 is empty"
        );
    }

    #[test]
    fn rejects_base64_with_whitespace() {
        // Valid-ish base64 but containing a newline — strict RE must reject.
        let inp = input("f.bin", "application/octet-stream", "aGVs\nbG8=");
        assert_eq!(
            validate_attachment(&inp, &opts()).unwrap_err(),
            "content_base64 contains invalid characters"
        );
    }

    #[test]
    fn rejects_excess_padding() {
        // The strict base64 RE only allows up to two trailing '='. More padding
        // is rejected at the RE gate — the `bytes.is_empty()` guard downstream is
        // a defensive backstop that is effectively unreachable for non-empty
        // input under this RE (kept for faithful parity with the TS port).
        let inp = input("f.bin", "application/octet-stream", "====");
        assert_eq!(
            validate_attachment(&inp, &opts()).unwrap_err(),
            "content_base64 contains invalid characters"
        );
    }

    #[test]
    fn rejects_oversize_content() {
        let big = b64("aaaaaaaaaa"); // 10 bytes decoded
        let inp = input("f.bin", "application/octet-stream", &big);
        let o = AttachmentValidationOpts {
            max_bytes: 5,
            existing_filenames: None,
        };
        let err = validate_attachment(&inp, &o).unwrap_err();
        assert!(err.contains("exceeds maxBytes 5"), "got: {err}");
    }

    #[test]
    fn rejects_duplicate_filename_early() {
        let mut existing = HashSet::new();
        existing.insert("dup.txt".to_string());
        let inp = input("dup.txt", "text/plain", &b64("x"));
        let o = AttachmentValidationOpts {
            max_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
            existing_filenames: Some(&existing),
        };
        assert_eq!(
            validate_attachment(&inp, &o).unwrap_err(),
            "filename already exists for this job: dup.txt"
        );
    }
}
