//! `zbrain check-update` — port of `src/commands/check-update.ts`.
//!
//! This module is built in vertical slices (see roadmap 1-6-4-12):
//! - slice 1: pure functions (`parse_semver` / `is_minor_or_major_bump` /
//!   `extract_changelog_between` / `upgrade_command_for_method`) — offline-testable. [DONE]
//! - slice 2: `detect_install_method` (fs walk + clawhub probe). [DONE]
//! - slice 3: network layer (reqwest) + `CheckUpdateResult` builder. [DONE]
//! - slice 4: CLI wiring (`Commands::CheckUpdate` + `run_check_update`) + delete TS command. [DONE]
//!
//! `VERSION` is supplied by the caller via `env!("CARGO_PKG_VERSION")`
//! (mirrors TS `import { VERSION } from '../version.ts'` → `pkg.version`).

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

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

/// Network-fetched release metadata from the GitHub releases API.
/// Mirrors the TS `fetchLatestRelease` return shape `{ tag, published_at, url }`.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub tag: String,
    pub published_at: String,
    pub url: String,
}

/// Result struct for `zbrain check-update --json`.
///
/// Mirrors TS `CheckUpdateResult` exactly (snake_case wire fields).
/// `error` is `Option<String>` and skipped from serialization when absent,
/// matching the TS `error?: string` optional field (only present on the
/// no-release branch).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CheckUpdateResult {
    pub current_version: String,
    pub current_source: String,
    pub latest_version: String,
    pub update_available: bool,
    pub upgrade_command: String,
    pub release_url: String,
    pub changelog_diff: String,
    pub published_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Build the reqwest client used for update checks.
///
/// Mirrors TS `AbortSignal.timeout(10_000)` via a 10s overall request timeout.
pub fn build_update_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("reqwest client should build")
}

