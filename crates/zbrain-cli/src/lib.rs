//! `zbrain-cli` — command-line entry point.
//!
//! Slice 1-3-1: clap CLI framework with 4 command stubs.
//! Slice 1-3-1-2: Config file discovery, YAML parsing, and env var overrides.
//! Next slices add command implementations.

pub mod config;
pub mod mcp_client;

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zbrain_core::engine::BrainEngine;
use zbrain_core::operation::{OperationContext, OperationRegistry};

/// Doctor check status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

/// A single doctor check result
struct DoctorCheck {
    name: String,
    status: CheckStatus,
    message: String,
}

impl DoctorCheck {
    fn new(name: &str, status: CheckStatus, message: &str) -> Self {
        Self {
            name: name.to_string(),
            status,
            message: message.to_string(),
        }
    }

    fn ok(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::Ok, message)
    }

    fn warn(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::Warn, message)
    }

    fn fail(name: &str, message: &str) -> Self {
        Self::new(name, CheckStatus::Fail, message)
    }
}

/// Static crate name.
#[must_use]
pub const fn crate_name() -> &'static str {
    "zbrain-cli"
}

/// Banner string used by the binary entry point.
#[must_use]
pub fn banner() -> String {
    format!(
        "{} v{} (core: {} v{})",
        crate_name(),
        env!("CARGO_PKG_VERSION"),
        zbrain_core::crate_name(),
        zbrain_core::version(),
    )
}

/// ZBrain command-line interface.
#[derive(Debug, Parser)]
#[command(name = "zbrain")]
#[command(about = "AI-powered knowledge base and semantic search engine", long_about = None)]
#[command(version = env!("CARGO_PKG_VERSION"))]
pub struct Cli {
    /// Path to config file (defaults: ./zbrain.yml then ~/.zbrain/config)
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Enable debug logging
    #[arg(short, long, global = true)]
    pub debug: bool,

    /// Subcommand to execute
    #[command(subcommand)]
    pub command: Commands,
}

/// Available CLI commands.
#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Initialize a new ZBrain project
    Init(InitArgs),

    /// Validate installation and connectivity
    Doctor(DoctorArgs),

    /// Manage configuration values
    Config(ConfigArgs),

    /// Print database schema SQL
    Schema(SchemaArgs),

    /// Read a page by slug
    GetPage(GetPageArgs),

    /// Synthesize answers across the knowledge base
    Think(ThinkArgs),
    /// Search pages by keyword query
    Query(QueryArgs),

    /// Create or update a page
    PutPage(PutPageArgs),

    /// Delete a page by slug (soft delete)
    DeletePage(DeletePageArgs),

    /// Restore a deleted page
    RestorePage(RestorePageArgs),

    /// Permanently remove all soft-deleted pages
    PurgeDeletedPages(PurgeDeletedPagesArgs),

    /// List pages with optional filtering
    ListPages(ListPagesArgs),

    /// Start the MCP stdio server (Model Context Protocol)
    ServeMcp(ServeMcpArgs),

    /// Start the HTTP API and admin SPA server
    #[command(name = "serve")]
    ServeHttp(ServeHttpArgs),

    /// Sync files from a git repository into the knowledge base
    Sync(SyncArgs),

    /// Manage knowledge base sources
    #[command(subcommand)]
    Sources(SourcesAction),

    /// Capture content from files or stdin into the knowledge base
    Capture(CaptureArgs),
}

/// Subcommands for `zbrain sources`.
#[derive(Debug, Subcommand)]
pub enum SourcesAction {
    /// Register a new source (local path or remote git URL)
    Add(SourcesAddArgs),

    /// List all registered sources
    List(SourcesListArgs),

    /// Remove a source and optionally its local clone
    Remove(SourcesRemoveArgs),

    /// Show source health dashboard
    Status(SourcesStatusArgs),
}

/// Arguments for `zbrain sources add`.
#[derive(Debug, Parser)]
pub struct SourcesAddArgs {
    /// Source ID (1-32 lowercase alphanumeric chars with optional interior hyphens)
    pub id: String,

    /// Display name (defaults to id if omitted)
    #[arg(long)]
    pub name: Option<String>,

    /// Local path to an existing repo directory
    #[arg(long, conflicts_with = "url")]
    pub path: Option<PathBuf>,

    /// Remote git URL to clone
    #[arg(long, conflicts_with = "path")]
    pub url: Option<String>,

    /// Mark as a federated source
    #[arg(long)]
    pub federated: bool,

    /// Override clone destination (default: ~/.zbrain/clones/<id>/)
    #[arg(long)]
    pub clone_dir: Option<PathBuf>,

    /// Clone depth (0 = full clone, default: 1)
    #[arg(long, default_value = "1")]
    pub depth: u32,

    /// Branch to clone (default: repo default)
    #[arg(long)]
    pub branch: Option<String>,
}

/// Arguments for `zbrain sources list`.
#[derive(Debug, Parser)]
pub struct SourcesListArgs {
    /// Output as JSON instead of table
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain sources remove`.
#[derive(Debug, Parser)]
pub struct SourcesRemoveArgs {
    /// Source ID to remove
    pub id: String,

    /// Confirm removal even if source has pages
    #[arg(long)]
    pub confirm_destructive: bool,

    /// Show what would happen without actually removing
    #[arg(long)]
    pub dry_run: bool,

    /// Keep local clone directory (don't delete it)
    #[arg(long)]
    pub keep_storage: bool,

    /// Skip interactive confirmation prompt
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Arguments for `zbrain sources status`.
#[derive(Debug, Parser)]
pub struct SourcesStatusArgs {
    /// Source ID to inspect (omit for all sources)
    pub source_id: Option<String>,

    /// Output as JSON instead of table
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain capture` command.
#[derive(Debug, Parser)]
pub struct CaptureArgs {
    /// Content source: path to file, or omit for stdin
    pub content: Option<String>,

    /// Content type (markdown, text)
    #[arg(long, default_value = "markdown")]
    pub r#type: String,

    /// Source ID to associate with
    #[arg(long)]
    pub source: Option<String>,

    /// Custom slug for the page
    #[arg(long)]
    pub slug: Option<String>,

    /// Output as JSON instead of human-readable
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `zbrain get-page` command.
#[derive(Debug, Parser)]
pub struct GetPageArgs {
    /// Page slug to retrieve
    pub slug: String,

    /// Enable fuzzy slug matching
    #[arg(long)]
    pub fuzzy: bool,

    /// Include soft-deleted pages
    #[arg(long)]
    pub include_deleted: bool,
}

/// Arguments for `zbrain think` command.
#[derive(Debug, Parser)]
pub struct ThinkArgs {
    /// Question to answer
    pub question: String,

    /// Optional anchor page for context focus
    #[arg(long)]
    pub anchor: Option<String>,

    /// Number of reasoning rounds (default: 1)
    #[arg(long)]
    pub rounds: Option<u32>,

    /// Model override
    #[arg(long)]
    pub model: Option<String>,

    /// Time range start (ISO 8601)
    #[arg(long)]
    pub since: Option<String>,

    /// Time range end (ISO 8601)
    #[arg(long)]
    pub until: Option<String>,
}

/// Arguments for `zbrain query` command.
#[derive(Debug, Parser)]
pub struct QueryArgs {
    /// Search query text
    pub query: String,

    /// Maximum number of results (default: 20)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Pagination offset (default: 0)
    #[arg(long)]
    pub offset: Option<usize>,

    /// Scope search to a specific source
    #[arg(long)]
    pub source_id: Option<String>,
}

/// Arguments for `zbrain init` command.
#[derive(Debug, Parser)]
pub struct InitArgs {
    /// Overwrite existing config if present
    #[arg(short, long)]
    pub force: bool,
}

/// Arguments for `zbrain doctor` command.
#[derive(Debug, Parser)]
pub struct DoctorArgs {
    /// Skip network connectivity checks
    #[arg(long)]
    pub offline: bool,
}

/// Arguments for `zbrain config` command and its subcommands.
#[derive(Debug, Parser)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: ConfigAction,
}

/// Subcommands for `zbrain config`.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Show all configuration values (redacted)
    Show,

    /// Get a single config value
    Get { key: String },

    /// Set a config value
    Set { key: String, value: String },

    /// Unset a config value
    Unset {
        /// Config key to unset (optional, use --pattern for bulk unset)
        #[arg(required_unless_present = "pattern")]
        key: Option<String>,
        /// Bulk unset by key prefix pattern
        #[arg(long)]
        pattern: Option<String>,
    },
}

