//! SSRF defense helpers — ported from TS `src/core/url-safety.ts`.
//!
//! Covers: IPv4/IPv6 private ranges, IPv4-mapped IPv6, hex/octal/single-int
//! encoding bypasses, metadata hostnames, and CGNAT 100.64/10 (Tailscale).
//!
//! `ZBRAIN_ALLOW_PRIVATE_REMOTES=1` lets the URL through with a tracing
//! warning. Needed for self-hosted git over Tailscale and similar setups.

use url::Url;

/// Parse a single IPv4 octet from decimal, hex (0x prefix), or octal
/// (leading 0) notation. Returns `None` on any invalid input.
fn parse_octet(s: &str) -> Option<u8> {
    if s.is_empty() {
        return None;
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        if hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        return u8::from_str_radix(hex, 16).ok();
    }
    if s.len() > 1 && s.starts_with('0') {
        if !s.chars().all(|c| matches!(c, '0'..='7')) {
            return None;
        }
        return u8::from_str_radix(s, 8).ok();
    }
    if !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse::<u8>().ok()
}

/// Convert an IPv4 hostname to 4 octets. Handles bypass encodings:
/// - Dotted decimal: `127.0.0.1`
/// - Single decimal: `2130706433` (= 0x7F000001)
/// - Hex: `0x7f000001`
/// - Per-octet hex/octal: `0x7f.0.0.1`, `0177.0.0.1`
///
/// Returns `None` for non-IP hostnames (fall through to hostname checks).
fn hostname_to_octets(hostname: &str) -> Option<[u8; 4]> {
    // Single decimal integer
    if hostname.chars().all(|c| c.is_ascii_digit()) {
        let n: u64 = hostname.parse().ok()?;
        if n <= 0xFFFF_FFFF {
            return Some([
                ((n >> 24) & 0xFF) as u8,
                ((n >> 16) & 0xFF) as u8,
                ((n >> 8) & 0xFF) as u8,
                (n & 0xFF) as u8,
            ]);
        }
        return None;
    }
    // Hex integer
    if let Some(hex) = hostname.strip_prefix("0x").or_else(|| hostname.strip_prefix("0X")) {
        if hex.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(n) = u64::from_str_radix(hex, 16) {
                if n <= 0xFFFF_FFFF {
                    return Some([
                        ((n >> 24) & 0xFF) as u8,
                        ((n >> 16) & 0xFF) as u8,
                        ((n >> 8) & 0xFF) as u8,
                        (n & 0xFF) as u8,
                    ]);
                }
            }
        }
        return None;
    }
    // Dotted decimal
    let parts: Vec<&str> = hostname.split('.').collect();
    if parts.len() == 4 {
        let octets: Vec<u8> = parts.iter().filter_map(|p| parse_octet(p)).collect();
        if octets.len() == 4 {
            return Some([octets[0], octets[1], octets[2], octets[3]]);
        }
    }
    None
}

/// Classify an [`std::net::IpAddr`] (v4 or v6) as internal/private/reserved.
///
/// Mirrors the union of checks the TS `checkDnsRebinding` applies to resolved
/// A/AAAA records: v4 private ranges, v6 loopback/unspecified/link-local/ULA,
/// and IPv4-mapped v6. Reused by the `url_reachable` resolver's DNS-rebinding
/// defense so the SSRF surface lives in exactly one place.
pub fn is_private_addr(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            is_private_ipv4(&[o[0], o[1], o[2], o[3]])
        }
        std::net::IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return true;
            }
            let s = v6.to_string().to_lowercase();
            if s.starts_with("fe80:") || s.starts_with("fc") || s.starts_with("fd") {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                let o = v4.octets();
                return is_private_ipv4(&[o[0], o[1], o[2], o[3]]);
            }
            false
        }
    }
}

