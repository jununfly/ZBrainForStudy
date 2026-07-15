//! `zbrain schema` subcommand — 27 verbs (9 inspection + 3 activation + 15 authoring).
//!
//! Tracer bullet: first end-to-end CLI → core → DB slice for schema-pack.
//!
//! Inspection verbs: active, list, show, validate, graph, lint, stats, explain, usage.
//! Activation verbs: use, downgrade, reload.
//! Authoring verbs: init, fork, edit, diff, add-type, remove-type, update-type,
//!   add-alias, remove-alias, add-prefix, remove-prefix,
//!   add-link-type, remove-link-type, set-extractable, set-expert-routing.

use std::path::PathBuf;

use clap::Subcommand;
use zbrain_core::schema_pack::{
    activate,
    lint_rules,
    loader::load_pack_from_string,
    manifest::{self, PackPrimitive, SchemaPackManifest},
    mutate,
    registry,
};

/// Built-in pack names.
const BUILTIN_PACKS: &[&str] = &[
    "zbrain-base",
    "zbrain-recommended",
    "zbrain-creator",
    "zbrain-engineer",
    "zbrain-investor",
    "zbrain-everything",
];

/// Embedded zbrain-base pack YAML.
const ZBRAIN_BASE_YAML: &str = include_str!("schema_pack_base/zbrain-base.yaml");

/// `zbrain schema` subcommands.
#[derive(Debug, Subcommand)]
pub enum SchemaSubcommand {
    /// Show the active schema pack name and source tier
    Active,

    /// List available schema packs (built-in + user-installed)
    List,

