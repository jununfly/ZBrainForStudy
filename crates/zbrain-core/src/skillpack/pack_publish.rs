/**
 * skillpack/pack_publish.rs — `zbrain skillpack pack` orchestrator.
 *
 * Runs the publisher's local validation + deterministic tarball emit:
 * 1. runDoctor(--quick) over the pack root; refuse if tier_eligibility
 *    is `blocked` (any core dim failing).
 * 2. packTarball into <out>/<name>-<version>.tgz with deterministic
 *    flags. Computes + reports SHA-256.
 * 3. Returns a structured result the CLI consumes.
 *
 * --dry-run runs the doctor only and skips the tarball step. --skip-doctor
 * is the escape hatch for the publish-gate skill which already runs the
 * doctor server-side. Validation results are persisted into the audit
 * log so the publish-gate skill can read the local-run history.
 */

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::skillpack::audit::log_skillpack_event;
use crate::skillpack::doctor::{run_doctor, DoctorResult, DoctorMode};
use crate::skillpack::rubric::RubricTier;
use crate::skillpack::manifest_v1::{parse_validate_manifest, SkillpackManifest, SkillpackManifestError};
use crate::skillpack::tarball::{pack_tarball, TarballOutput};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackPublishOptions {
    /// Absolute path to the pack root.
    pub pack_root: PathBuf,
    /// Output directory for the tarball (default: <packRoot>).
    pub out_dir: Option<PathBuf>,
    /// Skip the doctor gate (publish-gate skill uses this; the gate runs server-side).
    #[serde(default)]
    pub skip_doctor: bool,
    /// Dry-run: validate only, no tarball.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackPublishResult {
    pub schema_version: &'static str,
    pub pack_name: String,
    pub pack_version: String,
    pub doctor: Option<DoctorResult>,
    pub tarball: Option<crate::skillpack::tarball::TarballOutput>,
    pub tier_eligibility: Option<String>,
    pub refused_reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum PackPublishError {
    #[error("Failed to load skillpack.json: {0}")]
    ManifestLoadFailed(String),

    #[error("Doctor blocked publication: {0}")]
    DoctorBlocked(String),

    #[error("Tarball pack failed: {0}")]
    PackFailed(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Manifest error: {0}")]
    Manifest(#[from] SkillpackManifestError),
}

/// Run the full pack/publish flow: validate → pack → audit.
pub async fn run_pack_publish(opts: PackPublishOptions) -> Result<PackPublishResult, PackPublishError> {
    let manifest_path = opts.pack_root.join("skillpack.json");
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| PackPublishError::ManifestLoadFailed(e.to_string()))?;
    let manifest = parse_validate_manifest(&content)
        .map_err(|e| PackPublishError::Manifest(e.into()))?;

    let mut doctor: Option<DoctorResult> = None;
    if !opts.skip_doctor {
        doctor = Some(run_doctor(&crate::skillpack::doctor::DoctorOptions {
            pack_root: opts.pack_root.clone(),
            mode: DoctorMode::Quick,
            fix: None,
            yes: None,
        }).map_err(|e| PackPublishError::Manifest(e.into()))?);

        let tier_eligibility = doctor.as_ref().map(|d| format!("{:?}", d.tier_eligibility));

        if let Some(doc) = &doctor {
            if doc.tier_eligibility == crate::skillpack::rubric::RubricTier::Blocked {
                // Audit the refusal.
                let blockers: Vec<String> = doc.promotion_blockers.iter().cloned().collect();
                log_skillpack_event(&crate::skillpack::audit::SkillpackAuditEvent {
                    ts: chrono::Utc::now().to_rfc3339(),
                    event: crate::skillpack::audit::SkillpackAuditEventKind::DoctorRun,
                    pack: Some(manifest.name.clone()),
                    version: Some(manifest.version.clone()),
                    source: None,
                    source_kind: None,
                    pinned_commit: None,
                    tarball_sha256: None,
                    tier: tier_eligibility.clone(),
                    score: Some(doc.score as u8),
                    outcome: crate::skillpack::audit::AuditOutcome::Error,
                    error: Some(format!("pack refused: {}", blockers.join(", "))),
                    meta: Some(serde_json::json!({
                        "mode": "pack-publish-gate",
                        "score": doc.score
                    })),
                });

                return Ok(PackPublishResult {
                    schema_version: "skillpack-pack-v1",
                    pack_name: manifest.name,
                    pack_version: manifest.version,
                    doctor: doctor.clone(),
                    tarball: None,
                    tier_eligibility: Some("blocked".to_string()),
                    refused_reason: Some(format!("doctor blocked: {}", blockers.join(", "))),
                });
            }
        }
    }

    if opts.dry_run {
        return Ok(PackPublishResult {
            schema_version: "skillpack-pack-v1",
            pack_name: manifest.name,
            pack_version: manifest.version,
            doctor: doctor.clone(),
            tarball: None,
            tier_eligibility: doctor.as_ref().map(|d| format!("{:?}", d.tier_eligibility)),
            refused_reason: None,
        });
    }

    // Pack tarball into <outDir>/<name>-<version>.tgz.
    let out_dir = opts.out_dir.unwrap_or_else(|| opts.pack_root.clone());
    fs::create_dir_all(&out_dir)?;
    let out_path = out_dir.join(format!("{}-{}.tgz", manifest.name, manifest.version));

    // pack_tarball currently doesn't support exclude list directly
    // exclude list will need to be handled by caller
    let tarball = pack_tarball(&opts.pack_root, &out_path)
        .map_err(|e| PackPublishError::PackFailed(e.to_string()))?;

    let tier_eligibility = doctor.as_ref().map(|d| format!("{:?}", d.tier_eligibility));
    let score = doctor.as_ref().map(|d| d.score);

    log_skillpack_event(&crate::skillpack::audit::SkillpackAuditEvent {
        ts: chrono::Utc::now().to_rfc3339(),
        event: crate::skillpack::audit::SkillpackAuditEventKind::DoctorRun,
        pack: Some(manifest.name.clone()),
        version: Some(manifest.version.clone()),
        source: None,
        source_kind: None,
        pinned_commit: None,
        tarball_sha256: Some(tarball.sha256_hex.clone()),
        tier: tier_eligibility.clone(),
            score: score.map(|s| s.try_into().unwrap()),
            outcome: crate::skillpack::audit::AuditOutcome::Ok,
        error: None,
        meta: Some(serde_json::json!({
            "mode": "pack-publish-gate",
            "score": score,
            "tier": tier_eligibility,
            "tarball_sha256": tarball.sha256_hex,
        })),
    });

    Ok(PackPublishResult {
        schema_version: "skillpack-pack-v1",
        pack_name: manifest.name,
        pack_version: manifest.version,
        doctor,
        tarball: Some(tarball),
        tier_eligibility,
        refused_reason: None,
    })
}