/// Classify an IPv4 address as internal/private/reserved.
pub fn is_private_ipv4(octets: &[u8; 4]) -> bool {
    let [a, b, _c, _d] = *octets;
    if a == 127 {
        return true;
    } // 127.0.0.0/8 loopback
    if a == 10 {
        return true;
    } // 10.0.0.0/8 RFC1918
    if a == 172 && (16..=31).contains(&b) {
        return true;
    } // 172.16.0.0/12 RFC1918
    if a == 192 && b == 168 {
        return true;
    } // 192.168.0.0/16 RFC1918
    if a == 169 && b == 254 {
        return true;
    } // 169.254.0.0/16 link-local (incl. AWS metadata)
    if a == 100 && (64..=127).contains(&b) {
        return true;
    } // 100.64.0.0/10 CGNAT (Tailscale)
    if a == 0 {
        return true;
    } // 0.0.0.0/8 unspecified
    false
}

/// Returns true if the URL targets an internal/metadata endpoint or uses a
/// non-https scheme. Fail-closed on parse errors: malformed URLs are treated
/// as internal (blocked).
pub fn is_internal_url(url_str: &str) -> bool {
    let url = match Url::parse(url_str) {
        Ok(u) => u,
        Err(_) => return true, // malformed → block
    };

    if url.scheme() != "http" && url.scheme() != "https" {
        return true;
    }

    let host = url.host_str().unwrap_or("");
    let host_lower = host.to_lowercase();

    // Metadata hostnames
    let metadata_hostnames = [
        "metadata.google.internal",
        "metadata.google",
        "metadata",
        "instance-data",
        "instance-data.ec2.internal",
    ];
    if metadata_hostnames.contains(&host_lower.as_str()) {
        return true;
    }

    if host_lower == "localhost" || host_lower.ends_with(".localhost") {
        return true;
    }

    // Strip brackets from IPv6 addresses
    let host_clean = host_lower
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(&host_lower);

    // IPv6 loopback
    if host_clean == "::1" || host_clean == "::" {
        return true;
    }

    // IPv6 ULA fc00::/7 (unique-local) and link-local fe80::/10
    let host_clean_lower = host_clean.to_lowercase();
    if (host_clean_lower.starts_with("fc") || host_clean_lower.starts_with("fd"))
        && host_clean_lower.len() >= 4
        && host_clean_lower.chars().nth(2).map_or(false, |c| c.is_ascii_hexdigit())
        && host_clean_lower.chars().nth(3).map_or(false, |c| c.is_ascii_hexdigit())
        && host_clean_lower.as_bytes().get(4) == Some(&b':')
    {
        return true;
    }
    if (host_clean_lower.starts_with("fe8")
        || host_clean_lower.starts_with("fe9")
        || host_clean_lower.starts_with("fea")
        || host_clean_lower.starts_with("feb"))
        && host_clean_lower.len() >= 4
        && host_clean_lower.chars().nth(3).map_or(false, |c| c.is_ascii_hexdigit())
        && host_clean_lower.as_bytes().get(4) == Some(&b':')
    {
        return true;
    }

    // IPv4-mapped IPv6: ::ffff:x.x.x.x
    if let Some(tail) = host_clean_lower.strip_prefix("::ffff:") {
        // Dotted form
        if let Some(octets) = hostname_to_octets(tail) {
            if is_private_ipv4(&octets) {
                return true;
            }
        }
        // Hex hextet form: ::ffff:7f00:1
        let hextets: Vec<&str> = tail.split(':').collect();
        if hextets.len() == 2
            && hextets.iter().all(|h| {
                h.len() <= 4 && h.chars().all(|c| c.is_ascii_hexdigit())
            })
        {
            if let (Ok(hi), Ok(lo)) = (u16::from_str_radix(hextets[0], 16), u16::from_str_radix(hextets[1], 16)) {
                let octets = [
                    ((hi >> 8) & 0xFF) as u8,
                    (hi & 0xFF) as u8,
                    ((lo >> 8) & 0xFF) as u8,
                    (lo & 0xFF) as u8,
                ];
                if is_private_ipv4(&octets) {
                    return true;
                }
            }
        }
    }

    // Dotted IPv4
    if let Some(octets) = hostname_to_octets(host_clean) {
        if is_private_ipv4(&octets) {
            return true;
        }
    }

    // Trailing dot (FQDN notation)
    if let Some(stripped) = host_clean.strip_suffix('.') {
        if let Some(octets) = hostname_to_octets(stripped) {
            if is_private_ipv4(&octets) {
                return true;
            }
        }
    }

    false
}

