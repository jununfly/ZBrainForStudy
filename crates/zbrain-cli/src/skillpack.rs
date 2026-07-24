//! Skillpack CLI subcommands — install, scaffold, search, harvest, doctor.
//!
//! All the CLI wrapping for the skillpack subsystem lives here.

use std::path::PathBuf;
use clap::Parser;
use anyhow::{Result, anyhow};
use zbrain_core::skillpack::{
    load_registry, LoadRegistryOptions,
    SkillpackTier,
    bundle::{self, find_zbrain_root},
    registry_schema::{self, RegistryTier},
};
use zbrain_core::skillpack::doctor::DoctorMode;

/// Subcommands for `zbrain skillpack`.
#[derive(Debug, Clone, Parser)]
#[command(arg_required_else_help = true)]
pub enum SkillpackSubcommand {
    /// Initialize a new empty skillpack in the current directory (cathedral scaffolding).
    Init(InitOptions),

    /// Scaffold a skillpack from the canonical registry into your workspace (or --all for starter bundle).
    Scaffold(ScaffoldOptionsCli),

    /// Search the registry for skillpacks by free text query.
    Search(SearchOptionsCli),

    /// Show information about a specific skillpack from the registry.
    Info(InfoOptionsCli),

    /// Install a skillpack from a git URL / owner/repo / local directory / tarball.
    Install(InstallOptionsCli),

    /// Run quality doctor on a local skillpack directory (checks rubric scoring).
    Doctor(DoctorOptionsCli),

    /// Pack a local skillpack directory into a gzipped tarball for publication.
    Pack(PackOptionsCli),

    /// Harvest a skill from a host repo into this zbrain repo (used by editorial workflow).
    Harvest(HarvestOptionsCli),

    /// Scrub legacy fence rows after migration to the frontmatter model (cleanup step).
    ScrubLegacyFenceRows(ScrubLegacyOptionsCli),

    /// List every skill bundled in openclaw.plugin.json.
    List(ListOptionsCli),

    /// Read-only diff: compare bundled vs your local copy (--all to sweep all).
    Reference(ReferenceOptionsCli),

    /// One-shot conversion: strip legacy managed-block fence comments (upgrade v0.32 → v0.33).
    MigrateFence(MigrateFenceOptionsCli),

    /// Run skillpack conformance check (health report, --strict exits non-zero on drift).
    Check(CheckOptionsCli),

    /// Show/set the configured skillpack registry URL (writes to ~/.zbrain/config.json).
    Registry(RegistryOptionsCli),

    /// (Operator-only) Set the tier for a skillpack in a registry repo clone.
    Endorse(EndorseOptionsCli),
}

#[derive(Debug, Clone, Parser)]
pub struct InitOptions {
    /// Pack name (lowercase kebab-case).
    pub name: String,
    /// Skip non-essential files (tests/unit, e2e, evals) for minimal scaffold.
    #[arg(long)]
    pub minimal: bool,
    /// Initial skill slug (defaults to pack name).
    #[arg(long)]
    pub first_skill_slug: Option<String>,
    /// Pre-fill author name.
    #[arg(long)]
    pub author: Option<String>,
    /// Pre-fill license (SPDX id like MIT, Apache-2.0).
    #[arg(long)]
    pub license: Option<String>,
    /// Pre-fill homepage URL.
    #[arg(long)]
    pub homepage: Option<String>,
    /// Dry-run: show what would be created without writing.
    #[arg(long)]
    pub dry_run: bool,
    /// Target directory (defaults to current directory/`<name>`).
    #[arg(long)]
    pub target_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Parser)]
