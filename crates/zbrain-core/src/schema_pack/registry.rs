//! Schema pack registry — 7-tier resolution chain + extends-chain BFS + stat-TTL cache.
//!
//! Ported from TS `src/core/schema-pack/registry.ts`.
//!
//! Key concepts:
//! - **7-tier resolution**: perCall → env → perSourceDb → dbConfig → zbrainYml → homeConfig → default
//! - **Extends-chain BFS**: walk parent packs with cycle detection + depth caps (warn 4, hard 8)
//! - **Stat-TTL cache**: ~1s TTL window, stat-mtime invalidation, cascade on parent change

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::closure::{self, AliasGraph, AliasCycleError};
use super::manifest::{
    compute_manifest_sha8, pack_identity, SchemaPackManifest,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const EXTENDS_DEPTH_WARN: usize = 4;
pub const EXTENDS_DEPTH_HARD_CAP: usize = 8;
pub const STAT_TTL_MS_DEFAULT: u64 = 1000;

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Thrown when extends-chain depth exceeds hard cap or contains a cycle.
#[derive(Debug, Clone)]
pub struct ExtendsChainTooDeepError {
    pub depth: usize,
    pub chain: Vec<String>,
}

impl std::fmt::Display for ExtendsChainTooDeepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pack extends chain depth {} exceeds hard cap {}: {}",
            self.depth,
            EXTENDS_DEPTH_HARD_CAP,
            self.chain.join(" -> ")
        )
    }
}

impl std::error::Error for ExtendsChainTooDeepError {}

/// Thrown when a referenced pack name cannot be resolved.
#[derive(Debug, Clone)]
pub struct UnknownPackError {
    pub name: String,
}

impl std::fmt::Display for UnknownPackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown schema pack: {}", self.name)
    }
}

impl std::error::Error for UnknownPackError {}

/// Combined error for resolve_pack.
#[derive(Debug)]
pub enum ResolvePackError {
    ExtendsChainTooDeep(ExtendsChainTooDeepError),
    UnknownPack(UnknownPackError),
    AliasCycle(AliasCycleError),
}

impl std::fmt::Display for ResolvePackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExtendsChainTooDeep(e) => write!(f, "{e}"),
            Self::UnknownPack(e) => write!(f, "{e}"),
            Self::AliasCycle(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ResolvePackError {}

impl From<ExtendsChainTooDeepError> for ResolvePackError {
    fn from(e: ExtendsChainTooDeepError) -> Self {
        Self::ExtendsChainTooDeep(e)
    }
}
impl From<UnknownPackError> for ResolvePackError {
    fn from(e: UnknownPackError) -> Self {
        Self::UnknownPack(e)
    }
}
impl From<AliasCycleError> for ResolvePackError {
    fn from(e: AliasCycleError) -> Self {
        Self::AliasCycle(e)
    }
}

// ---------------------------------------------------------------------------
// Resolution types
// ---------------------------------------------------------------------------

/// Source tier that provided the pack name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackSource {
    PerCall,
    Env,
    PerSourceDb,
    DbConfig,
    ZbrainYml,
    HomeConfig,
    Default,
}

impl PackSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PerCall => "per-call",
            Self::Env => "env",
            Self::PerSourceDb => "per-source-db",
            Self::DbConfig => "db-config",
            Self::ZbrainYml => "zbrain-yml",
            Self::HomeConfig => "home-config",
            Self::Default => "default",
        }
    }
}

/// Input for the 7-tier resolution chain.
#[derive(Debug, Clone, Default)]
pub struct ResolutionInput {
    pub per_call: Option<String>,
    pub remote: bool,
    pub per_source_db: Option<HashMap<String, String>>,
    pub source_id: Option<String>,
    pub env_var: Option<String>,
    pub db_config: Option<String>,
    pub zbrain_yml: Option<String>,
    pub home_config: Option<String>,
}

/// Result of pack name resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionResult {
    pub pack_name: String,
    pub source: PackSource,
}

/// A fully resolved pack — manifest + identity + alias graph.
#[derive(Debug, Clone)]
pub struct ResolvedPack {
    pub manifest: SchemaPackManifest,
    pub identity: String,
    pub manifest_sha8: String,
    pub alias_closure_hash: String,
    pub alias_graph: AliasGraph,
}

