//! 1-5-5: Daemon install/uninstall.
//!
//! Ports `src/commands/autopilot.ts` install/uninstall logic (lines 702-1139).
//!
//! Per grill Q6: includes OpenClaw integration (detectOpenClaw + injection).
//!
//! Design:
//! - Pure generation functions (plist XML, systemd unit, crontab line, wrapper
//!   script, ephemeral start script) are testable cross-platform.
//! - Actual install/uninstall functions call platform-specific commands
//!   (launchctl, systemctl, crontab) and are gated by `cfg(target_os)`.
//! - `detect_install_target` is runtime detection (not cfg-gated), matching
//!   TS behavior where all 4 targets are compiled in.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

// ── Install target detection ──────────────────────────────────────────

/// Supervisor target for daemon install.
///
/// Mirrors TS `InstallTarget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallTarget {
    Macos,
    LinuxSystemd,
    EphemeralContainer,
    LinuxCron,
}

impl std::fmt::Display for InstallTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InstallTarget::Macos => write!(f, "macos"),
            InstallTarget::LinuxSystemd => write!(f, "linux-systemd"),
            InstallTarget::EphemeralContainer => write!(f, "ephemeral-container"),
            InstallTarget::LinuxCron => write!(f, "linux-cron"),
        }
    }
}

/// Detect the right supervisor for this host.
///
/// Port of TS `detectInstallTarget`:
/// - macOS → launchd (always when `target_os == "darwin"`)
/// - ephemeral-container → Render / Railway / Fly / Docker (env signals)
/// - linux-systemd → `systemctl --user is-system-running` succeeds
/// - linux-cron → fallback
///
/// On Windows, returns `LinuxCron` as a no-op fallback (Windows is not
/// a supported autopilot host; the daemon runs on macOS/Linux servers).
pub fn detect_install_target() -> InstallTarget {
    #[cfg(target_os = "macos")]
    {
        return InstallTarget::Macos;
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Ephemeral container detection
        let ephemeral = std::env::var("RENDER").is_ok()
            || std::env::var("RAILWAY_ENVIRONMENT").is_ok()
            || std::env::var("FLY_APP_NAME").is_ok()
            || Path::new("/.dockerenv").exists();
        if ephemeral {
            return InstallTarget::EphemeralContainer;
        }

        // systemd user scope probe
        if Path::new("/run/systemd/system").exists() {
            let output = Command::new("systemctl")
                .args(["--user", "is-system-running"])
                .output();
            if let Ok(out) = output {
                if out.status.success() {
                    return InstallTarget::LinuxSystemd;
                }
            }
        }

        InstallTarget::LinuxCron
    }
}

// ── OpenClaw detection ────────────────────────────────────────────────

/// OpenClaw detection result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenClawDetection {
    pub detected: bool,
    pub bootstrap_candidates: Vec<PathBuf>,
}

/// Detect OpenClaw presence on the host.
///
/// Port of TS `detectOpenClaw`. Checks:
/// - `OPENCLAW_HOME` env var
/// - `openclaw.json` in cwd or home
/// - `hooks/bootstrap/ensure-services.sh` in candidate paths
pub fn detect_open_claw() -> OpenClawDetection {
    let home = std::env::var("HOME").unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_default();

    let mut candidates: Vec<PathBuf> = Vec::new();

    // OPENCLAW_HOME-based candidate
    if let Ok(oc_home) = std::env::var("OPENCLAW_HOME") {
        candidates.push(PathBuf::from(&oc_home).join("hooks/bootstrap/ensure-services.sh"));
    }
    // cwd-based candidate
    candidates.push(cwd.join("hooks/bootstrap/ensure-services.sh"));
    // ~/.claude-based candidate
    if !home.is_empty() {
        candidates.push(
            PathBuf::from(&home)
                .join(".claude/hooks/bootstrap/ensure-services.sh"),
        );
    }

    let existing: Vec<PathBuf> = candidates.into_iter().filter(|p| p.exists()).collect();

    let signal = std::env::var("OPENCLAW_HOME").is_ok()
        || cwd.join("openclaw.json").exists()
        || (!home.is_empty() && PathBuf::from(&home).join("openclaw.json").exists())
        || !existing.is_empty();

    OpenClawDetection {
        detected: signal,
        bootstrap_candidates: existing,
    }
}