pub struct ScaffoldOptionsCli {
    /// Skillpack name or owner/repo / URL to scaffold.
    pub spec: String,
    /// Target workspace directory where files land (defaults to current directory).
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Force trust and skip prompt (CI / automated use only).
    #[arg(long)]
    pub trust: bool,
    /// Force refresh registry before scaffolding.
    #[arg(long)]
    pub refresh: bool,
    /// Dry-run: enumerate what would be written without writing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct SearchOptionsCli {
    /// Free-text search query (matches name, description, author, tags).
    pub query: Option<String>,
    /// Filter to only skillpacks of a specific tier (endorsed|community|experimental|dead).
    #[arg(long)]
    pub tier: Option<String>,
    /// Force refresh registry cache.
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct InfoOptionsCli {
    /// Skillpack name to show info for.
    pub name: String,
    /// Force refresh registry cache.
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct InstallOptionsCli {
    /// Source spec: name / owner/repo / URL / path to local directory / path to .tgz.
    pub spec: String,
    /// Target workspace directory where files land (defaults to current directory).
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Force trust and skip prompt.
    #[arg(long)]
    pub trust: bool,
    /// Dry-run: enumerate what would be written without writing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct DoctorOptionsCli {
    /// Path to the skillpack root directory to check.
    pub pack_root: PathBuf,
    /// Run quick quality check only (rubric scoring only, no full publish-gate checks).
    #[arg(long)]
    pub quick: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct PackOptionsCli {
    /// Path to the skillpack root directory to pack.
    pub pack_root: PathBuf,
    /// Output directory for the tarball (defaults to pack root).
    #[arg(long)]
    pub out_dir: Option<PathBuf>,
    /// Skip doctor quality check before packing.
    #[arg(long)]
    pub skip_doctor: bool,
    /// Dry-run: run doctor only, do not pack tarball.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct HarvestOptionsCli {
    /// Slug of the skill to harvest from host repo.
    pub slug: String,
    /// Path to the host repo root where the skill lives.
    #[arg(long)]
    pub from: PathBuf,
    /// Skip privacy linting (used after manual scrub).
    #[arg(long)]
    pub no_lint: bool,
    /// Allow overwriting existing skill directory.
    #[arg(long)]
    pub overwrite: bool,
    /// Dry-run: show what would be copied.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct ScrubLegacyOptionsCli {
    /// Target workspace directory to clean up.
    pub target_workspace: PathBuf,
    /// Dry-run: show what would be removed without modifying the file.
    #[arg(long)]
    pub dry_run: bool,
}

/// Parse a tier string into a `RegistryTier` (used by `search` filtering).
fn parse_tier(s: &str) -> anyhow::Result<RegistryTier> {
    match s.to_ascii_lowercase().as_str() {
        "endorsed" => Ok(RegistryTier::Endorsed),
        "community" => Ok(RegistryTier::Community),
        "experimental" => Ok(RegistryTier::Experimental),
        "dead" => Ok(RegistryTier::Dead),
        other => Err(anyhow!(
            "Unknown tier: {other} (expected endorsed|community|experimental|dead)"
        )),
    }
}

/// Convert a registry-resolved tier into the `SkillpackTier` the scaffold
/// trust gate expects (`RegistryTier` has no `Local` variant).
fn registry_to_skillpack_tier(t: RegistryTier) -> SkillpackTier {
    match t {
        RegistryTier::Endorsed => SkillpackTier::Endorsed,
        RegistryTier::Community => SkillpackTier::Community,
        RegistryTier::Experimental => SkillpackTier::Experimental,
        RegistryTier::Dead => SkillpackTier::Dead,
    }
}

/// Main entry point for `zbrain skillpack` subcommands.
pub async fn run_skillpack(cmd: SkillpackSubcommand) -> Result<()> {
    match cmd {
        SkillpackSubcommand::Init(opts) => run_init(opts),
        SkillpackSubcommand::Scaffold(opts) => run_scaffold_cli(opts).await,
        SkillpackSubcommand::Search(opts) => run_search(opts).await,
        SkillpackSubcommand::Info(opts) => run_info(opts).await,
        SkillpackSubcommand::Install(opts) => run_install(opts).await,
        SkillpackSubcommand::Doctor(opts) => run_doctor_cli(opts),
        SkillpackSubcommand::Pack(opts) => run_pack(opts).await,
        SkillpackSubcommand::Harvest(opts) => run_harvest_cli(opts),
        SkillpackSubcommand::ScrubLegacyFenceRows(opts) => run_scrub_legacy_cli(opts),
        SkillpackSubcommand::List(opts) => run_list(opts),
        SkillpackSubcommand::Reference(opts) => run_reference_cli(opts),
        SkillpackSubcommand::MigrateFence(_opts) => {
            anyhow::bail!("migrate-fence not implemented yet — core exists but CLI wiring incomplete");
        }
        SkillpackSubcommand::Check(_opts) => {
            anyhow::bail!("check not implemented yet — routes to skillpack-check still in TS");
        }
        SkillpackSubcommand::Registry(opts) => run_registry(opts).await,
        SkillpackSubcommand::Endorse(opts) => run_endorse(opts),
    }
}

fn run_init(opts: InitOptions) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let target_dir = opts.target_dir.unwrap_or_else(|| cwd.join(&opts.name));

    let result = zbrain_core::skillpack::run_init_scaffold(zbrain_core::skillpack::InitScaffoldOptions {
        target_dir: target_dir.clone(),
        name: opts.name,
        minimal: opts.minimal,
        first_skill_slug: opts.first_skill_slug,
        author: opts.author,
        license: opts.license,
        homepage: opts.homepage,
        dry_run: opts.dry_run,
    });

    match result {
        Ok(result) => {
            println!("📦 Created new skillpack at: {}", result.target_dir.display());
            println!("  Files written: {}", result.files_written.len());
            if !result.files_skipped_existing.is_empty() {
                println!("  Files skipped (existing): {}", result.files_skipped_existing.len());
            }
            if opts.dry_run {
                println!("  (dry-run - no files written)");
            }
            Ok(())
        }
        Err(e) => Err(anyhow!("Init failed: {}", e)),
    }
}

async fn run_scaffold_cli(opts: ScaffoldOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let target_workspace = opts.workspace.unwrap_or_else(|| cwd.clone());

    // `zbrain_root` is the canonical bundle repo (contains openclaw.plugin.json),
    // i.e. the zbrain checkout you invoked `zbrain` from — NOT the destination
    // workspace passed via --workspace. `find_zbrain_root` matches the marker
    // `run_scaffold` actually requires (openclaw.plugin.json), unlike
    // `find_repo_root` which looks for a `skills/` resolver dir.
    let zbrain_root = find_zbrain_root(Some(&cwd))
        .ok_or_else(|| anyhow!("Could not find zbrain repo root (looking for openclaw.plugin.json)"))?;

    let loaded = zbrain_core::skillpack::load_registry(zbrain_core::skillpack::LoadRegistryOptions {
        refresh: opts.refresh,
        ..Default::default()
    }).await?;

    // If spec is a registry name, resolve it then scaffold third-party.
    // Otherwise it's a direct spec (git URL / local / tarball).
        if let Some((entry, tier)) = zbrain_core::skillpack::find_pack_with_tier(&loaded, &opts.spec) {
            // It's a registry name - use third-party scaffold.
            let resolved = zbrain_core::skillpack::resolve_source(&entry.source.url, zbrain_core::skillpack::ResolveSourceOptions::default()).await?;
            let result = zbrain_core::skillpack::run_scaffold_third_party(zbrain_core::skillpack::ScaffoldThirdPartyOptions {
                resolved,
                target_workspace: target_workspace.clone(),
                tier: Some(registry_to_skillpack_tier(tier)),
                trust_flag: if opts.trust { Some(true) } else { None },
                is_tty: None,
                state_path: None,
                dry_run: opts.dry_run,
            }, env!("CARGO_PKG_VERSION")).await?;

        match result.status {
            zbrain_core::skillpack::ScaffoldThirdPartyStatus::WroteNew => {
                println!("✅ Scaffold complete: wrote {} file(s)", result.copy.as_ref().unwrap().summary.wrote_new);
                if !result.bootstrap.text.is_empty() {
                    println!("\n{}", result.bootstrap.text);
                }
            }
            zbrain_core::skillpack::ScaffoldThirdPartyStatus::AllSkippedExisting => {
                println!("✅ All files already exist — nothing to do.");
            }
            zbrain_core::skillpack::ScaffoldThirdPartyStatus::DryRun => {
                println!("🔍 Dry-run complete: {} file(s) would be written", result.entries.len());
                for entry in result.entries {
                    println!("  - {}", entry.rel_target.display());
                }
            }
            zbrain_core::skillpack::ScaffoldThirdPartyStatus::AbortedNoTrust => {
                println!("⛔ Installation aborted — user did not trust this skillpack.");
                std::process::exit(1);
            }
        }

        Ok(())
    } else {
        // Direct scaffold from local zbrain bundle (spec is skill slug in this repo).
        let result = zbrain_core::skillpack::run_scaffold(zbrain_core::skillpack::ScaffoldOptions {
            zbrain_root: zbrain_root.clone(),
            target_workspace: target_workspace.clone(),
            skill_slug: Some(opts.spec.clone()),
            dry_run: opts.dry_run,
        });

        match result {
            Ok(result) => {
                println!("✅ Scaffold complete:");
                println!("  Wrote {} new file(s)", result.summary.wrote_new);
                println!("  Skipped {} existing file(s)", result.summary.skipped_existing);
                if result.summary.paired_sources_written > 0 {
                    println!("  Added {} missing paired source file(s)", result.summary.paired_sources_written);
                }
                if opts.dry_run {
                    println!("  (dry-run - no files written)");
                }
                Ok(())
            }
            Err(e) => Err(anyhow!("Scaffold failed: {}", e))
        }
    }
}

async fn run_search(opts: SearchOptionsCli) -> Result<()> {
    let loaded = zbrain_core::skillpack::load_registry(zbrain_core::skillpack::LoadRegistryOptions {
        refresh: opts.refresh,
        ..Default::default()
    }).await?;

    let tier = match opts.tier {
        Some(ref t) => Some(parse_tier(t)?),
        None => None,
    };

    let results = zbrain_core::skillpack::search_packs(&loaded, opts.query.as_deref(), tier);

    if results.is_empty() {
        println!("No matching skillpacks found.");
        return Ok(());
    }

    println!("Found {} matching skillpack(s):\n", results.len());
    for (entry, tier) in results {
        println!("**{}**  (tier: {tier})", entry.name);
        println!("  * Author: {}", entry.author);
        println!("  * Description: {}", entry.description);
        println!("  * Version: {}", entry.version);
        println!();
    }

    Ok(())
}

async fn run_info(opts: InfoOptionsCli) -> Result<()> {
    let loaded = zbrain_core::skillpack::load_registry(zbrain_core::skillpack::LoadRegistryOptions {
        refresh: opts.refresh,
        ..Default::default()
    }).await?;

    let Some((entry, tier)) = zbrain_core::skillpack::find_pack_with_tier(&loaded, &opts.name) else {
        println!("Skillpack '{}' not found in registry.", opts.name);
        std::process::exit(1);
    };

    println!("Name:        {}", entry.name);
    println!("Version:     {}", entry.version);
    println!("Author:      {}", entry.author);
    println!("Description: {}", entry.description);
    println!("Tier:        {:?} (registry default: {:?})", tier, entry.default_tier);
    println!("Source:      {}", entry.source.url);
    println!("Pinned:      {}", entry.source.pinned_commit);
    println!("Homepage:     {}", entry.homepage);
    println!("Tags:        {}", entry.tags.join(", "));
    println!("Validated:    {}", entry.validated_at);
    println!("Skills:       {} skill(s)", entry.skills.len());

    Ok(())
}

async fn run_install(opts: InstallOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let target_workspace = opts.workspace.unwrap_or(cwd);

    let resolved = zbrain_core::skillpack::resolve_source(&opts.spec, zbrain_core::skillpack::ResolveSourceOptions {
        ..Default::default()
    }).await?;

    let loaded = if !opts.spec.starts_with('.') && !opts.spec.contains('/') {
        Some(zbrain_core::skillpack::load_registry(zbrain_core::skillpack::LoadRegistryOptions {
            ..Default::default()
        }).await?)
    } else {
        None
    };

        let tier = if let Some(loaded) = &loaded {
            zbrain_core::skillpack::find_pack_with_tier(loaded, &opts.spec).map(|(_, t)| registry_to_skillpack_tier(t))
        } else {
            None
        };

    let result = zbrain_core::skillpack::run_scaffold_third_party(zbrain_core::skillpack::ScaffoldThirdPartyOptions {
        resolved,
        target_workspace: target_workspace.clone(),
        tier,
        trust_flag: if opts.trust { Some(true) } else { None },
        is_tty: None,
        state_path: None,
        dry_run: opts.dry_run,
    }, env!("CARGO_PKG_VERSION")).await?;

    match result.status {
        zbrain_core::skillpack::ScaffoldThirdPartyStatus::WroteNew => {
            println!("✅ Install complete: wrote {} file(s)", result.copy.as_ref().unwrap().summary.wrote_new);
            if !result.bootstrap.text.is_empty() {
                println!("\n{}", result.bootstrap.text);
            }
        }
        zbrain_core::skillpack::ScaffoldThirdPartyStatus::AllSkippedExisting => {
            println!("✅ All files already exist — nothing to do.");
        }
        zbrain_core::skillpack::ScaffoldThirdPartyStatus::DryRun => {
            println!("🔍 Dry-run complete: {} file(s) would be written", result.entries.len());
            for entry in result.entries {
                println!("  - {}", entry.rel_target.display());
            }
        }
        zbrain_core::skillpack::ScaffoldThirdPartyStatus::AbortedNoTrust => {
            println!("⛔ Installation aborted — user did not trust this skillpack.");
            std::process::exit(1);
        }
    }

    Ok(())
}

fn run_doctor_cli(opts: DoctorOptionsCli) -> Result<()> {
    let mode = if opts.quick { DoctorMode::Quick } else { DoctorMode::Full };
    let doctor_opts = zbrain_core::skillpack::DoctorOptions {
        pack_root: opts.pack_root,
        mode,
        fix: None,
        yes: None,
    };
    let result = zbrain_core::skillpack::run_doctor(&doctor_opts);

    match result {
        Ok(report) => {
            println!("🧪 Skillpack Doctor Report:\n");
            println!("Pack:             {} @ {}", report.pack_name, report.pack_version);
            println!("Rubric score:     {}/{}", report.score, report.max_score);
            println!("Tier eligibility: {}", report.tier_eligibility);

            if !report.promotion_blockers.is_empty() {
                println!("\n❌ Promotion blockers:");
                for b in &report.promotion_blockers {
                    println!("  - {}", b);
                }
            }

            if !report.dimensions.is_empty() {
                println!("\nDimensions:");
                for d in &report.dimensions {
                    println!("  - [{}] {}: {}", d.id, d.name, if d.passed { "✅" } else { "❌" });
                }
            }

            if report.promotion_blockers.is_empty() {
                println!("\n✅ This skillpack passes all publish-gate checks.");
            } else {
                println!("\n⚠️ This skillpack does not pass all publish-gate checks. Fix failures before publishing.");
                std::process::exit(1);
            }

            Ok(())
        }
        Err(e) => Err(anyhow!("Doctor failed: {}", e)),
    }
}

async fn run_pack(opts: PackOptionsCli) -> Result<()> {
    let pack_root = opts.pack_root;
    let out_dir = opts.out_dir.unwrap_or_else(|| pack_root.clone());

    let result = zbrain_core::skillpack::run_pack_publish(zbrain_core::skillpack::PackPublishOptions {
        pack_root: pack_root.clone(),
        out_dir: Some(out_dir.clone()),
        skip_doctor: opts.skip_doctor,
        dry_run: opts.dry_run,
    }).await?;

    if opts.dry_run {
        println!("🔍 Dry-run complete: doctor passed.");
        if let Some(doctor) = &result.doctor {
            println!("  Rubric score: {}/{}", doctor.score, doctor.max_score);
            println!("  Tier eligibility: {}", doctor.tier_eligibility);
        }
        if let Some(tier) = &result.tier_eligibility {
            println!("  Tier: {}", tier);
        }
    } else {
        println!("📦 Pack complete:");
        println!("  Pack: {} @ {}", result.pack_name, result.pack_version);
        if let Some(tarball) = &result.tarball {
            println!("  Output: {}", tarball.tarball_path.display());
            println!("  Size:   {} bytes", tarball.tarball_size);
            println!("  SHA256: {}", tarball.sha256_hex);
            if tarball.audit_passed {
                println!("  Audit:  ✅ Passed");
            } else {
                println!("  Audit:  ⚠️ Warnings found");
            }
        }
        if let Some(tier) = &result.tier_eligibility {
            println!("  Tier:   {}", tier);
        }
    }

    Ok(())
}

fn run_harvest_cli(opts: HarvestOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    // Harvest writes into THIS zbrain repo (the bundle root containing
    // openclaw.plugin.json), derived from cwd — the editorial workflow runs
    // `zbrain skillpack harvest` from within the zbrain repo, pulling the skill
    // FROM the host repo given by --from. `find_zbrain_root` matches the marker
    // `run_harvest` requires for add_to_bundle_manifest.
    let zbrain_root = find_zbrain_root(Some(&cwd))
        .ok_or_else(|| anyhow!("Could not find zbrain repo root (looking for openclaw.plugin.json)"))?;

    let result = zbrain_core::skillpack::run_harvest(zbrain_core::skillpack::HarvestOptions {
        slug: opts.slug,
        host_repo_root: opts.from,
        zbrain_root,
        no_lint: opts.no_lint,
        dry_run: opts.dry_run,
        private_patterns_path: None,
        overwrite_local: opts.overwrite,
    });

    match result {
        Ok(result) => {
            println!("🌾 Harvest complete:");
            println!("  Files copied: {}", result.files_copied.len());
            if !result.paired_sources.is_empty() {
                println!("  Paired sources: {}", result.paired_sources.join(", "));
            }
            if opts.dry_run {
                println!("  (dry-run - no files copied)");
            }
            Ok(())
        }
        Err(e) => Err(anyhow!("Harvest failed: {}", e)),
    }
}

fn run_scrub_legacy_cli(opts: ScrubLegacyOptionsCli) -> Result<()> {
    let result = zbrain_core::skillpack::run_scrub_legacy(zbrain_core::skillpack::ScrubLegacyOptions {
        target_workspace: opts.target_workspace.clone(),
        dry_run: opts.dry_run,
    });

    println!("🧹 Scrub legacy complete:");
    println!("  Rows removed: {}", result.removed.len());
    println!("  Rows preserved: {}", result.preserved.len());
    if opts.dry_run {
        println!("  (dry-run - no files modified)");
    }

    Ok(())
}

#[derive(Debug, Clone, Parser)]
pub struct ListOptionsCli {
    /// Output JSON instead of human-readable list.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct ReferenceOptionsCli {
    /// Skillpack name to diff.
    pub name: Option<String>,
    /// Sweep over every bundled skill instead of a single one.
    #[arg(long)]
    pub all: bool,
    /// Only apply clean (non-conflicting) hunks automatically.
    #[arg(long)]
    pub apply_clean_hunks: bool,
    /// Restrict to skills changed since this version (only with --all).
    #[arg(long)]
    pub since: Option<String>,
    /// Target workspace directory (defaults to auto-detected).
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Dry-run: show diffs without writing.
    #[arg(long)]
    pub dry_run: bool,
    /// Output stable JSON envelope instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct MigrateFenceOptionsCli {
    /// Target workspace directory (defaults to auto-detected).
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Dry-run: report what would change without writing.
    #[arg(long)]
    pub dry_run: bool,
    /// Output JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct CheckOptionsCli {
    /// Exit non-zero if any drift detected (CI gating).
    #[arg(long)]
    pub strict: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct RegistryOptionsCli {
    /// Set a new registry URL (persists to ~/.zbrain/config.json).
    #[arg(long)]
    pub url: Option<String>,
    /// Force fresh fetch from the current registry.
    #[arg(long)]
    pub refresh: bool,
    /// Output JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Parser)]
pub struct EndorseOptionsCli {
    /// Skillpack name as it appears in registry.json.
    pub name: String,
    /// Target tier (default: endorsed).
    #[arg(long)]
    pub tier: Option<String>,
    /// Path to a clone of the registry repo (default: current directory).
    #[arg(long)]
    pub repo: Option<PathBuf>,
    /// Optional human note recorded in endorsements.json.
    #[arg(long)]
    pub note: Option<String>,
    /// git push after committing (only makes sense in a clone).
    #[arg(long)]
    pub push: bool,
    /// Dry-run: report what would change without committing.
    #[arg(long)]
    pub dry_run: bool,
    /// Output JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

fn run_list(opts: ListOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let zbrain_root = find_zbrain_root(Some(&cwd))
        .ok_or_else(|| anyhow!("Could not find zbrain repo root (looking for openclaw.plugin.json)"))?;

    let (manifest, slugs) = bundle::bundled_skill_slugs(&zbrain_root)
        .map_err(|e| anyhow!("Failed to load bundle manifest: {}", e))?;

    if opts.json {
        use serde_json::json;
        let mut entries = Vec::new();
        for slug in &slugs {
            let description = zbrain_core::skillpack::bundle::get_skill_description(&zbrain_root, slug);
            entries.push(json!({
                "name": slug,
                "description": description
            }));
        }
        println!("{}", serde_json::to_string_pretty(&json!({
            "name": manifest.name,
            "version": manifest.version,
            "skills": entries
        }))?);
    } else {
        println!("{} {} — {} skills:", manifest.name, manifest.version, slugs.len());
        for slug in slugs {
            println!("  {}", slug);
        }
    }

    Ok(())
}

fn run_reference_cli(opts: ReferenceOptionsCli) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let zbrain_root = find_zbrain_root(Some(&cwd))
        .ok_or_else(|| anyhow!("Could not find zbrain repo root (looking for openclaw.plugin.json)"))?;
    let target_workspace = opts.workspace.unwrap_or_else(|| cwd.clone());

    let core_opts = zbrain_core::skillpack::reference::ReferenceOptions {
        zbrain_root,
        target_workspace,
        skill_slug: opts.name.clone(),
    };

    let result = zbrain_core::skillpack::reference::run_reference(&core_opts)?;

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", result.framing);
        println!();
        println!("  Summary: {} identical, {} differ, {} missing",
            result.summary.identical, result.summary.differs, result.summary.missing);
        for f in &result.files {
            let status = match f.status {
                zbrain_core::skillpack::reference::ReferenceStatus::Identical => "✅",
                zbrain_core::skillpack::reference::ReferenceStatus::Differs => "⚠️",
                zbrain_core::skillpack::reference::ReferenceStatus::Missing => "❌",
            };
            println!("  {} {}", status, f.target.display());
        }
    }

    Ok(())
}

async fn run_registry(opts: RegistryOptionsCli) -> Result<()> {
    use std::fs;
    use std::path::PathBuf;

    if let Some(url) = opts.url {
        let cfg_path = if let Some(home) = crate::config::zbrain_home() {
            home.join("config.json")
        } else {
            anyhow::bail!("Could not find zbrain home directory");
        };
        let mut cfg: serde_json::Value = if cfg_path.exists() {
            let content = fs::read_to_string(&cfg_path)?;
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            serde_json::json!({})
        };

        // Update or insert skillpack.registry_url
        if let Some(obj) = cfg.as_object_mut() {
            obj.insert("skillpack".into(), serde_json::json!({
                "registry_url": url
            }));
        }

        let tmp = PathBuf::from(format!("{}.tmp", cfg_path.display()));
        fs::create_dir_all(cfg_path.parent().unwrap())?;
        fs::write(&tmp, serde_json::to_string_pretty(&cfg)? + "\n")?;
        fs::rename(tmp, &cfg_path)?;
        println!("Set skillpack.registry_url = {}", url);
    }

    let loaded = load_registry(LoadRegistryOptions {
        refresh: opts.refresh,
        ..Default::default()
    }).await?;

    if opts.json {
        use serde_json::json;
        println!("{}", serde_json::to_string_pretty(&json!({
            "registry_url": loaded.registry_url,
            "origin": loaded.origin,
            "cache_age_ms": loaded.cache_age_ms,
            "skillpack_count": loaded.catalog.skillpacks.len(),
            "bundles": loaded.catalog.bundles.as_ref().map(|b| b.keys().collect::<Vec<_>>()).unwrap_or_default(),
        }))?);
    } else {
        println!("Registry: {}", loaded.registry_url);
        println!("Origin:   {}", loaded.origin);
        if let Some(age) = loaded.cache_age_ms {
            println!("Cache age: {}ms", age);
        }
        println!("Skillpacks: {}", loaded.catalog.skillpacks.len());
        if let Some(bundles) = &loaded.catalog.bundles {
            if !bundles.is_empty() {
                println!("Bundles:   {}", bundles.keys().cloned().collect::<Vec<_>>().join(", "));
            }
        }
    }

    Ok(())
}

fn run_endorse(opts: EndorseOptionsCli) -> Result<()> {
    let repo_root = opts.repo.unwrap_or_else(|| std::env::current_dir().unwrap());
    let tier = opts.tier.unwrap_or_else(|| "endorsed".into());

    let tier_parsed = match tier.as_str() {
        "endorsed" => registry_schema::RegistryTier::Endorsed,
        "community" => registry_schema::RegistryTier::Community,
        "experimental" => registry_schema::RegistryTier::Experimental,
        "dead" => registry_schema::RegistryTier::Dead,
        _ => anyhow::bail!("Invalid tier '{}' — must be endorsed|community|experimental|dead", tier),
    };

    let core_opts = zbrain_core::skillpack::EndorseOptions {
        registry_repo_root: repo_root,
        pack_name: opts.name,
        tier: Some(tier_parsed),
        note: opts.note.clone(),
        push: opts.push,
        dry_run: opts.dry_run,
    };

    let result = zbrain_core::skillpack::run_endorse(core_opts)
        .map_err(|e| anyhow!("Endorse failed: {}", e))?;

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let verb = if opts.dry_run { "would endorse" } else { "endorsed" };
        let from_to = if let Some(prior) = result.prior_tier {
            format!("{} -> {}", prior, tier)
        } else {
            format!("(unset) -> {}", tier)
        };
        println!("{}: {} {}", verb, result.pack_name, from_to);
        if let Some(commit_sha) = result.commit_sha {
            println!("  commit: {}", commit_sha);
        }
        if result.pushed {
            println!("  pushed to origin");
        }
        if opts.dry_run {
            println!("  (no writes; dry-run)");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// clap's own structural validation — catches duplicate flags, bad arg
    /// definitions, and conflicting shorthands across all 15 subcommands.
    #[test]
    fn skillpack_subcommand_definition_is_valid() {
        SkillpackSubcommand::command().debug_assert();
    }

    /// Every one of the 15 skillpack verbs must parse from its canonical
    /// invocation. This is the wiring guard: adding the 6 missing verbs
    /// (list/reference/migrate-fence/check/registry/endorse) must not have
    /// regressed the 9 that already worked.
    #[test]
    fn all_skillpack_verbs_parse() {
        let cases: &[&[&str]] = &[
            &["skillpack", "init", "my-pack"],
            &["skillpack", "scaffold", "some/repo"],
            &["skillpack", "search", "query"],
            &["skillpack", "info", "some-pack"],
            &["skillpack", "install", "owner/repo"],
            &["skillpack", "doctor", "/tmp/pack"],
            &["skillpack", "pack", "/tmp/pack"],
            &["skillpack", "harvest", "my-slug", "--from", "/tmp/host"],
            &["skillpack", "scrub-legacy-fence-rows", "/tmp/ws"],
            &["skillpack", "list"],
            &["skillpack", "reference", "some-pack"],
            &["skillpack", "migrate-fence"],
            &["skillpack", "check"],
            &["skillpack", "registry"],
            &["skillpack", "endorse", "some-pack"],
        ];
        for argv in cases {
            let parsed = SkillpackSubcommand::try_parse_from(*argv);
            assert!(
                parsed.is_ok(),
                "skillpack verb failed to parse: {:?} -> {:?}",
                argv,
                parsed.err()
            );
        }
    }

    /// `list --json` and `registry --url <u>` prove the new subcommands expose
    /// their documented flags (not just the bare verb).
    #[test]
    fn new_verbs_accept_documented_flags() {
        assert!(SkillpackSubcommand::try_parse_from(["skillpack", "list", "--json"]).is_ok());
        assert!(SkillpackSubcommand::try_parse_from([
            "skillpack", "registry", "--url", "https://example.com/registry.json"
        ])
        .is_ok());
        assert!(SkillpackSubcommand::try_parse_from([
            "skillpack", "endorse", "pack", "--tier", "endorsed", "--dry-run"
        ])
        .is_ok());
        assert!(SkillpackSubcommand::try_parse_from([
            "skillpack", "reference", "--all", "--dry-run"
        ])
        .is_ok());
    }
}
