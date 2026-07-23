//! Skillpack subsystem — package and install skillpack artifacts.
//!
//! Skillpack is a packaged distribution of one or more ZBrain skills,
//! containing compiled skill metadata, trigger index, routing evaluation fixtures,
//! and can be installed locally from a tarball or a directory.

pub mod apply_hunks;
pub mod audit;
pub mod bootstrap_display;
pub mod bundle;
pub mod copy;
pub mod diff_text;
pub mod doctor;
pub mod endorse;
pub mod harvest;
pub mod harvest_lint;
pub mod init_scaffold;
pub mod installer;
pub mod manifest_v1;
pub mod pack_publish;
pub mod post_install_advisory;
pub mod reference;
#[cfg(feature = "skillpack")]
pub mod registry_client;
pub mod registry_schema;
pub mod remote_source;
pub mod rubric;
pub mod scaffold;
pub mod scaffold_third_party;
pub mod scrub_legacy;
pub mod state;
pub mod tarball;
pub mod trust_prompt;

// Re-export the public skillpack API consumed by the CLI and other callers.
pub use crate::skillpack::copy::CopyItem;
pub use crate::skillpack::doctor::{run_doctor, DoctorOptions};
pub use crate::skillpack::harvest::{run_harvest, HarvestOptions};
pub use crate::skillpack::init_scaffold::{run_init_scaffold, InitScaffoldOptions};
pub use crate::skillpack::pack_publish::{run_pack_publish, PackPublishOptions};
pub use crate::skillpack::remote_source::{resolve_source, ResolveSourceOptions};
pub use crate::skillpack::scaffold::{run_scaffold, ScaffoldOptions};
pub use crate::skillpack::scaffold_third_party::{
    run_scaffold_third_party, ScaffoldThirdPartyOptions, ScaffoldThirdPartyStatus,
};
pub use crate::skillpack::scrub_legacy::{run_scrub_legacy, ScrubLegacyOptions};
pub use crate::skillpack::trust_prompt::SkillpackTier;

#[cfg(feature = "skillpack")]
pub use crate::skillpack::registry_client::{
    find_pack_with_tier, load_registry, search_packs, LoadRegistryOptions,
};

use serde::{Deserialize, Serialize};

/// Discriminator for artifact type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// Schema pack — collection of schema type definitions.
    SchemaPack,
    /// Skill pack — collection of ZBrain agent skills.
    SkillPack,
}

impl ArtifactKind {
    /// Get the expected manifest filename for this artifact kind.
    #[must_use]
    pub fn manifest_filename(&self) -> &'static str {
        match self {
            Self::SchemaPack => "pack.yaml",
            Self::SkillPack => "skillpack.json",
        }
    }

    /// Get the API version string expected in the manifest.
    #[must_use]
    pub fn api_version(&self) -> &'static str {
        match self {
            Self::SchemaPack => "zbrain-schema-pack-v1",
            Self::SkillPack => "zbrain-skillpack-v1",
        }
    }

    /// Get the file extension for this artifact kind when packed as a tarball.
    #[must_use]
    pub fn extension(&self) -> &'static str {
        match self {
            Self::SchemaPack => ".zbrain-schema",
            Self::SkillPack => ".zbrain-skillpack",
        }
    }
}

/// Descriptor for a detected artifact on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactDescriptor {
    /// Kind of the artifact (skillpack or schemapack).
    pub kind: ArtifactKind,
    /// Name of the artifact.
    pub name: String,
    /// Version of the artifact.
    pub version: String,
    /// Absolute path to the artifact root (file or directory).
    pub path: String,
    /// Parsed + validated manifest object. Shape varies by kind.
    pub manifest: serde_json::Value,
}
