//! Image loader for the `search_by_image` operation (1-6-7-11).
//!
//! Supports three input formats:
//!   - `image_path`  — local filesystem path (read + base64-encode)
//!   - `image_url`   — HTTP/HTTPS URL with SSRF protection
//!   - `image_data`  — raw base64 bytes (data URI prefix stripped)
//!
//! SSRF protection: Before connecting to an image_url, we resolve the
//! hostname and reject any IP that falls inside a private / loopback /
//! link-local / unspecified range. This prevents the MCP operation from
//! being used as a proxy into internal networks.

use std::path::Path;

use crate::error::{Error, Result};

/// Image source: one of the three supported input formats.
#[derive(Debug, Clone)]
pub enum ImageSource {
    /// Local filesystem path.
    Path(String),
    /// HTTP/HTTPS URL (SSRF-protected).
    Url(String),
    /// Raw base64 bytes (data URI prefix is optional and will be stripped).
    Data(String),
}

/// Loaded image: base64-encoded bytes + MIME type.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    /// Raw base64 string (no data URI prefix).
    pub base64: String,
    /// Inferred MIME type (e.g. "image/png", "image/jpeg").
    pub mime: Option<String>,
}

/// Load an image from the given source. Returns the base64-encoded bytes
/// and an inferred MIME type.
pub async fn load_image(source: &ImageSource) -> Result<LoadedImage> {
    match source {
        ImageSource::Path(path) => load_from_path(path),
        ImageSource::Url(url) => load_from_url(url).await,
        ImageSource::Data(data) => load_from_data(data),
    }
}

// ── image_path ───────────────────────────────────────────────────────────

fn load_from_path(path: &str) -> Result<LoadedImage> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(Error::engine(format!("image file not found: {path}")));
    }
    if !p.is_file() {
        return Err(Error::engine(format!("image path is not a regular file: {path}")));
    }

    let bytes = std::fs::read(p)
        .map_err(|e| Error::engine(format!("failed to read image file {path}: {e}")))?;

    let mime = infer_mime_from_bytes(&bytes);

    Ok(LoadedImage {
        base64: base64_encode(&bytes),
        mime,
    })
}

// ── image_url ────────────────────────────────────────────────────────────

#[cfg(feature = "embedding")]
async fn load_from_url(url: &str) -> Result<LoadedImage> {
    // Parse URL early for SSRF hostname check.
    let parsed =
        url::Url::parse(url).map_err(|e| Error::engine(format!("invalid image URL: {e}")))?;

    // SSRF gate: reject non-HTTP(S) schemes before DNS resolution.
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(Error::engine(format!(
                "unsupported URL scheme for image fetch: {other} (only http/https allowed)"
            )));
        }
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| Error::engine("image URL has no host"))?;

    // DNS resolution for IP-based SSRF check.
    // We resolve synchronously with tokio's blocking pool to keep the
    // load_image signature async-compatible.
    let ips = tokio::task::spawn_blocking({
        let host = host.to_string();
        move || std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), 0))
    })
    .await
    .map_err(|e| Error::engine(format!("DNS resolution task panicked: {e}")))?
    .map_err(|e| Error::engine(format!("DNS resolution failed for {host}: {e}")))?;

    // Check every resolved address against private ranges.
    for addr in ips {
        let ip = addr.ip();
        if is_private_or_reserved(ip) {
            return Err(Error::engine(format!(
                "image URL resolves to a private/reserved IP ({ip}): SSRF blocked"
            )));
        }
    }

    // Fetch the image.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| Error::engine(format!("failed to create HTTP client: {e}")))?;

    let response = client
        .get(parsed.as_str())
        .send()
        .await
        .map_err(|e| Error::engine(format!("image URL fetch failed: {e}")))?;

    if !response.status().is_success() {
        return Err(Error::engine(format!(
            "image URL returned HTTP {}",
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::engine(format!("failed to read image response body: {e}")))?;

    let mime = infer_mime_from_bytes(&bytes);

    Ok(LoadedImage {
        base64: base64_encode(&bytes),
        mime,
    })
}

#[cfg(not(feature = "embedding"))]
async fn load_from_url(_url: &str) -> Result<LoadedImage> {
    Err(Error::engine(
        "image_url loading requires the 'embedding' feature (reqwest)",
    ))
}

// ── image_data ───────────────────────────────────────────────────────────

fn load_from_data(data: &str) -> Result<LoadedImage> {
    // Strip optional data URI prefix: "data:image/png;base64,<b64>"
    let (stripped, mime_from_prefix) = if let Some(rest) = data.strip_prefix("data:") {
        // Split at the first comma (or base64, marker).
        if let Some(comma_pos) = rest.find(',') {
            let header = &rest[..comma_pos];
            let payload = &rest[comma_pos + 1..];

            // Extract MIME from header if present (e.g. "image/png;base64").
            let mime = header
                .split(';')
                .next()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            (payload.to_string(), mime)
        } else {
            // No comma found — treat entire data:... as raw base64.
            (data.to_string(), None)
        }
    } else {
        (data.to_string(), None)
    };

    // Quick validation: base64 should decode to bytes.
    if base64_decode_len(&stripped) == 0 {
        return Err(Error::engine("image_data is empty or not valid base64"));
    }

    let mime = mime_from_prefix.or_else(|| {
        // Try to decode and infer from magic bytes.
        base64_decode(&stripped)
            .ok()
            .as_deref()
            .and_then(|bytes| infer_mime_from_bytes(bytes))
    });

    Ok(LoadedImage {
        base64: stripped,
        mime,
    })
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Infer MIME type from magic bytes. Returns None if unrecognized.
fn infer_mime_from_bytes(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[..4] {
        [0x89, b'P', b'N', b'G'] => Some("image/png".to_string()),
        [0xFF, 0xD8, 0xFF, _] => Some("image/jpeg".to_string()),
        [b'G', b'I', b'F', b'8'] => Some("image/gif".to_string()),
        [b'R', b'I', b'F', b'F'] if bytes.len() >= 12 && &bytes[8..12] == b"WEBP" => {
            Some("image/webp".to_string())
        }
        _ => None,
    }
}

/// Standard base64 encode.
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decode base64 to bytes.
fn base64_decode(b64: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| Error::engine(format!("base64 decode failed: {e}")))
}

/// Compute decoded length of a base64 string (without actually decoding).
fn base64_decode_len(b64: &str) -> usize {
    // Count valid base64 characters, estimate output byte length.
    let valid_chars = b64
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/')
        .count();
    (valid_chars * 3) / 4
}

/// Check if an IP address is in a private, loopback, link-local, or
/// unspecified range — the standard SSRF block-list.
fn is_private_or_reserved(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()           // 127.0.0.0/8
                || v4.is_private()     // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                || v4.is_link_local()  // 169.254.0.0/16
                || v4.is_unspecified() // 0.0.0.0
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()      // ::1
                || v6.is_unspecified() // ::
        }
    }
}

// ── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_from_data_strips_prefix() {
        let result = load_from_data("data:image/png;base64,iVBORw0KGgo=").unwrap();
        assert_eq!(result.base64, "iVBORw0KGgo=");
        assert_eq!(result.mime.as_deref(), Some("image/png"));
    }

    #[test]
    fn load_from_data_no_prefix() {
        let result = load_from_data("iVBORw0KGgo=").unwrap();
        assert_eq!(result.base64, "iVBORw0KGgo=");
    }

    #[test]
    fn load_from_data_empty_rejected() {
        assert!(load_from_data("").is_err());
    }

    #[test]
    fn infer_mime_png() {
        let mime = infer_mime_from_bytes(&[0x89, b'P', b'N', b'G']);
        assert_eq!(mime.as_deref(), Some("image/png"));
    }

    #[test]
    fn infer_mime_jpeg() {
        let mime = infer_mime_from_bytes(&[0xFF, 0xD8, 0xFF, 0xE0]);
        assert_eq!(mime.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn infer_mime_gif() {
        let mime = infer_mime_from_bytes(b"GIF8");
        assert_eq!(mime.as_deref(), Some("image/gif"));
    }

    #[test]
    fn infer_mime_unknown() {
        let mime = infer_mime_from_bytes(&[0x00, 0x00, 0x00, 0x00]);
        assert_eq!(mime, None);
    }

    #[test]
    fn infer_mime_too_short() {
        let mime = infer_mime_from_bytes(&[0x89]);
        assert_eq!(mime, None);
    }

    #[test]
    fn ssrf_blocks_loopback_v4() {
        let ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        assert!(is_private_or_reserved(ip));
    }

    #[test]
    fn ssrf_blocks_private_v4() {
        let ip: std::net::IpAddr = "192.168.1.1".parse().unwrap();
        assert!(is_private_or_reserved(ip));
    }

    #[test]
    fn ssrf_blocks_link_local_v4() {
        let ip: std::net::IpAddr = "169.254.1.1".parse().unwrap();
        assert!(is_private_or_reserved(ip));
    }

    #[test]
    fn ssrf_blocks_unspecified_v4() {
        let ip: std::net::IpAddr = "0.0.0.0".parse().unwrap();
        assert!(is_private_or_reserved(ip));
    }

    #[test]
    fn ssrf_blocks_loopback_v6() {
        let ip: std::net::IpAddr = "::1".parse().unwrap();
        assert!(is_private_or_reserved(ip));
    }

    #[test]
    fn ssrf_allows_public() {
        let ip: std::net::IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!is_private_or_reserved(ip));
    }
}
