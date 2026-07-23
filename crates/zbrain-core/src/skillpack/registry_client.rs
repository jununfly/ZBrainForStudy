/**
 * skillpack/registry_client.rs — fetch + cache + stale-fallback for the
 * `zbrain-skillpack-registry` catalog.
 *
 * The default registry lives at
 *   https://raw.githubusercontent.com/garrytan/zbrain-skillpack-registry/main/registry.json
 *   https://raw.githubusercontent.com/garrytan/zbrain-skillpack-registry/main/endorsements.json
 *
 * Offline-safe per the user's decision: when the network fetch fails (DNS
 * miss, 5xx, timeout), fall back to the on-disk cache and emit a single
 * stderr warning per process. Cache freshness threshold: 1h soft TTL for
 * normal use, 7d before the "registry cache is stale" escalation fires.
 * Hard-fail only when there is NO cache at all (first run + offline).
 *
 * Uses reqwest for HTTP fetching. Honors ETag for cheap polling so successive
 * fetches against an unchanged registry are effectively free.
 */

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, Duration, UNIX_EPOCH};
use sha2::{Sha256, Digest};
use serde::{Deserialize, Serialize};
use reqwest::{Client, header, header::HeaderName};
use chrono::{DateTime, FixedOffset, Utc};
use crate::paths::zbrain_path;
use crate::skillpack::registry_schema::{
    validate_registry_catalog, validate_endorsements_file, effective_tier,
    RegistryCatalog, RegistryEntry, EndorsementsFile, RegistryTier, RegistrySchemaError,
};

/// Default registry URL — the canonical Garry-controlled catalog.
pub const DEFAULT_REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/garrytan/zbrain-skillpack-registry/main/registry.json";

/// Default endorsements URL — sibling file in the same repo.
pub const DEFAULT_ENDORSEMENTS_URL: &str =
    "https://raw.githubusercontent.com/garrytan/zbrain-skillpack-registry/main/endorsements.json";

/// Soft TTL: prefer cache when it's younger than this (no fetch attempt).
const SOFT_TTL: Duration = Duration::from_secs(60 * 60); // 1 hour
/// Stale escalation: surface a louder warning when cache is older than this.
const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60); // 7 days

/// Cache file payload — wraps the validated registry + freshness metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryCacheFile {
    fetched_at: String, // ISO 8601
    etag: Option<String>,
    url: String,
    catalog: RegistryCatalog,
    endorsements: Option<EndorsementsFile>,
}

/// Origin of the returned registry data — informational for status output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryOrigin {
    FreshFetch,
    CacheWarm,
    CacheSoftStale,
    CacheHardStale,
}

impl std::fmt::Display for RegistryOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryOrigin::FreshFetch => write!(f, "fresh_fetch"),
            RegistryOrigin::CacheWarm => write!(f, "cache_warm"),
            RegistryOrigin::CacheSoftStale => write!(f, "cache_soft_stale"),
            RegistryOrigin::CacheHardStale => write!(f, "cache_hard_stale"),
        }
    }
}