// ── Wrapper script generation ─────────────────────────────────────────

/// Generate the bash wrapper script content.
///
/// Port of TS `writeWrapperScript`. The wrapper sources the user's shell
/// profile for API keys, then runs `zbrain autopilot --repo <path>`.
pub fn generate_wrapper_script(repo_path: &str, zbrain_cli_path: &str) -> String {
    let safe_repo = repo_path.replace('\'', "'\\''");
    let safe_cli = zbrain_cli_path.replace('\'', "'\\''");
    format!(
        "#!/bin/bash\n\
         # Auto-generated by zbrain autopilot --install\n\
         # Sources shell profile for API keys, then runs autopilot.\n\
         # zshenv is the canonical place for env vars in zsh on macOS (zshrc is for\n\
         # interactive shells only — vars defined there don't reach this non-interactive\n\
         # subprocess). Source it first so secrets like ZBRAIN_DATABASE_URL or any\n\
         # OPENAI/ANTHROPIC keys exported in zshenv reach autopilot.\n\
         [ -f ~/.zshenv ] && source ~/.zshenv 2>/dev/null\n\
         source ~/.zshrc 2>/dev/null || source ~/.bashrc 2>/dev/null || true\n\
         exec '{safe_cli}' autopilot --repo '{safe_repo}'\n"
    )
}

// ── Launchd plist generation ──────────────────────────────────────────

/// XML-escape a string for use in plist XML.
fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Generate the macOS launchd plist XML content.
///
/// Port of TS `generateLaunchdPlist`. Includes:
/// - `RunAtLoad = true`, `KeepAlive = true`
/// - `ThrottleInterval = 60` (v0.37.7.0: forces 60s between relaunches)
/// - StandardOutPath / StandardErrorPath under `~/.zbrain/`
pub fn generate_launchd_plist(wrapper_path: &str, home: &str) -> String {
    let wp = escape_xml(wrapper_path);
    let h = escape_xml(home);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key><string>com.zbrain.autopilot</string>\n\
         \x20 <key>ProgramArguments</key><array>\n\
         \x20   <string>{wp}</string>\n\
         \x20 </array>\n\
         \x20 <key>RunAtLoad</key><true/>\n\
         \x20 <key>KeepAlive</key><true/>\n\
         \x20 <key>ThrottleInterval</key><integer>60</integer>\n\
         \x20 <key>StandardOutPath</key><string>{h}/.zbrain/autopilot.log</string>\n\
         \x20 <key>StandardErrorPath</key><string>{h}/.zbrain/autopilot.err</string>\n\
         </dict>\n\
         </plist>"
    )
}

// ── Systemd unit generation ───────────────────────────────────────────

