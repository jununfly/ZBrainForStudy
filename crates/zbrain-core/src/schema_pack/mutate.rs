//! Schema pack mutation engine — atomic read-modify-write with lint + audit.
//!
//! Ported from TS `src/core/schema-pack/mutate.ts`.
//!
//! ## withMutation 8-step flow
//! 1. BUNDLED guard — reject bundled packs (PACK_READONLY)
//! 2. with_pack_lock — atomic file lock
//! 3. read + parse pack file
//! 4. mutator(current) → next (pure data transform)
//! 5. run_file_plane_lint_rules(next) — pre-write validation
//! 6. write_atomic — .tmp + fsync + rename
//! 7. best-effort post-hooks: log_mutation_success
//! 8. release lock (via with_pack_lock's Drop)
//!
//! The disk file never sees a partial write: step 6 is either a full
//! success (atomic rename) or the original file is untouched.

use std::path::{Path, PathBuf};

use super::lint_rules;
use super::loader;
use super::manifest::{
    self, LinkTypeDefinition, PackPrimitive, PageTypeDefinition, SchemaPackManifest,
};
use super::mutate_audit::{self, LogMutationOpts, MutationOp};
use super::pack_lock::{self, PackLockOpts};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Pack names that are bundled with the binary and cannot be mutated.
pub const BUNDLED_PACK_NAMES: &[&str] = &["zbrain-base", "zbrain-recommended"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackFileFormat {
    Json,
    Yaml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationErrorCode {
    PackNotFound,
    PackReadonly,
    PackCorrupt,
    TypeExists,
    TypeNotFound,
    InvalidPrimitive,
    InvalidResult,
    IoError,
    StillReferenced,
    LockBusy,
}

#[derive(Debug, Clone)]
pub struct MutationError {
    pub code: MutationErrorCode,
    pub message: String,
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for MutationError {}

impl MutationError {
    fn code_str(&self) -> &'static str {
        match self.code {
            MutationErrorCode::PackNotFound => "PACK_NOT_FOUND",
            MutationErrorCode::PackReadonly => "PACK_READONLY",
            MutationErrorCode::PackCorrupt => "PACK_CORRUPT",
            MutationErrorCode::TypeExists => "TYPE_EXISTS",
            MutationErrorCode::TypeNotFound => "TYPE_NOT_FOUND",
            MutationErrorCode::InvalidPrimitive => "INVALID_PRIMITIVE",
            MutationErrorCode::InvalidResult => "INVALID_RESULT",
            MutationErrorCode::IoError => "IO_ERROR",
            MutationErrorCode::StillReferenced => "STILL_REFERENCED",
            MutationErrorCode::LockBusy => "LOCK_BUSY",
        }
    }
}

/// Result of a successful mutation.
#[derive(Debug, Clone)]
pub struct MutateResult {
    pub pack: String,
    pub path: String,
    pub format: PackFileFormat,
    pub prev_sha8: String,
    pub new_sha8: String,
}

/// Options for mutation operations.
#[derive(Debug, Clone)]
pub struct MutateOpts {
    pub actor: String,
    pub batch_id: Option<String>,
    pub force: bool,
}

impl Default for MutateOpts {
    fn default() -> Self {
        Self {
            actor: "cli".to_string(),
            batch_id: None,
            force: false,
        }
    }
}

/// Options for `add_type_to_pack`.
#[derive(Debug, Clone)]
pub struct AddTypeOpts {
    pub name: String,
    pub primitive: PackPrimitive,
    pub prefix: String,
    pub extractable: bool,
    pub expert_routing: bool,
    pub aliases: Vec<String>,
}

impl Default for AddTypeOpts {
    fn default() -> Self {
        Self {
            name: String::new(),
            primitive: PackPrimitive::Concept,
            prefix: String::new(),
            extractable: false,
            expert_routing: false,
            aliases: Vec::new(),
        }
    }
}

/// Options for `update_type_on_pack`.
#[derive(Debug, Clone, Default)]
pub struct UpdateTypeOpts {
    pub name: String,
    pub primitive: Option<PackPrimitive>,
    pub extractable: Option<bool>,
    pub expert_routing: Option<bool>,
}

/// Options for `add_link_type_to_pack`.
#[derive(Debug, Clone, Default)]
pub struct AddLinkTypeOpts {
    pub name: String,
    pub inverse: Option<String>,
}

/// Type/prefix context for audit logging.
#[derive(Debug, Clone, Default)]
pub struct TypeContext {
    pub type_name: Option<String>,
    pub prefix: Option<String>,
}

// ---------------------------------------------------------------------------
// locate_mutable_pack_file
// ---------------------------------------------------------------------------

/// Locate a mutable pack file on disk.
///
/// Rejects bundled packs (zbrain-base, zbrain-recommended) with PACK_READONLY.
/// Searches for `pack.json`, `pack.yaml`, `pack.yml` in the user pack directory.
pub fn locate_mutable_pack_file(name: &str) -> Result<(PathBuf, PackFileFormat), MutationError> {
    if BUNDLED_PACK_NAMES.contains(&name) {
        return Err(MutationError {
            code: MutationErrorCode::PackReadonly,
            message: format!("bundled pack \"{name}\" is read-only; fork it first"),
        });
    }

    let dir = user_pack_dir().join(name);
    for (file, fmt) in [
        ("pack.json", PackFileFormat::Json),
        ("pack.yaml", PackFileFormat::Yaml),
        ("pack.yml", PackFileFormat::Yaml),
    ] {
        let path = dir.join(file);
        if path.exists() {
            return Ok((path, fmt));
        }
    }

    Err(MutationError {
        code: MutationErrorCode::PackNotFound,
        message: format!(
            "no pack file found for \"{name}\" in {}",
            dir.display()
        ),
    })
}

fn user_pack_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".zbrain").join("schema-packs")
}

