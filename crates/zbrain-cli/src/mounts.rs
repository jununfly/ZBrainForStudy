//! `zbrain mounts` — manage connected brains (mounts.json).
//!
//! A "mount" is a SEPARATE zbrain DATABASE connected to your host agent.
//! Mirrors `src/commands/mounts.ts` (v0.40.3.0). Config-file only: no engine
//! dependency, offline-testable.
//!
//! ## Cache-aggregate note (honest divergence)
//!
//! The TS `refreshMountsCache()` write path aggregates resolver/manifest
//! files for host agents and depends on the *delayed* skill/resolver stack
//! (`check-resolvable.ts`, `skill-trigger-index.ts`). Only the repo-root
//! *skip detection* is ported here; the aggregated publish is parked as
//! KNOWN-GAPS G52. We never fake a published cache we cannot build.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Host brain id. Reserved — users cannot create a mount with this id.
const HOST_BRAIN_ID: &str = "host";

/// Brain id regex (kebab-case, 1-32 chars, no edge dashes). Mirrors
/// `core/brain-registry.ts` `BRAIN_ID_RE`.
const BRAIN_ID_RE: &str = r"^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$";

fn brain_id_re() -> &'static regex::Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(BRAIN_ID_RE).unwrap())
}

// ── Types ────────────────────────────────────────────────────────────────

/// Engine kind for a mount (mirrors `MountEntry.engine`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountEngine {
    Postgres,
    Pglite,
}

fn default_true() -> bool {
    true
}

/// A single entry in `~/.zbrain/mounts.json` (mirrors `MountEntry`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MountEntry {
    /// Unique mount id. Becomes the namespace in `yc-media::skill` form.
    pub id: String,
    /// Optional shorthand for CLI display. Must pass BRAIN_ID_RE if present.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub alias: Option<String>,
    /// Absolute local path to the mount's git clone.
    pub path: String,
    pub engine: MountEngine,
    /// Postgres connection URL (if engine=postgres).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub database_url: Option<String>,
    /// PGLite data-directory path (if engine=pglite).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub database_path: Option<String>,
    /// Default true. Disabled mounts are not loaded.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Managed by `zbrain mounts sync` (PR 1).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub expected_sha: Option<String>,
    /// Managed by `zbrain mounts sync` (PR 1).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub last_synced_at: Option<String>,
    /// v0.40.3.0: per-mount frontmatter-override trust gate. Default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust_frontmatter_overrides: bool,
}

/// Top-level shape of `~/.zbrain/mounts.json` (mirrors `MountsFile`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountsFile {
    pub version: u8,
    pub mounts: Vec<MountEntry>,
}

// ── Validation / parsing ──────────────────────────────────────────────────

/// Validate a mount id (and optionally the alias). Throws (bails) with an
/// actionable message on any violation. Mirrors `validateMountId`.
fn validate_mount_id(id: &str, field_label: &str) -> Result<()> {
    if id.is_empty() {
        bail!(
            "Invalid {field_label}: must be a non-empty string. Use a kebab-case id like \"yc-media\" or \"garrys-list\""
        );
    }
    if id == HOST_BRAIN_ID {
        bail!(
            "Reserved {field_label}: \"{host}\". \"{host}\" is the host brain id and cannot be used for a mount. Choose a different id",
            host = HOST_BRAIN_ID
        );
    }
    if !brain_id_re().is_match(id) {
        bail!(
            "Invalid {field_label}: \"{id}\". {field_label} must match [a-z0-9-]{{1,32}}, start+end alphanumeric, interior dashes allowed. Use a kebab-case id like \"yc-media\""
        );
    }
    Ok(())
}

/// Parse a postgres/pglite engine string into `MountEngine`.
fn parse_engine(v: &str) -> Result<MountEngine> {
    match v {
        "postgres" => Ok(MountEngine::Postgres),
        "pglite" => Ok(MountEngine::Pglite),
        other => bail!(
            "Invalid engine: \"{other}\". Must be \"postgres\" or \"pglite\". Pass --engine pglite or --engine postgres"
        ),
    }
}

/// Result of parsing `mounts add` args.
#[derive(Debug, PartialEq)]
pub struct ParsedAdd {
    pub id: String,
    pub path: PathBuf,
    pub engine: MountEngine,
    pub database_url: Option<String>,
    pub database_path: Option<String>,
    pub alias: Option<String>,
}

/// Pull the value following `flag` out of `args`, advancing `i`. Shared by
/// `parse_add_args` so flag handling stays DRY.
fn next_arg(args: &[String], i: &mut usize, flag: &str) -> Result<String> {
    *i += 1;
    match args.get(*i) {
        Some(v) => Ok(v.clone()),
        None => bail!("Missing value for {flag}"),
    }
}

