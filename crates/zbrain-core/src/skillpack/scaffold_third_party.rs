/**
 * skillpack/scaffold_third_party.rs — orchestrator for scaffolding a
 * third-party skillpack into the user's workspace.
 *
 * Composes the foundation pieces:
 *   resolve_source → load_skillpack_manifest → ask_trust → enumerate_scaffold_entries
 *   → copy_artifacts → save_state (~/.zbrain/skillpack-state.json) → build_bootstrap_display
 *
 * Mirrors the contracts of v0.36's `run_scaffold` (no managed-block writes,
 * refuses to overwrite, partial-state policy via enumerate_scaffold_entries +
 * paired sources) — third-party packs land the same way bundled ones do.
 * The only difference is the source manifest format (skillpack.json vs
 * openclaw.plugin.json) and the trust gate that wraps the copy step.
 *
 * Returns a structured result the CLI and the publish-gate both consume.
 */

use std::path::{Path, PathBuf};
use chrono::Utc;
use semver::Version;
use serde::{Deserialize, Serialize};
use crate::skillpack::manifest_v1::{parse_validate_manifest, SkillpackManifest, SkillpackManifestError};
use crate::skillpack::bootstrap_display::{build_bootstrap_display, BootstrapDisplayResult};
use crate::skillpack::copy::{copy_artifacts, CopyArtifactsOpts, CopyItem, CopyResult};
use crate::skillpack::bundle::{enumerate_scaffold_entries, ScaffoldEntry, BundleManifest};
use crate::skillpack::state::{default_state_path, load_state, save_state, upsert_entry, SkillpackState, SkillpackStateEntry};
use crate::skillpack::trust_prompt::{ask_trust, SkillpackTier, TrustPromptDecision};
use crate::skillpack::remote_source::ResolvedSource;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldThirdPartyOptions {
    /// Result of resolve_source() (already cached/cloned/extracted).
    pub resolved: ResolvedSource,
    /// Absolute path to the target workspace where files should land.
    pub target_workspace: PathBuf,
    /// Tier the registry assigned the pack at scaffold time (informational).
    pub tier: Option<SkillpackTier>,
    /// Skip the trust prompt (CI / agent use).
    pub trust_flag: Option<bool>,
    /// Test seam: TTY override.
    pub is_tty: Option<bool>,
    /// Test seam: state-file path override.
    pub state_path: Option<PathBuf>,
    /// Dry-run: validate + enumerate; no writes.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaffoldThirdPartyStatus {
    WroteNew,
    AllSkippedExisting,
    DryRun,
    AbortedNoTrust,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldThirdPartyResult {
    pub status: ScaffoldThirdPartyStatus,
    pub manifest: SkillpackManifest,
    pub resolved: ResolvedSource,
    pub trust_decision: TrustPromptDecision,
    pub copy: Option<CopyResult>,
    pub entries: Vec<ScaffoldEntry>,
    pub bootstrap: BootstrapDisplayResult,
    pub state: SkillpackState,
}

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldThirdPartyError {
    #[error("skillpack requires zbrain >= {required}; you have {actual}. Run `zbrain upgrade` first.")]
    ZbrainVersionTooOld { required: String, actual: String },

    #[error("skillpack manifest invalid: {0}")]
    ManifestInvalid(String),

    #[error("scaffold enumeration failed: {0}")]
    ScaffoldFailed(String),
}

impl From<crate::error::StructuredError> for ScaffoldThirdPartyError {
    fn from(e: crate::error::StructuredError) -> Self {
        ScaffoldThirdPartyError::ScaffoldFailed(e.to_string())
    }
}

/// Semver compare: returns true when actual >= required.
fn semver_gte(actual: &str, required: &str) -> bool {
    let parse = |s: &str| -> Version {
        let s = s.strip_prefix('v').unwrap_or(s);
        Version::parse(s).unwrap_or_else(|_| {
            // Fallback: parse as far as possible, treat missing components as 0
            let mut parts: Vec<u64> = s.split(&['.', '-', '_'][..])
                .filter_map(|p| p.parse::<u64>().ok())
                .collect();
            while parts.len() < 3 {
                parts.push(0);
            }
            Version::new(parts[0], parts[1], parts[2])
        })
    };

    let a = parse(actual);
    let r = parse(required);
    a >= r
}

/// Run scaffold for a third-party skillpack resolved from remote.
pub async fn run_scaffold_third_party(
    opts: ScaffoldThirdPartyOptions,
    current_zbrain_version: &str,
) -> Result<ScaffoldThirdPartyResult, ScaffoldThirdPartyError> {
    // 1. Load + validate the manifest from the resolved pack root.
    let manifest_path = opts.resolved.path.join("skillpack.json");
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| ScaffoldThirdPartyError::ManifestInvalid(e.to_string()))?;
    let manifest = parse_validate_manifest(&content)
        .map_err(|e| ScaffoldThirdPartyError::ManifestInvalid(e.to_string()))?;

    // 2. zbrain version check.
    if !semver_gte(current_zbrain_version, &manifest.zbrain_min_version) {
        return Err(ScaffoldThirdPartyError::ZbrainVersionTooOld {
            required: manifest.zbrain_min_version.clone(),
            actual: current_zbrain_version.to_string(),
        });
    }

    // 3. Trust prompt (skipped for local sources, already-trusted, or --trust).
    let mut state = load_state(opts.state_path.as_deref());
    let tier = opts.tier.unwrap_or_else(|| {
        match opts.resolved.kind {
            crate::skillpack::remote_source::ResolvedSourceKind::Local => SkillpackTier::Local,
            _ => SkillpackTier::Community,
        }
    });

    let trust_decision = ask_trust(
        &crate::skillpack::trust_prompt::TrustPromptInput {
            manifest: manifest.clone(),
            resolved: opts.resolved.clone(),
            tier,
            state: state.clone(),
        },
        crate::skillpack::trust_prompt::AskTrustOptions {
            trust_flag: opts.trust_flag,
            is_tty: opts.is_tty,
        },
    ).await;

    if !trust_decision.trusted {
        return Ok(ScaffoldThirdPartyResult {
            status: ScaffoldThirdPartyStatus::AbortedNoTrust,
            manifest,
            resolved: opts.resolved,
            trust_decision,
            copy: None,
            entries: Vec::new(),
            bootstrap: BootstrapDisplayResult {
                shown: false,
                text: String::new(),
                bootstrap_path: None,
            },
            state,
        });
    }

    // 4. Convert skillpack manifest to bundle format for enumeration.
    let mut bundle_manifest = crate::skillpack::bundle::BundleManifest {
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: Some(manifest.description.clone()),
        skills: manifest.skills.clone(),
        shared_deps: manifest.shared_deps.clone().unwrap_or_default(),
        excluded_from_install: manifest.excluded_from_install.clone(),
    };

    // 5. Enumerate scaffold entries (every file under skills/<slug>/ + paired
    //    sources declared in each SKILL.md's frontmatter). Throws BundleError
    //    on missing skill dirs (we already validated that, but defense in depth).
    let excluded = manifest.excluded_from_install.clone();
    let entries = match crate::skillpack::bundle::enumerate_scaffold_entries(
        &opts.resolved.path,
        &bundle_manifest,
        excluded.as_deref().unwrap_or(&[]),
    ) {
        Ok(e) => e,
        Err(e) => {
            return Err(ScaffoldThirdPartyError::ScaffoldFailed(e.to_string()));
        }
    };

    // 6. Copy.
    let items: Vec<CopyItem> = entries
        .iter()
        .map(|e| CopyItem {
            source: e.source.clone(),
            target: opts.target_workspace.join(&e.rel_target),
        })
        .collect();

    let copy_opts = CopyArtifactsOpts {
        dry_run: Some(opts.dry_run),
        ..Default::default()
    };
    let copy = copy_artifacts(
        &items,
        &copy_opts,
    );

    // 7. Bootstrap runbook display (no executor — codex T1).
    let bootstrap = build_bootstrap_display(
        &crate::skillpack::bootstrap_display::BootstrapDisplayInput {
            pack_root: opts.resolved.path.clone(),
            manifest: manifest.clone(),
            workspace: opts.target_workspace.clone(),
        }
    );

    // 8. Update state.json (skip on dry-run).
    let copy = copy?;
    let mut new_state = state;
    if !opts.dry_run && copy.summary.wrote_new > 0 {
        let skill_slugs = manifest.skills.clone();
        let entry = SkillpackStateEntry {
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            author: manifest.author.clone(),
            source: opts.resolved.source.clone(),
            source_kind: Some(opts.resolved.kind),
            pinned_commit: opts.resolved.pinned_commit.clone(),
            tarball_sha256: opts.resolved.tarball_sha256.clone(),
            tier: Some(tier),
            scaffolded_at: Some(Utc::now().to_rfc3339()),
            workspace: Some(opts.target_workspace.clone()),
            skill_slugs,
        };
        new_state = upsert_entry(new_state, entry);
        save_state(&new_state, opts.state_path.as_deref());
    }

    let status = if opts.dry_run {
        ScaffoldThirdPartyStatus::DryRun
    } else if copy.summary.wrote_new > 0 {
        ScaffoldThirdPartyStatus::WroteNew
    } else {
        ScaffoldThirdPartyStatus::AllSkippedExisting
    };

    Ok(ScaffoldThirdPartyResult {
        status,
        manifest,
        resolved: opts.resolved,
        trust_decision,
        copy: Some(copy),
        entries,
        bootstrap,
        state: new_state,
    })
}