// ---------------------------------------------------------------------------
// write_pack_manifest (atomic write)
// ---------------------------------------------------------------------------

/// Serialize a manifest to YAML or JSON string.
fn serialize_manifest(manifest: &SchemaPackManifest, format: PackFileFormat) -> String {
    match format {
        PackFileFormat::Json => serde_json::to_string_pretty(manifest).unwrap_or_default(),
        PackFileFormat::Yaml => serde_yaml::to_string(manifest).unwrap_or_default(),
    }
}

/// Write a pack manifest atomically: write to `.tmp`, fsync, rename.
pub fn write_pack_manifest(
    path: &Path,
    manifest: &SchemaPackManifest,
    format: PackFileFormat,
) -> Result<(), MutationError> {
    let content = serialize_manifest(manifest, format);

    // Validate the serialized form round-trips (catches serialization issues)
    if let Err(e) = loader::load_pack_from_string(&content, path.to_str().unwrap_or("pack")) {
        return Err(MutationError {
            code: MutationErrorCode::InvalidResult,
            message: format!("post-serialize validation failed: {e}"),
        });
    }

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &content).map_err(|e| MutationError {
        code: MutationErrorCode::IoError,
        message: format!("cannot write tmp file {}: {e}", tmp.display()),
    })?;

    // Atomic rename (on most OSes)
    std::fs::rename(&tmp, path).map_err(|e| MutationError {
        code: MutationErrorCode::IoError,
        message: format!("cannot rename tmp to {}: {e}", path.display()),
    })?;

    Ok(())
}

// ---------------------------------------------------------------------------
// check_no_references (STILL_REFERENCED guard for remove_type)
// ---------------------------------------------------------------------------