/// Arguments for `zbrain schema` command.
#[derive(Debug, Parser)]
pub struct SchemaArgs {
    /// Which backend schema to print
    #[arg(short, long, default_value = "libsql")]
    pub backend: String,
}

/// Arguments for `zbrain put-page` command.
#[derive(Debug, Parser)]
pub struct PutPageArgs {
    /// Page slug to create or update
    pub slug: String,

    /// Page type (default: note)
    #[arg(long)]
    pub page_type: Option<String>,

    /// Page title (defaults to slug)
    #[arg(long)]
    pub title: Option<String>,

    /// Page content (markdown)
    #[arg(long)]
    pub content: Option<String>,
}

/// Arguments for `zbrain delete-page` command.
#[derive(Debug, Parser)]
pub struct DeletePageArgs {
    /// Page slug to delete
    pub slug: String,
}

/// Arguments for `zbrain restore-page` command.
#[derive(Debug, Parser)]
pub struct RestorePageArgs {
    /// Page slug to restore
    pub slug: String,
}

/// Arguments for `zbrain purge-deleted-pages` command.
#[derive(Debug, Parser)]
pub struct PurgeDeletedPagesArgs {
    /// Confirm permanent deletion
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `zbrain list-pages` command.
#[derive(Debug, Parser)]
pub struct ListPagesArgs {
    /// Filter by page type
    #[arg(long)]
    pub page_type: Option<String>,

    /// Filter by tag
    #[arg(long)]
    pub tag: Option<String>,

    /// Maximum number of results (default: 50)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Pagination offset (default: 0)
    #[arg(long)]
    pub offset: Option<usize>,

    /// Include soft-deleted pages
    #[arg(long)]
    pub include_deleted: bool,
}

/// Arguments for `zbrain serve-mcp` command.
#[derive(Debug, Parser)]
pub struct ServeMcpArgs {
    /// Source ID to scope operations to (default: $ZBRAIN_SOURCE or "default")
    #[arg(long)]
    pub source: Option<String>,
}

/// Arguments for `zbrain serve --http` command.
#[derive(Debug, Parser)]
pub struct ServeHttpArgs {
    /// Enable HTTP server mode
    #[arg(long)]
    pub http: bool,

    /// Port to listen on (default: 3000, or zbrain.yml server.port)
    #[arg(long)]
    pub port: Option<u16>,

    /// Address to bind to (default: 127.0.0.1, or zbrain.yml server.bind)
    #[arg(long)]
    pub bind: Option<String>,

    /// Path to admin SPA static files directory
    #[arg(long)]
    pub spa_dir: Option<PathBuf>,
}

/// Arguments for `zbrain sync` command.
#[derive(Debug, Parser)]
pub struct SyncArgs {
    /// Source identifier (creates if not exists)
    #[arg(long, default_value = "default")]
    pub source_id: String,

    /// Path to the git repository root to sync
    #[arg(long)]
    pub repo_path: Option<PathBuf>,

    /// Force a full sync even if an anchor exists
    #[arg(long)]
    pub full_sync: bool,

    /// Chunker version to stamp on pages (detected from config if omitted)
    #[arg(long)]
    pub chunker_version: Option<i32>,

    /// Maximum file size in bytes (0 = no limit)
    #[arg(long, default_value = "0")]
    pub max_file_size: u64,

    /// Directory for recording sync failures
    #[arg(long)]
    pub failures_dir: Option<PathBuf>,

    /// Number of parallel imports (0 = auto-detect, 1 = serial)
    #[arg(long, default_value = "0")]
    pub parallelism: usize,
}

/// Execute the parsed CLI command.
pub async fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Commands::Init(args) => run_init_command(args, cli.config.as_deref()).await?,
        Commands::Doctor(args) => run_doctor_command(args, cli.config.as_deref()).await?,
        Commands::Config(args) => run_config_command(args, cli.config.as_deref()).await?,
        Commands::Schema(args) => run_schema_command(args)?,
        Commands::GetPage(args) => run_get_page_command(args, cli.config.as_deref()).await?,
        Commands::Think(args) => run_think_command(args, cli.config.as_deref()).await?,
        Commands::Query(args) => run_query_command(args, cli.config.as_deref()).await?,
        Commands::PutPage(args) => run_put_page_command(args, cli.config.as_deref()).await?,
        Commands::DeletePage(args) => run_delete_page_command(args, cli.config.as_deref()).await?,
        Commands::RestorePage(args) => run_restore_page_command(args, cli.config.as_deref()).await?,
        Commands::PurgeDeletedPages(args) => run_purge_deleted_pages_command(args, cli.config.as_deref()).await?,
        Commands::ListPages(args) => run_list_pages_command(args, cli.config.as_deref()).await?,
        Commands::ServeMcp(args) => run_serve_mcp_command(args, cli.config.as_deref()).await?,
        Commands::ServeHttp(args) => run_serve_http_command(args, cli.config.as_deref()).await?,
        Commands::Sync(args) => run_sync_command(args, cli.config.as_deref()).await?,
        Commands::Sources(action) => run_sources_command(action, cli.config.as_deref()).await?,
        Commands::Capture(args) => run_capture_command(args, cli.config.as_deref()).await?,
    }
    Ok(())
}