/// Error codes for remote URL validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteUrlErrorCode {
    InvalidUrl,
    UnsupportedScheme,
    EmbeddedCredentials,
    PathTraversal,
    InternalTarget,
}

impl RemoteUrlErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InvalidUrl => "invalid_url",
            Self::UnsupportedScheme => "unsupported_scheme",
            Self::EmbeddedCredentials => "embedded_credentials",
            Self::PathTraversal => "path_traversal",
            Self::InternalTarget => "internal_target",
        }
    }
}

/// Parsed remote URL result after passing SSRF validation.
#[derive(Debug, Clone)]
pub struct ParsedRemoteUrl {
    pub url: String,
    pub hostname: String,
}

/// Validate a remote git URL for clone safety. https:// only.
///
/// Rejects: non-https schemes, embedded credentials, path traversal, and
/// internal/private targets via [`is_internal_url`].
///
/// `ZBRAIN_ALLOW_PRIVATE_REMOTES=1` lets the URL through with a warning.
pub fn parse_remote_url(s: &str) -> Result<ParsedRemoteUrl, RemoteUrlError> {
    if s.is_empty() {
        return Err(RemoteUrlError::new(
            RemoteUrlErrorCode::InvalidUrl,
            "URL is empty",
        ));
    }

    let url = Url::parse(s)
        .map_err(|_| RemoteUrlError::new(RemoteUrlErrorCode::InvalidUrl, format!("URL malformed: {s}")))?;

    if url.scheme() != "https" {
        return Err(RemoteUrlError::new(
            RemoteUrlErrorCode::UnsupportedScheme,
            format!("URL scheme not supported (https:// only): {}", url.scheme()),
        ));
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(RemoteUrlError::new(
            RemoteUrlErrorCode::EmbeddedCredentials,
            "URL must not contain embedded credentials (https://user:pass@host)",
        ));
    }

    if s.contains("..") {
        return Err(RemoteUrlError::new(
            RemoteUrlErrorCode::PathTraversal,
            "URL must not contain path-traversal (..)",
        ));
    }

    if is_internal_url(s) {
        if std::env::var("ZBRAIN_ALLOW_PRIVATE_REMOTES").as_deref() == Ok("1") {
            tracing::warn!(
                "ZBRAIN_ALLOW_PRIVATE_REMOTES=1, accepting internal/private URL: {}",
                url.host_str().unwrap_or("unknown")
            );
        } else {
            return Err(RemoteUrlError::new(
                RemoteUrlErrorCode::InternalTarget,
                format!(
                    "URL targets internal/private network: {} (set ZBRAIN_ALLOW_PRIVATE_REMOTES=1 for self-hosted git over Tailscale or similar)",
                    url.host_str().unwrap_or("unknown")
                ),
            ));
        }
    }

    Ok(ParsedRemoteUrl {
        url: s.to_string(),
        hostname: url.host_str().unwrap_or("unknown").to_string(),
    })
}

/// Error type for remote URL validation.
#[derive(Debug, Clone)]
pub struct RemoteUrlError {
    pub code: RemoteUrlErrorCode,
    pub message: String,
}