    /// Show a pack manifest
    Show {
        /// Pack name (built-in) or file path
        pack: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Validate a pack manifest file
    Validate {
        /// Path to manifest file (.yaml/.yml/.json)
        path: String,
    },

    /// Print the type/primitive graph
    Graph {
        /// Pack name or file path
        pack: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Run lint rules on a pack
    Lint {
        /// Pack name or file path
        pack: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show per-type page statistics (requires engine)
    Stats {
        /// Source ID filter
        #[arg(long)]
        source: Option<String>,
    },

    /// Explain resolved settings for a type
    Explain {
        /// Type name to explain
        type_name: String,
        /// Pack name or file path (optional, defaults to active)
        pack: Option<String>,
    },

    /// Show schema usage telemetry
    Usage {
        /// Time window (e.g. 7d, 2w, 1m)
        #[arg(long)]
        since: Option<String>,
    },

    // -- Activation verbs (3) --

    /// Set the active schema pack (writes to ~/.zbrain/config.json)
    Use {
        /// Pack name to activate
        pack: String,
    },

    /// Revert to the default schema pack (zbrain-base)
    Downgrade,

    /// Flush stale pack lock files
    Reload {
        /// Specific pack name (omit to clear all)
        #[arg(long)]
        pack: Option<String>,
    },

    // -- Authoring verbs (15) --

    /// Create a new empty pack file
    Init {
        /// New pack name
        name: String,
        /// Parent pack to extend (default: zbrain-base)
        #[arg(long)]
        extends: Option<String>,
    },

    /// Copy a built-in pack to a new user pack
    Fork {
        /// Source pack name (built-in)
        source: String,
        /// Destination pack name (new user pack)
        dest: String,
    },

    /// Open a pack file in $EDITOR (prints path if no editor)
    Edit {
        /// Pack name or file path
        pack: String,
    },

    /// Show diff between a pack and its parent (stub)
    Diff {
        /// Pack name or file path
        pack: String,
    },

    /// Add a page type to a pack
    AddType {
        /// Pack name
        pack: String,
        /// Type name
        name: String,
        /// Primitive (entity, media, temporal, annotation, concept)
        primitive: String,
        /// Path prefix
        #[arg(long)]
        prefix: Option<String>,
        /// Extractable flag
        #[arg(long)]
        extractable: bool,
        /// Expert routing flag
        #[arg(long)]
        expert_routing: bool,
    },

    /// Remove a page type from a pack
    RemoveType {
        /// Pack name
        pack: String,
        /// Type name
        type_name: String,
    },

    /// Update a page type (partial patch)
    UpdateType {
        /// Pack name
        pack: String,
        /// Type name
        type_name: String,
        /// Set extractable flag true
        #[arg(long, action = clap::ArgAction::SetTrue)]
        extractable: bool,
        /// Set expert routing flag true
        #[arg(long, action = clap::ArgAction::SetTrue)]
        expert_routing: bool,
    },

    /// Add an alias to a type (idempotent)
    AddAlias {
        pack: String,
        type_name: String,
        alias: String,
    },

    /// Remove an alias from a type (idempotent)
    RemoveAlias {
        pack: String,
        type_name: String,
        alias: String,
    },

    /// Add a path prefix to a type (idempotent)
    AddPrefix {
        pack: String,
        type_name: String,
        prefix: String,
    },

    /// Remove a path prefix from a type (idempotent)
    RemovePrefix {
        pack: String,
        type_name: String,
        prefix: String,
    },

    /// Add a link type to a pack
    AddLinkType {
        pack: String,
        name: String,
        /// Inverse link type name
        #[arg(long)]
        inverse: Option<String>,
    },

    /// Remove a link type from a pack
    RemoveLinkType {
        pack: String,
        name: String,
    },

    /// Set the extractable flag on a type
    SetExtractable {
        pack: String,
        type_name: String,
        #[arg(action = clap::ArgAction::Set)]
        value: bool,
    },

    /// Set the expert_routing flag on a type
    SetExpertRouting {
        pack: String,
        type_name: String,
        #[arg(action = clap::ArgAction::Set)]
        value: bool,
    },
}

/// Run a `zbrain schema` subcommand.
pub fn run_schema_pack_command(cmd: SchemaSubcommand) -> anyhow::Result<()> {
    match cmd {
        SchemaSubcommand::Active => run_active(),
        SchemaSubcommand::List => run_list(),
        SchemaSubcommand::Show { pack, json } => run_show(pack.as_deref(), json),
        SchemaSubcommand::Validate { path } => run_validate(&path),
        SchemaSubcommand::Graph { pack, json } => run_graph(pack.as_deref(), json),
        SchemaSubcommand::Lint { pack, json } => run_lint(pack.as_deref(), json),
        SchemaSubcommand::Stats { source } => run_stats(source.as_deref()),
        SchemaSubcommand::Explain { type_name, pack } => run_explain(&type_name, pack.as_deref()),
        SchemaSubcommand::Usage { since } => run_usage(since.as_deref()),
        // Activation
        SchemaSubcommand::Use { pack } => run_use(&pack),
        SchemaSubcommand::Downgrade => run_downgrade(),
        SchemaSubcommand::Reload { pack } => run_reload(pack.as_deref()),
        // Authoring
        SchemaSubcommand::Init { name, extends } => run_init(&name, extends.as_deref()),
        SchemaSubcommand::Fork { source, dest } => run_fork(&source, &dest),
        SchemaSubcommand::Edit { pack } => run_edit(&pack),
        SchemaSubcommand::Diff { pack } => run_diff(&pack),
        SchemaSubcommand::AddType { pack, name, primitive, prefix, extractable, expert_routing } => {
            run_add_type(&pack, &name, &primitive, prefix.as_deref(), extractable, expert_routing)
        }
        SchemaSubcommand::RemoveType { pack, type_name } => run_remove_type(&pack, &type_name),
        SchemaSubcommand::UpdateType { pack, type_name, extractable, expert_routing } => {
            run_update_type(&pack, &type_name, extractable, expert_routing)
        }
        SchemaSubcommand::AddAlias { pack, type_name, alias } => run_add_alias(&pack, &type_name, &alias),
        SchemaSubcommand::RemoveAlias { pack, type_name, alias } => run_remove_alias(&pack, &type_name, &alias),
        SchemaSubcommand::AddPrefix { pack, type_name, prefix } => run_add_prefix(&pack, &type_name, &prefix),
        SchemaSubcommand::RemovePrefix { pack, type_name, prefix } => run_remove_prefix(&pack, &type_name, &prefix),
        SchemaSubcommand::AddLinkType { pack, name, inverse } => run_add_link_type(&pack, &name, inverse.as_deref()),
        SchemaSubcommand::RemoveLinkType { pack, name } => run_remove_link_type(&pack, &name),
        SchemaSubcommand::SetExtractable { pack, type_name, value } => run_set_extractable(&pack, &type_name, value),
        SchemaSubcommand::SetExpertRouting { pack, type_name, value } => run_set_expert_routing(&pack, &type_name, value),
    }
}

// ---------------------------------------------------------------------------
// Verb handlers
// ---------------------------------------------------------------------------

fn run_active() -> anyhow::Result<()> {
    let env_var = std::env::var("ZBRAIN_SCHEMA_PACK").ok().filter(|s| !s.is_empty());
    let home_config = activate::get_active_pack_from_config();
    let input = registry::ResolutionInput {
        env_var,
        home_config,
        remote: false,
        ..Default::default()
    };
    let result = registry::resolve_active_pack_name(&input);
    println!("Active pack: {}", result.pack_name);
    println!("Source: {}", result.source.as_str());
    Ok(())
}

fn run_list() -> anyhow::Result<()> {
    println!("Built-in packs:");
    for name in BUILTIN_PACKS {
        println!("  {name}");
    }

    // Scan user packs
    let user_dir = user_pack_dir();
    if user_dir.exists() {
        let mut user_packs: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&user_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        user_packs.push(name.to_string());
                    }
                }
            }
        }
        if !user_packs.is_empty() {
            user_packs.sort();
            println!("\nUser packs (~/.zbrain/schema-packs/):");
            for name in &user_packs {
                println!("  {name}");
            }
        }
    }

    Ok(())
}