/// Execute `zbrain think` command.
async fn run_think_command(args: ThinkArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "question": args.question,
        "anchor": args.anchor,
        "rounds": args.rounds,
        "model": args.model,
        "since": args.since,
        "until": args.until,
    });

    let output = run_operation("think", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain get-page` command.
async fn run_get_page_command(args: GetPageArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
        "fuzzy": args.fuzzy,
        "include_deleted": args.include_deleted,
    });

    let output = run_operation("get_page", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain query` command.
async fn run_query_command(args: QueryArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "query": args.query,
        "limit": args.limit,
        "offset": args.offset,
        "source_id": args.source_id,
    });

    let output = run_operation("query", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain put-page` command.
async fn run_put_page_command(args: PutPageArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    // Get content from --content flag or stdin
    let content = match args.content {
        Some(c) => c,
        None => {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer)?;
            buffer
        }
    };

    let params = serde_json::json!({
        "slug": args.slug,
        "page_type": args.page_type,
        "title": args.title,
        "compiled_truth": content,
    });

    let output = run_operation("put_page", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain delete-page` command.
async fn run_delete_page_command(args: DeletePageArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
    });

    let output = run_operation("delete_page", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain restore-page` command.
async fn run_restore_page_command(args: RestorePageArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "slug": args.slug,
    });

    let output = run_operation("restore_page", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain purge-deleted-pages` command.
async fn run_purge_deleted_pages_command(args: PurgeDeletedPagesArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    // --force is required as a safety measure
    if !args.force {
        eprintln!("Error: --force flag is required to permanently purge deleted pages");
        std::process::exit(1);
    }

    let params = serde_json::json!({
        "older_than_days": null,
    });

    let output = run_operation("purge_deleted_pages", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain list-pages` command.
async fn run_list_pages_command(args: ListPagesArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    let params = serde_json::json!({
        "kind": args.page_type,
        "tag": args.tag,
        "limit": args.limit.map(|l| l as u32),
        "offset": args.offset.map(|o| o as u32),
        "include_deleted": args.include_deleted,
    });

    let output = run_operation("list_pages", params, config_path).await?;

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

/// Execute `zbrain serve-mcp` command.
///
/// Starts the MCP stdio server. Reads JSON-RPC 2.0 messages from stdin,
/// writes responses to stdout. Suitable for use with Claude Desktop / Claude Code.
///
/// Mirrors `startMcpServer()` in TS `src/mcp/server.ts`.
async fn run_serve_mcp_command(args: ServeMcpArgs, _config_path: Option<&Path>) -> anyhow::Result<()> {
    // Initialize tracing subscriber for audit logs (stderr output)
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("zbrain_mcp=info".parse().unwrap()))
        .with_writer(std::io::stderr)
        .try_init();

    use zbrain_core::operation::{
        GetPageOperation, ThinkOperation, QueryOperation,
        PutPageOperation, DeletePageOperation, RestorePageOperation,
        PurgeDeletedPagesOperation, ListPagesOperation,
    };

    // Load config for MCP settings (rate limit)
    let config_file = _config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;
    let mcp_config = if config_file.exists() {
        let cfg = config::load_config_from_path(&config_file)?;
        cfg.mcp
    } else {
        Default::default()
    };

    // Set source_id env for the MCP server if provided via --source flag
    if let Some(source) = &args.source {
        std::env::set_var("ZBRAIN_SOURCE", source);
    }

    // Build registry
    let mut registry = OperationRegistry::new();
    registry.register(GetPageOperation);
    registry.register(ThinkOperation);
    registry.register(QueryOperation);
    registry.register(PutPageOperation);
    registry.register(DeletePageOperation);
    registry.register(RestorePageOperation);
    registry.register(PurgeDeletedPagesOperation);
    registry.register(ListPagesOperation);

    // Log startup to stderr (MCP protocol uses stdout for JSON-RPC)
    let source_id = std::env::var("ZBRAIN_SOURCE").unwrap_or_else(|_| "default".to_string());
    eprintln!("[zbrain-mcp] starting stdio MCP server (source: {})", source_id);

    let version = env!("CARGO_PKG_VERSION");
    let engine = std::sync::Arc::new(zbrain_core::InMemoryEngine::default());
    let server = zbrain_mcp::StdioMcpServer::new(
        registry,
        engine,
        "zbrain",
        version,
        mcp_config.rate_limit,
    );

    server.run().await.context("MCP stdio server error")?;

    eprintln!("[zbrain-mcp] shutdown: stdin closed");
    Ok(())
}

/// Build the standard operation registry with all registered operations.
fn build_operation_registry() -> Arc<OperationRegistry> {
    use zbrain_core::operation::{
        GetPageOperation, ThinkOperation, QueryOperation, PutPageOperation,
        DeletePageOperation, RestorePageOperation, PurgeDeletedPagesOperation,
        ListPagesOperation,
    };
    let mut registry = OperationRegistry::new();
    registry.register(GetPageOperation);
    registry.register(ThinkOperation);
    registry.register(QueryOperation);
    registry.register(PutPageOperation);
    registry.register(DeletePageOperation);
    registry.register(RestorePageOperation);
    registry.register(PurgeDeletedPagesOperation);
    registry.register(ListPagesOperation);
    Arc::new(registry)
}

/// Execute `zbrain sync` command.
///
/// Syncs markdown files from a git repository into the knowledge base.
/// Performs an incremental sync by default (git diff since last anchor),
/// or a full sync if `--full-sync` is passed or no anchor exists.
async fn run_sync_command(args: SyncArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sync::core::{perform_full_sync, perform_sync, FullSyncOpts, IncrementalSyncOpts};

    let config = config::load_config(config_path)?;

    // Resolve repo_path: from flag, or from config.sync.default_repo, or CWD
    let repo_path = args
        .repo_path
        .clone()
        .or_else(|| config.sync.as_ref().and_then(|s| s.default_repo.clone()))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Ensure repo_path is absolute
    let repo_path = if repo_path.is_absolute() {
        repo_path
    } else {
        std::env::current_dir()?.join(repo_path)
    };

    // Get current git commit
    let current_commit = get_git_head_commit(&repo_path)?;

    // Resolve chunker_version: from flag, or from config.sync.chunker_version, or default 1
    let chunker_version = args.chunker_version.or_else(|| {
        config.sync.as_ref().and_then(|s| s.chunker_version)
    });

    // Resolve failures_dir
    let failures_dir = args.failures_dir.clone().unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".zbrain")
            .join("sync-failures")
    });
    std::fs::create_dir_all(&failures_dir)?;

    // Max file size: 0 means no limit
    let max_file_size = if args.max_file_size == 0 {
        None
    } else {
        Some(args.max_file_size)
    };

    // Build engine
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine: std::sync::Arc<dyn BrainEngine> = std::sync::Arc::new(engine);

    // Ensure source exists
    ensure_source_exists(&engine, &args.source_id).await?;

    let result = if args.full_sync {
        eprintln!("[zbrain-sync] performing full sync for source: {}", args.source_id);
        let opts = FullSyncOpts {
            source_id: args.source_id.clone(),
            repo_path: repo_path.clone(),
            current_commit: current_commit.clone(),
            chunker_version,
            failures_dir: failures_dir.clone(),
            max_file_size,
        };
        perform_full_sync(&engine, &opts).await?
    } else {
        // Get previous anchor for incremental sync
        let source = engine
            .get_source(&args.source_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("source not found: {}", args.source_id))?;

        let previous_commit = source.last_commit.clone();

        eprintln!("[zbrain-sync] incremental sync for source: {} ({}..{})",
            args.source_id,
            previous_commit.as_deref().unwrap_or("none"),
            current_commit,
        );

        let opts = IncrementalSyncOpts {
            source_id: args.source_id.clone(),
            repo_path: repo_path.clone(),
            current_commit: current_commit.clone(),
            previous_commit,
            chunker_version,
            failures_dir: failures_dir.clone(),
            max_file_size,
        };
        perform_sync(&engine, &opts).await?
    };

    // Print result
    let mode = if result.full_sync { "full sync" } else { "incremental sync" };
    println!("{} complete: {} imported, {} deleted, {} failures",
        mode, result.imported, result.deleted, result.failures);

    engine.disconnect().await?;
    Ok(())
}

/// Ensure a source exists in the engine, creating it if necessary.
async fn ensure_source_exists(engine: &std::sync::Arc<dyn zbrain_core::engine::BrainEngine>, source_id: &str) -> anyhow::Result<()> {
    use zbrain_core::engine::CreateSourceInput;

    if engine.get_source(source_id).await?.is_none() {
        engine
            .create_source(&CreateSourceInput {
                id: source_id.to_string(),
                name: source_id.to_string(),
                config: None,
            })
            .await?;
        eprintln!("[zbrain-sync] created source: {}", source_id);
    }
    Ok(())
}

/// Get the current HEAD commit SHA from a git repository.
fn get_git_head_commit(repo_path: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "rev-parse", "HEAD"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("failed to get git HEAD commit: {stderr}"));
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(sha)
}

/// Start the HTTP API and admin SPA server.
///
/// Loads server configuration from zbrain.yml (with CLI flag overrides),
/// builds the axum router, and starts listening on the configured address.
async fn run_serve_http_command(
    args: ServeHttpArgs,
    config_path: Option<&Path>,
) -> anyhow::Result<()> {
    let config = config::load_config(config_path)?;

    let port = args.port.unwrap_or(config.server.port);
    let bind_addr = args.bind.unwrap_or(config.server.bind);

    let addr: std::net::SocketAddr = format!("{bind_addr}:{port}")
        .parse()
        .context("Invalid bind address")?;

    // Determine admin SPA directory
    let spa_dir = if let Some(ref dir) = args.spa_dir {
        dir.clone()
    } else {
        // Default: look for admin/dist/ relative to CWD
        let cwd_spa = std::env::current_dir()?.join("admin").join("dist");
        if cwd_spa.exists() {
            cwd_spa
        } else {
            // Fallback: use a temp dir (SPA won't be served, but server starts)
            std::env::temp_dir().join("zbrain-admin-empty")
        }
    };

    // Initialize admin auth with optional env token
    let admin_token = std::env::var("ZBRAIN_ADMIN_BOOTSTRAP_TOKEN").ok();
    let admin_auth = zbrain_web::AdminAuth::new(admin_token);

    // Initialize engine for admin queries
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = zbrain_core::engine::EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;
    let engine = std::sync::Arc::new(engine);
    let (tx, _rx) = tokio::sync::broadcast::channel(64);

    let state = zbrain_web::AppState {
        admin_auth,
        magic_link: zbrain_web::MagicLinkAuth::new(),
        admin_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::AdminQueries>,
        calibration_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::CalibrationQueries>,
        oauth_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::OAuthQueries>,
        token_queries: engine.clone() as std::sync::Arc<dyn zbrain_core::TokenQueries>,
        activity_tx: tx,
        spa_dir,
        operation_registry: build_operation_registry(),
        engine: engine as std::sync::Arc<dyn zbrain_core::BrainEngine>,
        zbrain_home: dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".zbrain"),
    };

    eprintln!("[zbrain-web] starting HTTP server on {addr}");
    zbrain_web::run(addr, state).await
}

/// Execute an operation by name with JSON params.
///
/// Supports two execution modes:
/// 1. Local: executes directly against the local database engine (default)
/// 2. Thin-client: routes the call through a remote MCP server (when remote_mcp is configured)
///
/// Local-only operations are refused on thin-client installs with a helpful message,
/// matching the TypeScript behavior in `refuseThinClient`.
async fn run_operation(
    name: &str,
    params: serde_json::Value,
    config_path: Option<&Path>,
) -> anyhow::Result<serde_json::Value> {
    // Load config and create engine
    let config_file = config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .ok_or_else(|| anyhow::anyhow!("Could not determine config path"))?;

    let config = config::load_config_from_path(&config_file)?;

    // Build operation registry early so thin-client check can query local_only status
    // from the canonical TypedOperation trait (not a hardcoded list).
    let mut registry = OperationRegistry::new();
    registry.register(zbrain_core::operation::GetPageOperation);
    registry.register(zbrain_core::operation::ThinkOperation);
    registry.register(zbrain_core::operation::QueryOperation);
    registry.register(zbrain_core::operation::PutPageOperation);
    registry.register(zbrain_core::operation::DeletePageOperation);
    registry.register(zbrain_core::operation::RestorePageOperation);
    registry.register(zbrain_core::operation::PurgeDeletedPagesOperation);
    registry.register(zbrain_core::operation::ListPagesOperation);

    // Check for thin-client mode (v0.31.1 Issue #734)
    if config::is_thin_client(&config) {
        let remote_mcp = config.remote_mcp.as_ref().expect("is_thin_client guarantees this");

        // Query registry for local_only status (avoids hardcoded match drift from trait)
        let is_local_only = registry
            .lookup(name)
            .map(|op| op.local_only())
            .unwrap_or(false);

        if is_local_only {
            eprintln!(
                "zbrain {name}: this operation requires a local engine. This install is a thin client of {}.",
                remote_mcp.mcp_url
            );
            eprintln!();
            eprintln!("Thin-client routing for {name} is planned for a future release.");
            eprintln!("Run on the host instead, or re-init with `zbrain init` to use local mode.");
            std::process::exit(1);
        }

        // Non-local-only operations: route through remote MCP
        let mcp_client = mcp_client::McpClient::new(config);
        let result = mcp_client.call_tool(name, params).await.map_err(|e| {
            eprintln!("Remote MCP call failed: {}", e);
            std::process::exit(1);
        }).unwrap();
        return Ok(result);
    }

    // Local mode: execute against local engine
    let engine_config = zbrain_core::engine::EngineConfig {
        database_path: None,
        database_url: Some(config.database_url),
    };

    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    let ctx = OperationContext::local_cli().with_engine(std::sync::Arc::new(engine));

    // Use shared MCP dispatch path (dispatch_tool_call) so CLI and future MCP server
    // produce identical result formatting and error handling.
    // Mirrors TS `dispatchToolCall()` in src/mcp/dispatch.ts.
    let tool_result = registry.dispatch_tool_call(name, &ctx, params).await;

    if tool_result.is_error {
        // Parse error JSON to get exit code from OperationError shape
        let exit_code = tool_result
            .parse_json()
            .and_then(|j| {
                let code = j["error"].as_str()?;
                // permission_denied → exit 126 (matches TS + OperationError::exit_code)
                Some(if code == "permission_denied" { 126i32 } else { 1i32 })
            })
            .unwrap_or(1);
        // Print error text to stderr
        if let Some(text) = tool_result.text() {
            eprintln!("{}", text);
        }
        std::process::exit(exit_code);
    }

    // Success: return the parsed JSON value
    let value = tool_result
        .parse_json()
        .ok_or_else(|| anyhow::anyhow!("Operation returned non-JSON output"))?;

    Ok(value)
}

/// Execute `zbrain sources` subcommands.
async fn run_sources_command(action: SourcesAction, config_path: Option<&Path>) -> anyhow::Result<()> {
    match action {
        SourcesAction::Add(args) => run_sources_add(args, config_path).await?,
        SourcesAction::List(args) => run_sources_list(args, config_path).await?,
        SourcesAction::Remove(args) => run_sources_remove(args, config_path).await?,
        SourcesAction::Status(args) => run_sources_status(args, config_path).await?,
    }
    Ok(())
}

/// Execute `zbrain sources add` command.
async fn run_sources_add(args: SourcesAddArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sources_ops::{self, AddSourceOpts};

    let config = config::load_config(config_path)?;

    // Resolve zbrain_home (default: ~/.zbrain)
    let zbrain_home = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zbrain");

    // Build engine
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = AddSourceOpts {
        id: args.id.clone(),
        name: args.name.clone(),
        local_path: args.path.as_ref().map(|p| p.to_string_lossy().to_string()),
        remote_url: args.url.clone(),
        federated: if args.federated { Some(true) } else { None },
        clone_dir: args.clone_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        depth: args.depth,
        branch: args.branch.clone(),
    };

    let source = sources_ops::add_source(&engine, opts, &zbrain_home)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    println!("Source added: {}", source.id);
    println!("  name: {}", source.name);
    if let Some(ref path) = source.local_path {
        println!("  path: {path}");
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain sources list` command.
async fn run_sources_list(args: SourcesListArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};

    let config = config::load_config(config_path)?;

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let sources = engine.list_sources(false).await?;

    if args.json {
        let json = serde_json::to_string_pretty(&sources)?;
        println!("{json}");
    } else {
        // Table header
        println!("{:<20} {:<20} {:<12} {:<40}  LAST SYNC", "ID", "NAME", "ARCHIVED", "PATH",);
        for src in &sources {
            let path = src.local_path.as_deref().unwrap_or("-");
            let last_sync = src.last_sync_at.as_deref().unwrap_or("-");
            println!(
                "{:<20} {:<20} {:<12} {:<40}  {}",
                src.id, src.name, if src.archived { "yes" } else { "no" }, path, last_sync,
            );
        }
        println!("\n{} source(s)", sources.len());
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain sources remove` command.
async fn run_sources_remove(args: SourcesRemoveArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sources_ops::{self, RemoveSourceOpts};

    let config = config::load_config(config_path)?;

    let zbrain_home = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zbrain");

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let opts = RemoveSourceOpts {
        id: args.id.clone(),
        confirm_destructive: args.confirm_destructive,
        dry_run: args.dry_run,
        keep_storage: args.keep_storage,
    };

    let result = sources_ops::remove_source(&engine, opts, &zbrain_home)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if result.dry_run {
        println!("[DRY RUN] Would remove source: {}", result.id);
        println!("  pages to delete: {}", result.pages_deleted);
        if let Some(ref path) = result.clone_path {
            println!("  clone would be {}deleted: {path}",
                if !args.keep_storage { "" } else { "kept — " });
        }
    } else {
        println!("Source removed: {}", result.id);
        println!("  pages deleted: {}", result.pages_deleted);
        if result.clone_removed {
            if let Some(ref path) = result.clone_path {
                println!("  clone removed: {path}");
            }
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain sources status` command.
async fn run_sources_status(args: SourcesStatusArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use zbrain_core::engine::{BrainEngine, EngineConfig};
    use zbrain_core::sources_ops;

    let config = config::load_config(config_path)?;

    let _zbrain_home = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zbrain");

    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    // Collect source IDs
    let source_ids: Vec<String> = if let Some(ref sid) = args.source_id {
        vec![sid.clone()]
    } else {
        engine.list_sources(false).await?.into_iter().map(|s| s.id).collect()
    };

    // Gather status for each source
    let mut statuses: Vec<sources_ops::SourceStatus> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for sid in &source_ids {
        match sources_ops::get_source_status(&engine, sid).await {
            Ok(s) => statuses.push(s),
            Err(e) => errors.push(format!("{sid}: {e}")),
        }
    }

    if args.json {
        let output = if statuses.is_empty() && !errors.is_empty() {
            serde_json::json!({ "errors": errors })
        } else {
            serde_json::json!({ "sources": statuses, "errors": errors })
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        // Table header
        println!(
            "{:<20} {:<10} {:<8} {:<8} {:<8} {:<8} {:<20}",
            "SOURCE", "LAG", "EMBED", "FAILS", "QUEUE", "PAGES", "LAST SYNC"
        );

        for s in &statuses {
            let lag = compute_lag(&s);
            let embed = "-";
            let fails = "-";
            let queue = "-";
            let last_sync = s.last_sync_at.as_deref().unwrap_or("-");
            println!(
                "{:<20} {:<10} {:<8} {:<8} {:<8} {:<8} {:<20}",
                s.name, lag, embed, fails, queue, s.page_count, last_sync
            );
        }

        // Print errors after the table
        for e in &errors {
            eprintln!("error: {e}");
        }
    }

    engine.disconnect().await?;
    Ok(())
}

/// Compute git lag for a source: number of commits behind HEAD.
fn compute_lag(status: &zbrain_core::sources_ops::SourceStatus) -> String {
    let Some(ref local_path) = status.local_path else {
        return "-".to_string();
    };
    let Some(ref last_commit) = status.last_commit else {
        return "?".to_string();
    };

    let output = std::process::Command::new("git")
        .args(["-C", local_path.as_str(), "rev-list", "--count", &format!("{last_commit}..HEAD")])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let count = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if count.is_empty() { "0".to_string() } else { format!("{count}c") }
        }
        _ => "?".to_string(),
    }
}

/// Execute `zbrain capture` command.
async fn run_capture_command(args: CaptureArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    use std::io::Read;
    use zbrain_core::capture::{CaptureOpts, capture_content};
    use zbrain_core::engine::{BrainEngine, EngineConfig, PageInput};
    use zbrain_core::markdown::parse_markdown;
    use zbrain_core::time::current_utc_iso8601;
    use zbrain_core::types::PageKind;

    // 1. Read content from file or stdin
    let raw = match args.content {
        Some(ref path_str) => {
            let path = Path::new(path_str);
            std::fs::read(path)
                .with_context(|| format!("Failed to read file: {path_str}"))?
        }
        None => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
    };

    // 2. Run capture pipeline
    let captured_at = current_utc_iso8601();
    let opts = CaptureOpts {
        page_type: Some(args.r#type.clone()),
        source: args.source.clone(),
        captured_at: Some(captured_at.clone()),
    };

    let capture_result = capture_content(&raw, &opts)
        .map_err(|e| anyhow::anyhow!("Capture failed: {e}"))?;

    // 3. Parse markdown
    let source_path = args.content.as_deref().unwrap_or("stdin");
    let parsed = parse_markdown(
        &capture_result.body,
        source_path,
        None,
    );

    // 4. Determine slug: explicit > frontmatter title > UUID fallback
    let slug = args.slug.clone().unwrap_or_else(|| {
        capture_result.frontmatter
            .get("title")
            .and_then(|v| v.as_str())
            .map(|t| slugify(t))
            .unwrap_or_else(|| format!("capture-{}", &capture_result.content_hash[..12]))
    });

    // 5. Determine title
    let title = capture_result.frontmatter
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(parsed.title.clone());

    // 6. Build PageInput
    let page_type = if zbrain_core::types::is_base_page_type(&parsed.type_) {
        parsed.type_.clone()
    } else {
        args.r#type.clone()
    };

    let page_input = PageInput {
        page_type,
        title: title.clone(),
        compiled_truth: parsed.compiled_truth,
        timeline: if parsed.timeline.is_empty() { None } else { Some(parsed.timeline) },
        frontmatter: Some(capture_result.frontmatter),
        content_hash: Some(capture_result.content_hash.clone()),
        page_kind: Some(PageKind::Markdown),
        effective_date: None,
        effective_date_source: None,
        import_filename: args.content.clone(),
        chunker_version: None,
        source_path: Some(source_path.to_string()),
        source_kind: Some("capture".to_string()),
        source_uri: None,
        ingested_via: Some("zbrain capture CLI".to_string()),
        ingested_at: Some(captured_at.clone()),
        last_retrieved_at: None,
        embedding: None,
    };

    // 7. Connect to engine and put_page
    let config = config::load_config(config_path)?;
    let db_path = resolve_database_path(&config.database_url);
    let engine_config = EngineConfig {
        database_url: None,
        database_path: Some(db_path),
    };
    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;
    engine.init_schema().await?;

    let source_id_ref = args.source.as_deref();
    let page = engine.put_page(&slug, source_id_ref, &page_input).await?;

    engine.disconnect().await?;

    // 8. Output
    if args.json {
        let output = serde_json::json!({
            "slug": page.slug,
            "title": page.title,
            "content_hash": page.content_hash,
            "page_type": page.page_type,
            "source": args.source,
            "captured_at": captured_at,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("Captured page: {}", page.slug);
        println!("  title: {}", page.title);
        if let Some(ref hash) = page.content_hash {
            println!("  hash: {hash}");
        }
        if let Some(ref source_id) = args.source {
            println!("  source: {source_id}");
        }
    }

    Ok(())
}

/// Convert a string to a URL-safe slug.
fn slugify(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Execute `zbrain init` command.
///
/// Initializes a new ZBrain instance with the specified configuration.
/// Supports two modes:
/// - PGLite (embedded, zero-config, default)
/// - Postgres (Supabase or custom connection string)
///
/// Key behaviors:
/// - Creates `~/.zbrain/` directory if needed
/// - Generates default config if none exists
/// - Applies schema migrations
/// - Handles `--force` to overwrite existing config
async fn run_init_command(args: InitArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    println!("Setting up ZBrain...");

    // 1. Determine config location and ensure directory exists
    let config_file = config_path
        .map(PathBuf::from)
        .or_else(|| config::user_config_path())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".zbrain")
                .join("config")
        });

    if let Some(parent) = config_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    // 2. Check for existing config and --force flag
    if config_file.exists() && !args.force {
        println!("Config already exists at: {}", config_file.display());
        println!("Use --force to overwrite, or `zbrain init --migrate-only` to apply schema changes");
        return Ok(());
    }

    // 3. Load or create default config
    let mut config = if config_file.exists() {
        config::load_config_from_path(&config_file)?
    } else {
        config::Config::default()
    };

    // 4. Default to PGLite for now (Postgres/Supabase wizard coming later)
    let default_db_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zbrain")
        .join("brain.pglite");

    // 5. Initialize database and apply schema migrations
    println!("Initializing database...");

    // Create engine (Libsql for now, matches PGLite behavior)
    let engine_config = zbrain_core::engine::EngineConfig {
        database_path: Some(default_db_path.to_string_lossy().to_string()),
        database_url: None,
    };

    let engine = zbrain_core::libsql::LibsqlEngine::new();
    engine.connect(&engine_config).await?;

    // Apply schema migrations
    println!("Applying schema migrations...");
    engine.init_schema().await?;

    // 6. Save config to disk
    config.database_url = format!("sqlite://{}", default_db_path.display());
    config::write_config(&config, &config_file)?;

    // 7. Print success message
    println!("\n✅ ZBrain initialized successfully!");
    println!("   Config: {}", config_file.display());
    println!("   Database: {}", default_db_path.display());
    println!("\nNext steps:");
    println!("  zbrain config show           View current configuration");
    println!("  zbrain import <dir>          Import markdown files");
    println!("  zbrain doctor                 Verify installation");

    engine.disconnect().await?;
    Ok(())
}

/// Execute `zbrain doctor` command.
///
/// Validates the ZBrain installation and connectivity:
/// - Config file validation (exists, valid YAML)
/// - Database connectivity check
/// - Migration status verification
/// - Network connectivity check (for providers)
///
/// Returns exit code 0 if all checks pass, non-zero otherwise.
async fn run_doctor_command(_args: DoctorArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    println!("Running ZBrain doctor...");
    println!();

    let mut checks: Vec<DoctorCheck> = Vec::new();

    // 1. Config file validation
    match config::load_config(config_path) {
        Ok(config) => {
            checks.push(DoctorCheck::ok("config", &format!("Loaded config with database: {}", config.database_url)));
        }
        Err(e) => {
            checks.push(DoctorCheck::fail("config", &format!("Failed to load config: {}", e)));
        }
    }

    // 2. Database connectivity check
    let db_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".zbrain")
        .join("brain.pglite");

    if db_path.exists() {
        let engine_config = zbrain_core::engine::EngineConfig {
            database_path: Some(db_path.to_string_lossy().to_string()),
            database_url: None,
        };

        let engine = zbrain_core::libsql::LibsqlEngine::new();
        match engine.connect(&engine_config).await {
            Ok(_) => {
                checks.push(DoctorCheck::ok("database", "Database connection successful"));

                // 3. Migration status verification
                match engine.list_pages(&Default::default()).await {
                    Ok(pages) => {
                        checks.push(DoctorCheck::ok("schema", &format!("Schema verified: {} pages found", pages.len())));
                    }
                    Err(e) => {
                        checks.push(DoctorCheck::warn("schema", &format!("Schema check failed: {}", e)));
                    }
                }

                engine.disconnect().await?;
            }
            Err(e) => {
                checks.push(DoctorCheck::fail("database", &format!("Connection failed: {}", e)));
                checks.push(DoctorCheck::warn("schema", "Skipped (no database connection)"));
            }
        }
    } else {
        checks.push(DoctorCheck::warn("database", "Database file not found - run `zbrain init` first"));
        checks.push(DoctorCheck::warn("schema", "Skipped (no database file)"));
    }

    // 4. Network connectivity check (simple DNS lookup via std::net)
    match std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([8, 8, 8, 8], 53)),
        std::time::Duration::from_secs(3),
    ) {
        Ok(_) => {
            checks.push(DoctorCheck::ok("network", "Network connectivity verified"));
        }
        Err(_) => {
            checks.push(DoctorCheck::warn("network", "Network check failed - offline or DNS issue"));
        }
    }

    // Print results
    let mut pass_count = 0;
    let mut warn_count = 0;
    let mut fail_count = 0;

    for check in &checks {
        let (status_icon, _status_label) = match check.status {
            CheckStatus::Ok => ("✅", "PASS"),
            CheckStatus::Warn => ("⚠️", "WARN"),
            CheckStatus::Fail => ("❌", "FAIL"),
        };

        println!("{} {}: {}", status_icon, check.name, check.message);

        match check.status {
            CheckStatus::Ok => pass_count += 1,
            CheckStatus::Warn => warn_count += 1,
            CheckStatus::Fail => fail_count += 1,
        }
    }

    println!();
    println!("--- Summary ---");
    println!("Pass: {}, Warn: {}, Fail: {}", pass_count, warn_count, fail_count);

    if fail_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}

/// Execute `zbrain schema` command.
///
/// Prints the database schema SQL for the specified backend.
/// Supports: libsql (default), postgres
fn run_schema_command(args: SchemaArgs) -> anyhow::Result<()> {
    let backend = args.backend.to_lowercase();

    match backend.as_str() {
        "libsql" | "sqlite" | "pglite" => {
            println!("-- ZBrain libsql/SQLite Schema");
            println!();
            for migration in zbrain_core::libsql::LIBQL_MIGRATIONS.iter() {
                println!("-- Migration {}: {}", migration.version(), migration.name());
                println!("{}", migration.sql());
                println!();
            }
        }
        "postgres" | "pg" => {
            println!("-- ZBrain Postgres Schema");
            println!();
            for migration in zbrain_core::postgres::POSTGRES_MIGRATIONS.iter() {
                println!("-- Migration {}: {}", migration.version(), migration.name());
                println!("{}", migration.sql());
                println!();
            }
        }
        _ => {
            eprintln!("Unknown backend: {}", args.backend);
            eprintln!("Supported backends: libsql, postgres");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Execute `zbrain config` subcommands.
async fn run_config_command(args: ConfigArgs, config_path: Option<&Path>) -> anyhow::Result<()> {
    match args.action {
        ConfigAction::Show => {
            let config = config::load_config(config_path)?;
            println!("ZBrain config:");
            print_config_value("", &serde_yaml::to_value(&config)?, 2);
        }
        ConfigAction::Get { key } => {
            let config = config::load_config(config_path)?;
            let value = get_config_value(&key, &serde_yaml::to_value(&config)?);
            match value {
                Some(v) => println!("{}", config::redact_value(&key, &v)),
                None => eprintln!("Config key not found: {}", key),
            }
        }
        ConfigAction::Set { key, value } => {
            let mut config = config::load_config(config_path)?;
            set_config_value(&mut config, &key, value)?;
            // Default to user config directory if no explicit path
            let output_path = config_path
                .map(PathBuf::from)
                .or_else(config::user_config_path)
                .unwrap_or_else(|| PathBuf::from("zbrain.yml"));
            config::write_config(&config, &output_path)?;
            println!("Set config key: {}", key);
        }
        ConfigAction::Unset { key: _, pattern: Some(ref pattern) } => {
            // Bulk unset by prefix pattern
            let mut config = config::load_config(config_path)?;
            let count = unset_config_by_pattern(&mut config, pattern)?;
            let output_path = config_path
                .map(PathBuf::from)
                .or_else(config::user_config_path)
                .unwrap_or_else(|| PathBuf::from("zbrain.yml"));
            config::write_config(&config, &output_path)?;
            println!("Unset {} key(s) matching pattern: {}", count, pattern);
        }
        ConfigAction::Unset { ref key, pattern: None } => {
            // Single key unset
            let mut config = config::load_config(config_path)?;
            if let Some(ref k) = key {
                if unset_config_value(&mut config, k)? {
                    let output_path = config_path
                        .map(PathBuf::from)
                        .or_else(config::user_config_path)
                        .unwrap_or_else(|| PathBuf::from("zbrain.yml"));
                    config::write_config(&config, &output_path)?;
                    println!("Unset config key: {}", k);
                } else {
                    eprintln!("Config key not found: {}", k);
                }
            }
        }
    }
    Ok(())
}

/// Helper to print config values with proper indentation and redaction.
fn print_config_value(key: &str, value: &serde_yaml::Value, indent: usize) {
    use serde_yaml::Value;

    match value {
        Value::Mapping(map) => {
            for (k, v) in map {
                let k_str = k.as_str().unwrap_or_default();
                let new_key = if key.is_empty() {
                    k_str.to_string()
                } else {
                    format!("{}.{}", key, k_str)
                };

                if let Value::Mapping(_) = v {
                    println!("{:indent$}{}:", "", k_str, indent = indent);
                    print_config_value(&new_key, v, indent + 2);
                } else {
                    let display = match v {
                        Value::String(s) => config::redact_value(&new_key, s),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        Value::Null => "null".to_string(),
                        Value::Sequence(_) => "[array]".to_string(),
                        _ => format!("{:?}", v),
                    };
                    println!("{:indent$}{}: {}", "", k_str, display, indent = indent);
                }
            }
        }
        _ => {} // Only mappings at top level, should not happen with Config struct
    }
}

/// Get a nested config value by dot-separated key path.
fn get_config_value(key: &str, config: &serde_yaml::Value) -> Option<String> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = config;

    for part in parts {
        match current {
            serde_yaml::Value::Mapping(map) => {
                current = map.get(&serde_yaml::Value::String(part.to_string()))?;
            }
            _ => return None,
        }
    }

    match current {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        _ => Some(format!("{:?}", current)),
    }
}

/// Set a nested config value by dot-separated key path.
fn set_config_value(config: &mut config::Config, key: &str, value: String) -> anyhow::Result<()> {
    // Convert to value representation, then apply change
    // Use &* to reborrow and avoid "value used after move"
    let mut cfg_value = serde_yaml::to_value(&*config)?;
    {
        let parts: Vec<&str> = key.split('.').collect();
        let mut current = &mut cfg_value;

        for (i, part) in parts.iter().enumerate() {
            if i == parts.len() - 1 {
                // Leaf node - set the value
                if let serde_yaml::Value::Mapping(map) = current {
                    map.insert(
                        serde_yaml::Value::String(part.to_string()),
                        serde_yaml::Value::String(value.clone()),
                    );
                }
            } else {
                // Traverse or create nested mapping
                if let serde_yaml::Value::Mapping(map) = current {
                    let key_val = serde_yaml::Value::String(part.to_string());
                    if !map.contains_key(&key_val) {
                        map.insert(key_val.clone(), serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
                    }
                    current = map.get_mut(&key_val).unwrap();
                }
            }
        }
    }
    // Convert back to Config struct
    *config = serde_yaml::from_value(cfg_value)?;
    Ok(())
}

/// Unset a specific config key. Returns true if the key existed.
fn unset_config_value(config: &mut config::Config, key: &str) -> anyhow::Result<bool> {
    let mut cfg_value = serde_yaml::to_value(&*config)?;
    let parts: Vec<&str> = key.split('.').collect();

    let result = if parts.len() == 1 {
        // Top level key
        if let serde_yaml::Value::Mapping(map) = &mut cfg_value {
            map.remove(&serde_yaml::Value::String(key.to_string())).is_some()
        } else {
            false
        }
    } else {
        // Nested key
        let mut current = &mut cfg_value;
        for (i, part) in parts[..parts.len() - 1].iter().enumerate() {
            if let serde_yaml::Value::Mapping(map) = current {
                let key_val = serde_yaml::Value::String(part.to_string());
                if i == parts.len() - 2 {
                    // Parent of leaf - remove the leaf
                    return Ok(map.remove(&serde_yaml::Value::String(parts[parts.len() - 1].to_string())).is_some());
                }
                current = map.get_mut(&key_val).ok_or_else(|| anyhow::anyhow!("Key path not found"))?;
            } else {
                return Ok(false);
            }
        }
        false
    };

    if result {
        *config = serde_yaml::from_value(cfg_value)?;
    }
    Ok(result)
}

/// Unset all keys matching a prefix pattern. Returns count of removed keys.
fn unset_config_by_pattern(config: &mut config::Config, prefix: &str) -> anyhow::Result<usize> {
    let mut cfg_value = serde_yaml::to_value(&*config)?;
    let mut count = 0;

    if let serde_yaml::Value::Mapping(map) = &mut cfg_value {
        let keys_to_remove: Vec<_> = map
            .keys()
            .filter_map(|k| k.as_str())
            .filter(|k| k.starts_with(prefix))
            .map(|k| k.to_string())
            .collect();

        for key in keys_to_remove {
            if map.remove(&serde_yaml::Value::String(key)).is_some() {
                count += 1;
            }
        }
    }

    if count > 0 {
        *config = serde_yaml::from_value(cfg_value)?;
    }
    Ok(count)
}

/// Resolve a `sqlite://path` database URL to a filesystem path,
/// expanding `~` to the home directory.
fn resolve_database_path(database_url: &str) -> String {
    let path = database_url
        .strip_prefix("sqlite://")
        .unwrap_or(database_url);
    if path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            return format!("{}{}", home.display(), &path[1..]);
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn crate_name_is_zbrain_cli() {
        assert_eq!(crate_name(), "zbrain-cli");
    }

    #[test]
    fn banner_mentions_both_crates() {
        let b = banner();
        assert!(b.contains("zbrain-cli"), "banner missing cli name: {b}");
        assert!(b.contains("zbrain-core"), "banner missing core name: {b}");
    }

    #[test]
    fn cli_parses_successfully() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_flag_works() {
        let result = Cli::try_parse_from(["zbrain", "--help"]);
        assert!(result.is_err()); // help returns a special exit error
    }

    #[test]
    fn version_flag_works() {
        let result = Cli::try_parse_from(["zbrain", "--version"]);
        assert!(result.is_err()); // version returns a special exit error
    }

    #[test]
    fn init_command_parses() {
        let result = Cli::try_parse_from(["zbrain", "init"]);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap().command, Commands::Init(_)));
    }

    #[test]
    fn init_force_flag_parses() {
        let result = Cli::try_parse_from(["zbrain", "init", "--force"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Init(args) if args.force));
    }

    #[test]
    fn doctor_command_parses() {
        let result = Cli::try_parse_from(["zbrain", "doctor"]);
        assert!(result.is_ok());
        assert!(matches!(result.unwrap().command, Commands::Doctor(_)));
    }

    #[test]
    fn config_show_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "show"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Config(args) if matches!(args.action, ConfigAction::Show)));
    }

    #[test]
    fn config_get_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "get", "database.url"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Config(args) if matches!(&args.action, ConfigAction::Get { key } if key == "database.url")));
    }

    #[test]
    fn config_set_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "set", "database.url", "sqlite://db"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config(args)
            if matches!(&args.action, ConfigAction::Set { key, value }
                        if key == "database.url" && value == "sqlite://db")
        ));
    }

    #[test]
    fn config_unset_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "unset", "old.key"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(
            cli.command,
            Commands::Config(args)
            if matches!(&args.action, ConfigAction::Unset { key: Some(ref k), pattern: None }
                        if k == "old.key")
        ));
    }

    #[test]
    fn config_unset_pattern_parses() {
        let result = Cli::try_parse_from(["zbrain", "config", "unset", "--pattern", "legacy_"]);
        assert!(result.is_ok());
    }

    #[test]
    fn schema_command_parses_default() {
        let result = Cli::try_parse_from(["zbrain", "schema"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Schema(args) if args.backend == "libsql"));
    }

    #[test]
    fn schema_command_postgres_parses() {
        let result = Cli::try_parse_from(["zbrain", "schema", "--backend", "postgres"]);
        assert!(result.is_ok());
        let cli = result.unwrap();
        assert!(matches!(cli.command, Commands::Schema(args) if args.backend == "postgres"));
    }

    #[tokio::test]
    async fn run_executes_init_stub() {
        let cli = Cli::try_parse_from(["zbrain", "init"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_executes_doctor_stub() {
        let cli = Cli::try_parse_from(["zbrain", "doctor"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_executes_config_stub() {
        let cli = Cli::try_parse_from(["zbrain", "config", "show"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_executes_schema_stub() {
        let cli = Cli::try_parse_from(["zbrain", "schema"]).unwrap();
        let result = run(cli).await;
        assert!(result.is_ok());
    }

    #[test]
    fn registry_dynamic_local_only_consistent_with_trait() {
        use zbrain_core::operation::{
            GetPageOperation, ThinkOperation, QueryOperation,
            PutPageOperation, DeletePageOperation, RestorePageOperation,
            PurgeDeletedPagesOperation, ListPagesOperation,
        };

        let mut registry = OperationRegistry::new();
        registry.register(GetPageOperation);
        registry.register(ThinkOperation);
        registry.register(QueryOperation);
        registry.register(PutPageOperation);
        registry.register(DeletePageOperation);
        registry.register(RestorePageOperation);
        registry.register(PurgeDeletedPagesOperation);
        registry.register(ListPagesOperation);

        // local_only ops: must return true from both trait AND registry lookup
        for name in &["put_page", "delete_page", "restore_page", "purge_deleted_pages"] {
            let op = registry.lookup(name).expect(&format!("{} should be registered", name));
            assert!(op.local_only(), "{} should be local_only", name);
        }

        // non-local_only ops: must return false
        for name in &["get_page", "think", "query", "list_pages"] {
            let op = registry.lookup(name).expect(&format!("{} should be registered", name));
            assert!(!op.local_only(), "{} should NOT be local_only", name);
        }
    }

    #[test]
    fn dynamic_local_only_unknown_operation_defaults_to_false() {
        let registry = OperationRegistry::new();

        // Unknown operations should NOT be treated as local_only.
        // This ensures the thin-client guard does not block operations
        // it has never registered — defaulting to permissive.
        let is_local = registry
            .lookup("nonexistent_op")
            .map(|op| op.local_only())
            .unwrap_or(false);
        assert!(!is_local, "unknown operation should default to not-local_only");
    }

    // --- ServeHttp arg tests (#69) ---

    #[test]
    fn serve_http_parses_with_no_flags() {
        let cli = Cli::try_parse_from(["zbrain", "serve", "--http"]).unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert!(args.port.is_none());
                assert!(args.bind.is_none());
                assert!(args.spa_dir.is_none());
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[test]
    fn serve_http_parses_with_port_flag() {
        let cli = Cli::try_parse_from(["zbrain", "serve", "--http", "--port", "4000"]).unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert_eq!(args.port, Some(4000));
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[test]
    fn serve_http_parses_with_bind_flag() {
        let cli = Cli::try_parse_from(["zbrain", "serve", "--http", "--bind", "0.0.0.0"]).unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert_eq!(args.bind.as_deref(), Some("0.0.0.0"));
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[test]
    fn serve_http_parses_all_flags_together() {
        let cli = Cli::try_parse_from([
            "zbrain", "serve", "--http", "--port", "8080", "--bind", "::1",
            "--spa-dir", "/tmp/admin-dist",
        ])
        .unwrap();
        match cli.command {
            Commands::ServeHttp(args) => {
                assert_eq!(args.port, Some(8080));
                assert_eq!(args.bind.as_deref(), Some("::1"));
                assert_eq!(args.spa_dir.as_deref(), Some(std::path::Path::new("/tmp/admin-dist")));
            }
            _ => panic!("expected ServeHttp"),
        }
    }

    #[tokio::test]
    async fn serve_http_integration_health_and_spa() {
        let tmp = tempfile::tempdir().unwrap();
        let spa_dir = tmp.path().to_path_buf();
        std::fs::write(spa_dir.join("index.html"), "<!DOCTYPE html><html><body>INTEGRATION_TEST_SPA</body></html>").unwrap();

        // Use a temp database so the engine can connect
        let db_path = tmp.path().join("test.db");
        std::env::set_var("ZBRAIN_DATABASE_URL", format!("sqlite://{}", db_path.display()));

        // Use a high port unlikely to conflict
        let test_port: u16 = 19876;

        let args = ServeHttpArgs {
            http: true,
            port: Some(test_port),
            bind: Some("127.0.0.1".to_string()),
            spa_dir: Some(spa_dir),
        };

        // Spawn the server in background
        let server_handle = tokio::spawn(async move {
            let _ = run_serve_http_command(args, None).await;
        });

        // Give the server a moment to bind and init schema
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        // Test /health
        let health_url = format!("http://127.0.0.1:{test_port}/health");
        let resp = reqwest::get(&health_url).await;
        assert!(resp.is_ok(), "health endpoint should be reachable");
        let resp = resp.unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok");

        // Test /admin/ SPA
        let admin_url = format!("http://127.0.0.1:{test_port}/admin/");
        let resp = reqwest::get(&admin_url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = resp.text().await.unwrap();
        assert!(body.contains("INTEGRATION_TEST_SPA"), "SPA content not found: {body}");

        // Abort server task (don't wait for graceful shutdown)
        server_handle.abort();
    }

    // --- Sync CLI arg tests (#101) ---

    #[test]
    fn sync_command_parses_defaults() {
        let cli = Cli::try_parse_from(["zbrain", "sync"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.source_id, "default");
                assert!(args.repo_path.is_none());
                assert!(!args.full_sync);
                assert!(args.chunker_version.is_none());
                assert_eq!(args.max_file_size, 0);
                assert!(args.failures_dir.is_none());
                assert_eq!(args.parallelism, 0);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_source_id() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--source-id", "my-docs"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.source_id, "my-docs");
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_repo_path() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--repo-path", "/home/user/repo"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.repo_path, Some(PathBuf::from("/home/user/repo")));
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_full_sync_flag() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--full-sync"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert!(args.full_sync);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_chunker_version() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--chunker-version", "2"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.chunker_version, Some(2));
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_max_file_size() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--max-file-size", "1048576"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.max_file_size, 1048576);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_failures_dir() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--failures-dir", "/tmp/sync-failures"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.failures_dir, Some(PathBuf::from("/tmp/sync-failures")));
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_parallelism() {
        let cli = Cli::try_parse_from(["zbrain", "sync", "--parallelism", "4"]).unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.parallelism, 4);
            }
            _ => panic!("expected Sync"),
        }
    }

    #[test]
    fn sync_command_parses_all_flags_together() {
        let cli = Cli::try_parse_from([
            "zbrain", "sync",
            "--source-id", "my-docs",
            "--repo-path", "/tmp/myrepo",
            "--full-sync",
            "--chunker-version", "3",
            "--max-file-size", "524288",
            "--failures-dir", "/tmp/failures",
            "--parallelism", "2",
        ])
        .unwrap();
        match cli.command {
            Commands::Sync(args) => {
                assert_eq!(args.source_id, "my-docs");
                assert_eq!(args.repo_path, Some(PathBuf::from("/tmp/myrepo")));
                assert!(args.full_sync);
                assert_eq!(args.chunker_version, Some(3));
                assert_eq!(args.max_file_size, 524288);
                assert_eq!(args.failures_dir, Some(PathBuf::from("/tmp/failures")));
                assert_eq!(args.parallelism, 2);
            }
            _ => panic!("expected Sync"),
        }
    }

    // --- Sources CLI arg tests (#105 sources add) ---

    #[test]
    fn sources_add_parses_required_id() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => {
                    assert_eq!(args.id, "my-source");
                    assert!(args.name.is_none());
                    assert!(args.path.is_none());
                    assert!(args.url.is_none());
                    assert!(!args.federated);
                    assert_eq!(args.depth, 1);
                    assert!(args.branch.is_none());
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_with_name() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--name", "My Source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.name.as_deref(), Some("My Source")),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_with_path() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--path", "/tmp/repo"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.path, Some(PathBuf::from("/tmp/repo"))),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_with_url() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--url", "https://github.com/foo/bar.git"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.url.as_deref(), Some("https://github.com/foo/bar.git")),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_path_url_conflict() {
        let result = Cli::try_parse_from([
            "zbrain", "sources", "add", "my-source",
            "--path", "/tmp/repo",
            "--url", "https://example.com/repo.git",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn sources_add_parses_federated() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "add", "my-source", "--federated"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert!(args.federated),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_clone_dir() {
        let cli = Cli::try_parse_from([
            "zbrain", "sources", "add", "my-source",
            "--clone-dir", "/custom/clone",
        ]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => assert_eq!(args.clone_dir, Some(PathBuf::from("/custom/clone"))),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_add_parses_depth_and_branch() {
        let cli = Cli::try_parse_from([
            "zbrain", "sources", "add", "my-source",
            "--depth", "0",
            "--branch", "main",
        ]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Add(args) => {
                    assert_eq!(args.depth, 0);
                    assert_eq!(args.branch.as_deref(), Some("main"));
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Sources list CLI tests (#102) ---

    #[test]
    fn sources_list_parses_default() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "list"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::List(args) => assert!(!args.json),
                _ => panic!("expected List"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_list_parses_json_flag() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "list", "--json"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::List(args) => assert!(args.json),
                _ => panic!("expected List"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Sources remove CLI tests (#104) ---

    #[test]
    fn sources_remove_parses_required_id() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => {
                    assert_eq!(args.id, "my-source");
                    assert!(!args.confirm_destructive);
                    assert!(!args.dry_run);
                    assert!(!args.keep_storage);
                    assert!(!args.yes);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_confirm_destructive() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--confirm-destructive"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.confirm_destructive),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_dry_run() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--dry-run"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.dry_run),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_keep_storage() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--keep-storage"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.keep_storage),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_yes_short() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "-y"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.yes),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_yes_long() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "remove", "my-source", "--yes"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => assert!(args.yes),
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_remove_parses_all_flags_together() {
        let cli = Cli::try_parse_from([
            "zbrain", "sources", "remove", "my-source",
            "--confirm-destructive",
            "--dry-run",
            "--keep-storage",
            "--yes",
        ]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Remove(args) => {
                    assert_eq!(args.id, "my-source");
                    assert!(args.confirm_destructive);
                    assert!(args.dry_run);
                    assert!(args.keep_storage);
                    assert!(args.yes);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Sources status CLI tests (#106) ---

    #[test]
    fn sources_status_parses_all_sources_default() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert!(args.source_id.is_none());
                    assert!(!args.json);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_status_parses_single_source() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status", "my-source"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert_eq!(args.source_id.as_deref(), Some("my-source"));
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_status_parses_json_flag() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status", "--json"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert!(args.json);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    #[test]
    fn sources_status_parses_source_with_json() {
        let cli = Cli::try_parse_from(["zbrain", "sources", "status", "my-source", "--json"]).unwrap();
        match cli.command {
            Commands::Sources(action) => match action {
                SourcesAction::Status(args) => {
                    assert_eq!(args.source_id.as_deref(), Some("my-source"));
                    assert!(args.json);
                },
                _ => panic!("unexpected SourcesAction"),
            },
            _ => panic!("expected Sources"),
        }
    }

    // --- Capture CLI tests (#103) ---

    #[test]
    fn capture_parses_file_input() {
        let cli = Cli::try_parse_from(["zbrain", "capture", "/path/to/note.md"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.content.as_deref(), Some("/path/to/note.md"));
                assert_eq!(args.r#type, "markdown");
                assert!(args.source.is_none());
                assert!(args.slug.is_none());
                assert!(!args.json);
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_stdin_when_no_file() {
        let cli = Cli::try_parse_from(["zbrain", "capture"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert!(args.content.is_none());
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_type_flag() {
        let cli = Cli::try_parse_from(["zbrain", "capture", "--type", "text", "myfile.txt"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.r#type, "text");
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_source_and_slug() {
        let cli = Cli::try_parse_from([
            "zbrain", "capture",
            "--source", "my-docs",
            "--slug", "custom-slug",
            "file.md",
        ]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.source.as_deref(), Some("my-docs"));
                assert_eq!(args.slug.as_deref(), Some("custom-slug"));
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_json_flag() {
        let cli = Cli::try_parse_from(["zbrain", "capture", "--json", "file.md"]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert!(args.json);
            },
            _ => panic!("expected Capture"),
        }
    }

    #[test]
    fn capture_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "zbrain", "capture",
            "--type", "markdown",
            "--source", "my-docs",
            "--slug", "my-page",
            "--json",
            "path/to/file.md",
        ]).unwrap();
        match cli.command {
            Commands::Capture(args) => {
                assert_eq!(args.r#type, "markdown");
                assert_eq!(args.source.as_deref(), Some("my-docs"));
                assert_eq!(args.slug.as_deref(), Some("my-page"));
                assert!(args.json);
                assert_eq!(args.content.as_deref(), Some("path/to/file.md"));
            },
            _ => panic!("expected Capture"),
        }
    }
}