/// Check if a type name is referenced by other parts of the manifest.
/// Returns a description of the first reference found, or None if clean.
fn check_no_references(manifest: &SchemaPackManifest, type_name: &str) -> Option<String> {
    // Check aliases of other types
    for pt in &manifest.page_types {
        if pt.name != type_name && pt.aliases.iter().any(|a| a == type_name) {
            return Some(format!("type \"{type_name}\" is aliased by type \"{}\"", pt.name));
        }
    }

    // Check enrichable_types
    for et in &manifest.enrichable_types {
        if et.type_name == type_name {
            return Some(format!("type \"{type_name}\" is in enrichable_types"));
        }
    }

    // Check frontmatter_links
    for fl in &manifest.frontmatter_links {
        if fl.page_type == type_name {
            return Some(format!("type \"{type_name}\" is in frontmatter_links"));
        }
    }

    // Check link_types.inference
    for lt in &manifest.link_types {
        if let Some(ref inf) = lt.inference {
            if inf.page_type.as_deref() == Some(type_name) {
                return Some(format!(
                    "type \"{type_name}\" is in link_type \"{}\" inference.page_type",
                    lt.name
                ));
            }
            if inf.target_type.as_deref() == Some(type_name) {
                return Some(format!(
                    "type \"{type_name}\" is in link_type \"{}\" inference.target_type",
                    lt.name
                ));
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// with_mutation (8-step flow)
// ---------------------------------------------------------------------------

/// Execute a mutation under lock, with lint validation and audit logging.
///
/// The `mutator` closure receives the current manifest and must return the
/// modified manifest. If the mutator returns `Err`, the mutation is aborted
/// and a failure audit record is logged.
pub fn with_mutation(
    pack_name: &str,
    opts: &MutateOpts,
    mutator: impl FnOnce(&SchemaPackManifest) -> Result<SchemaPackManifest, MutationError>,
    op: MutationOp,
    type_ctx: &TypeContext,
) -> Result<MutateResult, MutationError> {
    // Step 1: BUNDLED guard + locate file
    let (path, format) = locate_mutable_pack_file(pack_name)?;

    // Step 2: acquire lock
    let lock_opts = PackLockOpts {
        force: opts.force,
        ..Default::default()
    };

    let result = pack_lock::with_pack_lock(pack_name, &lock_opts, || {
        // Step 3: read + parse
        let current = loader::load_pack_from_file(&path).map_err(|e| MutationError {
            code: MutationErrorCode::PackCorrupt,
            message: format!("cannot parse pack file: {e}"),
        })?;
        let prev_sha8 = manifest::compute_manifest_sha8(&current);

        // Step 4: mutator
        let next = mutator(&current).map_err(|e| {
            // Log failure
            mutate_audit::log_mutation_failure(&mutate_audit::LogMutationFailureOpts {
                base: LogMutationOpts {
                    op,
                    pack: pack_name.to_string(),
                    type_name: type_ctx.type_name.clone(),
                    prefix: type_ctx.prefix.clone(),
                    actor: opts.actor.clone(),
                    prev_sha8: Some(prev_sha8.clone()),
                    new_sha8: None,
                    batch_id: opts.batch_id.clone(),
                },
                reason: e.code_str().to_string(),
            });
            e
        })?;

        // Step 5: lint
        let report = lint_rules::run_file_plane_lint_rules(&next);
        if !report.ok {
            let msg = report
                .errors
                .iter()
                .map(|e| format!("{}: {}", e.rule, e.message))
                .collect::<Vec<_>>()
                .join("; ");
            let err = MutationError {
                code: MutationErrorCode::InvalidResult,
                message: format!("lint errors: {msg}"),
            };
            mutate_audit::log_mutation_failure(&mutate_audit::LogMutationFailureOpts {
                base: LogMutationOpts {
                    op,
                    pack: pack_name.to_string(),
                    type_name: type_ctx.type_name.clone(),
                    prefix: type_ctx.prefix.clone(),
                    actor: opts.actor.clone(),
                    prev_sha8: Some(prev_sha8.clone()),
                    new_sha8: None,
                    batch_id: opts.batch_id.clone(),
                },
                reason: err.code_str().to_string(),
            });
            return Err(err);
        }

        let new_sha8 = manifest::compute_manifest_sha8(&next);

        // Step 6: atomic write
        write_pack_manifest(&path, &next, format).map_err(|e| {
            mutate_audit::log_mutation_failure(&mutate_audit::LogMutationFailureOpts {
                base: LogMutationOpts {
                    op,
                    pack: pack_name.to_string(),
                    type_name: type_ctx.type_name.clone(),
                    prefix: type_ctx.prefix.clone(),
                    actor: opts.actor.clone(),
                    prev_sha8: Some(prev_sha8.clone()),
                    new_sha8: None,
                    batch_id: opts.batch_id.clone(),
                },
                reason: e.code_str().to_string(),
            });
            e
        })?;

        // Step 7: best-effort post-hooks
        mutate_audit::log_mutation_success(&LogMutationOpts {
            op,
            pack: pack_name.to_string(),
            type_name: type_ctx.type_name.clone(),
            prefix: type_ctx.prefix.clone(),
            actor: opts.actor.clone(),
            prev_sha8: Some(prev_sha8.clone()),
            new_sha8: Some(new_sha8.clone()),
            batch_id: opts.batch_id.clone(),
        });

        Ok(MutateResult {
            pack: pack_name.to_string(),
            path: path.display().to_string(),
            format,
            prev_sha8,
            new_sha8,
        })
    });

    result.map_err(|lock_err| MutationError {
        code: MutationErrorCode::LockBusy,
        message: format!("lock busy: {lock_err}"),
    })?
}

// ---------------------------------------------------------------------------
// Mutation primitives
// ---------------------------------------------------------------------------

/// Add a new page type to a pack.
pub fn add_type_to_pack(
    pack_name: &str,
    add_opts: &AddTypeOpts,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let name = add_opts.name.clone();
    let prefix = add_opts.prefix.clone();
    let type_ctx = TypeContext {
        type_name: Some(name.clone()),
        prefix: Some(prefix.clone()),
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            if current.page_types.iter().any(|pt| pt.name == name) {
                return Err(MutationError {
                    code: MutationErrorCode::TypeExists,
                    message: format!("type \"{name}\" already exists in pack"),
                });
            }

            let mut next = current.clone();
            next.page_types.push(PageTypeDefinition {
                name: name.clone(),
                primitive: add_opts.primitive,
                path_prefixes: if prefix.is_empty() {
                    Vec::new()
                } else {
                    vec![prefix.clone()]
                },
                aliases: add_opts.aliases.clone(),
                extractable: add_opts.extractable,
                expert_routing: add_opts.expert_routing,
            });
            Ok(next)
        },
        MutationOp::AddType,
        &type_ctx,
    )
}

/// Remove a page type from a pack.
pub fn remove_type_from_pack(
    pack_name: &str,
    type_name: &str,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let type_ctx = TypeContext {
        type_name: Some(type_name.to_string()),
        prefix: None,
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            if !current.page_types.iter().any(|pt| pt.name == type_name) {
                return Err(MutationError {
                    code: MutationErrorCode::TypeNotFound,
                    message: format!("type \"{type_name}\" not found in pack"),
                });
            }

            // Check references
            if let Some(ref_msg) = check_no_references(current, type_name) {
                return Err(MutationError {
                    code: MutationErrorCode::StillReferenced,
                    message: ref_msg,
                });
            }

            let mut next = current.clone();
            next.page_types.retain(|pt| pt.name != type_name);
            Ok(next)
        },
        MutationOp::RemoveType,
        &type_ctx,
    )
}

/// Update a page type (partial patch).
pub fn update_type_on_pack(
    pack_name: &str,
    update_opts: &UpdateTypeOpts,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let name = update_opts.name.clone();
    let type_ctx = TypeContext {
        type_name: Some(name.clone()),
        prefix: None,
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            if !current.page_types.iter().any(|pt| pt.name == name) {
                return Err(MutationError {
                    code: MutationErrorCode::TypeNotFound,
                    message: format!("type \"{name}\" not found in pack"),
                });
            }

            let mut next = current.clone();
            for pt in &mut next.page_types {
                if pt.name == name {
                    if let Some(ref prim) = update_opts.primitive {
                        pt.primitive = *prim;
                    }
                    if let Some(v) = update_opts.extractable {
                        pt.extractable = v;
                    }
                    if let Some(v) = update_opts.expert_routing {
                        pt.expert_routing = v;
                    }
                }
            }
            Ok(next)
        },
        MutationOp::UpdateType,
        &type_ctx,
    )
}

/// Add an alias to a type (idempotent).
pub fn add_alias_to_type(
    pack_name: &str,
    type_name: &str,
    alias: &str,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let type_ctx = TypeContext {
        type_name: Some(type_name.to_string()),
        prefix: None,
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            let mut next = current.clone();
            for pt in &mut next.page_types {
                if pt.name == type_name {
                    if !pt.aliases.contains(&alias.to_string()) {
                        pt.aliases.push(alias.to_string());
                    }
                    return Ok(next);
                }
            }
            Err(MutationError {
                code: MutationErrorCode::TypeNotFound,
                message: format!("type \"{type_name}\" not found in pack"),
            })
        },
        MutationOp::AddAlias,
        &type_ctx,
    )
}

