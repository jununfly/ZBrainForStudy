//! Configuration file discovery, parsing, and serialization.
//!
//! Matches the TS config system semantics:
//! 1. Look for `zbrain.yml` in current working directory
//! 2. Fall back to `~/.zbrain/config` if no cwd config found
//! 3. Environment variable overrides (ZBRAIN_* prefix)
//! 4. CLI flag overrides (--config)

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// ZBrain configuration.
///
/// Represents the full configuration loaded from YAML files and
/// environment variables. Field order matches the TypeScript schema
/// for compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// Database connection URL
    #[serde(default = "default_database_url")]
    pub database_url: String,

    /// API keys for external providers
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,

    /// Embedding model configuration
    #[serde(default)]
    pub embedding: EmbeddingConfig,

    /// Multimodal embedding model for `reindex multimodal` (e.g.
    /// `voyage:voyage-multimodal-3`). When unset, `reindex multimodal` refuses
    /// to run rather than silently embedding with the text model. The API key
    /// is read from `ZEROENTROPY_API_KEY` (same as text embeddings); the
    /// provider base URL is resolved from the model prefix / environment by the
    /// embedding client (same as text embeddings).
    #[serde(default)]
    pub embedding_multimodal_model: Option<String>,

    /// Search behavior settings
    #[serde(default)]
    pub search: SearchConfig,

    /// Agent and worker configuration
    #[serde(default)]
    pub agents: AgentsConfig,

    /// Logging and output configuration
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Remote MCP configuration for thin-client mode (multi-topology v1)
    /// When set, this install routes all DB operations through a remote
    /// `zbrain serve --http` server instead of using a local DB.
    #[serde(default)]
    pub remote_mcp: Option<RemoteMcpConfig>,

    /// HTTP server configuration (used when running `zbrain serve --http`).
    #[serde(default)]
    pub server: ServerConfig,

    /// MCP server configuration (only used when running `zbrain serve-mcp`).
    #[serde(default)]
    pub mcp: McpConfig,

    /// Sync configuration (used by `zbrain sync`).
    #[serde(default)]
    pub sync: Option<SyncConfig>,

    /// Arbitrary extra config keys (forward compatibility)
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct McpConfig {
    /// Rate limit for tools/call requests (requests per minute).
    /// `None` disables rate limiting entirely.
    #[serde(default)]
    pub rate_limit: Option<u64>,
}

/// Sync configuration (used by `zbrain sync`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SyncConfig {
    /// Default git repository path to sync from.
    pub default_repo: Option<PathBuf>,

    /// Chunker version to use (defaults to 1 if not set).
    pub chunker_version: Option<i32>,
}

/// HTTP server configuration (used by `zbrain serve --http`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerConfig {
    /// Listen port (default: 3000).
    #[serde(default = "default_server_port")]
    pub port: u16,

    /// Bind address (default: 127.0.0.1).
    #[serde(default = "default_server_bind")]
    pub bind: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: default_server_port(),
            bind: default_server_bind(),
        }
    }
}

/// Remote MCP configuration for thin-client mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RemoteMcpConfig {
    /// OAuth issuer URL for authentication
    pub issuer_url: String,
    /// MCP tool dispatch endpoint URL
    pub mcp_url: String,
    /// OAuth client ID for this brain
    pub oauth_client_id: String,
    /// OAuth client secret (can also be set via ZBRAIN_REMOTE_CLIENT_SECRET env)
    pub oauth_client_secret: Option<String>,
}

/// Returns true if the config has remote_mcp configured (thin-client mode)
pub fn is_thin_client(config: &Config) -> bool {
    config.remote_mcp.is_some()
}