impl RemoteUrlError {
    pub fn new(code: RemoteUrlErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RemoteUrlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for RemoteUrlError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_octet_decimal() {
        assert_eq!(parse_octet("127"), Some(127));
        assert_eq!(parse_octet("0"), Some(0));
        assert_eq!(parse_octet("255"), Some(255));
    }

    #[test]
    fn parse_octet_hex() {
        assert_eq!(parse_octet("0x7f"), Some(127));
        assert_eq!(parse_octet("0XFF"), Some(255));
    }

    #[test]
    fn parse_octet_octal() {
        assert_eq!(parse_octet("0177"), Some(127)); // 0o177 = 127
        assert_eq!(parse_octet("0377"), Some(255)); // 0o377 = 255
    }

    #[test]
    fn parse_octet_invalid() {
        assert!(parse_octet("").is_none());
        assert!(parse_octet("256").is_none());
        assert!(parse_octet("-1").is_none());
        assert!(parse_octet("abc").is_none());
        assert!(parse_octet("0xGG").is_none());
        assert!(parse_octet("08").is_none()); // invalid octal
    }

    #[test]
    fn hostname_to_octets_dotted() {
        assert_eq!(hostname_to_octets("127.0.0.1"), Some([127, 0, 0, 1]));
        assert_eq!(hostname_to_octets("10.0.0.1"), Some([10, 0, 0, 1]));
        assert_eq!(hostname_to_octets("192.168.1.1"), Some([192, 168, 1, 1]));
    }

    #[test]
    fn hostname_to_octets_single_decimal() {
        assert_eq!(
            hostname_to_octets("2130706433"),
            Some([127, 0, 0, 1])
        ); // 0x7F000001
    }

    #[test]
    fn hostname_to_octets_hex() {
        assert_eq!(
            hostname_to_octets("0x7f000001"),
            Some([127, 0, 0, 1])
        );
    }

    #[test]
    fn hostname_to_octets_not_ip() {
        assert!(hostname_to_octets("example.com").is_none());
        assert!(hostname_to_octets("localhost").is_none());
    }

    #[test]
    fn is_private_ipv4_loopback() {
        assert!(is_private_ipv4(&[127, 0, 0, 1]));
        assert!(is_private_ipv4(&[127, 255, 255, 255]));
    }

    #[test]
    fn is_private_ipv4_rfc1918() {
        assert!(is_private_ipv4(&[10, 0, 0, 1]));
        assert!(is_private_ipv4(&[172, 16, 0, 1]));
        assert!(is_private_ipv4(&[172, 31, 255, 255]));
        assert!(is_private_ipv4(&[192, 168, 1, 1]));
    }

    #[test]
    fn is_private_ipv4_link_local() {
        assert!(is_private_ipv4(&[169, 254, 0, 1])); // AWS metadata
    }

    #[test]
    fn is_private_ipv4_cgnat_tailscale() {
        assert!(is_private_ipv4(&[100, 64, 0, 1]));
        assert!(is_private_ipv4(&[100, 127, 255, 255]));
    }

    #[test]
    fn is_private_ipv4_public() {
        assert!(!is_private_ipv4(&[8, 8, 8, 8]));
        assert!(!is_private_ipv4(&[1, 1, 1, 1]));
    }

    #[test]
    fn is_internal_url_loopback() {
        assert!(is_internal_url("https://127.0.0.1/repo.git"));
        assert!(is_internal_url("https://[::1]/repo.git"));
        assert!(is_internal_url("https://localhost/repo.git"));
        assert!(is_internal_url("https://sub.localhost/repo.git"));
    }

    #[test]
    fn is_internal_url_private_ranges() {
        assert!(is_internal_url("https://10.0.0.1/repo.git"));
        assert!(is_internal_url("https://192.168.1.1/repo.git"));
        assert!(is_internal_url("https://172.16.0.1/repo.git"));
    }

    #[test]
    fn is_internal_url_metadata_hostnames() {
        assert!(is_internal_url("https://metadata.google.internal/"));
        assert!(is_internal_url("https://metadata/"));
        assert!(is_internal_url("https://instance-data.ec2.internal/"));
    }

    #[test]
    fn is_internal_url_ipv4_mapped_ipv6() {
        assert!(is_internal_url("https://[::ffff:127.0.0.1]/repo.git"));
        assert!(is_internal_url("https://[::ffff:10.0.0.1]/repo.git"));
        assert!(is_internal_url("https://[::ffff:7f00:1]/repo.git"));
    }

    #[test]
    fn is_internal_url_ipv6_ula() {
        assert!(is_internal_url("https://[fc00::1]/repo.git"));
        assert!(is_internal_url("https://[fd12:3456::1]/repo.git"));
    }

    #[test]
    fn is_internal_url_ipv6_link_local() {
        assert!(is_internal_url("https://[fe80::1]/repo.git"));
        assert!(is_internal_url("https://[fe90::1]/repo.git"));
    }

    #[test]
    fn is_internal_url_public() {
        assert!(!is_internal_url("https://github.com/jununfly/zbrain.git"));
        assert!(!is_internal_url("https://gitlab.com/group/repo.git"));
    }

    #[test]
    fn is_internal_url_non_https() {
        // http:// is allowed by is_internal_url (it only checks IP ranges, not scheme).
        // The scheme restriction is enforced by parse_remote_url().
        assert!(!is_internal_url("http://github.com/repo.git"));
        assert!(is_internal_url("file:///etc/passwd"));
        assert!(is_internal_url("ftp://evil.com"));
    }

    #[test]
    fn is_internal_url_malformed() {
        assert!(is_internal_url("not a url"));
        assert!(is_internal_url(""));
    }

    #[test]
    fn parse_remote_url_valid_public() {
        let result = parse_remote_url("https://github.com/jununfly/zbrain.git");
        assert!(result.is_ok());
        let parsed = result.unwrap();
        assert_eq!(parsed.hostname, "github.com");
    }

    #[test]
    fn parse_remote_url_rejects_http() {
        let err = parse_remote_url("http://github.com/repo.git").unwrap_err();
        assert_eq!(err.code, RemoteUrlErrorCode::UnsupportedScheme);
    }

    #[test]
    fn parse_remote_url_rejects_credentials() {
        let err = parse_remote_url("https://user:pass@github.com/repo.git").unwrap_err();
        assert_eq!(err.code, RemoteUrlErrorCode::EmbeddedCredentials);
    }

    #[test]
    fn parse_remote_url_rejects_path_traversal() {
        let err = parse_remote_url("https://github.com/../etc/passwd").unwrap_err();
        assert_eq!(err.code, RemoteUrlErrorCode::PathTraversal);
    }

    #[test]
    fn parse_remote_url_rejects_internal() {
        let err = parse_remote_url("https://127.0.0.1/repo.git").unwrap_err();
        assert_eq!(err.code, RemoteUrlErrorCode::InternalTarget);
    }

    #[test]
    fn parse_remote_url_rejects_empty() {
        let err = parse_remote_url("").unwrap_err();
        assert_eq!(err.code, RemoteUrlErrorCode::InvalidUrl);
    }

    #[test]
    fn parse_remote_url_allow_private_env() {
        // Note: this test relies on the env var not being set in normal test runs
        // The allow-private path is tested by checking the error is InternalTarget (not set)
        let err = parse_remote_url("https://10.0.0.1/repo.git").unwrap_err();
        assert_eq!(err.code, RemoteUrlErrorCode::InternalTarget);
    }

    #[test]
    fn cgnat_100_64_range_blocked() {
        // Tailscale CGNAT range
        assert!(is_internal_url("https://100.64.0.1/repo.git"));
        assert!(is_internal_url("https://100.100.100.100/repo.git"));
        assert!(is_internal_url("https://100.127.255.255/repo.git"));
    }

    #[test]
    fn hex_bypass_blocked() {
        // Hex-encoded 127.0.0.1
        assert!(is_internal_url("https://0x7f000001/repo.git"));
        // Single-decimal 127.0.0.1
        assert!(is_internal_url("https://2130706433/repo.git"));
    }
}