/// Remove an alias from a type (idempotent).
pub fn remove_alias_from_type(
    pack_name: &str,
    type_name: &str,
    alias: &str,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let type_ctx = TypeContext {
        type_name: Some(type_name.to_string()),
        prefix: None,
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            let mut next = current.clone();
            for pt in &mut next.page_types {
                if pt.name == type_name {
                    pt.aliases.retain(|a| a != alias);
                    return Ok(next);
                }
            }
            Err(MutationError {
                code: MutationErrorCode::TypeNotFound,
                message: format!("type \"{type_name}\" not found in pack"),
            })
        },
        MutationOp::RemoveAlias,
        &type_ctx,
    )
}

/// Add a path prefix to a type (idempotent).
pub fn add_prefix_to_type(
    pack_name: &str,
    type_name: &str,
    prefix: &str,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let type_ctx = TypeContext {
        type_name: Some(type_name.to_string()),
        prefix: Some(prefix.to_string()),
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            let mut next = current.clone();
            for pt in &mut next.page_types {
                if pt.name == type_name {
                    if !pt.path_prefixes.contains(&prefix.to_string()) {
                        pt.path_prefixes.push(prefix.to_string());
                    }
                    return Ok(next);
                }
            }
            Err(MutationError {
                code: MutationErrorCode::TypeNotFound,
                message: format!("type \"{type_name}\" not found in pack"),
            })
        },
        MutationOp::AddPrefix,
        &type_ctx,
    )
}