/// Provider-specific API configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,

    /// Extra provider-specific config
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Embedding model configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingConfig {
    /// Whether embedding generation is enabled (default: true)
    #[serde(default = "default_embedding_enabled")]
    pub enabled: bool,

    /// Which embedding model to use (default: all-minilm-l6-v2)
    #[serde(default = "default_embedding_model")]
    pub model: String,

    /// Optional embedding vector dimensions. None means model default.
    #[serde(default)]
    pub dimensions: Option<u32>,

    /// Maximum chunk size for documents (default: 512)
    #[serde(default = "default_chunk_size")]
    pub chunk_size: usize,

    /// Chunk overlap size (default: 64)
    #[serde(default = "default_chunk_overlap")]
    pub chunk_overlap: usize,

    /// Batch size for embedding generation (default: 32)
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

/// Search behavior configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchConfig {
    /// Number of results to return by default (default: 10)
    #[serde(default = "default_top_k")]
    pub top_k: usize,

    /// Minimum similarity score threshold (default: 0.0)
    #[serde(default)]
    pub min_score: f32,

    /// Whether to include related facts in results (default: true)
    #[serde(default = "default_true")]
    pub include_facts: bool,

    /// Whether to do hybrid search (keyword + semantic) (default: true)
    #[serde(default = "default_true")]
    pub hybrid_search: bool,

    /// Whether the cross-encoder reranker is enabled (default: false).
    ///
    /// Read by `zbrain doctor`'s `reranker_health` check to interpret an
    /// empty failure window: enabled + no failures = healthy; disabled =
    /// no failures expected. Mirrors the TS `search.reranker.enabled` key,
    /// but lives on the config file plane here (per the Rust config unifies
    /// on a single file plane; the TS DB-plane key is not migrated).
    #[serde(default)]
    pub reranker_enabled: bool,

    /// G70 — default embedding column for the vector-retrieval path. `None`
    /// (default) or `"embedding"` scores against the text page vectors;
    /// `"embedding_multimodal"` selects the multimodal (image) page vectors.
    /// The `zbrain query --embedding-column` flag overrides this per call.
    #[serde(default)]
    pub embedding_column: Option<String>,
}

/// Agent and worker configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentsConfig {
    /// Maximum number of concurrent agents (default: 4)
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,

    /// Agent idle timeout in seconds (default: 300)
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout_secs: u64,

    /// Whether to enable agent tracing (default: false)
    #[serde(default)]
    pub enable_tracing: bool,
}

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoggingConfig {
    /// Log level: error, warn, info, debug, trace (default: info)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Whether to log to file (default: false)
    #[serde(default)]
    pub file: bool,

    /// Log file path (default: ~/.zbrain/zbrain.log)
    #[serde(default = "default_log_file")]
    pub file_path: Option<String>,
}

// === Default values ===

fn default_database_url() -> String {
    "sqlite://~/.zbrain/zbrain.db".to_string()
}

fn default_embedding_enabled() -> bool {
    true
}

fn default_embedding_model() -> String {
    "all-minilm-l6-v2".to_string()
}

fn default_chunk_size() -> usize {
    512
}

fn default_chunk_overlap() -> usize {
    64
}

fn default_batch_size() -> usize {
    32
}

fn default_top_k() -> usize {
    10
}

fn default_true() -> bool {
    true
}

fn default_max_concurrent() -> usize {
    4
}

fn default_idle_timeout() -> u64 {
    300
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_file() -> Option<String> {
    Some("~/.zbrain/zbrain.log".to_string())
}

fn default_server_port() -> u16 {
    3000
}

fn default_server_bind() -> String {
    "127.0.0.1".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            database_url: default_database_url(),
            providers: BTreeMap::new(),
            embedding: EmbeddingConfig::default(),
            embedding_multimodal_model: None,
            search: SearchConfig::default(),
            agents: AgentsConfig::default(),
            logging: LoggingConfig::default(),
            remote_mcp: None,
            server: ServerConfig::default(),
            mcp: McpConfig::default(),
            sync: None,
            extra: BTreeMap::new(),
        }
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: default_embedding_enabled(),
            model: default_embedding_model(),
            dimensions: None,
            chunk_size: default_chunk_size(),
            chunk_overlap: default_chunk_overlap(),
            batch_size: default_batch_size(),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            top_k: default_top_k(),
            min_score: 0.0,
            include_facts: true,
            hybrid_search: true,
            reranker_enabled: false,
            embedding_column: None,
        }
    }
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            max_concurrent: default_max_concurrent(),
            idle_timeout_secs: default_idle_timeout(),
            enable_tracing: false,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: false,
            file_path: default_log_file(),
        }
    }
}

// === Sensitive key redaction ===

/// Returns true if the key should be redacted in output.
/// Matches TS `isSensitiveConfigKey` behavior:
/// - key, secret, token, password, pwd, passwd, auth (word-boundary matching)
#[must_use]
pub fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    let sensitive_words = [
        "key", "secret", "token", "password", "pwd", "passwd", "auth",
    ];

    for word in sensitive_words {
        // Word boundary matching:
        // - Key equals the word exactly
        // - Key starts with the word followed by non-alphanumeric
        // - Key ends with non-alphanumeric followed by the word
        // - Key contains word surrounded by non-alphanumeric
        if lower == word
            || lower.starts_with(&(word.to_string() + "_"))
            || lower.starts_with(&(word.to_string() + "."))
            || lower.starts_with(&(word.to_string() + "-"))
            || lower.ends_with(&("_".to_string() + word))
            || lower.ends_with(&(".".to_string() + word))
            || lower.ends_with(&("-".to_string() + word))
            || lower.contains(&("_".to_string() + word + "_"))
            || lower.contains(&(".".to_string() + word + "."))
            || lower.contains(&("-".to_string() + word + "-"))
        {
            return true;
        }
    }
    false
}