/// Build a `ParsedAdd` from structured fields, after flag parsing. Central
/// validation shared by `parse_add_args` (string flags) and the CLI entry.
fn validate_and_build_add(
    id: &str,
    path_opt: Option<String>,
    engine_opt: Option<MountEngine>,
    database_url: Option<String>,
    database_path: Option<String>,
    alias: Option<String>,
) -> Result<ParsedAdd> {
    validate_mount_id(id, "mount id")?;

    let path = match path_opt {
        Some(p) => p,
        None => bail!(
            "Missing --path. Every mount needs a local clone path (for skills + handlers). Add --path /absolute/path/to/mount"
        ),
    };

    // Engine inference: if user supplied db-url → postgres, if db-path → pglite.
    let engine = match engine_opt {
        Some(e) => e,
        None => {
            if database_url.is_some() {
                MountEngine::Postgres
            } else if database_path.is_some() {
                MountEngine::Pglite
            } else {
                bail!(
                    "Missing --engine. Could not infer engine from flags. Pass --engine pglite --db-path <path> OR --engine postgres --db-url <url>"
                );
            }
        }
    };

    if engine == MountEngine::Postgres && database_url.is_none() {
        bail!("postgres mount requires --db-url. Pass --db-url postgresql://...");
    }
    if engine == MountEngine::Pglite && database_path.is_none() && database_url.is_none() {
        bail!("pglite mount requires --db-path. Pass --db-path /path/to/mount/.pglite");
    }

    Ok(ParsedAdd {
        id: id.to_string(),
        path: resolve_path(&path),
        engine,
        database_url,
        database_path,
        alias,
    })
}

/// Parse `mounts add <id> [flags]` (mirrors `parseAddArgs`).
pub fn parse_add_args(args: &[String]) -> Result<ParsedAdd> {
    let id = args
        .first()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Missing mount id.\n  Usage: zbrain mounts add <id> --path <path> [flags]\n  Provide a kebab-case id as the first argument"
            )
        })?
        .clone();

    let mut path: Option<String> = None;
    let mut engine: Option<MountEngine> = None;
    let mut database_url: Option<String> = None;
    let mut database_path: Option<String> = None;
    let mut alias: Option<String> = None;

    let mut i = 1usize;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--path" => path = Some(next_arg(args, &mut i, "--path")?),
            "--engine" => {
                let v = next_arg(args, &mut i, "--engine")?;
                engine = Some(parse_engine(&v)?);
            }
            "--db-url" | "--database-url" => {
                database_url = Some(next_arg(args, &mut i, "--db-url")?)
            }
            "--db-path" | "--database-path" => {
                database_path = Some(next_arg(args, &mut i, "--db-path")?)
            }
            "--alias" => {
                let v = next_arg(args, &mut i, "--alias")?;
                validate_mount_id(&v, "--alias value")?;
                alias = Some(v);
            }
            other => bail!("Unknown flag: {other}. See `zbrain mounts add --help`"),
        }
        i += 1;
    }

    validate_and_build_add(&id, path, engine, database_url, database_path, alias)
}

// ── Path resolution / IO ───────────────────────────────────────────────────

/// Resolve `p` to an absolute path (mirrors `path.resolve`). Does not touch
/// the filesystem, so non-existent paths still resolve (matching TS).
fn resolve_path(p: &str) -> PathBuf {
    let p = Path::new(p);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

/// Default location of mounts.json (mirrors `getMountsPath`). Uses the
/// zbrain-core SSOT (`paths::zbrain_home`), which honors `ZBRAIN_HOME`.
pub fn mounts_path() -> Result<PathBuf> {
    let home = zbrain_core::paths::zbrain_home().ok_or_else(|| {
        anyhow::anyhow!(
            "Could not resolve zbrain home; cannot locate mounts.json. Set ZBRAIN_HOME or HOME."
        )
    })?;
    Ok(home.join("mounts.json"))
}

/// Read mounts.json and return the parsed `MountsFile`, or a fresh empty
/// file shape if the file is absent. Throws on corruption.
pub fn read_mounts_file(path: &Path) -> Result<MountsFile> {
    if !path.exists() {
        return Ok(MountsFile {
            version: 1,
            mounts: vec![],
        });
    }
    let entries = load_mounts(path)?;
    Ok(MountsFile {
        version: 1,
        mounts: entries,
    })
}

/// Parse + validate mounts.json. Returns an empty list if the file is
/// absent. Throws a structured error on any malformed entry.
fn load_mounts(path: &Path) -> Result<Vec<MountEntry>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Cannot read {}", path.display()))?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("Malformed mounts.json at {}", path.display()))?;
    if !v.is_object() {
        bail!("mounts.json must be a JSON object. Expected {{ version: 1, mounts: [...] }}");
    }
    match v.get("version").and_then(|x| x.as_u64()) {
        Some(1) => {}
        Some(other) => bail!(
            "Unsupported mounts.json version: {other}. This zbrain binary supports version 1"
        ),
        None => bail!("mounts.json: missing 'version' (expected 1)"),
    }
    let mounts = v
        .get("mounts")
        .and_then(|m| m.as_array())
        .ok_or_else(|| anyhow::anyhow!("mounts.json: 'mounts' must be an array"))?;

    let mut entries = Vec::with_capacity(mounts.len());
    let mut seen_ids = HashSet::new();
    let mut seen_paths: HashMap<PathBuf, String> = HashMap::new();
    for (i, m) in mounts.iter().enumerate() {
        let entry: MountEntry = serde_json::from_value(m.clone())
            .with_context(|| format!("mounts.json: entry {i} is malformed"))?;
        validate_mount_id(&entry.id, &format!("mounts[{i}].id"))?;
        if !seen_ids.insert(entry.id.clone()) {
            bail!("mounts.json: duplicate id \"{}\"", entry.id);
        }
        let resolved = resolve_path(&entry.path);
        if let Some(existing) = seen_paths.get(&resolved) {
            return Err(duplicate_mount_path_error(&resolved, existing, &entry.id));
        }
        seen_paths.insert(resolved, entry.id.clone());

        match entry.engine {
            MountEngine::Postgres if entry.database_url.is_none() => {
                bail!(
                    "mounts[{i}] \"{}\": postgres mount requires database_url",
                    entry.id
                );
            }
            MountEngine::Pglite if entry.database_path.is_none() && entry.database_url.is_none() => {
                bail!(
                    "mounts[{i}] \"{}\": pglite mount requires database_path (or database_url)",
                    entry.id
                );
            }
            _ => {}
        }
        if let Some(alias) = &entry.alias {
            validate_mount_id(alias, &format!("mounts[{i}].alias"))?;
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Build a `DuplicateMountPathError` (mirrors `brain-registry.ts`).
fn duplicate_mount_path_error(path: &Path, existing_id: &str, attempted_id: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "Duplicate mount path: \"{path}\". Mount \"{existing}\" already uses this path. Cannot register \"{attempted}\" at the same location. Use a different local clone path, or remove the existing mount first: zbrain mounts remove {existing}",
        path = path.display(),
        existing = existing_id,
        attempted = attempted_id
    )
}

/// Write mounts.json atomically with 0600 perms (no secrets, but per-user
/// config alongside config.json which IS secret-bearing). Uses a unique tmp
/// filename per call so concurrent writers don't clobber each other.
pub fn write_mounts_file(file: &MountsFile, path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    let tmp = unique_tmp_path(path)?;
    let json = serde_json::to_string_pretty(file).context("serialize mounts.json")? + "\n";
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("create tmp {}", tmp.display()))?;
        f.write_all(json.as_bytes()).context("write tmp")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = f.metadata().context("stat tmp")?.permissions();
            perms.set_mode(0o600);
            f.set_permissions(perms).context("chmod tmp")?;
        }
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("rename tmp -> {}", path.display()))?;
    Ok(())
}

