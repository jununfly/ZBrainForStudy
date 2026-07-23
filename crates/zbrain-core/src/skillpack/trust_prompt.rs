/**
 * skillpack/trust_prompt.rs — first-install identity confirm + TOFU.
 *
 * Codex G4: every first scaffold of a third-party pack surfaces a
 * confirm prompt with full identity (name, author, source URL, pinned
 * commit / tarball SHA, tier). Subsequent scaffolds of the same
 * `(name, author, pinned_commit_or_tarball_sha)` triple skip the
 * prompt — they're already trusted (state.json carries the record).
 * Different author or different pin re-prompts.
 *
 * Pure-data prompt builder + a TTY/non-TTY adapter. Tests exercise
 * the builder shape; the adapter is exercised via e2e (where we
 * inject a fake reader).
 */

use std::io::{self, Write, BufRead};
use serde::{Deserialize, Serialize};
use crate::skillpack::remote_source::ResolvedSource;
use crate::skillpack::manifest_v1::SkillpackManifest;
use crate::skillpack::state::{is_already_trusted, SkillpackState, SkillpackStateEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillpackTier {
    Endorsed,
    Community,
    Experimental,
    Dead,
    Local,
}

impl std::fmt::Display for SkillpackTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillpackTier::Endorsed => write!(f, "endorsed"),
            SkillpackTier::Community => write!(f, "community"),
            SkillpackTier::Experimental => write!(f, "experimental"),
            SkillpackTier::Dead => write!(f, "dead"),
            SkillpackTier::Local => write!(f, "local"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustPromptInput {
    pub manifest: SkillpackManifest,
    pub resolved: ResolvedSource,
    pub tier: SkillpackTier,
    pub state: SkillpackState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPromptReason {
    AlreadyTrusted,
    PromptAccepted,
    PromptRejected,
    LocalPathNoPrompt,
    TrustFlagBypassed,
    NonTtyNoTrustFlag,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustPromptDecision {
    /// True when prompt was shown and user accepted, OR when skipped because already trusted.
    pub trusted: bool,
    /// Reason for the decision, useful for stderr log lines + tests.
    pub reason: TrustPromptReason,
}

/// Render the identity block shown to the user. Pure function.
pub fn render_identity_block(input: &TrustPromptInput) -> String {
    let mut lines = Vec::new();
    lines.push("[skillpack] About to scaffold:".to_string());
    lines.push(format!("  Name:          {}", input.manifest.name));
    lines.push(format!("  Version:       {}", input.manifest.version));
    lines.push(format!("  Author:        {}", input.manifest.author));
    lines.push(format!("  Source:        {}", input.resolved.source));
    if let Some(pinned) = &input.resolved.pinned_commit {
        lines.push(format!("  Pinned commit: {}", pinned));
    }
    if let Some(sha) = &input.resolved.tarball_sha256 {
        lines.push(format!("  Tarball SHA:   sha256:{}", sha));
    }
    lines.push(format!("  Tier:          {}", input.tier));
    lines.push(format!("  Description:   {}", input.manifest.description));
    lines.join("\n")
}

#[derive(Debug, Clone, Default)]
pub struct AskTrustOptions {
    /// If true, --trust flag was passed; auto-accept regardless of TTY.
    pub trust_flag: Option<bool>,
    /// Whether we're running on an interactive TTY.
    pub is_tty: Option<bool>,
}

/// Decide whether the scaffold can proceed. Surfaces the identity block to the
/// user, runs the prompt, returns a structured decision.
pub async fn ask_trust(
    input: &TrustPromptInput,
    opts: AskTrustOptions,
) -> TrustPromptDecision {
    // Local-path sources skip the trust gate entirely. The user owns the
    // directory; they're already trusting whatever lives there.
    if input.resolved.kind == crate::skillpack::remote_source::ResolvedSourceKind::Local {
        return TrustPromptDecision { trusted: true, reason: TrustPromptReason::LocalPathNoPrompt };
    }

    // Already trusted check (codex G4 identity match).
    let candidate = SkillpackStateEntry {
        name: input.manifest.name.clone(),
        version: input.manifest.version.clone(),
        author: input.manifest.author.clone(),
        source: input.resolved.source.clone(),
        source_kind: Some(input.resolved.kind),
        pinned_commit: input.resolved.pinned_commit.clone(),
        tarball_sha256: input.resolved.tarball_sha256.clone(),
        tier: Some(input.tier),
        scaffolded_at: None,
        workspace: None,
        skill_slugs: Vec::new(),
    };

    if is_already_trusted(&input.state, &candidate) {
        return TrustPromptDecision { trusted: true, reason: TrustPromptReason::AlreadyTrusted };
    }

    let block = render_identity_block(input);
    eprintln!("\n{block}\n");

    if opts.trust_flag == Some(true) {
        eprintln!("[skillpack] --trust flag passed; proceeding without confirm prompt.");
        return TrustPromptDecision { trusted: true, reason: TrustPromptReason::TrustFlagBypassed };
    }

    let is_tty = opts.is_tty.unwrap_or_else(|| atty::is(atty::Stream::Stdin));
    if !is_tty {
        eprintln!(
            "[skillpack] non-TTY environment and no --trust flag; refusing to scaffold a new third-party source without explicit consent."
        );
        return TrustPromptDecision { trusted: false, reason: TrustPromptReason::NonTtyNoTrustFlag };
    }

    eprint!("Continue? [y/N]: ");
    io::stdout().flush().ok();

    let stdin = io::stdin();
    let mut line = String::new();
    let _ = stdin.lock().read_line(&mut line);
    let answer = line.trim().to_lowercase();

    if answer == "y" || answer == "yes" {
        TrustPromptDecision { trusted: true, reason: TrustPromptReason::PromptAccepted }
    } else {
        TrustPromptDecision { trusted: false, reason: TrustPromptReason::PromptRejected }
    }
}