fn run_show(pack: Option<&str>, json: bool) -> anyhow::Result<()> {
    let manifest = load_pack(pack)?;
    if json {
        let json_str = serde_json::to_string_pretty(&manifest)?;
        println!("{json_str}");
    } else {
        print_manifest_human(&manifest);
    }
    Ok(())
}

fn run_validate(path: &str) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("cannot read file {path}: {e}"))?;
    match load_pack_from_string(&content, path) {
        Ok(m) => {
            println!("OK: {} v{} ({} types, {} link types)",
                m.name, m.version, m.page_types.len(), m.link_types.len());
            Ok(())
        }
        Err(e) => {
            eprintln!("VALIDATION ERROR: {e}");
            std::process::exit(1);
        }
    }
}

fn run_graph(pack: Option<&str>, json: bool) -> anyhow::Result<()> {
    let m = load_pack(pack)?;
    if json {
        let types: Vec<serde_json::Value> = m.page_types.iter().map(|pt| {
            serde_json::json!({
                "name": pt.name,
                "primitive": format!("{:?}", pt.primitive).to_lowercase(),
                "aliases": pt.aliases,
            })
        }).collect();
        let output = serde_json::json!({ "types": types });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Type graph for {}:", m.name);
        for pt in &m.page_types {
            let aliases = if pt.aliases.is_empty() {
                String::new()
            } else {
                format!(" (aliases: {})", pt.aliases.join(", "))
            };
            println!("  {:?} → {}{}", pt.primitive, pt.name, aliases);
        }
    }
    Ok(())
}