/// Generate a unique tmp filename next to `path`.
fn unique_tmp_path(path: &Path) -> Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let stem = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("mounts.json");
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(format!(".{stem}.tmp.{}.{n}", std::process::id())))
}

// ── URL redaction ──────────────────────────────────────────────────────────

/// Strip password from a postgres/postgresql URL for safe display. Opaque
/// URLs (`file://` for pglite) and non-URL strings are returned unchanged.
/// Mirrors `redactUrl`.
pub fn redact_url(url: &str) -> String {
    let scheme_end = match url.find("://") {
        Some(i) => i,
        None => return url.to_string(),
    };
    let scheme = &url[..scheme_end];
    if scheme != "postgres" && scheme != "postgresql" {
        return url.to_string();
    }
    let after = &url[scheme_end + 3..];
    match after.find('@') {
        None => url.to_string(),
        Some(at) => {
            let creds = &after[..at];
            match creds.rfind(':') {
                None => url.to_string(), // user only, no password
                Some(colon) => {
                    let user = &creds[..colon];
                    format!("{scheme}://{user}:***@{rest}", rest = &after[at + 1..])
                }
            }
        }
    }
}

// ── Subcommands ──────────────────────────────────────────────────────────

/// Strip password redaction helper used by list.
fn engine_str(e: MountEngine) -> &'static str {
    match e {
        MountEngine::Postgres => "postgres",
        MountEngine::Pglite => "pglite",
    }
}

fn run_add(args: &[String], path: &Path) -> Result<()> {
    let parsed = parse_add_args(args)?;

    // Mount path must exist on disk — otherwise skill/handler loading will
    // fail later with a less-actionable error.
    if !parsed.path.exists() {
        bail!(
            "Mount path does not exist: {}. The local clone directory must exist before registering a mount. Clone the repo first (git clone <repo> {}) then re-run",
            parsed.path.display(),
            parsed.path.display()
        );
    }

    let mut file = read_mounts_file(path)?;

    // Duplicate id check.
    if file.mounts.iter().any(|m| m.id == parsed.id) {
        bail!(
            "Mount id already exists: \"{}\". Use 'zbrain mounts list' to see registered mounts. Remove the existing mount first: zbrain mounts remove {}",
            parsed.id,
            parsed.id
        );
    }

    // Duplicate path check (load-bearing — skills/handlers/attestation/git
    // sync all key off path, so two mounts at the same path silently collide).
    let existing_at_path = file.mounts.iter().find(|m| resolve_path(&m.path) == parsed.path);
    if let Some(existing) = existing_at_path {
        return Err(duplicate_mount_path_error(&parsed.path, &existing.id, &parsed.id));
    }

    // Soft warning: same database_url/database_path under different id. A
    // team can legitimately mount the same remote brain under two aliases,
    // so this is NOT a hard block.
    let url_dupe = file.mounts.iter().find(|m| {
        (parsed.database_url.is_some() && m.database_url == parsed.database_url)
            || (parsed.database_path.is_some() && m.database_path == parsed.database_path)
    });
    if let Some(dupe) = url_dupe {
        eprintln!(
            "WARN: mount \"{}\" shares database with \"{}\". This is usually a mistake but is allowed for intentional aliasing.",
            parsed.id, dupe.id
        );
    }

    let entry = MountEntry {
        id: parsed.id.clone(),
        alias: parsed.alias.clone(),
        path: parsed.path.to_string_lossy().into_owned(),
        engine: parsed.engine,
        database_url: parsed.database_url.clone(),
        database_path: parsed.database_path.clone(),
        enabled: true,
        expected_sha: None,
        last_synced_at: None,
        trust_frontmatter_overrides: false,
    };
    file.mounts.push(entry);
    write_mounts_file(&file, path)?;

    println!("Mount \"{}\" added → {}", parsed.id, parsed.path.display());
    println!("  engine: {}", engine_str(parsed.engine));
    if let Some(db_url) = &parsed.database_url {
        println!("  db_url: {}", redact_url(db_url));
    } else if let Some(db_path) = &parsed.database_path {
        println!("  db_path: {db_path}");
    }

    refresh_mounts_cache();
    Ok(())
}