/// Fetch the latest GitHub release via the public REST API.
///
/// Mirrors TS `fetchLatestRelease`: sets a `zbrain/{version}` user-agent on
/// this call (the changelog call omits it, matching TS), and **fails
/// silently** — any HTTP/transport/parse error yields `None` (TS returns
/// `null` on its `catch`).
pub async fn fetch_latest_release(client: &reqwest::Client, version: &str) -> Option<ReleaseInfo> {
    let res = client
        .get("https://api.github.com/repos/garrytan/zbrain/releases/latest")
        .header(reqwest::header::USER_AGENT, format!("zbrain/{version}"))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let data: serde_json::Value = res.json().await.ok()?;
    Some(ReleaseInfo {
        tag: data
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        published_at: data
            .get("published_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        url: data
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Fetch the raw `CHANGELOG.md` from the repo's `master` branch.
///
/// Mirrors TS `fetchChangelog`: on any failure returns an empty string. The
/// caller only invokes this when an update is available, so an empty result
/// simply yields no diff section.
pub async fn fetch_changelog(client: &reqwest::Client) -> String {
    let res = match client
        .get("https://raw.githubusercontent.com/garrytan/zbrain/master/CHANGELOG.md")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return String::new(),
    };
    if !res.status().is_success() {
        return String::new();
    }
    res.text().await.unwrap_or_default()
}

/// Pure builder — assembles a [`CheckUpdateResult`] from gathered inputs.
///
/// Mirrors the construction logic in TS `runCheckUpdate`: when `release` is
/// `None`, emit the `error: "no_releases"` shape; otherwise fill in the
/// version/url/changelog fields, stripping a leading `v` from the tag.
///
/// `changelog` is copied verbatim — the caller is responsible for only
/// fetching it when an update is available (mirroring TS's
/// `if (updateAvailable) changelogDiff = await fetchChangelog(...)`), so a
/// non-bump naturally passes an empty string here.
pub fn build_check_update_result(
    current_version: &str,
    upgrade_command: &str,
    release: Option<&ReleaseInfo>,
    changelog: &str,
) -> CheckUpdateResult {
    match release {
        None => CheckUpdateResult {
            current_version: current_version.to_string(),
            current_source: "package-json".to_string(),
            latest_version: String::new(),
            update_available: false,
            upgrade_command: upgrade_command.to_string(),
            release_url: String::new(),
            changelog_diff: String::new(),
            published_at: String::new(),
            error: Some("no_releases".to_string()),
        },
        Some(r) => {
            let latest_version = r.tag.strip_prefix('v').unwrap_or(&r.tag).to_string();
            let update_available = is_minor_or_major_bump(current_version, &latest_version);
            CheckUpdateResult {
                current_version: current_version.to_string(),
                current_source: "package-json".to_string(),
                latest_version,
                update_available,
                upgrade_command: upgrade_command.to_string(),
                release_url: r.url.clone(),
                changelog_diff: changelog.to_string(),
                published_at: r.published_at.clone(),
                error: None,
            }
        }
    }
}

/// Execute `zbrain check-update [--json]`.
///
/// Mirrors TS `runCheckUpdate` end-to-end: detect the install method, fetch
/// the latest GitHub release, and — only when a minor/major bump is detected —
/// fetch the changelog diff. Renders JSON (`--json`) or a human summary, and
/// fails silently on network errors (the `error: "no_releases"` shape).
pub async fn run_check_update(json: bool) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let method = detect_install_method();
    let upgrade_cmd = upgrade_command_for_method(method);

    let client = build_update_client();
    let release = fetch_latest_release(&client, version).await;

    if release.is_none() {
        let result = build_check_update_result(version, &upgrade_cmd, None, "");
        if json {
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        } else {
            println!(
                "ZBrain {version} — could not check for updates (no releases found or network unavailable)."
            );
        }
        return Ok(());
    }

    // Safe: confirmed `Some` above.
    let release = release.unwrap();
    let latest_version = release
        .tag
        .strip_prefix('v')
        .unwrap_or(&release.tag)
        .to_string();
    let update_available = is_minor_or_major_bump(version, &latest_version);

    // TS only fetches the changelog when an update is available.
    let changelog = if update_available {
        fetch_changelog(&client).await
    } else {
        String::new()
    };

    let result = build_check_update_result(version, &upgrade_cmd, Some(&release), &changelog);

    if json {
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    } else if update_available {
        println!("ZBrain update available: {version} → {latest_version}");
        println!("Run: {upgrade_cmd}");
        println!("Release: {}", result.release_url);
    } else {
        println!("ZBrain {version} is up to date.");
    }

    Ok(())
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

    // --- slice 3: network layer + CheckUpdateResult builder ---

    fn release(tag: &str) -> ReleaseInfo {
        ReleaseInfo {
            tag: tag.to_string(),
            published_at: "2026-01-02T03:04:05Z".to_string(),
            url: format!("https://github.com/garrytan/zbrain/releases/tag/{tag}"),
        }
    }

    #[test]
    fn build_result_no_release_emits_error_shape() {
        let r = build_check_update_result("1.2.3", "zbrain upgrade", None, "");
        assert_eq!(r.current_version, "1.2.3");
        assert_eq!(r.current_source, "package-json");
        assert!(!r.update_available);
        assert_eq!(r.latest_version, "");
        assert_eq!(r.release_url, "");
        assert_eq!(r.changelog_diff, "");
        assert_eq!(r.published_at, "");
        assert_eq!(r.error, Some("no_releases".to_string()));
    }

    #[test]
    fn build_result_with_release_minor_bump() {
        let rel = release("v1.3.0");
        let changelog = "## [1.3.0]\n- New.\n";
        let r = build_check_update_result("1.2.3", "bun update zbrain", Some(&rel), changelog);
        assert!(r.update_available);
        assert_eq!(r.latest_version, "1.3.0");
        assert_eq!(r.release_url, rel.url);
        assert_eq!(r.published_at, rel.published_at);
        assert_eq!(r.changelog_diff, changelog);
        assert_eq!(r.upgrade_command, "bun update zbrain");
        assert_eq!(r.error, None);
    }

    #[test]
    fn build_result_patch_bump_not_available() {
        let rel = release("1.2.9");
        // TS only fetches the changelog when update_available, so the caller
        // passes an empty string on a patch-only bump.
        let r = build_check_update_result("1.2.3", "zbrain upgrade", Some(&rel), "");
        assert!(!r.update_available);
        assert_eq!(r.latest_version, "1.2.9");
        assert_eq!(r.changelog_diff, "", "no changelog on non-bump");
        assert_eq!(r.error, None);
    }

    #[test]
    fn build_result_strips_v_prefix() {
        let rel = release("v2.0.0");
        let r = build_check_update_result("1.2.3", "zbrain upgrade", Some(&rel), "");
        assert!(r.update_available);
        assert_eq!(r.latest_version, "2.0.0", "leading v stripped");
    }

    #[test]
    fn result_json_omits_error_when_success() {
        let rel = release("v1.3.0");
        let r = build_check_update_result("1.2.3", "zbrain upgrade", Some(&rel), "diff");
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("error").is_none(), "success must not serialize error: {v}");
        assert_eq!(v["current_source"], "package-json");
        assert_eq!(v["current_version"], "1.2.3");
        assert_eq!(v["latest_version"], "1.3.0");
        assert_eq!(v["update_available"], true);
    }

    #[test]
    fn result_json_includes_error_when_no_release() {
        let r = build_check_update_result("1.2.3", "zbrain upgrade", None, "");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["error"], "no_releases");
        assert_eq!(v["update_available"], false);
    }
}