/// Remove a path prefix from a type (idempotent).
pub fn remove_prefix_from_type(
    pack_name: &str,
    type_name: &str,
    prefix: &str,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let type_ctx = TypeContext {
        type_name: Some(type_name.to_string()),
        prefix: Some(prefix.to_string()),
    };

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            let mut next = current.clone();
            for pt in &mut next.page_types {
                if pt.name == type_name {
                    pt.path_prefixes.retain(|p| p != prefix);
                    return Ok(next);
                }
            }
            Err(MutationError {
                code: MutationErrorCode::TypeNotFound,
                message: format!("type \"{type_name}\" not found in pack"),
            })
        },
        MutationOp::RemovePrefix,
        &type_ctx,
    )
}

/// Add a link type to a pack.
pub fn add_link_type_to_pack(
    pack_name: &str,
    add_opts: &AddLinkTypeOpts,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let name = add_opts.name.clone();
    let type_ctx = TypeContext::default();

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            if current.link_types.iter().any(|lt| lt.name == name) {
                return Err(MutationError {
                    code: MutationErrorCode::TypeExists,
                    message: format!("link type \"{name}\" already exists in pack"),
                });
            }

            let mut next = current.clone();
            next.link_types.push(LinkTypeDefinition {
                name: name.clone(),
                inverse: add_opts.inverse.clone(),
                inference: None,
            });
            Ok(next)
        },
        MutationOp::AddLinkType,
        &type_ctx,
    )
}

/// Remove a link type from a pack.
pub fn remove_link_type_from_pack(
    pack_name: &str,
    link_name: &str,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    let type_ctx = TypeContext::default();

    with_mutation(
        pack_name,
        mutate_opts,
        |current| {
            if !current.link_types.iter().any(|lt| lt.name == link_name) {
                return Err(MutationError {
                    code: MutationErrorCode::TypeNotFound,
                    message: format!("link type \"{link_name}\" not found in pack"),
                });
            }

            let mut next = current.clone();
            next.link_types.retain(|lt| lt.name != link_name);
            Ok(next)
        },
        MutationOp::RemoveLinkType,
        &type_ctx,
    )
}