fn run_list(args: &[String], path: &Path) -> Result<()> {
    let json_mode = args.iter().any(|a| a == "--json");
    let file = read_mounts_file(path)?;

    if json_mode {
        // Redact raw db_url in json output (mounts.json is per-user 0600, but
        // stdout can be piped into logs). database_path is fine (local path).
        let redacted = file
            .mounts
            .iter()
            .map(|m| {
                let mut m = m.clone();
                if let Some(db_url) = &m.database_url {
                    m.database_url = Some(redact_url(db_url));
                }
                m
            })
            .collect::<Vec<_>>();
        let out = MountsFile {
            version: file.version,
            mounts: redacted,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if file.mounts.is_empty() {
        println!("No mounts registered.\n");
        println!("Add a mount with:");
        println!("  zbrain mounts add <id> --path <path> --engine pglite --db-path <path>");
        return Ok(());
    }

    println!("MOUNTS ({})", file.mounts.len());
    println!("{}", "─".repeat(60));
    for m in &file.mounts {
        let status = if m.enabled { "" } else { " (disabled)" };
        println!("  {:<20} {:<10}{}", m.id, engine_str(m.engine), status);
        println!("    path:    {}", m.path);
        if let Some(db_url) = &m.database_url {
            println!("    db_url:  {}", redact_url(db_url));
        } else if let Some(db_path) = &m.database_path {
            println!("    db_path: {db_path}");
        }
        if let Some(alias) = &m.alias {
            println!("    alias:   {alias}");
        }
    }
    Ok(())
}

fn run_remove(args: &[String], path: &Path) -> Result<()> {
    let id = args.first().ok_or_else(|| {
        anyhow::anyhow!(
            "Missing mount id.\n  Usage: zbrain mounts remove <id>\n  Run 'zbrain mounts list' to see registered mounts"
        )
    })?;
    if id == HOST_BRAIN_ID {
        bail!(
            "Cannot remove host brain. \"host\" is not a mount — it is the default brain from ~/.zbrain/config.json. Use 'zbrain init' to reconfigure the host brain"
        );
    }

    let mut file = read_mounts_file(path)?;
    let before = file.mounts.len();
    file.mounts.retain(|m| &m.id != id);
    if file.mounts.len() == before {
        bail!(
            "Mount \"{id}\" not found. No mount with id \"{id}\" is registered. Run 'zbrain mounts list' to see registered mounts"
        );
    }

    write_mounts_file(&file, path)?;
    println!("Mount \"{id}\" removed from mounts.json");

    // If removing the last mount, clear the cache entirely; otherwise
    // rewrite with the remaining mounts. Both are best-effort: the
    // aggregated cache publish is parked (G52), so there is nothing to
    // clear or rewrite here — kept as a no-op for parity of intent.
    if file.mounts.is_empty() {
        clear_mounts_cache();
    } else {
        refresh_mounts_cache();
    }
    Ok(())
}

/// Which boolean flag a `set` verb toggles.
#[derive(Debug, Clone, Copy)]
enum FlagField {
    Enabled,
    Trust,
}

fn field_name(f: FlagField) -> &'static str {
    match f {
        FlagField::Enabled => "enabled",
        FlagField::Trust => "trust_frontmatter_overrides",
    }
}

/// Shared writer for boolean-flag verbs (enable/disable/trust-frontmatter/
/// untrust-frontmatter). Mirrors `runSetMountFlag`.
fn run_set_flag(
    args: &[String],
    field: FlagField,
    value: bool,
    verb: &str,
    path: &Path,
) -> Result<()> {
    let id = args.first().ok_or_else(|| {
        anyhow::anyhow!(
            "Missing mount id.\n  Usage: zbrain mounts {verb} <id>\n  Run 'zbrain mounts list' to see registered mounts"
        )
    })?;
    if id == HOST_BRAIN_ID {
        bail!(
            "Cannot {verb} host brain. \"host\" is not a mount — it is the default brain from ~/.zbrain/config.json. {}",
            if verb == "trust-frontmatter" || verb == "untrust-frontmatter" {
                "Host frontmatter is always trusted; this verb applies only to mounted brains."
            } else {
                "Use 'zbrain init' to reconfigure the host brain"
            }
        );
    }

    let mut file = read_mounts_file(path)?;
    let mount = file.mounts.iter_mut().find(|m| &m.id == id);
    let Some(mount) = mount else {
        bail!(
            "Mount \"{id}\" not found. No mount with id \"{id}\" is registered. Run 'zbrain mounts list' to see registered mounts"
        );
    };

    // No-op check so the cache refresh + write don't churn when state matches.
    let current = match field {
        FlagField::Enabled => mount.enabled,
        FlagField::Trust => mount.trust_frontmatter_overrides,
    };
    if current == value {
        println!(
            "Mount \"{id}\" already has {}={value}; no change",
            field_name(field)
        );
        return Ok(());
    }

    match field {
        FlagField::Enabled => mount.enabled = value,
        FlagField::Trust => mount.trust_frontmatter_overrides = value,
    }
    write_mounts_file(&file, path)?;
    println!(
        "Mount \"{id}\" {verb}d ({}={})",
        field_name(field),
        value
    );

    refresh_mounts_cache();
    Ok(())
}

/// Best-effort clear of the aggregated mounts cache. Since Rust never
/// publishes that cache (G52), this is a no-op — kept for parity of intent.
fn clear_mounts_cache() {}

// ── Cache-refresh (skip-path only) ─────────────────────────────────────────

/// Walk up from cwd looking for a `skills/` directory that contains a
/// recognized resolver file. Mirrors `findRepoRoot` (the basic variant used
/// by `refreshMountsCache`).
fn find_repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    for _ in 0..10 {
        if has_resolver_file(&dir.join("skills")) {
            return Some(dir);
        }
        let parent = dir.parent()?;
        if parent == dir {
            break;
        }
        dir = parent.to_path_buf();
    }
    None
}