// ---------------------------------------------------------------------------
// resolve_active_pack_name — pure 7-tier resolution chain
// ---------------------------------------------------------------------------

fn non_empty(opt: &Option<String>) -> bool {
    opt.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false)
}

/// Resolve the active pack name via the 7-tier chain.
///
/// Tiers (first hit wins):
/// 1. perCall (only if remote == false)
/// 2. envVar
/// 3. perSourceDb (if sourceId exists and has entry)
/// 4. dbConfig
/// 5. zbrainYml
/// 6. homeConfig
/// 7. default ("zbrain-base")
pub fn resolve_active_pack_name(input: &ResolutionInput) -> ResolutionResult {
    // Tier 1: per-call (trust-gated: only local callers)
    if non_empty(&input.per_call) && !input.remote {
        return ResolutionResult {
            pack_name: input.per_call.as_ref().unwrap().clone(),
            source: PackSource::PerCall,
        };
    }

    // Tier 2: env var
    if non_empty(&input.env_var) {
        return ResolutionResult {
            pack_name: input.env_var.as_ref().unwrap().clone(),
            source: PackSource::Env,
        };
    }

    // Tier 3: per-source DB
    if non_empty(&input.source_id) {
        if let Some(ref db) = input.per_source_db {
            if let Some(pack_name) = db.get(input.source_id.as_deref().unwrap()) {
                if !pack_name.trim().is_empty() {
                    return ResolutionResult {
                        pack_name: pack_name.clone(),
                        source: PackSource::PerSourceDb,
                    };
                }
            }
        }
    }

    // Tier 4: DB config
    if non_empty(&input.db_config) {
        return ResolutionResult {
            pack_name: input.db_config.as_ref().unwrap().clone(),
            source: PackSource::DbConfig,
        };
    }

    // Tier 5: zbrain.yml
    if non_empty(&input.zbrain_yml) {
        return ResolutionResult {
            pack_name: input.zbrain_yml.as_ref().unwrap().clone(),
            source: PackSource::ZbrainYml,
        };
    }

    // Tier 6: home config
    if non_empty(&input.home_config) {
        return ResolutionResult {
            pack_name: input.home_config.as_ref().unwrap().clone(),
            source: PackSource::HomeConfig,
        };
    }

    // Tier 7: default
    ResolutionResult {
        pack_name: "zbrain-base".to_string(),
        source: PackSource::Default,
    }
}

// ---------------------------------------------------------------------------
// Cache entry + PackRegistry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FileStat {
    name: String,
    path: String,
    mtime_ms: f64,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    resolved: ResolvedPack,
    chain: Vec<String>,
    files: Vec<FileStat>,
    last_stat_ms: u128,
}

/// Options for `PackRegistry::resolve_pack`.
pub struct ResolvePackOpts {
    /// Maps pack name → disk path (for stat-TTL cache). Optional.
    pub load_by_path: Option<Box<dyn Fn(&str) -> Option<String>>>,
    /// Soft warning when extends-chain depth exceeds WARN threshold.
    pub on_depth_warn: Option<Box<dyn FnMut(usize, &[String])>>,
}

impl Default for ResolvePackOpts {
    fn default() -> Self {
        Self {
            load_by_path: None,
            on_depth_warn: None,
        }
    }
}

/// Pack registry with stat-TTL cache and cascade invalidation.
pub struct PackRegistry {
    cache: HashMap<String, CacheEntry>,
    /// Injectable clock for tests. Returns ms since UNIX_EPOCH.
    clock: Box<dyn Fn() -> u128>,
}