/// Result of `load_registry`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedRegistry {
    pub catalog: RegistryCatalog,
    pub endorsements: Option<EndorsementsFile>,
    /// Where the data came from — informational for status output.
    pub origin: RegistryOrigin,
    /// How old the cache is in ms (always set when origin is one of the cache states).
    pub cache_age_ms: Option<u64>,
    /// URL the catalog came from (after config override).
    pub registry_url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryClientError {
    #[error("registry fetch failed ({0}) and no on-disk cache exists. First-run installs require network.")]
    NoCacheNoNetwork(String),

    #[error("fetched registry.json but schema is invalid: {0}")]
    FetchSucceededButSchemaInvalid(String),

    #[error("on-disk cache is corrupt: {0}")]
    CacheCorrupt(String),

    #[error("Invalid registry URL: {0}")]
    UrlInvalid(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] io::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LoadRegistryOptions {
    /// Override the registry URL (defaults to config key skillpack.registry_url then DEFAULT_REGISTRY_URL).
    pub url: Option<String>,
    /// Force a fresh fetch even when cache is within the soft TTL.
    #[serde(default)]
    pub refresh: bool,
    /// Test seam: override the cache directory.
    pub cache_dir: Option<PathBuf>,
    /// Test seam: short-circuit network entirely (forces cache-or-fail).
    #[serde(default)]
    pub no_network: bool,
}

/// Stable cache file path for a given registry URL.
fn cache_path_for(url: &str, cache_dir: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    let sha = format!("{:x}", hasher.finalize());
    let short_sha = &sha[0..16];
    cache_dir.join(format!("registry-{short_sha}.json"))
}

/// Convert the registry URL to the sibling endorsements.json URL.
fn endorsements_url_for(registry_url: &str) -> String {
    if registry_url == DEFAULT_REGISTRY_URL {
        return DEFAULT_ENDORSEMENTS_URL.to_string();
    }
    // Replace the trailing "registry.json" with "endorsements.json" so custom
    // registries that mirror the layout work transparently.
    registry_url.replace("registry.json$", "endorsements.json")
}

/// Resolve the active registry URL: opts → config → default.
pub fn resolve_registry_url(_opts: &LoadRegistryOptions) -> String {
    // TODO: load from config after config migration
    // if let Some(url) = &opts.url { return url.clone(); }
    // try {
    //   const cfg = loadConfig();
    //   const configured = (cfg as unknown as Record<string, unknown>).skillpack;
    //   if (configured && typeof configured === 'object') {
    //     const url = (configured as Record<string, unknown>).registry_url;
    //     if (typeof url === 'string' && url.length > 0) return url;
    //   }
    // } catch {
    //   // loadConfig() may throw on first-run before init; default is fine.
    // }
    DEFAULT_REGISTRY_URL.to_string()
}

/// Default cache directory under ~/.zbrain/skillpack-cache.
fn default_cache_dir() -> PathBuf {
    zbrain_path("skillpack-cache").unwrap_or_else(|| PathBuf::from("skillpack-cache"))
}

/// Read a cache file from disk; None if missing or malformed.
fn read_cache(cache_file: &Path) -> Option<RegistryCacheFile> {
    if !cache_file.exists() {
        return None;
    }

    let raw = match fs::read_to_string(cache_file) {
        Ok(r) => r,
        Err(_) => return None,
    };

    let parsed: serde_json::Result<RegistryCacheFile> = serde_json::from_str(&raw);
    let mut cached = match parsed {
        Ok(c) => c,
        Err(_) => return None,
    };

    // Validate schema
    if let Err(e) = validate_registry_catalog(serde_json::to_value(&cached.catalog).unwrap()) {
        tracing::warn!("Cached registry schema invalid: {}", e);
        return None;
    }

    if let Some(endorsements) = &cached.endorsements {
        if let Err(e) = validate_endorsements_file(serde_json::to_value(endorsements).unwrap()) {
            tracing::warn!("Cached endorsements schema invalid: {}", e);
            cached.endorsements = None;
        }
    }

    Some(cached)
}

/// Atomically write a cache file.
fn write_cache(cache_file: &Path, payload: &RegistryCacheFile) -> Result<(), io::Error> {
    if let Some(parent) = cache_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp = cache_file.with_extension("tmp");
    let json = serde_json::to_string_pretty(payload)?;
    fs::write(&tmp, json)?;
    fs::rename(&tmp, cache_file)?;
    Ok(())
}

/// Format cache age for log lines.
fn fmt_age(dur: Duration) -> String {
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 60 * 60 {
        let mins = secs / 60;
        format!("{mins}m")
    } else if secs < 24 * 60 * 60 {
        let hours = secs / (60 * 60);
        format!("{hours}h")
    } else {
        let days = secs / (24 * 60 * 60);
        format!("{days}d")
    }
}

/// Load the registry. Tries network first (unless cache is fresh and
/// refresh=false), falls back to cache on any failure, escalates to the
/// stale warning when cache > 7d. Hard-fails only with no cache + no
/// network.
pub async fn load_registry(
    opts: LoadRegistryOptions,
) -> Result<LoadedRegistry, RegistryClientError> {
    let url = resolve_registry_url(&opts);
    let cache_dir = opts.cache_dir.unwrap_or_else(default_cache_dir);
    let cache_file = cache_path_for(&url, &cache_dir);
    let cached = read_cache(&cache_file);

    let now = SystemTime::now();

    // Cache-warm fast path: cache is fresh AND refresh wasn't requested.
    if let Some(cached) = &cached {
        if !opts.refresh {
            let fetched_at = DateTime::parse_from_rfc3339(&cached.fetched_at)
                .map(|dt| {
                    let ts = dt.timestamp();
                    if ts >= 0 {
                        UNIX_EPOCH + Duration::from_secs(ts.abs() as u64)
                    } else {
                        UNIX_EPOCH - Duration::from_secs(ts.abs() as u64)
                    }
                })
                .unwrap_or_else(|_| SystemTime::now());
            let age = now.duration_since(fetched_at).unwrap_or(Duration::ZERO);
            if age < SOFT_TTL {
                return Ok(LoadedRegistry {
                    catalog: cached.catalog.clone(),
                    endorsements: cached.endorsements.clone(),
                    origin: RegistryOrigin::CacheWarm,
                    cache_age_ms: Some(age.as_millis() as u64),
                    registry_url: url,
                });
            }
        }
    }

    // Network path (unless explicitly disabled for tests).
    if !opts.no_network {
        let client = Client::new();
        let mut headers = header::HeaderMap::new();
        headers.insert(header::ACCEPT, header::HeaderValue::from_static("application/json"));

        let mut request = client.get(&url).headers(headers);
        if let Some(cached) = &cached {
            if let Some(etag) = &cached.etag {
                if !opts.refresh {
                    request = request.header(header::IF_NONE_MATCH, etag);
                }
            }
        }

        let res = request.send().await?;

        if res.status() == reqwest::StatusCode::NOT_MODIFIED {
            // 304: cache hit via etag; touch the fetched_at so we don't re-poll within the soft TTL.
            if let Some(mut cached) = cached {
                cached.fetched_at = Utc::now().to_rfc3339();
                write_cache(&cache_file, &cached)?;
                return Ok(LoadedRegistry {
                    catalog: cached.catalog,
                    endorsements: cached.endorsements,
                    origin: RegistryOrigin::FreshFetch,
                    cache_age_ms: Some(0),
                    registry_url: url,
                });
            }
        }

        if !res.status().is_success() {
            return Err(RegistryClientError::Http(
                reqwest::Error::from(res.error_for_status().unwrap_err()),
            ));
        }

        let etag = res.headers().get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let catalog_json: serde_json::Value = res.json().await?;
        let catalog = validate_registry_catalog(catalog_json)
            .map_err(|e| RegistryClientError::FetchSucceededButSchemaInvalid(e.to_string()))?;

        // Fetch endorsements.json (best-effort: a missing file is not an error).
        let mut endorsements: Option<EndorsementsFile> = None;
        let end_url = endorsements_url_for(&url);
        if let Ok(end_res) = client.get(&end_url).send().await {
            if end_res.status().is_success() {
                if let Ok(end_json) = end_res.json().await {
                    if let Ok(validated) = validate_endorsements_file(end_json) {
                        endorsements = Some(validated);
                    }
                }
            }
        }

        let payload = RegistryCacheFile {
            fetched_at: Utc::now().to_rfc3339(),
            etag,
            url: url.clone(),
            catalog: catalog.clone(),
            endorsements: endorsements.clone(),
        };
        write_cache(&cache_file, &payload)?;

        return Ok(LoadedRegistry {
            catalog,
            endorsements,
            origin: RegistryOrigin::FreshFetch,
            cache_age_ms: Some(0),
            registry_url: url,
        });
    }

    // no_network branch.
    if let Some(cached) = cached {
        let fetched_at = DateTime::parse_from_rfc3339(&cached.fetched_at)
            .map(|dt| {
                let ts = dt.timestamp();
                if ts >= 0 {
                    UNIX_EPOCH + Duration::from_secs(ts.abs() as u64)
                } else {
                    UNIX_EPOCH - Duration::from_secs(ts.abs() as u64)
                }
            })
            .unwrap_or_else(|_| SystemTime::now());
        let age = now.duration_since(fetched_at).unwrap_or(Duration::ZERO);
        let origin = if age > STALE_AFTER {
            RegistryOrigin::CacheHardStale
        } else if age > SOFT_TTL {
            RegistryOrigin::CacheSoftStale
        } else {
            RegistryOrigin::CacheWarm
        };
        return Ok(LoadedRegistry {
            catalog: cached.catalog,
            endorsements: cached.endorsements,
            origin,
            cache_age_ms: Some(age.as_millis() as u64),
            registry_url: url,
        });
    }

    Err(RegistryClientError::NoCacheNoNetwork(format!(
        "--no-network was set but no cache exists for {url}"
    )))
}

/// Lookup a pack by name. Returns None when not present.
pub fn find_pack<'a>(loaded: &'a LoadedRegistry, name: &str) -> Option<&'a RegistryEntry> {
    loaded.catalog.skillpacks.iter().find(|e| e.name == name)
}

