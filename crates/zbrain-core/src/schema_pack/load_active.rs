//! Load-active pack — boundary helper + best-effort fallback.
//!
//! Ported from TS `src/core/schema-pack/load-active.ts` + `best-effort.ts`.
//!
//! `load_active_pack` is the main entry point for consumers. It resolves
//! the active pack name via the 7-tier chain, checks the stat-TTL cache,
//! and loads from disk if needed.
//!
//! `load_active_pack_best_effort` wraps `load_active_pack` in a try/catch,
//! returning `None` on any failure. Callers MUST treat `None` as "empty
//! filter" — never fall back to hardcoded default types.

use std::collections::HashMap;
use std::path::PathBuf;

use super::loader;
use super::manifest::SchemaPackManifest;
use super::registry::{
    self, PackRegistry, ResolvedPack, ResolutionInput, ResolutionResult,
    ResolvePackOpts, UnknownPackError,
};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Input for `load_active_pack`. Fields map to the 7-tier resolution chain.
#[derive(Debug, Clone, Default)]
pub struct LoadActivePackInput {
    pub remote: bool,
    pub per_call: Option<String>,
    pub source_id: Option<String>,
    pub per_source_db: Option<HashMap<String, String>>,
    pub zbrain_yml: Option<String>,
    pub db_config: Option<String>,
    pub home_config: Option<String>,
    /// If None, reads from `ZBRAIN_SCHEMA_PACK` env var.
    pub env_var: Option<String>,
}

/// Combined error for the load-active boundary.
#[derive(Debug)]
pub enum LoadActivePackError {
    UnknownPack(String),
    Loader(loader::LoaderError),
    ResolvePack(registry::ResolvePackError),
}

impl std::fmt::Display for LoadActivePackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPack(name) => write!(f, "unknown schema pack: {name}"),
            Self::Loader(e) => write!(f, "{e}"),
            Self::ResolvePack(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LoadActivePackError {}

/// Pack locator — maps pack name to disk path.
pub type PackLocator = dyn Fn(&str) -> Option<PathBuf>;

// ---------------------------------------------------------------------------
// load_active_pack
// ---------------------------------------------------------------------------

/// Load the active schema pack. This is the main entry point for consumers.
///
/// Flow:
/// 1. Build ResolutionInput from LoadActivePackInput
/// 2. resolve_active_pack_name → pack_name + source
/// 3. try_cached_pack → if hit, return
/// 4. Locate pack on disk, load manifest
/// 5. resolve_pack (walks extends chain, builds alias graph, caches)
pub fn load_active_pack(
    registry: &mut PackRegistry,
    locator: &PackLocator,
    input: &LoadActivePackInput,
) -> Result<ResolvedPack, LoadActivePackError> {
    // Build ResolutionInput
    let env_var = input.env_var.clone().or_else(|| {
        std::env::var("ZBRAIN_SCHEMA_PACK")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });

    let resolution_input = ResolutionInput {
        per_call: input.per_call.clone(),
        remote: input.remote,
        per_source_db: input.per_source_db.clone(),
        source_id: input.source_id.clone(),
        env_var,
        db_config: input.db_config.clone(),
        zbrain_yml: input.zbrain_yml.clone(),
        home_config: input.home_config.clone(),
    };

    // Resolve pack name
    let resolution = registry::resolve_active_pack_name(&resolution_input);

    // Try cache
    if let Some(cached) = registry.try_cached_pack(&resolution.pack_name) {
        return Ok(cached);
    }

    // Load from disk
    let path = locator(&resolution.pack_name)
        .ok_or_else(|| LoadActivePackError::UnknownPack(resolution.pack_name.clone()))?;

    let manifest = loader::load_pack_from_file(&path).map_err(LoadActivePackError::Loader)?;

    // Build load_by_name closure for extends-chain resolution
    let mut load_by_name = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
        let p = locator(name).ok_or(UnknownPackError { name: name.into() })?;
        loader::load_pack_from_file(&p).map_err(|_| UnknownPackError { name: name.into() })
    };

    // Note: load_by_path is skipped in the initial port because
    // ResolvePackOpts.load_by_path requires 'static lifetime. The stat-TTL
    // cache still works for identity fast path and manual invalidation.
    let opts = ResolvePackOpts::default();

    registry
        .resolve_pack(&manifest, &mut load_by_name, Some(opts))
        .map_err(LoadActivePackError::ResolvePack)
}