/// Redact sensitive config values for display.
/// Passwords in URLs (postgresql://user:pass@host) are also redacted.
#[must_use]
pub fn redact_value(key: &str, value: &str) -> String {
    // Redact database URLs with embedded passwords
    if value.contains("postgresql://") || value.contains("postgres://") {
        return regex::Regex::new(r"(postgres(?:ql)?://[^:]+:)([^@]+)(@)")
            .map(|re| re.replace_all(value, "${1}***${3}").to_string())
            .unwrap_or_else(|_| "***".to_string());
    }

    // Redact sensitive keys
    if is_sensitive_key(key) {
        return "***".to_string();
    }

    value.to_string()
}

// === Config discovery and loading ===

/// Resolve the user home directory.
///
/// Honors the `ZBRAIN_HOME` environment variable as an explicit,
/// cross-platform override. Its value is treated as the home *root*, so the
/// zbrain home (`~/.zbrain`) lives at `<ZBRAIN_HOME>/.zbrain`. Falls back to
/// the OS home directory (`$HOME` on Unix, `%USERPROFILE%` on Windows).
///
/// `ZBRAIN_HOME=/tmp/x` redirects **all** `~/.zbrain` state in a
/// platform-independent way — unlike `HOME`, which `dirs::home_dir()` ignores
/// on Windows (so we read `HOME`/`USERPROFILE` directly instead of
/// `dirs::home_dir()`). This is the recommended isolation mechanism for tests
/// and isolated runs.
#[must_use]
pub fn home_root() -> Option<PathBuf> {
    zbrain_core::paths::home_root()
}

/// Get the default zbrain home directory (`~/.zbrain`), honoring `ZBRAIN_HOME`.
#[must_use]
pub fn zbrain_home() -> Option<PathBuf> {
    zbrain_core::paths::zbrain_home()
}

/// Get the default user config directory (~/.zbrain/).
#[must_use]
pub fn user_config_dir() -> Option<PathBuf> {
    zbrain_home()
}

/// Get the default user config file path (~/.zbrain/config).
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    user_config_dir().map(|dir| dir.join("config"))
}

/// Try to find the config file using discovery order:
/// 1. Explicit CLI path (if provided)
/// 2. ./zbrain.yml in current working directory
/// 3. ~/.zbrain/config
pub fn find_config_file(explicit_path: Option<&Path>) -> Option<PathBuf> {
    // 1. Explicit path if provided
    if let Some(path) = explicit_path {
        if path.exists() {
            return Some(path.to_path_buf());
        }
    }

    // 2. Current working directory
    let cwd_zbrain = Path::new("zbrain.yml");
    if cwd_zbrain.exists() {
        return Some(cwd_zbrain.canonicalize().unwrap_or_else(|_| cwd_zbrain.to_path_buf()));
    }

    // 3. User home directory config
    user_config_path().filter(|p| p.exists())
}

/// Load configuration from a specific file.
pub fn load_config_from_path(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config file: {}", path.display()))?;

    let config: Config = serde_yaml::from_str(&content)
        .with_context(|| format!("Failed to parse YAML config: {}", path.display()))?;

    Ok(config)
}

/// Load configuration with discovery:
/// 1. Explicit CLI path
/// 2. ./zbrain.yml
/// 3. ~/.zbrain/config
/// 4. Environment variable overrides (ZBRAIN_*)
///
/// Returns default config if no file found and no env vars set.
pub fn load_config(explicit_path: Option<&Path>) -> Result<Config> {
    // First try to load from file
    let mut config = if let Some(config_path) = find_config_file(explicit_path) {
        load_config_from_path(&config_path)?
    } else {
        Config::default()
    };

    // Apply environment variable overrides
    apply_env_overrides(&mut config)?;

    Ok(config)
}

/// Write configuration to a file.
pub fn write_config(config: &Config, path: &Path) -> Result<()> {
    // Create parent directory if needed
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create config directory: {}", parent.display()))?;
    }

    let yaml = serde_yaml::to_string(config)
        .context("Failed to serialize config to YAML")?;

    std::fs::write(path, yaml)
        .with_context(|| format!("Failed to write config file: {}", path.display()))?;

    Ok(())
}

