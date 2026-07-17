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
use std::path::{Path, PathBuf};
use std::process::Command;

/// Canonical GitHub repo slug — the non-spoofable signal for the bun-link and
/// bun-install authenticity checks (mirrors TS `ZBRAIN_GITHUB_REPO`).
const ZBRAIN_GITHUB_REPO: &str = "garrytan/zbrain";

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

/// Verdict from [`classify_bun_install_from`]: is a `node_modules` install the
/// canonical `garrytan/zbrain` package, or the npm-name squatter (#658)?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BunVerdict {
    Canonical,
    Suspect,
}

/// Make a path absolute without following symlinks (mirrors Node `resolve`,
/// **not** `realpath`). Relative paths are resolved against the cwd.
fn absolute_no_symlink(p: &Path) -> Option<PathBuf> {
    if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(p))
    }
}

/// Detect bun-link source-clone installs (mirrors TS `detectBunLink`, #656/#368).
///
/// Walk up from `argv1` (≤6 levels) looking for a `.git/config` whose contents
/// contain `garrytan/zbrain` (case-insensitive). Returns the repo root when
/// confident, else `None`. Uses `resolve` semantics (no symlink follow) exactly
/// like TS, because bun already resolves the symlink chain into `argv[1]`.
///
/// Parameterized on the start path so it is unit-testable against a temp
/// checkout without touching the real process argv.
fn detect_bun_link_from(argv1: &Path) -> Option<PathBuf> {
    let abs = absolute_no_symlink(argv1)?;
    let mut dir = abs.parent()?.to_path_buf();
    for _ in 0..6 {
        let git_config = dir.join(".git").join("config");
        if git_config.exists() {
            if let Ok(cfg) = std::fs::read_to_string(&git_config) {
                if cfg
                    .to_lowercase()
                    .contains(&ZBRAIN_GITHUB_REPO.to_lowercase())
                {
                    return Some(dir);
                }
            }
            // Found a `.git/config` but it is not ours (or unreadable) — stop.
            return None;
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }
    None
}

/// bun-install authenticity check (mirrors TS `classifyBunInstall`, #658).
///
/// Walk up from `realpath(argv1)` (≤6 levels) to the owning `package.json` and
/// check two non-spoofable-ish signals: `repository.url` contains
/// `garrytan/zbrain`, or the install dir ships `src/cli.ts` (source install,
/// not the squatter's compiled `dist/`). Any failure (unresolvable path,
/// unreadable/unparseable manifest, no match) yields `Suspect`.
fn classify_bun_install_from(argv1: &Path) -> BunVerdict {
    // realpathSync(argv1) — follow symlinks; failure => suspect.
    let real = match std::fs::canonicalize(argv1) {
        Ok(r) => r,
        Err(_) => return BunVerdict::Suspect,
    };
    let mut dir = match real.parent() {
        Some(d) => d.to_path_buf(),
        None => return BunVerdict::Suspect,
    };
    for _ in 0..6 {
        let pkg_path = dir.join("package.json");
        if pkg_path.exists() {
            let contents = match std::fs::read_to_string(&pkg_path) {
                Ok(c) => c,
                Err(_) => return BunVerdict::Suspect,
            };
            let pkg: serde_json::Value = match serde_json::from_str(&contents) {
                Ok(v) => v,
                Err(_) => return BunVerdict::Suspect,
            };
            // `repository` may be a bare string or an object with a `url` field.
            let repo_url = match &pkg["repository"] {
                serde_json::Value::String(s) => s.clone(),
                other => other["url"].as_str().unwrap_or("").to_string(),
            };
            if repo_url
                .to_lowercase()
                .contains(&ZBRAIN_GITHUB_REPO.to_lowercase())
            {
                return BunVerdict::Canonical;
            }
            // Source-marker fallback: our source install ships src/cli.ts.
            if dir.join("src").join("cli.ts").exists() {
                return BunVerdict::Canonical;
            }
            return BunVerdict::Suspect;
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => break,
        }
    }
    BunVerdict::Suspect
}

/// Loud recovery message for a suspected npm-name-squatter install (#658).
/// Mirrors TS `printSquatterRecovery` (writes to stderr via `console.warn`).
fn print_squatter_recovery() {
    eprintln!();
    eprintln!("  WARNING: zbrain install does not appear to be from garrytan/zbrain.");
    eprintln!("  This is likely the npm-name collision tracked in issue #658:");
    eprintln!("    https://www.npmjs.com/package/zbrain (an unrelated package).");
    eprintln!();
    eprintln!("  Recovery options:");
    eprintln!("    1. Install from source:");
    eprintln!("         bun remove -g zbrain");
    eprintln!("         git clone https://github.com/garrytan/zbrain.git");
    eprintln!("         cd zbrain && bun install && bun link");
    eprintln!();
    eprintln!("    2. Download a release binary:");
    eprintln!("         https://github.com/garrytan/zbrain/releases");
    eprintln!();
    eprintln!("  See docs/INSTALL_FOR_AGENTS.md for the canonical install paths.");
    eprintln!();
}

/// Is `clawhub` installed? Probe via `clawhub --version` (not `which`, to avoid
/// false positives), mirroring TS `execSync('clawhub --version')`: available iff
/// the command spawns and exits 0. A spawn failure or nonzero exit => not
/// available. (The TS 5s timeout is omitted — `--version` returns promptly.)
fn clawhub_available() -> bool {
    Command::new("clawhub")
        .arg("--version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Detect how this `zbrain` was installed (mirrors TS `detectInstallMethod`).
///
/// Order matters (first match wins): bun-link → node_modules(bun) → binary →
/// clawhub → unknown. In compiled Rust there is no separate entry *script*, so
/// `current_exe()` plays the role of both TS `process.execPath` and
/// `process.argv[1]` (the fs walks resolve from the executable path).
pub fn detect_install_method() -> InstallMethod {
    let exe = std::env::current_exe().ok();
    let exec_path = exe
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // 1. bun-link signal first: walk up from the exe for our `.git/config`.
    if let Some(ref p) = exe {
        if detect_bun_link_from(p).is_some() {
            return InstallMethod::BunLink;
        }
    }

    // 2. node_modules install (bun/npm). Sub-classify + warn on squatter (#658).
    if exec_path.contains("node_modules") {
        if let Some(ref p) = exe {
            if classify_bun_install_from(p) == BunVerdict::Suspect {
                print_squatter_recovery();
            }
        }
        return InstallMethod::Bun;
    }

    // 3. compiled binary.
    if exec_path.ends_with("/zbrain") || exec_path.ends_with("\\zbrain.exe") {
        return InstallMethod::Binary;
    }

    // 4. clawhub availability.
    if clawhub_available() {
        return InstallMethod::Clawhub;
    }

    InstallMethod::Unknown
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

    // --- slice 2: detect_install_method helpers ---

    use std::fs;
    use tempfile::tempdir;

    /// Create `dir/relative` (with parents) and write `contents`. Returns the
    /// full path.
    fn write_file(dir: &Path, relative: &str, contents: &str) -> PathBuf {
        let full = dir.join(relative);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, contents).unwrap();
        full
    }

    #[test]
    fn detect_bun_link_matches_our_git_config() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(
            root,
            ".git/config",
            "[remote \"origin\"]\n  url = https://github.com/garrytan/zbrain.git\n",
        );
        // argv1 is the linked entry a couple levels down.
        let argv1 = write_file(root, "bin/zbrain", "#!/bin/sh\n");
        let found = detect_bun_link_from(&argv1).expect("should detect bun-link");
        assert_eq!(
            fs::canonicalize(&found).unwrap(),
            fs::canonicalize(root).unwrap(),
            "repo root should be the checkout dir"
        );
    }

    #[test]
    fn detect_bun_link_case_insensitive() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(root, ".git/config", "url = git@github.com:GarryTan/ZBrain.git\n");
        let argv1 = write_file(root, "src/cli.ts", "// entry\n");
        assert!(detect_bun_link_from(&argv1).is_some(), "case-insensitive slug match");
    }

    #[test]
    fn detect_bun_link_foreign_git_config_is_none() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(root, ".git/config", "url = https://github.com/someone/else.git\n");
        let argv1 = write_file(root, "bin/zbrain", "x\n");
        assert!(
            detect_bun_link_from(&argv1).is_none(),
            "a .git/config not pointing at our repo stops the walk"
        );
    }

    #[test]
    fn detect_bun_link_no_git_is_none() {
        let td = tempdir().unwrap();
        let argv1 = write_file(td.path(), "a/b/zbrain", "x\n");
        assert!(detect_bun_link_from(&argv1).is_none(), "no .git within 6 levels");
    }

    #[test]
    fn classify_bun_install_canonical_via_repo_string() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(
            root,
            "package.json",
            r#"{"name":"zbrain","repository":"https://github.com/garrytan/zbrain"}"#,
        );
        let argv1 = write_file(root, "bin/zbrain.js", "x\n");
        assert_eq!(classify_bun_install_from(&argv1), BunVerdict::Canonical);
    }

    #[test]
    fn classify_bun_install_canonical_via_repo_object() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(
            root,
            "package.json",
            r#"{"repository":{"type":"git","url":"git+https://github.com/GarryTan/zbrain.git"}}"#,
        );
        let argv1 = write_file(root, "dist/cli.js", "x\n");
        assert_eq!(classify_bun_install_from(&argv1), BunVerdict::Canonical);
    }

    #[test]
    fn classify_bun_install_canonical_via_src_cli_marker() {
        let td = tempdir().unwrap();
        let root = td.path();
        // No repository field, but ships src/cli.ts => source install => canonical.
        write_file(root, "package.json", r#"{"name":"zbrain"}"#);
        write_file(root, "src/cli.ts", "// entry\n");
        let argv1 = write_file(root, "bin/zbrain", "x\n");
        assert_eq!(classify_bun_install_from(&argv1), BunVerdict::Canonical);
    }

    #[test]
    fn classify_bun_install_suspect_without_signals() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(root, "package.json", r#"{"name":"zbrain","version":"1.3.7"}"#);
        let argv1 = write_file(root, "bin/zbrain", "x\n");
        assert_eq!(
            classify_bun_install_from(&argv1),
            BunVerdict::Suspect,
            "no repo slug + no src/cli.ts => squatter suspect"
        );
    }

    #[test]
    fn classify_bun_install_suspect_on_unparseable_manifest() {
        let td = tempdir().unwrap();
        let root = td.path();
        write_file(root, "package.json", "{ this is not json");
        let argv1 = write_file(root, "bin/zbrain", "x\n");
        assert_eq!(classify_bun_install_from(&argv1), BunVerdict::Suspect);
    }

    #[test]
    fn classify_bun_install_suspect_when_no_manifest() {
        let td = tempdir().unwrap();
        let argv1 = write_file(td.path(), "a/b/zbrain", "x\n");
        assert_eq!(
            classify_bun_install_from(&argv1),
            BunVerdict::Suspect,
            "no package.json within 6 levels => suspect"
        );
    }
}