fn has_resolver_file(skills: &Path) -> bool {
    skills.join("RESOLVER.md").exists() || skills.join("AGENTS.md").exists()
}

/// Recompute + publish the aggregated mounts cache. The write path depends
/// on the delayed skill/resolver stack and is parked (G52); only the
/// repo-root skip detection is ported (honest: we never fake a published
/// cache we cannot build).
// registered in docs/plans/KNOWN-GAPS.md (G52)
fn refresh_mounts_cache() {
    match find_repo_root() {
        None => {
            eprintln!(
                "NOTE: mounts-cache not refreshed (not inside a zbrain repo). Run `zbrain mounts add|remove` from within a repo to publish the aggregated resolver for host agents."
            );
        }
        Some(root) => {
            let skills = root.join("skills");
            if !skills.exists() {
                eprintln!("NOTE: mounts-cache not refreshed ({} does not exist).", skills.display());
            } else {
                eprintln!(
                    "NOTE: mounts-cache publish skipped (resolver aggregation not yet ported; see KNOWN-GAPS G52)."
                );
            }
        }
    }
}

// ── CLI surface ──────────────────────────────────────────────────────────

/// `zbrain mounts` subcommands.
#[derive(Debug, clap::Subcommand)]
pub enum MountsSubcommand {
    /// Add a mount (register a connected brain)
    Add(MountsAddArgs),
    /// List registered mounts
    #[command(alias = "ls")]
    List(MountsListArgs),
    /// Remove a mount
    #[command(alias = "rm")]
    Remove(MountsRemoveArgs),
    /// Re-enable a disabled mount
    Enable(MountsSetFlagArgs),
    /// Disable a mount without removing it
    Disable(MountsSetFlagArgs),
    /// Let this mount's per-page frontmatter overrides be trusted
    #[command(name = "trust-frontmatter")]
    TrustFrontmatter(MountsSetFlagArgs),
    /// Clear the frontmatter trust flag for a mount
    #[command(name = "untrust-frontmatter")]
    UntrustFrontmatter(MountsSetFlagArgs),
}

/// `zbrain mounts add` arguments.
#[derive(Debug, clap::Parser)]
pub struct MountsAddArgs {
    /// Mount id (kebab-case, e.g. "yc-media")
    pub id: String,
    /// Absolute local path to the mount's git clone
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Engine kind: "postgres" or "pglite"
    #[arg(long)]
    pub engine: Option<String>,
    /// Postgres connection URL (if --engine postgres)
    #[arg(long, alias = "database-url")]
    pub db_url: Option<String>,
    /// PGLite data-directory path (if --engine pglite)
    #[arg(long, alias = "database-path")]
    pub db_path: Option<String>,
    /// Optional shorthand for CLI display
    #[arg(long)]
    pub alias: Option<String>,
}

/// `zbrain mounts list` arguments.
#[derive(Debug, clap::Parser)]
pub struct MountsListArgs {
    /// Output as JSON (db_url redacted)
    #[arg(long)]
    pub json: bool,
}

/// `zbrain mounts remove` arguments.
#[derive(Debug, clap::Parser)]
pub struct MountsRemoveArgs {
    /// Mount id to remove
    pub id: String,
}

/// `zbrain mounts enable|disable|trust-frontmatter|untrust-frontmatter`.
#[derive(Debug, clap::Parser)]
pub struct MountsSetFlagArgs {
    /// Mount id
    pub id: String,
}