impl Default for PackRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PackRegistry {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            clock: Box::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            }),
        }
    }

    /// Create with a custom clock (for tests).
    pub fn with_clock(clock: impl Fn() -> u128 + 'static) -> Self {
        Self {
            cache: HashMap::new(),
            clock: Box::new(clock),
        }
    }

    /// Number of cached entries (for tests).
    pub fn cache_size(&self) -> usize {
        self.cache.len()
    }

    /// Names of cached entries (for tests).
    pub fn cache_names(&self) -> Vec<String> {
        self.cache.keys().cloned().collect()
    }

    // ---- resolve_pack ---------------------------------------------------

    /// Resolve a manifest through the extends-chain, build alias graph,
    /// and cache the result. Returns the fully resolved pack.
    pub fn resolve_pack(
        &mut self,
        manifest: &SchemaPackManifest,
        load_by_name: &mut dyn FnMut(&str) -> Result<SchemaPackManifest, UnknownPackError>,
        opts: Option<ResolvePackOpts>,
    ) -> Result<ResolvedPack, ResolvePackError> {
        let sha8 = compute_manifest_sha8(manifest);
        let identity = pack_identity(manifest, &sha8);

        // Reference-equality fast path: same identity → return cached.
        if let Some(entry) = self.cache.get(&manifest.name) {
            if entry.resolved.identity == identity {
                return Ok(entry.resolved.clone());
            }
        }

        // Walk extends chain.
        let mut chain = vec![manifest.name.clone()];
        let mut cursor = manifest.clone();
        let mut opts = opts.unwrap_or_default();

        loop {
            let parent_name = match &cursor.extends {
                Some(s) if !s.trim().is_empty() => s.clone(),
                _ => break, // None or empty = no parent
            };

            // Cycle detection
            if chain.contains(&parent_name) {
                return Err(ExtendsChainTooDeepError {
                    depth: chain.len(),
                    chain: chain.clone(),
                }
                .into());
            }

            chain.push(parent_name.clone());

            // Depth caps
            if chain.len() > EXTENDS_DEPTH_HARD_CAP {
                return Err(ExtendsChainTooDeepError {
                    depth: chain.len(),
                    chain: chain.clone(),
                }
                .into());
            }

            if chain.len() > EXTENDS_DEPTH_WARN {
                if let Some(ref mut cb) = opts.on_depth_warn {
                    cb(chain.len(), &chain);
                }
            }

            // Load parent
            cursor = load_by_name(&parent_name)?;
        }

        // Build alias graph + closure hash
        let alias_graph = closure::build_alias_graph(manifest)?;
        let alias_closure_hash = closure::compute_alias_closure_hash(manifest)?;

        let resolved = ResolvedPack {
            manifest: manifest.clone(),
            identity: identity.clone(),
            manifest_sha8: sha8,
            alias_closure_hash,
            alias_graph,
        };

        // Capture file stat snapshots (optional)
        let now = (self.clock)();
        let files: Vec<FileStat> = if let Some(ref load_by_path) = opts.load_by_path {
            chain
                .iter()
                .filter_map(|name| {
                    let path = load_by_path(name)?;
                    let mtime_ms = safe_mtime_ms(Path::new(&path));
                    Some(FileStat {
                        name: name.clone(),
                        path,
                        mtime_ms,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };

        // Write to cache
        self.cache.insert(
            manifest.name.clone(),
            CacheEntry {
                resolved: resolved.clone(),
                chain: chain.clone(),
                files,
                last_stat_ms: now,
            },
        );

        Ok(resolved)
    }

    // ---- try_cached_pack ------------------------------------------------

    /// TTL-gated cache lookup. Returns the cached pack if within TTL window
    /// or if all file mtimes match. Returns None if cache miss or stale.
    pub fn try_cached_pack(&mut self, name: &str) -> Option<ResolvedPack> {
        let entry = self.cache.get(name)?.clone();
        let now = (self.clock)();
        let ttl = resolve_stat_ttl_ms();

        if now.saturating_sub(entry.last_stat_ms) < ttl as u128 {
            // Hot path: within TTL window
            return Some(entry.resolved);
        }

        // TTL expired: stat each file
        if entry.files.is_empty() {
            // No files to stat (synthetic manifest) — treat as fresh
            // Update last_stat_ms in cache
            if let Some(e) = self.cache.get_mut(name) {
                e.last_stat_ms = now;
            }
            return Some(entry.resolved);
        }

        let matches = snapshot_matches(&entry.files);
        if matches {
            // All mtimes match — refresh TTL
            if let Some(e) = self.cache.get_mut(name) {
                e.last_stat_ms = now;
            }
            Some(entry.resolved)
        } else {
            // Stale — invalidate
            self.invalidate(Some(name));
            None
        }
    }

    // ---- invalidate -----------------------------------------------------

    /// Invalidate cache entries. If name is None, clear all.
    /// If name is Some, cascade-invalidate all entries whose chain contains name.
    /// Returns the names of invalidated entries.
    pub fn invalidate(&mut self, name: Option<&str>) -> Vec<String> {
        match name {
            None => {
                let names: Vec<String> = self.cache.keys().cloned().collect();
                self.cache.clear();
                names
            }
            Some(target) => {
                // Find all entries whose chain contains target (cascade)
                let mut to_remove: HashSet<String> = HashSet::new();
                to_remove.insert(target.to_string());

                for (key, entry) in &self.cache {
                    if entry.chain.iter().any(|c| c == target) {
                        to_remove.insert(key.clone());
                    }
                }

                let removed: Vec<String> = to_remove.iter().cloned().collect();
                for key in &to_remove {
                    self.cache.remove(key);
                }
                removed
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

fn resolve_stat_ttl_ms() -> u64 {
    std::env::var("ZBRAIN_PACK_STAT_TTL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(STAT_TTL_MS_DEFAULT)
}

fn safe_mtime_ms(path: &Path) -> f64 {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(f64::INFINITY) // Missing file = "changed" = force reload
}

fn snapshot_matches(files: &[FileStat]) -> bool {
    files.iter().all(|f| {
        let current = safe_mtime_ms(Path::new(&f.path));
        current == f.mtime_ms
    })
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema_pack::manifest::{
        PageTypeDefinition, PackPrimitive, SchemaPackManifest,
    };

    fn make_manifest(name: &str, extends: Option<&str>) -> SchemaPackManifest {
        SchemaPackManifest {
            name: name.into(),
            version: "1.0.0".into(),
            extends: extends.map(|s| s.to_string()),
            ..Default::default()
        }
    }

    // ---- resolve_active_pack_name ---------------------------------------

    #[test]
    fn tier1_per_call_local() {
        let input = ResolutionInput {
            per_call: Some("my-pack".into()),
            remote: false,
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "my-pack");
        assert_eq!(r.source, PackSource::PerCall);
    }

    #[test]
    fn tier1_per_call_blocked_for_remote() {
        let input = ResolutionInput {
            per_call: Some("my-pack".into()),
            remote: true,
            env_var: Some("env-pack".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        // per-call blocked, falls through to env
        assert_eq!(r.pack_name, "env-pack");
        assert_eq!(r.source, PackSource::Env);
    }

    #[test]
    fn tier2_env() {
        let input = ResolutionInput {
            env_var: Some("env-pack".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "env-pack");
        assert_eq!(r.source, PackSource::Env);
    }

    #[test]
    fn tier3_per_source_db() {
        let mut db = HashMap::new();
        db.insert("source-1".into(), "source-pack".into());
        let input = ResolutionInput {
            per_source_db: Some(db),
            source_id: Some("source-1".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "source-pack");
        assert_eq!(r.source, PackSource::PerSourceDb);
    }

    #[test]
    fn tier3_missing_source_id_falls_through() {
        let mut db = HashMap::new();
        db.insert("source-1".into(), "source-pack".into());
        let input = ResolutionInput {
            per_source_db: Some(db),
            source_id: Some("source-2".into()), // not in db
            db_config: Some("db-pack".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "db-pack");
        assert_eq!(r.source, PackSource::DbConfig);
    }

    #[test]
    fn tier4_db_config() {
        let input = ResolutionInput {
            db_config: Some("db-pack".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "db-pack");
        assert_eq!(r.source, PackSource::DbConfig);
    }

    #[test]
    fn tier5_zbrain_yml() {
        let input = ResolutionInput {
            zbrain_yml: Some("yml-pack".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "yml-pack");
        assert_eq!(r.source, PackSource::ZbrainYml);
    }

    #[test]
    fn tier6_home_config() {
        let input = ResolutionInput {
            home_config: Some("home-pack".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "home-pack");
        assert_eq!(r.source, PackSource::HomeConfig);
    }

    #[test]
    fn tier7_default() {
        let input = ResolutionInput::default();
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "zbrain-base");
        assert_eq!(r.source, PackSource::Default);
    }

    #[test]
    fn empty_string_treated_as_unset() {
        let input = ResolutionInput {
            per_call: Some("".into()),
            remote: false,
            env_var: Some("".into()),
            ..Default::default()
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.source, PackSource::Default);
    }

    #[test]
    fn tier_priority_order() {
        // All tiers set — tier 1 should win (local)
        let mut db = HashMap::new();
        db.insert("s1".into(), "src-pack".into());
        let input = ResolutionInput {
            per_call: Some("per-call-pack".into()),
            remote: false,
            per_source_db: Some(db),
            source_id: Some("s1".into()),
            env_var: Some("env-pack".into()),
            db_config: Some("db-pack".into()),
            zbrain_yml: Some("yml-pack".into()),
            home_config: Some("home-pack".into()),
        };
        let r = resolve_active_pack_name(&input);
        assert_eq!(r.pack_name, "per-call-pack");
        assert_eq!(r.source, PackSource::PerCall);
    }

    // ---- resolve_pack: extends chain ------------------------------------

    #[test]
    fn resolve_pack_no_extends() {
        let mut reg = PackRegistry::new();
        let m = make_manifest("my-pack", None);
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: name.into() })
        };
        let r = reg.resolve_pack(&m, &mut loader, None).unwrap();
        assert_eq!(r.manifest.name, "my-pack");
        assert!(r.identity.contains("my-pack@1.0.0+"));
        assert_eq!(r.manifest_sha8.len(), 8);
        assert_eq!(r.alias_closure_hash.len(), 16);
    }

    #[test]
    fn resolve_pack_walks_extends_chain() {
        let mut reg = PackRegistry::new();
        let m = make_manifest("child", Some("parent"));
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            match name {
                "parent" => Ok(make_manifest("parent", None)),
                _ => Err(UnknownPackError { name: name.into() }),
            }
        };
        let r = reg.resolve_pack(&m, &mut loader, None).unwrap();
        assert_eq!(r.manifest.name, "child");
    }

    #[test]
    fn resolve_pack_cycle_detection() {
        let mut reg = PackRegistry::new();
        // A extends B, B extends A — cycle
        let m = make_manifest("A", Some("B"));
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            match name {
                "B" => Ok(make_manifest("B", Some("A"))),
                _ => Err(UnknownPackError { name: name.into() }),
            }
        };
        let err = reg.resolve_pack(&m, &mut loader, None).unwrap_err();
        assert!(matches!(err, ResolvePackError::ExtendsChainTooDeep(_)));
    }

    #[test]
    fn resolve_pack_depth_hard_cap() {
        let mut reg = PackRegistry::new();
        // Chain: 0 -> 1 -> 2 -> ... -> 9 (10 deep, > 8 hard cap)
        let m = make_manifest("p0", Some("p1"));
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            let n: usize = name[1..].parse().unwrap();
            let parent = if n + 1 <= 10 {
                Some(format!("p{}", n + 1))
            } else {
                None
            };
            Ok(make_manifest(&format!("p{n}"), parent.as_deref()))
        };
        let err = reg.resolve_pack(&m, &mut loader, None).unwrap_err();
        assert!(matches!(err, ResolvePackError::ExtendsChainTooDeep(_)));
    }

    #[test]
    fn resolve_pack_depth_warn_callback() {
        use std::cell::Cell;
        use std::rc::Rc;

        let mut reg = PackRegistry::new();
        // Chain: 0 -> 1 -> 2 -> 3 -> 4 -> 5 -> base (7 deep, > warn 4 but < hard cap 8)
        let m = make_manifest("p0", Some("p1"));
        let warned = Rc::new(Cell::new(false));
        let warned_clone = Rc::clone(&warned);
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            let n: usize = name[1..].parse().unwrap();
            let parent = if n + 1 <= 5 {
                Some(format!("p{}", n + 1))
            } else {
                None
            };
            Ok(make_manifest(&format!("p{n}"), parent.as_deref()))
        };
        let opts = ResolvePackOpts {
            on_depth_warn: Some(Box::new(move |_depth, _chain| {
                warned_clone.set(true);
            })),
            load_by_path: None,
        };
        let _r = reg.resolve_pack(&m, &mut loader, Some(opts)).unwrap();
        assert!(warned.get(), "depth warn callback should fire for chain > 4");
    }

    #[test]
    fn resolve_pack_identity_fast_path() {
        let mut reg = PackRegistry::new();
        let m = make_manifest("my-pack", None);
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: name.into() })
        };
        let r1 = reg.resolve_pack(&m, &mut loader, None).unwrap();
        let r2 = reg.resolve_pack(&m, &mut loader, None).unwrap();
        // Same identity → same result (fast path, no re-resolve)
        assert_eq!(r1.identity, r2.identity);
        assert_eq!(r1.manifest_sha8, r2.manifest_sha8);
    }

    #[test]
    fn resolve_pack_with_alias_graph() {
        let mut reg = PackRegistry::new();
        let m = SchemaPackManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            extends: None,
            page_types: vec![
                PageTypeDefinition {
                    name: "person".into(),
                    primitive: PackPrimitive::Entity,
                    aliases: vec!["researcher".into()],
                    ..Default::default()
                },
                PageTypeDefinition {
                    name: "researcher".into(),
                    primitive: PackPrimitive::Entity,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: name.into() })
        };
        let r = reg.resolve_pack(&m, &mut loader, None).unwrap();
        assert!(r.alias_graph.get("person").unwrap().contains("researcher"));
        assert!(r.alias_graph.get("researcher").unwrap().contains("person"));
    }

    // ---- cache: try_cached_pack -----------------------------------------

    #[test]
    fn cache_hit_within_ttl() {
        let mut time = 1000u128;
        let mut reg = PackRegistry::with_clock(move || time);

        let m = make_manifest("cached-pack", None);
        let mut loader = |n: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: n.into() })
        };
        let _r = reg.resolve_pack(&m, &mut loader, None).unwrap();

        // Within TTL window — should hit
        time = 1500;
        let cached = reg.try_cached_pack("cached-pack");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().manifest.name, "cached-pack");
    }

    #[test]
    fn cache_miss_after_invalidate() {
        let mut reg = PackRegistry::new();
        let m = make_manifest("my-pack", None);
        let mut loader = |n: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: n.into() })
        };
        let _r = reg.resolve_pack(&m, &mut loader, None).unwrap();
        assert_eq!(reg.cache_size(), 1);

        let removed = reg.invalidate(Some("my-pack"));
        assert!(removed.contains(&"my-pack".to_string()));
        assert_eq!(reg.cache_size(), 0);

        assert!(reg.try_cached_pack("my-pack").is_none());
    }

    #[test]
    fn cache_cascade_invalidation() {
        let mut reg = PackRegistry::new();
        // child extends parent — both cached
        let child = make_manifest("child", Some("parent"));
        let mut loader = |name: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            match name {
                "parent" => Ok(make_manifest("parent", None)),
                _ => Err(UnknownPackError { name: name.into() }),
            }
        };
        let _r = reg.resolve_pack(&child, &mut loader, None).unwrap();
        assert_eq!(reg.cache_size(), 1); // only "child" cached

        // Invalidate "parent" — should cascade to "child"
        let removed = reg.invalidate(Some("parent"));
        assert!(removed.contains(&"parent".to_string()));
        assert!(removed.contains(&"child".to_string()));
        assert_eq!(reg.cache_size(), 0);
    }

    #[test]
    fn cache_invalidate_all() {
        let mut reg = PackRegistry::new();
        let m1 = make_manifest("pack-a", None);
        let m2 = make_manifest("pack-b", None);
        let mut loader = |n: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: n.into() })
        };
        let _r1 = reg.resolve_pack(&m1, &mut loader, None).unwrap();
        let _r2 = reg.resolve_pack(&m2, &mut loader, None).unwrap();
        assert_eq!(reg.cache_size(), 2);

        let removed = reg.invalidate(None);
        assert_eq!(removed.len(), 2);
        assert_eq!(reg.cache_size(), 0);
    }

    #[test]
    fn cache_ttl_expired_no_files_returns_cached() {
        let mut time = 1000u128;
        let mut reg = PackRegistry::with_clock(move || time);

        let m = make_manifest("synthetic", None);
        let mut loader = |n: &str| -> Result<SchemaPackManifest, UnknownPackError> {
            Err(UnknownPackError { name: n.into() })
        };
        let _r = reg.resolve_pack(&m, &mut loader, None).unwrap();

        // Far future — TTL expired, but no files to stat → still cached
        time = 999_999_999;
        let cached = reg.try_cached_pack("synthetic");
        assert!(cached.is_some(), "synthetic manifest (no files) should remain cached");
    }
}