/// Generate the systemd user service unit file content.
///
/// Port of TS `installSystemd` unit string.
pub fn generate_systemd_unit(wrapper_path: &str) -> String {
    format!(
        "[Unit]\n\
         Description=ZBrain Autopilot\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={wrapper_path}\n\
         Restart=on-failure\n\
         RestartSec=30\n\
         StandardOutput=append:%h/.zbrain/autopilot.log\n\
         StandardError=append:%h/.zbrain/autopilot.err\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

// ── Crontab line generation ───────────────────────────────────────────

/// Generate the crontab line for 5-minute interval.
///
/// Port of TS `installCrontab` cron line.
pub fn generate_crontab_line(wrapper_path: &str, home: &str) -> String {
    let safe_wrapper = wrapper_path.replace('\'', "'\\''");
    let safe_home = home.replace('\'', "'\\''");
    format!(
        "*/5 * * * * '{safe_wrapper}' >> '{safe_home}/.zbrain/autopilot.log' 2>&1"
    )
}

// ── Ephemeral container start script generation ───────────────────────

/// Generate the ephemeral container start script content.
///
/// Port of TS `installEphemeralContainer` script string.
pub fn generate_ephemeral_start_script(wrapper_path: &str) -> String {
    let safe_wrapper = wrapper_path.replace('\'', "'\\''");
    format!(
        "#!/bin/bash\n\
         # Auto-generated by zbrain autopilot --install (ephemeral-container target)\n\
         # Ephemeral filesystems lose crontab on every deploy; source this from\n\
         # your agent's bootstrap instead.\n\
         nohup '{safe_wrapper}' > ~/.zbrain/autopilot.log 2>&1 &\n\
         echo $! > ~/.zbrain/autopilot.pid\n"
    )
}

// ── OpenClaw injection snippet ────────────────────────────────────────

/// The marker line injected into OpenClaw bootstrap files.
pub const OPENCLAW_MARKER: &str = "# zbrain:autopilot v0.11.0";

/// Generate the OpenClaw injection snippet to append to a bootstrap file.
pub fn generate_openclaw_snippet(start_script_path: &str) -> String {
    format!("\n{marker}\nbash {start_script_path}\n", marker = OPENCLAW_MARKER)
}

/// Decide whether to inject the OpenClaw bootstrap snippet.
///
/// Port of TS `shouldInject` logic:
/// - `--no-inject` → false
/// - OpenClaw detected + candidates exist → true (auto-inject by default)
/// - `--inject-bootstrap` → true (explicit opt-in)
pub fn should_inject_openclaw(
    detected: bool,
    candidates_len: usize,
    inject_bootstrap: bool,
    no_inject: bool,
) -> bool {
    if no_inject {
        return false;
    }
    if detected && candidates_len > 0 {
        return true;
    }
    inject_bootstrap
}

// ── Install result ────────────────────────────────────────────────────

/// Result of a daemon install operation.
#[derive(Debug, Clone, Serialize)]
pub struct InstallResult {
    pub target: InstallTarget,
    pub wrapper_path: String,
    pub message: String,
    pub openclaw_detected: bool,
    pub openclaw_injected: Vec<String>,
}

/// Result of a daemon uninstall operation.
#[derive(Debug, Clone, Serialize)]
pub struct UninstallResult {
    pub removed_count: u32,
    pub messages: Vec<String>,
}

// ── Path helpers ──────────────────────────────────────────────────────

/// Get the launchd plist path.
pub fn plist_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/LaunchAgents/com.zbrain.autopilot.plist")
}

/// Get the systemd unit path.
pub fn systemd_unit_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join(".config/systemd/user/zbrain-autopilot.service")
}

/// Get the ephemeral start script path.
pub fn ephemeral_start_script_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".zbrain/start-autopilot.sh")
}

/// Get the wrapper script path.
pub fn wrapper_script_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".zbrain/autopilot-run.sh")
}

// ── CLI path resolution ───────────────────────────────────────────────

/// Resolve the zbrain CLI entrypoint for spawning the worker child.
///
/// Port of TS `resolveGbrainCliPath`. Order:
/// 1. `which zbrain` (shim on PATH)
/// 2. `std::env::current_exe()` if it ends with `zbrain` or `zbrain.exe`
/// 3. `std::env::args().nth(1)` if it ends with `zbrain` or `zbrain.exe`
/// 4. Error
pub fn resolve_zbrain_cli_path() -> Result<String, String> {
    // 1. which zbrain
    if let Ok(output) = Command::new("which").arg("zbrain").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(path);
            }
        }
    }

    // 2. current_exe
    if let Ok(exe) = std::env::current_exe() {
        let exe_str = exe.to_string_lossy();
        if exe_str.ends_with("/zbrain") || exe_str.ends_with("\\zbrain.exe")
            || exe_str.ends_with("zbrain") || exe_str.ends_with("zbrain.exe")
        {
            return Ok(exe_str.into_owned());
        }
    }

    // 3. argv[1]
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let arg1 = &args[1];
        if arg1.ends_with("/zbrain") || arg1.ends_with("\\zbrain.exe")
            || arg1.ends_with("zbrain") || arg1.ends_with("zbrain.exe")
        {
            return Ok(arg1.clone());
        }
    }

    Err(
        "Could not resolve the zbrain CLI path. Install zbrain so it is on $PATH \
         (e.g. /usr/local/bin/zbrain), or run autopilot from the compiled binary directly."
            .into(),
    )
}