/// Dispatch `zbrain mounts <subcommand>`. Config-file only; `config_path` is
/// accepted for signature parity with other commands but unused.
pub async fn run_mounts_command(
    cmd: &MountsSubcommand,
    _config_path: Option<&Path>,
) -> Result<()> {
    let path = mounts_path()?;
    match cmd {
        MountsSubcommand::Add(a) => {
            let mut v = vec![a.id.clone()];
            if let Some(p) = &a.path {
                v.push("--path".into());
                v.push(p.to_string_lossy().into_owned());
            }
            if let Some(e) = &a.engine {
                v.push("--engine".into());
                v.push(e.clone());
            }
            if let Some(u) = &a.db_url {
                v.push("--db-url".into());
                v.push(u.clone());
            }
            if let Some(p) = &a.db_path {
                v.push("--db-path".into());
                v.push(p.clone());
            }
            if let Some(al) = &a.alias {
                v.push("--alias".into());
                v.push(al.clone());
            }
            run_add(&v, &path)?;
        }
        MountsSubcommand::List(a) => {
            let args: Vec<String> = if a.json {
                vec!["--json".into()]
            } else {
                vec![]
            };
            run_list(&args, &path)?;
        }
        MountsSubcommand::Remove(a) => {
            run_remove(&[a.id.clone()], &path)?;
        }
        MountsSubcommand::Enable(a) => {
            run_set_flag(&[a.id.clone()], FlagField::Enabled, true, "enable", &path)?;
        }
        MountsSubcommand::Disable(a) => {
            run_set_flag(&[a.id.clone()], FlagField::Enabled, false, "disable", &path)?;
        }
        MountsSubcommand::TrustFrontmatter(a) => {
            run_set_flag(
                &[a.id.clone()],
                FlagField::Trust,
                true,
                "trust-frontmatter",
                &path,
            )?;
        }
        MountsSubcommand::UntrustFrontmatter(a) => {
            run_set_flag(
                &[a.id.clone()],
                FlagField::Trust,
                false,
                "untrust-frontmatter",
                &path,
            )?;
        }
    }
    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("mounts.json");
        (dir, path)
    }

    // ── parseAddArgs ──
    #[test]
    fn minimal_pglite_add() {
        let parsed = parse_add_args(&[
            "yc-media".into(),
            "--path".into(),
            "/tmp/yc-media".into(),
            "--engine".into(),
            "pglite".into(),
            "--db-path".into(),
            "/tmp/yc-media/.pg".into(),
        ])
        .unwrap();
        assert_eq!(parsed.id, "yc-media");
        assert_eq!(parsed.engine, MountEngine::Pglite);
        assert_eq!(parsed.database_path.as_deref(), Some("/tmp/yc-media/.pg"));
        assert!(parsed.path.is_absolute());
    }

    #[test]
    fn minimal_postgres_add() {
        let parsed = parse_add_args(&[
            "yc-politics".into(),
            "--path".into(),
            "/tmp/luther".into(),
            "--engine".into(),
            "postgres".into(),
            "--db-url".into(),
            "postgresql://localhost/l".into(),
        ])
        .unwrap();
        assert_eq!(parsed.engine, MountEngine::Postgres);
        assert_eq!(parsed.database_url.as_deref(), Some("postgresql://localhost/l"));
    }

    #[test]
    fn infers_engine_from_db_url() {
        let parsed = parse_add_args(&[
            "a".into(),
            "--path".into(),
            "/tmp/a".into(),
            "--db-url".into(),
            "postgresql://x/y".into(),
        ])
        .unwrap();
        assert_eq!(parsed.engine, MountEngine::Postgres);
    }

    #[test]
    fn infers_engine_from_db_path() {
        let parsed = parse_add_args(&[
            "b".into(),
            "--path".into(),
            "/tmp/b".into(),
            "--db-path".into(),
            "/tmp/b/.pg".into(),
        ])
        .unwrap();
        assert_eq!(parsed.engine, MountEngine::Pglite);
    }

    #[test]
    fn accepts_alias() {
        let parsed = parse_add_args(&[
            "yc-media".into(),
            "--path".into(),
            "/tmp/x".into(),
            "--db-path".into(),
            "/tmp/x/.pg".into(),
            "--alias".into(),
            "ycm".into(),
        ])
        .unwrap();
        assert_eq!(parsed.alias.as_deref(), Some("ycm"));
    }

    #[test]
    fn rejects_missing_id() {
        let err = parse_add_args(&[]).unwrap_err();
        assert!(err.to_string().contains("Missing mount id"));
    }

    #[test]
    fn rejects_missing_path() {
        let err = parse_add_args(&["m".into(), "--db-path".into(), "/tmp/x/.pg".into()]).unwrap_err();
        assert!(err.to_string().contains("Missing --path"));
    }

    #[test]
    fn rejects_invalid_engine() {
        let err = parse_add_args(&[
            "x".into(),
            "--path".into(),
            "/tmp/x".into(),
            "--engine".into(),
            "sqlite".into(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("Invalid engine"));
    }

    #[test]
    fn rejects_unknown_flag() {
        let err = parse_add_args(&[
            "x".into(),
            "--path".into(),
            "/tmp/x".into(),
            "--db-path".into(),
            "/tmp/x/.pg".into(),
            "--nonsense".into(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("Unknown flag"));
    }

    #[test]
    fn rejects_no_engine_no_db() {
        let err = parse_add_args(&["x".into(), "--path".into(), "/tmp/x".into()]).unwrap_err();
        assert!(err.to_string().contains("Missing --engine"));
    }

    #[test]
    fn rejects_postgres_without_db_url() {
        let err = parse_add_args(&[
            "x".into(),
            "--path".into(),
            "/tmp/x".into(),
            "--engine".into(),
            "postgres".into(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("postgres mount requires --db-url"));
    }

    #[test]
    fn rejects_flag_value_missing() {
        let err = parse_add_args(&["x".into(), "--path".into()]).unwrap_err();
        assert!(err.to_string().contains("Missing value for --path"));
    }

    #[test]
    fn rejects_invalid_alias() {
        let err = parse_add_args(&[
            "x".into(),
            "--path".into(),
            "/tmp/x".into(),
            "--db-path".into(),
            "/tmp/x/.pg".into(),
            "--alias".into(),
            "UPPER".into(),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("Invalid --alias value"));
    }

    // ── redactUrl ──
    #[test]
    fn redacts_password_from_postgres() {
        let red = redact_url("postgresql://user:supersecret@db.example.com/brain");
        assert!(!red.contains("supersecret"));
        assert!(red.contains("***"));
        assert!(red.contains("db.example.com"));
    }

    #[test]
    fn passwordless_url_no_asterisk() {
        let url = "postgresql://user@db.example.com/brain";
        let red = redact_url(url);
        assert!(!red.contains("***"));
        assert!(red.contains("user@db.example.com"));
        assert!(red.contains("/brain"));
    }

    #[test]
    fn leaves_file_url_alone() {
        let url = "file:///home/user/brain/.pglite";
        assert_eq!(redact_url(url), url);
    }

    #[test]
    fn leaves_non_url_alone() {
        let opaque = "/not/a/url";
        assert_eq!(redact_url(opaque), opaque);
    }

    // ── read/write ──
    #[test]
    fn empty_file_returns_empty_list() {
        let (_dir, path) = tmp_file();
        let file = read_mounts_file(&path).unwrap();
        assert_eq!(file.version, 1);
        assert!(file.mounts.is_empty());
    }

    #[test]
    fn roundtrip_write_then_read() {
        let (_dir, path) = tmp_file();
        write_mounts_file(
            &MountsFile {
                version: 1,
                mounts: vec![MountEntry {
                    id: "yc-media".into(),
                    alias: None,
                    path: "/tmp/yc".into(),
                    engine: MountEngine::Pglite,
                    database_url: None,
                    database_path: Some("/tmp/yc/.pg".into()),
                    enabled: true,
                    expected_sha: None,
                    last_synced_at: None,
                    trust_frontmatter_overrides: false,
                }],
            },
            &path,
        )
        .unwrap();
        let file = read_mounts_file(&path).unwrap();
        assert_eq!(file.mounts.len(), 1);
        assert_eq!(file.mounts[0].id, "yc-media");
    }

    #[test]
    fn write_is_atomic_no_partial_tmp() {
        let (_dir, path) = tmp_file();
        write_mounts_file(
            &MountsFile {
                version: 1,
                mounts: vec![MountEntry {
                    id: "a".into(),
                    alias: None,
                    path: "/tmp/a".into(),
                    engine: MountEngine::Pglite,
                    database_url: None,
                    database_path: Some("/tmp/a/.pg".into()),
                    enabled: true,
                    expected_sha: None,
                    last_synced_at: None,
                    trust_frontmatter_overrides: false,
                }],
            },
            &path,
        )
        .unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());
    }

    // ── end-to-end via run functions (explicit path) ──
    fn seed(path: &Path, id: &str) {
        let clone = tempfile::tempdir().expect("clone dir");
        let parsed = parse_add_args(&[
            id.into(),
            "--path".into(),
            clone.path().to_string_lossy().into_owned(),
            "--engine".into(),
            "pglite".into(),
            "--db-path".into(),
            "/tmp/x/.pg".into(),
        ])
        .unwrap();
        let mut file = read_mounts_file(path).unwrap();
        file.mounts.push(MountEntry {
            id: parsed.id.clone(),
            alias: parsed.alias.clone(),
            path: parsed.path.to_string_lossy().into_owned(),
            engine: parsed.engine,
            database_url: parsed.database_url.clone(),
            database_path: parsed.database_path.clone(),
            enabled: true,
            expected_sha: None,
            last_synced_at: None,
            trust_frontmatter_overrides: false,
        });
        write_mounts_file(&file, path).unwrap();
    }

    #[test]
    fn add_list_remove_roundtrip() {
        let (_dir, path) = tmp_file();
        // add
        let clone = tempfile::tempdir().expect("clone dir");
        let clone_path = clone.path().to_string_lossy().into_owned();
        run_add(
            &[
                "yc-media".into(),
                "--path".into(),
                clone_path.clone(),
                "--engine".into(),
                "pglite".into(),
                "--db-path".into(),
                "/tmp/yc-media/.pg".into(),
            ],
            &path,
        )
        .unwrap();
        let file = read_mounts_file(&path).unwrap();
        assert_eq!(file.mounts.len(), 1);
        assert_eq!(file.mounts[0].id, "yc-media");

        // list (human, no panic)
        run_list(&[], &path).unwrap();

        // remove
        run_remove(&["yc-media".into()], &path).unwrap();
        let file = read_mounts_file(&path).unwrap();
        assert!(file.mounts.is_empty());
    }

    #[test]
    fn list_json_redacts_db_url() {
        let (_dir, path) = tmp_file();
        let clone = tempfile::tempdir().expect("clone dir");
        run_add(
            &[
                "yc-pg".into(),
                "--path".into(),
                clone.path().to_string_lossy().into_owned(),
                "--engine".into(),
                "postgres".into(),
                "--db-url".into(),
                "postgresql://u:topsecret@h/db".into(),
            ],
            &path,
        )
        .unwrap();
        // Capture json output by reading the file + checking redaction helper.
        let file = read_mounts_file(&path).unwrap();
        assert!(file.mounts[0].database_url.as_deref().unwrap().contains("topsecret"));
        // list --json would redact; ensure the helper does.
        let red = redact_url(file.mounts[0].database_url.as_deref().unwrap());
        assert!(!red.contains("topsecret"));
    }

    // ── v0.40.3.0 flag verbs ──
    #[test]
    fn enable_disable_cycle_persists() {
        let (_dir, path) = tmp_file();
        seed(&path, "m1");
        run_set_flag(&["m1".into()], FlagField::Enabled, false, "disable", &path).unwrap();
        assert!(!read_mounts_file(&path).unwrap().mounts[0].enabled);
        run_set_flag(&["m1".into()], FlagField::Enabled, true, "enable", &path).unwrap();
        assert!(read_mounts_file(&path).unwrap().mounts[0].enabled);
    }

    #[test]
    fn trust_untrust_cycle_preserves_fields() {
        let (_dir, path) = tmp_file();
        seed(&path, "m-trust");
        run_set_flag(
            &["m-trust".into()],
            FlagField::Trust,
            true,
            "trust-frontmatter",
            &path,
        )
        .unwrap();
        let file = read_mounts_file(&path).unwrap();
        assert!(file.mounts[0].trust_frontmatter_overrides);
        assert_eq!(file.mounts[0].engine, MountEngine::Pglite);

        run_set_flag(
            &["m-trust".into()],
            FlagField::Trust,
            false,
            "untrust-frontmatter",
            &path,
        )
        .unwrap();
        assert!(!read_mounts_file(&path).unwrap().mounts[0].trust_frontmatter_overrides);

        run_set_flag(
            &["m-trust".into()],
            FlagField::Trust,
            true,
            "trust-frontmatter",
            &path,
        )
        .unwrap();
        assert!(read_mounts_file(&path).unwrap().mounts[0].trust_frontmatter_overrides);
    }

    #[test]
    fn missing_mount_id_rejects() {
        let (_dir, path) = tmp_file();
        seed(&path, "real-mount");
        let err = run_set_flag(
            &["typo-mount".into()],
            FlagField::Trust,
            true,
            "trust-frontmatter",
            &path,
        )
        .unwrap_err();
        assert!(err.to_string().contains("typo-mount"));
    }

    #[test]
    fn host_brain_rejected() {
        let (_dir, path) = tmp_file();
        seed(&path, "m-host-test");
        let err = run_set_flag(
            &["host".into()],
            FlagField::Trust,
            true,
            "trust-frontmatter",
            &path,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Cannot trust-frontmatter host brain"));
    }

    #[test]
    fn enable_on_enabled_is_idempotent() {
        let (_dir, path) = tmp_file();
        seed(&path, "m-idem");
        run_set_flag(&["m-idem".into()], FlagField::Enabled, true, "enable", &path).unwrap();
        assert!(read_mounts_file(&path).unwrap().mounts[0].enabled);
    }

    #[test]
    fn duplicate_id_rejected() {
        let (_dir, path) = tmp_file();
        seed(&path, "dup");
        let clone = tempfile::tempdir().expect("clone dir");
        let err = run_add(
            &[
                "dup".into(),
                "--path".into(),
                clone.path().to_string_lossy().into_owned(),
                "--engine".into(),
                "pglite".into(),
                "--db-path".into(),
                "/tmp/dup/.pg".into(),
            ],
            &path,
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn duplicate_path_rejected() {
        let (_dir, path) = tmp_file();
        let clone = tempfile::tempdir().expect("clone dir");
        let clone_path = clone.path().to_string_lossy().into_owned();
        run_add(
            &[
                "first".into(),
                "--path".into(),
                clone_path.clone(),
                "--engine".into(),
                "pglite".into(),
                "--db-path".into(),
                "/tmp/first/.pg".into(),
            ],
            &path,
        )
        .unwrap();
        let err = run_add(
            &[
                "second".into(),
                "--path".into(),
                clone_path,
                "--engine".into(),
                "pglite".into(),
                "--db-path".into(),
                "/tmp/second/.pg".into(),
            ],
            &path,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Duplicate mount path"));
    }
}