/// Lookup a pack with its effective tier applied.
pub fn find_pack_with_tier<'a>(
    loaded: &'a LoadedRegistry,
    name: &str,
) -> Option<(&'a RegistryEntry, RegistryTier)> {
    let entry = find_pack(loaded, name)?;
    let tier = effective_tier(entry, loaded.endorsements.as_ref());
    Some((entry, tier))
}

/// Search the catalog by free-text query. Matches against name, description,
/// author, and tags (lowercase contains). Returns entries paired with their
/// effective tier; sorted by tier (endorsed > community > experimental > dead)
/// then alphabetical by name.
pub fn search_packs<'a>(
    loaded: &'a LoadedRegistry,
    query: Option<&'a str>,
    tier_filter: Option<RegistryTier>,
) -> Vec<(&'a RegistryEntry, RegistryTier)> {
    let q = query.unwrap_or("").trim().to_lowercase();
    let tier_order = [
        RegistryTier::Endorsed,
        RegistryTier::Community,
        RegistryTier::Experimental,
        RegistryTier::Dead,
    ];

    let mut results: Vec<_> = loaded.catalog.skillpacks
        .iter()
        .filter_map(|entry| {
            let tier = effective_tier(entry, loaded.endorsements.as_ref());
            if let Some(filter_tier) = tier_filter {
                if tier != filter_tier {
                    return None;
                }
            }

            if !q.is_empty() {
                let mut haystack = String::new();
                haystack.push_str(&entry.name);
                haystack.push(' ');
                haystack.push_str(&entry.description);
                haystack.push(' ');
                haystack.push_str(&entry.author);
                haystack.push(' ');
                haystack.push_str(&entry.author_handle);
                haystack.push(' ');
                for tag in &entry.tags {
                    haystack.push_str(tag);
                    haystack.push(' ');
                }

                if !haystack.to_lowercase().contains(&q) {
                    return None;
                }
            }

            Some((entry, tier))
        })
        .collect();

    results.sort_by(|a, b| {
        let a_ord = tier_order.iter().position(|&t| t == a.1);
        let b_ord = tier_order.iter().position(|&t| t == b.1);
        match (a_ord, b_ord) {
            (Some(a), Some(b)) if a != b => a.cmp(&b),
            _ => a.0.name.cmp(&b.0.name),
        }
    });

    results
}