fn run_lint(pack: Option<&str>, json: bool) -> anyhow::Result<()> {
    let m = load_pack(pack)?;
    let report = lint_rules::run_file_plane_lint_rules(&m);
    if json {
        let output = serde_json::json!({
            "schema_version": 1,
            "ok": report.ok,
            "errors": report.errors.len(),
            "warnings": report.warnings.len(),
            "issues": report.errors.iter().chain(report.warnings.iter()).map(|i| {
                serde_json::json!({
                    "rule": i.rule,
                    "severity": i.severity.as_str(),
                    "message": i.message,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        if report.ok && report.warnings.is_empty() {
            println!("No issues found.");
        } else {
            for issue in &report.errors {
                println!("[ERROR] {}: {}", issue.rule, issue.message);
            }
            for issue in &report.warnings {
                println!("[WARN]  {}: {}", issue.rule, issue.message);
            }
        }
    }
    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn run_stats(_source: Option<&str>) -> anyhow::Result<()> {
    // Tracer bullet stub: stats requires a connected engine.
    // Full implementation in a later node (needs BrainEngine trait).
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": 1,
        "status": "not_implemented",
        "message": "stats requires engine connection; will be wired in a later node",
    }))?);
    Ok(())
}

fn run_explain(type_name: &str, pack: Option<&str>) -> anyhow::Result<()> {
    let m = load_pack(pack)?;
    let pt = m.page_types.iter().find(|pt| pt.name == type_name)
        .ok_or_else(|| anyhow::anyhow!("type \"{type_name}\" not found in pack \"{}\"", m.name))?;

    println!("Type: {}", pt.name);
    println!("Primitive: {:?}", pt.primitive);
    println!("Extractable: {}", pt.extractable);
    println!("Expert routing: {}", pt.expert_routing);
    if !pt.path_prefixes.is_empty() {
        println!("Path prefixes: {}", pt.path_prefixes.join(", "));
    }
    if !pt.aliases.is_empty() {
        println!("Aliases: {}", pt.aliases.join(", "));
    }
    Ok(())
}

fn run_usage(_since: Option<&str>) -> anyhow::Result<()> {
    // Tracer bullet stub: usage requires telemetry data.
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({
        "schema_version": 1,
        "status": "not_implemented",
        "message": "usage telemetry will be wired in a later node",
    }))?);
    Ok(())
}

// ---------------------------------------------------------------------------
// Activation verb handlers
// ---------------------------------------------------------------------------

fn run_use(pack: &str) -> anyhow::Result<()> {
    // Verify the pack exists
    if !BUILTIN_PACKS.contains(&pack) {
        let dir = user_pack_dir().join(pack);
        if !dir.join("pack.yaml").exists() && !dir.join("pack.json").exists() {
            anyhow::bail!("pack \"{pack}\" not found (not built-in and no user pack file)");
        }
    }
    activate::set_active_pack(pack).map_err(|e| anyhow::anyhow!("cannot set active pack: {e}"))?;
    println!("Active pack set to: {pack}");
    Ok(())
}

fn run_downgrade() -> anyhow::Result<()> {
    activate::clear_active_pack().map_err(|e| anyhow::anyhow!("cannot clear active pack: {e}"))?;
    println!("Active pack cleared. Defaulting to zbrain-base.");
    Ok(())
}

fn run_reload(pack: Option<&str>) -> anyhow::Result<()> {
    let cleared = activate::reload_pack_cache(pack);
    if cleared.is_empty() {
        println!("No stale lock files found.");
    } else {
        println!("Cleared {} lock file(s):", cleared.len());
        for path in &cleared {
            println!("  {path}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Authoring verb handlers
// ---------------------------------------------------------------------------

fn run_init(name: &str, extends: Option<&str>) -> anyhow::Result<()> {
    let dir = user_pack_dir().join(name);
    if dir.exists() && (dir.join("pack.yaml").exists() || dir.join("pack.json").exists()) {
        anyhow::bail!("pack \"{name}\" already exists at {}", dir.display());
    }

    let parent = extends.unwrap_or("zbrain-base");
    let m = SchemaPackManifest {
        name: name.to_string(),
        version: "1.0.0".to_string(),
        extends: Some(parent.to_string()),
        ..Default::default()
    };

    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("cannot create pack dir: {e}"))?;
    let path = dir.join("pack.yaml");
    let yaml = serde_yaml::to_string(&m)?;
    std::fs::write(&path, yaml)?;

    println!("Created pack \"{name}\" at {}", path.display());
    println!("Extends: {parent}");
    Ok(())
}

fn run_fork(source: &str, dest: &str) -> anyhow::Result<()> {
    // Load source pack
    let m = load_pack(Some(source))?;

    let dir = user_pack_dir().join(dest);
    if dir.exists() && (dir.join("pack.yaml").exists() || dir.join("pack.json").exists()) {
        anyhow::bail!("pack \"{dest}\" already exists at {}", dir.display());
    }

    // Create forked manifest
    let mut forked = m.clone();
    forked.name = dest.to_string();
    forked.extends = Some(source.to_string());

    std::fs::create_dir_all(&dir).map_err(|e| anyhow::anyhow!("cannot create pack dir: {e}"))?;
    let path = dir.join("pack.yaml");
    let yaml = serde_yaml::to_string(&forked)?;
    std::fs::write(&path, yaml)?;

    println!("Forked \"{source}\" → \"{dest}\" at {}", path.display());
    Ok(())
}

fn run_edit(pack: &str) -> anyhow::Result<()> {
    let path = if pack.ends_with(".yaml") || pack.ends_with(".yml") || pack.ends_with(".json") {
        PathBuf::from(pack)
    } else {
        // Try user pack directory
        let dir = user_pack_dir().join(pack);
        let mut found = None;
        for file in ["pack.yaml", "pack.yml", "pack.json"] {
            let p = dir.join(file);
            if p.exists() {
                found = Some(p);
                break;
            }
        }
        found.unwrap_or_else(|| dir)
    };

    if !path.exists() {
        anyhow::bail!("pack file not found: {}", path.display());
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    println!("Pack file: {}", path.display());
    println!("To edit: {editor} {}", path.display());
    Ok(())
}

fn run_diff(pack: &str) -> anyhow::Result<()> {
    let m = load_pack(Some(pack))?;
    let parent = m.extends.as_deref().unwrap_or("zbrain-base");
    println!("Diff: {pack} vs parent {parent}");
    println!("(full diff implementation pending — use `zbrain schema show {pack}` and `zbrain schema show {parent}` to compare)");
    Ok(())
}

fn parse_primitive(s: &str) -> anyhow::Result<PackPrimitive> {
    let val = serde_json::Value::String(s.to_lowercase());
    serde_json::from_value(val)
        .map_err(|_| anyhow::anyhow!("invalid primitive \"{s}\"; expected: entity, media, temporal, annotation, concept"))
}

fn run_add_type(
    pack: &str,
    name: &str,
    primitive: &str,
    prefix: Option<&str>,
    extractable: bool,
    expert_routing: bool,
) -> anyhow::Result<()> {
    let prim = parse_primitive(primitive)?;
    let opts = mutate::AddTypeOpts {
        name: name.to_string(),
        primitive: prim,
        prefix: prefix.unwrap_or("").to_string(),
        extractable,
        expert_routing,
        ..Default::default()
    };
    let result = mutate::add_type_to_pack(pack, &opts, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Added type \"{name}\" to pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_remove_type(pack: &str, type_name: &str) -> anyhow::Result<()> {
    let result = mutate::remove_type_from_pack(pack, type_name, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Removed type \"{type_name}\" from pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_update_type(
    pack: &str,
    type_name: &str,
    extractable: bool,
    expert_routing: bool,
) -> anyhow::Result<()> {
    let opts = mutate::UpdateTypeOpts {
        name: type_name.to_string(),
        primitive: None,
        extractable: if extractable { Some(true) } else { None },
        expert_routing: if expert_routing { Some(true) } else { None },
    };
    let result = mutate::update_type_on_pack(pack, &opts, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Updated type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_add_alias(pack: &str, type_name: &str, alias: &str) -> anyhow::Result<()> {
    let result = mutate::add_alias_to_type(pack, type_name, alias, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Added alias \"{alias}\" to type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_remove_alias(pack: &str, type_name: &str, alias: &str) -> anyhow::Result<()> {
    let result = mutate::remove_alias_from_type(pack, type_name, alias, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Removed alias \"{alias}\" from type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_add_prefix(pack: &str, type_name: &str, prefix: &str) -> anyhow::Result<()> {
    let result = mutate::add_prefix_to_type(pack, type_name, prefix, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Added prefix \"{prefix}\" to type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_remove_prefix(pack: &str, type_name: &str, prefix: &str) -> anyhow::Result<()> {
    let result = mutate::remove_prefix_from_type(pack, type_name, prefix, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Removed prefix \"{prefix}\" from type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_add_link_type(pack: &str, name: &str, inverse: Option<&str>) -> anyhow::Result<()> {
    let opts = mutate::AddLinkTypeOpts {
        name: name.to_string(),
        inverse: inverse.map(|s| s.to_string()),
    };
    let result = mutate::add_link_type_to_pack(pack, &opts, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Added link type \"{name}\" to pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_remove_link_type(pack: &str, name: &str) -> anyhow::Result<()> {
    let result = mutate::remove_link_type_from_pack(pack, name, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Removed link type \"{name}\" from pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_set_extractable(pack: &str, type_name: &str, value: bool) -> anyhow::Result<()> {
    let result = mutate::set_extractable_on_type(pack, type_name, value, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Set extractable={value} on type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

fn run_set_expert_routing(pack: &str, type_name: &str, value: bool) -> anyhow::Result<()> {
    let result = mutate::set_expert_routing_on_type(pack, type_name, value, &mutate::MutateOpts::default())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("Set expert_routing={value} on type \"{type_name}\" in pack \"{pack}\"");
    println!("  sha8: {} → {}", result.prev_sha8, result.new_sha8);
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Load a pack by name (built-in) or file path.
fn load_pack(pack: Option<&str>) -> anyhow::Result<SchemaPackManifest> {
    match pack {
        None => {
            // Default: load zbrain-base
            load_pack_from_string(ZBRAIN_BASE_YAML, "zbrain-base.yaml")
                .map_err(|e| anyhow::anyhow!("failed to load zbrain-base: {e}"))
        }
        Some(name) if name.ends_with(".yaml") || name.ends_with(".yml") || name.ends_with(".json") => {
            // File path
            let content = std::fs::read_to_string(name)
                .map_err(|e| anyhow::anyhow!("cannot read {name}: {e}"))?;
            load_pack_from_string(&content, name)
                .map_err(|e| anyhow::anyhow!("validation failed: {e}"))
        }
        Some(name) => {
            // Try user pack directory first
            let user_dir = user_pack_dir().join(name);
            for file in ["pack.yaml", "pack.yml", "pack.json"] {
                let p = user_dir.join(file);
                if p.exists() {
                    let content = std::fs::read_to_string(&p)
                        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", p.display()))?;
                    return load_pack_from_string(&content, &p.to_string_lossy())
                        .map_err(|e| anyhow::anyhow!("failed to load {name}: {e}"));
                }
            }

            // Built-in pack name
            let yaml = match name {
                "zbrain-base" => include_str!("schema_pack_base/zbrain-base.yaml"),
                "zbrain-recommended" => include_str!("schema_pack_base/zbrain-recommended.yaml"),
                "zbrain-creator" => include_str!("schema_pack_base/zbrain-creator.yaml"),
                "zbrain-engineer" => include_str!("schema_pack_base/zbrain-engineer.yaml"),
                "zbrain-investor" => include_str!("schema_pack_base/zbrain-investor.yaml"),
                "zbrain-everything" => include_str!("schema_pack_base/zbrain-everything.yaml"),
                _ => anyhow::bail!("unknown built-in pack: {name}"),
            };
            load_pack_from_string(yaml, &format!("{name}.yaml"))
                .map_err(|e| anyhow::anyhow!("failed to load {name}: {e}"))
        }
    }
}

fn print_manifest_human(m: &SchemaPackManifest) {
    println!("Pack: {} v{}", m.name, m.version);
    if !m.description.is_empty() {
        println!("Description: {}", m.description);
    }
    println!("Extends: {}", m.extends.as_deref().unwrap_or("(none)"));
    println!("\nPage types ({}):", m.page_types.len());
    for pt in &m.page_types {
        let mut details = vec![format!("{:?}", pt.primitive)];
        if pt.extractable { details.push("extractable".into()); }
        if pt.expert_routing { details.push("expert-routing".into()); }
        if !pt.path_prefixes.is_empty() {
            details.push(format!("prefixes=[{}]", pt.path_prefixes.join(",")));
        }
        if !pt.aliases.is_empty() {
            details.push(format!("aliases=[{}]", pt.aliases.join(",")));
        }
        println!("  {}: {}", pt.name, details.join(", "));
    }
    if !m.link_types.is_empty() {
        println!("\nLink types ({}):", m.link_types.len());
        for lt in &m.link_types {
            match &lt.inverse {
                Some(inv) => println!("  {} ↔ {}", lt.name, inv),
                None => println!("  {}", lt.name),
            }
        }
    }
}

fn user_pack_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".zbrain").join("schema-packs")
}