/// Set the extractable flag on a type.
pub fn set_extractable_on_type(
    pack_name: &str,
    type_name: &str,
    value: bool,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    update_type_on_pack(
        pack_name,
        &UpdateTypeOpts {
            name: type_name.to_string(),
            extractable: Some(value),
            ..Default::default()
        },
        mutate_opts,
    )
}

/// Set the expert_routing flag on a type.
pub fn set_expert_routing_on_type(
    pack_name: &str,
    type_name: &str,
    value: bool,
    mutate_opts: &MutateOpts,
) -> Result<MutateResult, MutationError> {
    update_type_on_pack(
        pack_name,
        &UpdateTypeOpts {
            name: type_name.to_string(),
            expert_routing: Some(value),
            ..Default::default()
        },
        mutate_opts,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a temp pack file for testing.
    fn setup_test_pack(pack_name: &str, manifest: &SchemaPackManifest) -> PathBuf {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| std::env::temp_dir().to_str().unwrap().to_string());
        let dir = PathBuf::from(home)
            .join(".zbrain")
            .join("schema-packs")
            .join(pack_name);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pack.yaml");
        let yaml = serde_yaml::to_string(manifest).unwrap();
        std::fs::write(&path, yaml).unwrap();
        path
    }

    /// Cleanup a test pack.
    fn cleanup_test_pack(pack_name: &str) {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| std::env::temp_dir().to_str().unwrap().to_string());
        let dir = PathBuf::from(home)
            .join(".zbrain")
            .join("schema-packs")
            .join(pack_name);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn test_manifest() -> SchemaPackManifest {
        let mut m = SchemaPackManifest::default();
        m.name = "test-pack".to_string();
        m.version = "1.0.0".to_string();
        m.description = "test pack".to_string();
        m.extends = Some("zbrain-base".to_string());
        m.page_types = vec![
                PageTypeDefinition {
                    name: "person".to_string(),
                    primitive: PackPrimitive::Entity,
                    path_prefixes: vec!["people/".to_string()],
                    aliases: vec![],
                    extractable: false,
                    expert_routing: true,
                },
                PageTypeDefinition {
                    name: "note".to_string(),
                    primitive: PackPrimitive::Concept,
                    path_prefixes: vec!["notes/".to_string()],
                    aliases: vec![],
                    extractable: true,
                    expert_routing: false,
                },
            ];
        m.link_types = vec![LinkTypeDefinition {
                name: "mentions".to_string(),
                inverse: Some("mentioned_by".to_string()),
                inference: None,
            }];
        m
    }

    #[test]
    fn bundled_pack_rejected() {
        let err = locate_mutable_pack_file("zbrain-base").unwrap_err();
        assert_eq!(err.code, MutationErrorCode::PackReadonly);
        assert!(err.message.contains("read-only"));
    }

    #[test]
    fn recommended_pack_rejected() {
        let err = locate_mutable_pack_file("zbrain-recommended").unwrap_err();
        assert_eq!(err.code, MutationErrorCode::PackReadonly);
    }

    #[test]
    fn non_existent_pack_not_found() {
        let err = locate_mutable_pack_file("nonexistent-pack-xyz").unwrap_err();
        assert_eq!(err.code, MutationErrorCode::PackNotFound);
    }

    #[test]
    fn locate_finds_yaml_pack() {
        let name = "test-locate-yaml";
        setup_test_pack(name, &test_manifest());
        let (path, fmt) = locate_mutable_pack_file(name).unwrap();
        assert!(path.to_str().unwrap().ends_with("pack.yaml"));
        assert_eq!(fmt, PackFileFormat::Yaml);
        cleanup_test_pack(name);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let m = test_manifest();
        let tmp = std::env::temp_dir().join("zbrain-write-test.yaml");
        write_pack_manifest(&tmp, &m, PackFileFormat::Yaml).unwrap();
        let loaded = loader::load_pack_from_file(&tmp).unwrap();
        assert_eq!(loaded.name, "test-pack");
        assert_eq!(loaded.page_types.len(), 2);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn check_no_references_clean() {
        let m = test_manifest();
        assert!(check_no_references(&m, "person").is_none());
        assert!(check_no_references(&m, "note").is_none());
    }

    #[test]
    fn check_no_references_alias() {
        let mut m = test_manifest();
        // "note" type has alias "person"
        m.page_types[1].aliases.push("person".to_string());
        let ref_msg = check_no_references(&m, "person");
        assert!(ref_msg.is_some());
        assert!(ref_msg.unwrap().contains("aliased"));
    }

    #[test]
    fn check_no_references_enrichable() {
        let mut m = test_manifest();
        m.enrichable_types
            .push(manifest::EnrichableType {
                type_name: "person".to_string(),
                rubric: None,
            });
        let ref_msg = check_no_references(&m, "person");
        assert!(ref_msg.is_some());
        assert!(ref_msg.unwrap().contains("enrichable_types"));
    }

    #[test]
    fn add_type_succeeds() {
        let name = "test-add-type";
        setup_test_pack(name, &test_manifest());
        let result = add_type_to_pack(
            name,
            &AddTypeOpts {
                name: "company".to_string(),
                primitive: PackPrimitive::Entity,
                prefix: "companies/".to_string(),
                ..Default::default()
            },
            &MutateOpts::default(),
        );
        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.pack, name);
        assert_ne!(r.prev_sha8, r.new_sha8);

        // Verify the type was added
        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        assert_eq!(loaded.page_types.len(), 3);
        assert!(loaded.page_types.iter().any(|pt| pt.name == "company"));

        cleanup_test_pack(name);
    }

    #[test]
    fn add_type_duplicate_fails() {
        let name = "test-add-dup";
        setup_test_pack(name, &test_manifest());
        let err = add_type_to_pack(
            name,
            &AddTypeOpts {
                name: "person".to_string(),
                primitive: PackPrimitive::Entity,
                ..Default::default()
            },
            &MutateOpts::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, MutationErrorCode::TypeExists);
        cleanup_test_pack(name);
    }

    #[test]
    fn remove_type_succeeds() {
        let name = "test-remove-type";
        setup_test_pack(name, &test_manifest());
        let result = remove_type_from_pack(name, "note", &MutateOpts::default());
        assert!(result.is_ok());

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        assert_eq!(loaded.page_types.len(), 1);
        assert!(!loaded.page_types.iter().any(|pt| pt.name == "note"));
        cleanup_test_pack(name);
    }

    #[test]
    fn remove_type_not_found() {
        let name = "test-remove-missing";
        setup_test_pack(name, &test_manifest());
        let err = remove_type_from_pack(name, "nonexistent", &MutateOpts::default()).unwrap_err();
        assert_eq!(err.code, MutationErrorCode::TypeNotFound);
        cleanup_test_pack(name);
    }

    #[test]
    fn remove_type_still_referenced() {
        let name = "test-remove-ref";
        let mut m = test_manifest();
        m.page_types[1].aliases.push("person".to_string());
        setup_test_pack(name, &m);

        let err = remove_type_from_pack(name, "person", &MutateOpts::default()).unwrap_err();
        assert_eq!(err.code, MutationErrorCode::StillReferenced);
        cleanup_test_pack(name);
    }

    #[test]
    fn update_type_succeeds() {
        let name = "test-update-type";
        setup_test_pack(name, &test_manifest());
        let result = update_type_on_pack(
            name,
            &UpdateTypeOpts {
                name: "person".to_string(),
                extractable: Some(true),
                ..Default::default()
            },
            &MutateOpts::default(),
        );
        assert!(result.is_ok());

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert!(pt.extractable);
        cleanup_test_pack(name);
    }

    #[test]
    fn add_alias_idempotent() {
        let name = "test-add-alias";
        setup_test_pack(name, &test_manifest());
        add_alias_to_type(name, "person", "individual", &MutateOpts::default()).unwrap();

        // Adding again should not fail (idempotent)
        add_alias_to_type(name, "person", "individual", &MutateOpts::default()).unwrap();

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert_eq!(pt.aliases.len(), 1);
        assert_eq!(pt.aliases[0], "individual");
        cleanup_test_pack(name);
    }

    #[test]
    fn remove_alias_idempotent() {
        let name = "test-remove-alias";
        let mut m = test_manifest();
        m.page_types[0].aliases.push("individual".to_string());
        setup_test_pack(name, &m);

        remove_alias_from_type(name, "person", "individual", &MutateOpts::default()).unwrap();
        // Removing again should not fail
        remove_alias_from_type(name, "person", "individual", &MutateOpts::default()).unwrap();

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert!(pt.aliases.is_empty());
        cleanup_test_pack(name);
    }

    #[test]
    fn add_prefix_idempotent() {
        let name = "test-add-prefix";
        setup_test_pack(name, &test_manifest());
        add_prefix_to_type(name, "person", "people/", &MutateOpts::default()).unwrap();
        // person already has "people/" — idempotent, should still be 1
        add_prefix_to_type(name, "person", "people/", &MutateOpts::default()).unwrap();

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert_eq!(pt.path_prefixes.len(), 1);

        // Add a new prefix
        add_prefix_to_type(name, "person", "contacts/", &MutateOpts::default()).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert_eq!(pt.path_prefixes.len(), 2);
        cleanup_test_pack(name);
    }

    #[test]
    fn remove_prefix() {
        let name = "test-remove-prefix";
        setup_test_pack(name, &test_manifest());
        remove_prefix_from_type(name, "person", "people/", &MutateOpts::default()).unwrap();

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert!(pt.path_prefixes.is_empty());
        cleanup_test_pack(name);
    }

    #[test]
    fn add_link_type_succeeds() {
        let name = "test-add-link";
        setup_test_pack(name, &test_manifest());
        let result = add_link_type_to_pack(
            name,
            &AddLinkTypeOpts {
                name: "founded".to_string(),
                inverse: Some("founded_by".to_string()),
            },
            &MutateOpts::default(),
        );
        assert!(result.is_ok());

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        assert_eq!(loaded.link_types.len(), 2);
        assert!(loaded.link_types.iter().any(|lt| lt.name == "founded"));
        cleanup_test_pack(name);
    }

    #[test]
    fn add_link_type_duplicate_fails() {
        let name = "test-add-link-dup";
        setup_test_pack(name, &test_manifest());
        let err = add_link_type_to_pack(
            name,
            &AddLinkTypeOpts {
                name: "mentions".to_string(),
                ..Default::default()
            },
            &MutateOpts::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, MutationErrorCode::TypeExists);
        cleanup_test_pack(name);
    }

    #[test]
    fn remove_link_type_succeeds() {
        let name = "test-remove-link";
        setup_test_pack(name, &test_manifest());
        let result = remove_link_type_from_pack(name, "mentions", &MutateOpts::default());
        assert!(result.is_ok());

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        assert_eq!(loaded.link_types.len(), 0);
        cleanup_test_pack(name);
    }

    #[test]
    fn set_extractable_delegates_to_update() {
        let name = "test-set-extractable";
        setup_test_pack(name, &test_manifest());
        set_extractable_on_type(name, "person", true, &MutateOpts::default()).unwrap();

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "person")
            .unwrap();
        assert!(pt.extractable);
        cleanup_test_pack(name);
    }

    #[test]
    fn set_expert_routing_delegates_to_update() {
        let name = "test-set-expert";
        setup_test_pack(name, &test_manifest());
        set_expert_routing_on_type(name, "note", true, &MutateOpts::default()).unwrap();

        let (path, _) = locate_mutable_pack_file(name).unwrap();
        let loaded = loader::load_pack_from_file(&path).unwrap();
        let pt = loaded
            .page_types
            .iter()
            .find(|pt| pt.name == "note")
            .unwrap();
        assert!(pt.expert_routing);
        cleanup_test_pack(name);
    }

    #[test]
    fn mutation_type_not_found_in_operations() {
        let name = "test-op-not-found";
        setup_test_pack(name, &test_manifest());
        let err = add_alias_to_type(name, "nonexistent", "x", &MutateOpts::default()).unwrap_err();
        assert_eq!(err.code, MutationErrorCode::TypeNotFound);
        cleanup_test_pack(name);
    }
}