/// Apply ZBRAIN_* environment variable overrides to config.
///
/// Mapping:
/// - ZBRAIN_DATABASE_URL → config.database_url
/// - ZBRAIN_SEARCH_TOP_K → config.search.top_k
/// - ZBRAIN_EMBEDDING_MODEL → config.embedding.model
/// - ZBRAIN_PROVIDER_{NAME}_{KEY} → config.providers[name].key
fn apply_env_overrides(config: &mut Config) -> Result<()> {
    for (key, value) in env::vars() {
        if !key.starts_with("ZBRAIN_") {
            continue;
        }

        let suffix = key.strip_prefix("ZBRAIN_").unwrap_or_default();

        match suffix {
            "DATABASE_URL" => config.database_url = value,
            "SEARCH_TOP_K" => config.search.top_k = value.parse()?,
            "SEARCH_MIN_SCORE" => config.search.min_score = value.parse()?,
            "EMBEDDING_MODEL" => config.embedding.model = value,
            "EMBEDDING_CHUNK_SIZE" => config.embedding.chunk_size = value.parse()?,
            "EMBEDDING_CHUNK_OVERLAP" => config.embedding.chunk_overlap = value.parse()?,
            "EMBEDDING_BATCH_SIZE" => config.embedding.batch_size = value.parse()?,
            "AGENTS_MAX_CONCURRENT" => config.agents.max_concurrent = value.parse()?,
            "AGENTS_IDLE_TIMEOUT" => config.agents.idle_timeout_secs = value.parse()?,
            "LOGGING_LEVEL" => config.logging.level = value,
            "LOGGING_FILE" => config.logging.file = value.parse()?,
            _ => {
                // Handle provider overrides: ZBRAIN_PROVIDER_{NAME}_{KEY}
                if let Some(provider_suffix) = suffix.strip_prefix("PROVIDER_") {
                    if let Some((name, provider_key)) = provider_suffix.split_once('_') {
                        let provider_name = name.to_lowercase();
                        let provider = config.providers
                            .entry(provider_name.clone())
                            .or_insert_with(ProviderConfig::default);

                        match provider_key {
                            "API_KEY" => provider.api_key = Some(value),
                            "BASE_URL" => provider.base_url = Some(value),
                            "MODEL" => provider.model = Some(value),
                            "MAX_TOKENS" => provider.max_tokens = Some(value.parse()?),
                            "TEMPERATURE" => provider.temperature = Some(value.parse()?),
                            _ => {} // Unknown provider key, ignore silently
                        }
                    }
                }
                // Otherwise, store in extra config for forward compatibility
                else {
                    config.extra.insert(
                        suffix.to_lowercase(),
                        serde_yaml::Value::String(value),
                    );
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_key_detection() {
        assert!(is_sensitive_key("api_key"));
        assert!(is_sensitive_key("secret_token"));
        assert!(is_sensitive_key("database.password"));
        assert!(is_sensitive_key("auth-header"));
        assert!(is_sensitive_key("service_pwd"));
        assert!(is_sensitive_key("passwd_file"));

        assert!(!is_sensitive_key("monkey")); // Should not match "key" inside word
        assert!(!is_sensitive_key("database_url"));
        assert!(!is_sensitive_key("chunk_size"));
        assert!(!is_sensitive_key("keyboard")); // Should not match partial
    }

    #[test]
    fn sensitive_value_redaction() {
        assert_eq!(redact_value("api_key", "secret123"), "***");
        assert_eq!(redact_value("database_url", "postgres://user:pass@host/db"),
            "postgres://user:***@host/db");
        assert_eq!(redact_value("database_url", "postgresql://admin:xyz123@localhost:5432/zbrain"),
            "postgresql://admin:***@localhost:5432/zbrain");
        assert_eq!(redact_value("chunk_size", "512"), "512");
    }

    #[test]
    fn config_defaults() {
        let config = Config::default();
        assert_eq!(config.database_url, "sqlite://~/.zbrain/zbrain.db");
        assert!(config.embedding.enabled);
        assert_eq!(config.embedding.model, "all-minilm-l6-v2");
        assert_eq!(config.embedding.dimensions, None);
        assert_eq!(config.embedding.chunk_size, 512);
        assert_eq!(config.embedding.chunk_overlap, 64);
        assert_eq!(config.search.top_k, 10);
        assert_eq!(config.agents.max_concurrent, 4);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn server_config_defaults() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.port, 3000);
        assert_eq!(cfg.bind, "127.0.0.1");
    }

    #[test]
    fn server_config_from_yaml() {
        let yaml = r#"
server:
  port: 8080
  bind: "0.0.0.0"
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.server.port, 8080);
        assert_eq!(config.server.bind, "0.0.0.0");
    }

    #[test]
    fn config_with_server_defaults_in_full_yaml() {
        let yaml = r#"
database_url: "sqlite://test.db"
server:
  port: 4000
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.server.port, 4000);
        assert_eq!(config.server.bind, "127.0.0.1"); // default
        assert_eq!(config.database_url, "sqlite://test.db");
    }
}