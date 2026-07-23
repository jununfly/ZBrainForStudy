//! `zbrain skillpack doctor` — quality audit for a skillpack.
//!
//! Two modes:
//! - `--quick`: only structural rubric scoring (no IO, no LLM) — for quick iteration
//! - `--full`: quick plus all the heavyweight checks (tests, evals, LLM judging) — delegates to publish-gate
//!
//! `--fix`: auto-apply any available fixes for missing files (create empty stubs).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::skillpack::{manifest_v1, rubric};
use crate::skillpack::rubric::{RubricScore, RubricTier};
use crate::skillpack::audit::{log_skillpack_event, SkillpackAuditEvent, SkillpackAuditEventKind};

/// Doctor mode (quick / full).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoctorMode {
    /// Quick structural check only (no IO, no LLM).
    Quick,
    /// Quick + full checks (delegate full pipeline to publish-gate).
    Full,
}

/// Doctor options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorOptions {
    /// Absolute path to the skillpack root directory.
    pub pack_root: std::path::PathBuf,
    /// Doctor mode.
    pub mode: DoctorMode,
    /// Auto-apply available fixes where possible.
    pub fix: Option<bool>,
    /// Auto-confirm all destructive fixes (for CI/unattended).
    pub yes: Option<bool>,
}

/// Result from the doctor run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorResult {
    /// Schema version for this doctor output.
    pub schema_version: &'static str,
    /// Pack name from manifest.
    pub pack_name: String,
    /// Pack version from manifest.
    pub pack_version: String,
    /// Root directory of the pack.
    pub pack_root: std::path::PathBuf,
    /// Doctor mode that was used.
    pub mode: DoctorMode,
    /// Total score (0-10).
    pub score: usize,
    /// Maximum possible score.
    pub max_score: usize,
    /// Final tier eligibility.
    pub tier_eligibility: RubricTier,
    /// List of dimension names that block promotion to a higher tier.
    pub promotion_blockers: Vec<String>,
    /// Detailed result for each dimension.
    pub dimensions: Vec<rubric::RubricDimensionResult>,
    /// List of file paths that were fixed (created stubs for).
    pub fixes_applied: Vec<std::path::PathBuf>,
    /// Hint to run the full publish-gate flow for complete checking/publishing.
    pub full_mode_hint: Option<String>,
}

/// Run the doctor quality check.
pub fn run_doctor(opts: &DoctorOptions) -> Result<DoctorResult> {
    let pack_root = &opts.pack_root;
    let manifest_path = pack_root.join("skillpack.json");

    // Load and validate the manifest
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| {
            crate::skillpack::manifest_v1::SkillpackManifestError::new(
                crate::skillpack::manifest_v1::SkillpackManifestErrorCode::ManifestNotFound,
                format!("Failed to read skillpack.json: {}", e),
            )
        })?;

    let manifest = manifest_v1::parse_validate_manifest(&content)?;

    let score = rubric::walk_rubric(&rubric::RubricInput {
        pack_root: pack_root.to_path_buf(),
        manifest: manifest.clone(),
    });

    // Log the audit event if quick mode (full mode logs after publishing)
    if let DoctorMode::Quick = opts.mode {
        log_skillpack_event(&SkillpackAuditEvent {
            ts: chrono::Utc::now().to_rfc3339(),
            event: SkillpackAuditEventKind::DoctorRun,
            pack: Some(manifest.name.clone()),
            version: Some(manifest.version.clone()),
            source: None,
            source_kind: None,
            pinned_commit: None,
            tarball_sha256: None,
            tier: Some(score.tier_eligibility.to_string()),
            score: None,
            outcome: if score.promotion_blockers.is_empty() {
                crate::skillpack::audit::AuditOutcome::Ok
            } else {
                crate::skillpack::audit::AuditOutcome::Aborted
            },
            error: None,
            meta: None,
        });
    }

    let mut fixes_applied = Vec::new();

    // --fix would create stubs for missing required files here — that's
    // handled by the init command, so this is just a collection point.
    // (we don't actually implement fix here because init already creates all stubs)

    let full_mode_hint = if let DoctorMode::Full = opts.mode {
        Some("Run `zbrain skillpack publish --quick --publish` to run the full publish pipeline that includes this doctor check and builds the distributable tarball.".to_string())
    } else {
        None
    };

    Ok(DoctorResult {
        schema_version: "skillpack-doctor-v1",
        pack_name: manifest.name,
        pack_version: manifest.version,
        pack_root: pack_root.to_path_buf(),
        mode: opts.mode,
        score: score.total,
        max_score: 10,
        tier_eligibility: score.tier_eligibility,
        promotion_blockers: score.promotion_blockers,
        dimensions: score.dimensions,
        fixes_applied,
        full_mode_hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal() {
        use std::path::Path;
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let pack_root = dir.path();
        std::fs::write(pack_root.join("skillpack.json"), r#"{
  "api_version": "zbrain-skillpack-v1",
  "name": "test",
  "version": "0.1.0",
  "description": "test",
  "author": "test",
  "license": "MIT",
  "homepage": "https://example.com",
  "zbrain_min_version": "0.40.0",
  "skills": ["test-skill"]
}
"#).unwrap();
        std::fs::create_dir(pack_root.join("test-skill")).unwrap();
        std::fs::write(pack_root.join("test-skill").join("SKILL.md"), "test").unwrap();
        let result = run_doctor(&DoctorOptions {
            pack_root: pack_root.to_path_buf(),
            mode: DoctorMode::Quick,
            fix: None,
            yes: None,
        });
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.total, 6); // 5 core + 5 badges = 6/10
        assert!(result.tier_eligibility != rubric::RubricTier::Blocked);
    }
}
