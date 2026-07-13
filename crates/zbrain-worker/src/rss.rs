//! Accurate RSS measurement for Linux /proc/self/status.
//!
//! Mirrors TS `parseRssFromProcStatus` + `getAccurateRss`.  RssAnon+RssShmem
//! measures non-file-backed resident memory (heap, stack, shmem) — the metric a
//! leak watchdog wants, not VmRSS which includes reclaimable file-backed pages.
//!
//! Falls back to 0 on non-Linux, missing /proc, or kernels < 4.5.
//! (TS falls back to `process.memoryUsage().rss`; Rust has no direct equivalent
//! without a platform crate, and the watchdog is disabled by default anyway.)

/// Extract a u64 kB value from a `/proc/self/status` field line like
/// `"RssAnon:\t   1024 kB"`.  Returns `None` when the line is missing or
/// the value is not a parseable integer.
fn parse_kb_field(status: &str, field: &str) -> Option<u64> {
    let prefix = format!("{}:", field);
    let line = status
        .lines()
        .find(|l| l.trim_start().starts_with(&prefix))?;
    // Line format: "RssAnon:\t   1024 kB" — grab the number before " kB".
    let after_colon = line.split_once(':')?.1.trim();
    let digits = after_colon.split_whitespace().next()?;
    digits.parse::<u64>().ok()
}

/// Parse `/proc/self/status` text and return RssAnon + RssShmem in **bytes**.
/// Returns `None` when neither field is present or either value is not a
/// parseable integer.  Mirrors TS `parseRssFromProcStatus`.
///
/// M1 fix: field-presence check, not value-presence.  A value of 0 is
/// legitimate (RssAnon:0 + RssShmem:512 in a shmem-only worker).  Only
/// return `None` when the regex doesn't match at all or the captured
/// digits don't parse as a u64.
pub fn parse_rss_from_proc_status(status: &str) -> Option<u64> {
    let anon_kb = parse_kb_field(status, "RssAnon")?;
    let shmem_kb = parse_kb_field(status, "RssShmem")?;
    Some((anon_kb + shmem_kb) * 1024)
}

/// Read accurate RSS via an injectable reader.  The reader is called without
/// arguments and must return the `/proc/self/status` text or an error.
///
/// Returns the parsed RssAnon+RssShmem value on success, or 0 on any failure
/// (non-Linux, /proc unavailable, kernel < 4.5, or malformed status text).
///
/// The reader is injectable so tests can verify the parse path without
/// touching the filesystem.  Production callers use `std::fs::read_to_string`.
pub fn get_accurate_rss(read_status: impl FnOnce() -> std::io::Result<String>) -> u64 {
    match read_status() {
        Ok(status) => parse_rss_from_proc_status(&status).unwrap_or(0),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- behavior 1: parse valid /proc/self/status -------------------------

    #[test]
    fn parse_valid_proc_status() {
        let status = "\
VmRSS:\t  123456 kB
RssAnon:\t   1024 kB
RssShmem:\t    512 kB
VmSize:\t 999999 kB
";
        let bytes = parse_rss_from_proc_status(status).unwrap();
        assert_eq!(bytes, (1024 + 512) * 1024);
    }

    #[test]
    fn parse_shmem_only_worker() {
        // M1 regression: RssAnon: 0 + RssShmem: 512 still parses correctly.
        let status = "RssAnon:\t      0 kB\nRssShmem:\t    512 kB\n";
        let bytes = parse_rss_from_proc_status(status).unwrap();
        assert_eq!(bytes, 512 * 1024);
    }

    // --- behavior 2: missing fields → None ---------------------------------

    #[test]
    fn missing_both_fields_returns_none() {
        let status = "VmRSS:\t  123456 kB\nVmSize:\t 999999 kB\n";
        assert!(parse_rss_from_proc_status(status).is_none());
    }

    #[test]
    fn missing_one_field_returns_none() {
        // RssAnon present, RssShmem missing → None (both required).
        let status = "RssAnon:\t   1024 kB\nVmSize:\t 999999 kB\n";
        assert!(parse_rss_from_proc_status(status).is_none());
    }

    // --- behavior 3: NaN values → None -------------------------------------

    #[test]
    fn nan_value_returns_none() {
        let status = "RssAnon:\t   xyz kB\nRssShmem:\t    512 kB\n";
        assert!(parse_rss_from_proc_status(status).is_none());
    }

    // --- behavior 4: get_accurate_rss with injectable reader ----------------

    #[test]
    fn get_accurate_rss_injectable() {
        let bytes = get_accurate_rss(|| {
            Ok("RssAnon:\t   2048 kB\nRssShmem:\t   1024 kB\n".to_string())
        });
        assert_eq!(bytes, (2048 + 1024) * 1024);
    }

    #[test]
    fn get_accurate_rss_fallback_on_reader_error() {
        let bytes = get_accurate_rss(|| Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no /proc")));
        assert_eq!(bytes, 0);
    }

    #[test]
    fn get_accurate_rss_fallback_on_parse_failure() {
        let bytes = get_accurate_rss(|| Ok("garbage\n".to_string()));
        assert_eq!(bytes, 0);
    }
}
