/**
 * skillpack/bootstrap_display.rs — post-scaffold runbook display.
 *
 * Codex T1 fix: third-party packs don't auto-execute their install
 * runbook. Instead, scaffold drops the files (additively, the v0.36
 * way) and then displays `runbooks/bootstrap.md` if present, framed
 * for the calling agent to walk per-step at its own discretion.
 *
 * No executor. No `agent:` / `show user:` / `ask user:` dispatch.
 * Just print the markdown with a header that signals to any agent
 * reading the output that these are SUGGESTED steps, not a runnable
 * script. The agent (Claude / OpenClaw / etc.) decides whether to
 * walk them and how.
 *
 * Stays pure-data: returns the framed text rather than writing
 * directly so tests can assert the shape and callers control the
 * output stream.
 */

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::skillpack::manifest_v1::SkillpackManifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapDisplayInput {
    /// Absolute path to the scaffolded skillpack root (pack cache or local).
    pub pack_root: PathBuf,
    /// Parsed manifest.
    pub manifest: SkillpackManifest,
    /// Absolute path to the user's workspace where files landed.
    pub workspace: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapDisplayResult {
    /// True when a bootstrap.md was found AND non-empty.
    pub shown: bool,
    /// The framed text the caller writes to stderr/stdout. Empty when shown=false.
    pub text: String,
    /// Resolved bootstrap.md path (informational).
    pub bootstrap_path: Option<PathBuf>,
}

const FRAME_HEADER: &str = r#"══════════════════════════════════════════════════════════════════════
 BOOTSTRAP STEPS (read-only — agent decides what to run)
══════════════════════════════════════════════════════════════════════
These are SUGGESTED next steps from the skillpack author. zbrain
deliberately does NOT auto-execute them — third-party packs run in
trusted-path mode and an automated walker would let a malicious pack
mutate your brain on install.

Read each step. Run what you understand. Skip what you don't. Use
`zbrain skillpack reference <name>` later if you want to see what
the author changed in a new version.
══════════════════════════════════════════════════════════════════════
"#;

const FRAME_FOOTER: &str = r#"══════════════════════════════════════════════════════════════════════
End of bootstrap steps. The skillpack files are already on disk —
nothing above has been executed.
══════════════════════════════════════════════════════════════════════
"#;

/// Build the framed bootstrap output. Pure function — does not write to any
/// stream. Returns shown=false when there's no bootstrap.md or it's empty.
pub fn build_bootstrap_display(
    input: &BootstrapDisplayInput,
) -> BootstrapDisplayResult {
    let Some(runbooks) = input.manifest.runbooks.as_ref() else {
        return BootstrapDisplayResult {
            shown: false,
            text: String::new(),
            bootstrap_path: None,
        };
    };
    let Some(rel_path) = runbooks.bootstrap.as_ref() else {
        return BootstrapDisplayResult {
            shown: false,
            text: String::new(),
            bootstrap_path: None,
        };
    };

    let abs_path = input.pack_root.join(rel_path);
    if !abs_path.exists() {
        return BootstrapDisplayResult {
            shown: false,
            text: String::new(),
            bootstrap_path: Some(abs_path),
        };
    }

    let content = match fs::read_to_string(&abs_path) {
        Ok(c) => c.trim().to_string(),
        Err(_) => String::new(),
    };

    if content.is_empty() {
        return BootstrapDisplayResult {
            shown: false,
            text: String::new(),
            bootstrap_path: Some(abs_path),
        };
    }

    let text = format!("{FRAME_HEADER}\n{content}\n\n{FRAME_FOOTER}");
    BootstrapDisplayResult {
        shown: true,
        text,
        bootstrap_path: Some(abs_path),
    }
}