/// Resolve only the pack name + source (no disk load, no manifest).
pub fn resolve_active_pack_name_only(input: &LoadActivePackInput) -> ResolutionResult {
    let env_var = input.env_var.clone().or_else(|| {
        std::env::var("ZBRAIN_SCHEMA_PACK")
            .ok()
            .filter(|s| !s.trim().is_empty())
    });

    let resolution_input = ResolutionInput {
        per_call: input.per_call.clone(),
        remote: input.remote,
        per_source_db: input.per_source_db.clone(),
        source_id: input.source_id.clone(),
        env_var,
        db_config: input.db_config.clone(),
        zbrain_yml: input.zbrain_yml.clone(),
        home_config: input.home_config.clone(),
    };

    registry::resolve_active_pack_name(&resolution_input)
}

// ---------------------------------------------------------------------------
// load_active_pack_best_effort
// ---------------------------------------------------------------------------

/// Best-effort pack loading. Returns `None` on any failure.
///
/// **Contract (D4)**: `None` means "empty filter" — callers MUST NOT fall
/// back to hardcoded default types. The absence of a pack means no type
/// filtering is applied, not that some default set is used.
pub fn load_active_pack_best_effort(
    registry: &mut PackRegistry,
    locator: &PackLocator,
    remote: bool,
) -> Option<ResolvedPack> {
    let input = LoadActivePackInput {
        remote,
        ..Default::default()
    };
    load_active_pack(registry, locator, &input).ok()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_pack::manifest::SchemaPackManifest;
    use std::io::Write;

    fn write_test_pack(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}.yaml"));
        let content = format!(
            r#"api_version: zbrain-schema-pack-v1
name: {name}
version: "1.0.0"
extends: null
"#
        );
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn best_effort_returns_none_when_pack_missing() {
        let mut reg = PackRegistry::new();
        let locator = |_: &str| None::<PathBuf>;
        let result = load_active_pack_best_effort(&mut reg, &locator, true);
        assert!(result.is_none());
    }

    #[test]
    fn best_effort_loads_pack_from_disk() {
        let dir = std::env::temp_dir().join(format!("zbrain-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        write_test_pack(&dir, "zbrain-base");

        let mut reg = PackRegistry::new();
        let dir_clone = dir.clone();
        let locator = move |name: &str| -> Option<PathBuf> {
            // Try .yaml then .yml then .json
            for ext in &["yaml", "yml", "json"] {
                let p = dir_clone.join(format!("{name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
            None
        };

        let result = load_active_pack_best_effort(&mut reg, &locator, true);
        assert!(result.is_some());
        let pack = result.unwrap();
        assert_eq!(pack.manifest.name, "zbrain-base");

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_active_pack_caches_after_first_load() {
        let dir = std::env::temp_dir().join(format!("zbrain-test-cache-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        write_test_pack(&dir, "zbrain-base");

        let mut reg = PackRegistry::new();
        let dir_clone = dir.clone();
        let locator = move |name: &str| -> Option<PathBuf> {
            for ext in &["yaml", "yml", "json"] {
                let p = dir_clone.join(format!("{name}.{ext}"));
                if p.exists() {
                    return Some(p);
                }
            }
            None
        };

        let input = LoadActivePackInput {
            remote: true,
            ..Default::default()
        };

        // First load — should populate cache
        let r1 = load_active_pack(&mut reg, &locator, &input).unwrap();
        assert_eq!(reg.cache_size(), 1);

        // Second load — should hit cache
        let r2 = load_active_pack(&mut reg, &locator, &input).unwrap();
        assert_eq!(r1.identity, r2.identity);

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_name_only_no_disk_access() {
        let input = LoadActivePackInput {
            remote: false,
            per_call: Some("my-pack".into()),
            ..Default::default()
        };
        let result = resolve_active_pack_name_only(&input);
        assert_eq!(result.pack_name, "my-pack");
    }
}