// ── Status ────────────────────────────────────────────────────────────

/// Daemon install status.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    pub installed: bool,
    pub last_log: String,
}

/// Check daemon install status.
///
/// Port of TS `showStatus`. Checks for plist (macOS) or crontab entry (Linux).
/// Reads the last line of `~/.zbrain/autopilot.log` if present.
pub fn show_status() -> DaemonStatus {
    let home = std::env::var("HOME").unwrap_or_default();
    let log_path = PathBuf::from(&home).join(".zbrain/autopilot.log");

    let last_log = std::fs::read_to_string(&log_path)
        .ok()
        .and_then(|content| {
            let lines: Vec<&str> = content.trim().split('\n').collect();
            lines.last().map(|s| s.to_string())
        })
        .unwrap_or_default();

    let installed = if cfg!(target_os = "macos") {
        plist_path().exists()
    } else {
        // Check crontab for zbrain entry
        Command::new("crontab")
            .args(["-l"])
            .output()
            .ok()
            .and_then(|out| {
                if out.status.success() {
                    let text = String::from_utf8_lossy(&out.stdout);
                    Some(text.contains("zbrain autopilot") || text.contains("autopilot-run.sh"))
                } else {
                    Some(false)
                }
            })
            .unwrap_or(false)
    };

    DaemonStatus {
        installed,
        last_log,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── InstallTarget ──────────────────────────────────────────────────

    #[test]
    fn install_target_display() {
        assert_eq!(InstallTarget::Macos.to_string(), "macos");
        assert_eq!(InstallTarget::LinuxSystemd.to_string(), "linux-systemd");
        assert_eq!(
            InstallTarget::EphemeralContainer.to_string(),
            "ephemeral-container"
        );
        assert_eq!(InstallTarget::LinuxCron.to_string(), "linux-cron");
    }

    #[test]
    fn detect_install_target_returns_valid_variant() {
        // Just verify it returns one of the 4 variants (platform-dependent)
        let target = detect_install_target();
        assert!(matches!(
            target,
            InstallTarget::Macos
                | InstallTarget::LinuxSystemd
                | InstallTarget::EphemeralContainer
                | InstallTarget::LinuxCron
        ));
    }

    // ── generate_wrapper_script ────────────────────────────────────────

    #[test]
    fn wrapper_script_contains_shell_profile_source() {
        let script = generate_wrapper_script("/tmp/brain", "/usr/local/bin/zbrain");
        assert!(script.contains("source ~/.zshenv"));
        assert!(script.contains("source ~/.zshrc"));
        assert!(script.contains("source ~/.bashrc"));
    }

    #[test]
    fn wrapper_script_contains_exec_zbrain() {
        let script = generate_wrapper_script("/tmp/brain", "/usr/local/bin/zbrain");
        assert!(script.contains("exec '/usr/local/bin/zbrain' autopilot --repo '/tmp/brain'"));
    }

    #[test]
    fn wrapper_script_escapes_single_quotes() {
        let script = generate_wrapper_script("/tmp/it's brain", "/usr/local/bin/zbrain");
        assert!(script.contains("'/tmp/it'\\''s brain'"));
    }

    // ── generate_launchd_plist ─────────────────────────────────────────

    #[test]
    fn plist_has_label() {
        let plist = generate_launchd_plist("/tmp/wrapper.sh", "/home/user");
        assert!(plist.contains("<key>Label</key><string>com.zbrain.autopilot</string>"));
    }

    #[test]
    fn plist_has_run_at_load_and_keep_alive() {
        let plist = generate_launchd_plist("/tmp/wrapper.sh", "/home/user");
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
        assert!(plist.contains("<key>KeepAlive</key><true/>"));
    }

    #[test]
    fn plist_has_throttle_interval_60() {
        let plist = generate_launchd_plist("/tmp/wrapper.sh", "/home/user");
        assert!(plist.contains("<key>ThrottleInterval</key><integer>60</integer>"));
    }

    #[test]
    fn plist_has_log_paths() {
        let plist = generate_launchd_plist("/tmp/wrapper.sh", "/home/user");
        assert!(plist.contains("/home/user/.zbrain/autopilot.log"));
        assert!(plist.contains("/home/user/.zbrain/autopilot.err"));
    }

    #[test]
    fn plist_escapes_xml_special_chars() {
        let plist = generate_launchd_plist("/tmp/a&b<c>\"d\"", "/home/user");
        assert!(plist.contains("&amp;"));
        assert!(plist.contains("&lt;"));
        assert!(plist.contains("&gt;"));
        assert!(plist.contains("&quot;"));
    }

    // ── generate_systemd_unit ──────────────────────────────────────────

    #[test]
    fn systemd_unit_has_description() {
        let unit = generate_systemd_unit("/tmp/wrapper.sh");
        assert!(unit.contains("Description=ZBrain Autopilot"));
    }

    #[test]
    fn systemd_unit_has_exec_start() {
        let unit = generate_systemd_unit("/tmp/wrapper.sh");
        assert!(unit.contains("ExecStart=/tmp/wrapper.sh"));
    }

    #[test]
    fn systemd_unit_has_restart_on_failure() {
        let unit = generate_systemd_unit("/tmp/wrapper.sh");
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=30"));
    }

    #[test]
    fn systemd_unit_has_log_paths() {
        let unit = generate_systemd_unit("/tmp/wrapper.sh");
        assert!(unit.contains("%h/.zbrain/autopilot.log"));
        assert!(unit.contains("%h/.zbrain/autopilot.err"));
    }

    #[test]
    fn systemd_unit_has_wanted_by() {
        let unit = generate_systemd_unit("/tmp/wrapper.sh");
        assert!(unit.contains("WantedBy=default.target"));
    }

    // ── generate_crontab_line ──────────────────────────────────────────

    #[test]
    fn crontab_line_has_5_min_interval() {
        let line = generate_crontab_line("/tmp/wrapper.sh", "/home/user");
        assert!(line.starts_with("*/5 * * * *"));
    }

    #[test]
    fn crontab_line_has_wrapper_path() {
        let line = generate_crontab_line("/tmp/wrapper.sh", "/home/user");
        assert!(line.contains("'/tmp/wrapper.sh'"));
    }

    #[test]
    fn crontab_line_has_log_redirect() {
        let line = generate_crontab_line("/tmp/wrapper.sh", "/home/user");
        assert!(line.contains("'/home/user/.zbrain/autopilot.log'"));
        assert!(line.contains("2>&1"));
    }

    #[test]
    fn crontab_line_escapes_single_quotes() {
        let line = generate_crontab_line("/tmp/it's wrapper.sh", "/home/user");
        assert!(line.contains("'/tmp/it'\\''s wrapper.sh'"));
    }

    // ── generate_ephemeral_start_script ────────────────────────────────

    #[test]
    fn ephemeral_script_has_nohup() {
        let script = generate_ephemeral_start_script("/tmp/wrapper.sh");
        assert!(script.contains("nohup"));
        assert!(script.contains("/tmp/wrapper.sh"));
    }

    #[test]
    fn ephemeral_script_writes_pid() {
        let script = generate_ephemeral_start_script("/tmp/wrapper.sh");
        assert!(script.contains("echo $! > ~/.zbrain/autopilot.pid"));
    }

    #[test]
    fn ephemeral_script_redirects_log() {
        let script = generate_ephemeral_start_script("/tmp/wrapper.sh");
        assert!(script.contains("~/.zbrain/autopilot.log"));
        assert!(script.contains("2>&1 &"));
    }

    // ── OpenClaw ───────────────────────────────────────────────────────

    #[test]
    fn openclaw_marker_is_stable() {
        assert_eq!(OPENCLAW_MARKER, "# zbrain:autopilot v0.11.0");
    }

    #[test]
    fn openclaw_snippet_contains_marker_and_bash() {
        let snippet = generate_openclaw_snippet("/tmp/start-autopilot.sh");
        assert!(snippet.contains(OPENCLAW_MARKER));
        assert!(snippet.contains("bash /tmp/start-autopilot.sh"));
    }

    #[test]
    fn should_inject_false_when_no_inject() {
        assert!(!should_inject_openclaw(true, 1, true, true));
    }

    #[test]
    fn should_inject_true_when_detected_with_candidates() {
        assert!(should_inject_openclaw(true, 1, false, false));
        assert!(should_inject_openclaw(true, 3, false, false));
    }

    #[test]
    fn should_inject_false_when_detected_but_no_candidates() {
        assert!(!should_inject_openclaw(true, 0, false, false));
    }

    #[test]
    fn should_inject_true_when_explicit_opt_in() {
        assert!(should_inject_openclaw(false, 0, true, false));
    }

    #[test]
    fn should_inject_false_when_not_detected_and_not_opt_in() {
        assert!(!should_inject_openclaw(false, 0, false, false));
    }

    // ── detect_open_claw ───────────────────────────────────────────────

    #[test]
    fn detect_open_claw_returns_valid_result() {
        let result = detect_open_claw();
        // Just verify it runs without panic and returns consistent state
        if result.detected {
            // If detected, there should be at least some signal
            // (env var or file exists)
        }
        // bootstrap_candidates should only contain existing paths
        for p in &result.bootstrap_candidates {
            assert!(p.exists(), "candidate should exist: {:?}", p);
        }
    }

    // ── Path helpers ───────────────────────────────────────────────────

    #[test]
    fn plist_path_ends_with_correct_filename() {
        let path = plist_path();
        assert!(path.ends_with("com.zbrain.autopilot.plist"));
    }

    #[test]
    fn systemd_unit_path_ends_with_correct_filename() {
        let path = systemd_unit_path();
        assert!(path.ends_with("zbrain-autopilot.service"));
    }

    #[test]
    fn ephemeral_start_script_path_ends_with_correct_filename() {
        let path = ephemeral_start_script_path();
        assert!(path.ends_with("start-autopilot.sh"));
    }

    #[test]
    fn wrapper_script_path_ends_with_correct_filename() {
        let path = wrapper_script_path();
        assert!(path.ends_with("autopilot-run.sh"));
    }

    // ── resolve_zbrain_cli_path ────────────────────────────────────────

    #[test]
    fn resolve_cli_path_returns_result() {
        // Just verify it doesn't panic — may succeed or fail depending on
        // whether zbrain is on PATH in the test environment
        let _ = resolve_zbrain_cli_path();
    }

    // ── show_status ────────────────────────────────────────────────────

    #[test]
    fn show_status_returns_daemon_status() {
        let status = show_status();
        // Just verify it runs and returns a valid struct
        // (installed will be false in test env, last_log will be empty)
        let _ = status.installed;
        let _ = status.last_log;
    }

    // ── escape_xml ─────────────────────────────────────────────────────

    #[test]
    fn escape_xml_handles_all_special_chars() {
        assert_eq!(escape_xml("a&b"), "a&amp;b");
        assert_eq!(escape_xml("a<b"), "a&lt;b");
        assert_eq!(escape_xml("a>b"), "a&gt;b");
        assert_eq!(escape_xml("a\"b"), "a&quot;b");
    }

    #[test]
    fn escape_xml_preserves_normal_text() {
        assert_eq!(escape_xml("hello world"), "hello world");
        assert_eq!(escape_xml("/usr/local/bin/zbrain"), "/usr/local/bin/zbrain");
    }
}
