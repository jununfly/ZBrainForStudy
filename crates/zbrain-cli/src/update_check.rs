//! `zbrain check-update` — port of `src/commands/check-update.ts`.
//!
//! This module is built in vertical slices (see roadmap 1-6-4-12):
//! - slice 1: pure functions (`parse_semver` / `is_minor_or_major_bump` /
//!   `extract_changelog_between` / `upgrade_command_for_method`) — offline-testable.
//! - slice 2: `detect_install_method` (fs walk + clawhub probe).
//! - slice 3: network layer (reqwest) + `CheckUpdateResult` builder.
//! - slice 4: CLI wiring (`Commands::CheckUpdate`) + delete TS command.
//!
//! `VERSION` is supplied by the caller via `env!("CARGO_PKG_VERSION")`
//! (mirrors TS `import { VERSION } from '../version.ts'` → `pkg.version`).

use std::cmp::Ordering;

/// Install-method discriminant, mirrors TS `detectInstallMethod`'s return union
/// `'bun' | 'bun-link' | 'binary' | 'clawhub' | 'unknown'`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    Bun,
    BunLink,
    Binary,
    Clawhub,
    Unknown,
}

/// Parse a semver-ish string into `[major, minor, patch]`.
///
/// Mirrors TS: strip a single leading `v`, split on `.`, require ≥3 parts,
/// parse the first three as numbers, reject if any part is non-numeric
/// (including prerelease suffixes like `3-beta`).
pub fn parse_semver(v: &str) -> Option<[u32; 3]> {
    let clean = v.strip_prefix('v').unwrap_or(v);
    let parts: Vec<&str> = clean.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let mut nums = [0u32; 3];
    for (i, p) in parts.iter().take(3).enumerate() {
        nums[i] = p.parse().ok()?;
    }
    Some(nums)
}

/// True when `latest` is a minor or major bump over `current`
/// (major increased, or major equal & minor increased). Patch bumps are not.
pub fn is_minor_or_major_bump(current: &str, latest: &str) -> bool {
    let cur = match parse_semver(current) {
        Some(c) => c,
        None => return false,
    };
    let lat = match parse_semver(latest) {
        Some(l) => l,
        None => return false,
    };
    if lat[0] > cur[0] {
        return true;
    }
    if lat[0] == cur[0] && lat[1] > cur[1] {
        return true;
    }
    false
}

/// `a > b` lexicographically over `[major, minor, patch]`.
fn semver_gt(a: [u32; 3], b: [u32; 3]) -> bool {
    matches!(a.cmp(&b), Ordering::Greater)
}

/// `a <= b` — the inverse of [`semver_gt`].
fn semver_lte(a: [u32; 3], b: [u32; 3]) -> bool {
    !semver_gt(a, b)
}

/// Map an install method to the command a user should run to upgrade.
///
/// Mirrors TS `upgradeCommandForMethod` exactly (note: `bun-link` and
/// `unknown` fall through to the default `zbrain upgrade`).
pub fn upgrade_command_for_method(method: InstallMethod) -> String {
    match method {
        InstallMethod::Bun => "bun update zbrain".to_string(),
        InstallMethod::Clawhub => "clawhub update zbrain".to_string(),
        InstallMethod::Binary => {
            "Download from https://github.com/garrytan/zbrain/releases".to_string()
        }
        InstallMethod::BunLink | InstallMethod::Unknown => "zbrain upgrade".to_string(),
    }
}

