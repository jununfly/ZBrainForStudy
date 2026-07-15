//! `zbrain schema` subcommand — 9 read-only inspection verbs.
//!
//! Tracer bullet: first end-to-end CLI → core → DB slice for schema-pack.
//!
//! Verbs: active, list, show, validate, graph, lint, stats, explain, usage.

use std::path::PathBuf;

use clap::Subcommand;
use zbrain_core::schema_pack::{
    lint_rules,
    loader::load_pack_from_string,
    manifest::SchemaPackManifest,
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
    }
}

// ---------------------------------------------------------------------------
// Verb handlers
// ---------------------------------------------------------------------------

fn run_active() -> anyhow::Result<()> {
    let env_var = std::env::var("ZBRAIN_SCHEMA_PACK").ok().filter(|s| !s.is_empty());
    let input = registry::ResolutionInput {
        env_var,
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