/// Extract the CHANGELOG section between `from` (exclusive) and the newest
/// entry newer than `from` (inclusive), stopping at the first header whose
/// version is `<= from`.
///
/// The `to` argument is accepted for API parity with TS but is **unused**:
/// capture always runs from the first version newer than `from` down to (and
/// stopping at) the version `<= from`, exactly as the TS implementation does.
pub fn extract_changelog_between(changelog: &str, from: &str, to: &str) -> String {
    let _ = to; // API-parity only; TS ignores it too.
    let from_parsed = match parse_semver(from) {
        Some(f) => f,
        None => return String::new(),
    };

    // Matches `## [1.2.3]` or `## [1.2.3.4]` (4th component optional).
    let re = regex::Regex::new(r"^## \[(\d+\.\d+\.\d+(?:\.\d+)?)\]").expect("valid changelog regex");

    let mut entries: Vec<&str> = Vec::new();
    let mut capturing = false;

    for line in changelog.split('\n') {
        if let Some(caps) = re.captures(line) {
            let ver = caps.get(1).expect("capture group 1").as_str();
            let ver_parsed = match parse_semver(ver) {
                Some(v) => v,
                None => {
                    // Unparseable version header while capturing — keep the line,
                    // then continue (don't change capture state).
                    if capturing {
                        entries.push(line);
                    }
                    continue;
                }
            };
            if !capturing {
                if semver_gt(ver_parsed, from_parsed) {
                    capturing = true;
                    entries.push(line);
                }
            } else if semver_lte(ver_parsed, from_parsed) {
                // Hit the current version (or older) — stop capturing.
                break;
            } else {
                entries.push(line);
            }
        } else if capturing {
            entries.push(line);
        }
    }

    entries.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_ok() {
        assert_eq!(parse_semver("1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_semver("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(parse_semver("10.20.30"), Some([10, 20, 30]));
        // Extra components are truncated to the first three (matches TS slice(0,3)).
        assert_eq!(parse_semver("1.2.3.4"), Some([1, 2, 3]));
        assert_eq!(parse_semver("1.2.3.4.5"), Some([1, 2, 3]));
    }

    #[test]
    fn parse_semver_none() {
        assert_eq!(parse_semver("1.2"), None, "fewer than 3 parts");
        assert_eq!(parse_semver("1.2.x"), None, "non-numeric part");
        assert_eq!(parse_semver("1.2.3-beta"), None, "prerelease suffix not numeric");
        assert_eq!(parse_semver(""), None);
        assert_eq!(parse_semver("v"), None);
    }

    #[test]
    fn is_minor_or_major_bump_flags() {
        assert!(!is_minor_or_major_bump("1.2.3", "1.2.9"), "patch only");
        assert!(is_minor_or_major_bump("1.2.3", "1.3.0"), "minor bump");
        assert!(is_minor_or_major_bump("1.2.3", "2.0.0"), "major bump");
        assert!(!is_minor_or_major_bump("1.2.3", "1.2.3"), "equal");
        assert!(!is_minor_or_major_bump("1.2.3", "0.9.0"), "downgrade");
        assert!(!is_minor_or_major_bump("1.2.x", "1.3.0"), "unparseable current");
        assert!(!is_minor_or_major_bump("1.2.3", "bogus"), "unparseable latest");
    }

    #[test]
    fn upgrade_command_for_method_maps() {
        assert_eq!(upgrade_command_for_method(InstallMethod::Bun), "bun update zbrain");
        assert_eq!(upgrade_command_for_method(InstallMethod::Clawhub), "clawhub update zbrain");
        assert_eq!(
            upgrade_command_for_method(InstallMethod::Binary),
            "Download from https://github.com/garrytan/zbrain/releases"
        );
        // bun-link and unknown fall through to the default.
        assert_eq!(upgrade_command_for_method(InstallMethod::BunLink), "zbrain upgrade");
        assert_eq!(upgrade_command_for_method(InstallMethod::Unknown), "zbrain upgrade");
    }

    #[test]
    fn extract_changelog_between_captures_new_range() {
        let changelog = "\
## [1.5.0]
- Added X.

## [1.4.0]
- Fixed Y.

## [1.3.0]
- Changed Z.

## [1.2.0]
- Initial.
";
        // from 1.3.0 → capture 1.5.0 + 1.4.0, stop at 1.3.0.
        let out = extract_changelog_between(changelog, "1.3.0", "1.5.0");
        assert!(out.contains("## [1.5.0]"), "missing 1.5.0 header: {out}");
        assert!(out.contains("- Added X."), "missing 1.5.0 body: {out}");
        assert!(out.contains("## [1.4.0]"), "missing 1.4.0 header: {out}");
        assert!(out.contains("- Fixed Y."), "missing 1.4.0 body: {out}");
        assert!(!out.contains("## [1.3.0]"), "should stop before 1.3.0: {out}");
        assert!(!out.contains("## [1.2.0]"), "should not include 1.2.0: {out}");
    }

    #[test]
    fn extract_changelog_between_unparseable_from_is_empty() {
        let changelog = "## [1.5.0]\n- x\n";
        assert_eq!(extract_changelog_between(changelog, "not-a-version", "1.5.0"), "");
    }

    #[test]
    fn extract_changelog_between_stops_at_equal_version() {
        let changelog = "\
## [1.2.0]
- Current.

## [1.1.0]
- Older.
";
        // from 1.2.0 → 1.2.0 is not > 1.2.0, so nothing is captured.
        let out = extract_changelog_between(changelog, "1.2.0", "1.2.0");
        assert_eq!(out, "", "nothing newer than current: {out}");
    }

    #[test]
    fn extract_changelog_between_keeps_unparseable_header_while_capturing() {
        let changelog = "\
## [1.5.0]
- Good.

## [weird]
- Odd header, kept while capturing.

## [1.2.0]
- Stop.
";
        let out = extract_changelog_between(changelog, "1.2.0", "1.5.0");
        assert!(out.contains("## [1.5.0]"));
        assert!(out.contains("## [weird]"), "unparseable header kept while capturing: {out}");
        assert!(!out.contains("## [1.2.0]"), "stop before 1.2.0: {out}");
    }
}
