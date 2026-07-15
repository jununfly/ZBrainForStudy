//! Schema pack activation — set/clear active pack + reload (flush stale locks).
//!
//! Activation model: the 7-tier resolution chain in `registry.rs` determines
//! the active pack. This module provides helpers for Tier 6 (home config)
//! to set/clear the `schema_pack` field in `~/.zbrain/config.json`.
//!
//! `reload` clears stale lock files — the in-process cache is per-invocation
//! for CLI, so there's nothing to flush. For long-running processes (MCP,
//! autopilot), the PackRegistry's `invalidate` method handles cache flushing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Home config (Tier 6 in the resolution chain)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HomeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_pack: Option<String>,
    /// Catch-all for unknown fields (preserves them on write-back).
    #[serde(flatten)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

fn home_config_path() -> PathBuf {
    crate::paths::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("config.json")
}

fn read_home_config() -> HomeConfig {
    let path = home_config_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn write_home_config(config: &HomeConfig) -> Result<(), String> {
    let path = home_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create config dir: {e}"))?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("cannot serialize config: {e}"))?;
    std::fs::write(&path, json).map_err(|e| format!("cannot write config: {e}"))
}

// ---------------------------------------------------------------------------
// Activation verbs
// ---------------------------------------------------------------------------

/// Set the active schema pack (writes to `~/.zbrain/config.json`).
pub fn set_active_pack(name: &str) -> Result<(), String> {
    let mut config = read_home_config();
    config.schema_pack = Some(name.to_string());
    write_home_config(&config)
}

/// Clear the active schema pack (reverts to default `zbrain-base`).
pub fn clear_active_pack() -> Result<(), String> {
    let mut config = read_home_config();
    config.schema_pack = None;
    write_home_config(&config)
}

/// Get the active pack from home config (Tier 6).
pub fn get_active_pack_from_config() -> Option<String> {
    read_home_config().schema_pack
}

// ---------------------------------------------------------------------------
// Reload (flush stale locks)
// ---------------------------------------------------------------------------

/// Clear stale pack lock files.
///
/// If `pack_name` is `None`, clears all lock files in the lock directory.
/// Returns the list of cleared lock file paths.
pub fn reload_pack_cache(pack_name: Option<&str>) -> Vec<String> {
    let lock_dir = crate::paths::zbrain_home()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("schema-packs")
        .join(".locks");

    let mut cleared = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&lock_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            // Filter by pack name if specified
            if let Some(pn) = pack_name {
                if !name.starts_with(pn) {
                    continue;
                }
            }

            // Remove the lock file
            if std::fs::remove_file(&path).is_ok() {
                cleared.push(path.display().to_string());
            }
        }
    }
    cleared
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Each test injects an isolated `~/.zbrain` via `ScopedTestHome` (thread-local,
    // no global env mutation), so the suite runs fully in parallel.

    #[test]
    fn set_and_read_active_pack() {
        let _home = crate::paths::ScopedTestHome::new();

        set_active_pack("my-custom-pack").unwrap();
        let active = get_active_pack_from_config();
        assert_eq!(active.as_deref(), Some("my-custom-pack"));

        // Verify file exists
        let config_path = home_config_path();
        assert!(config_path.exists());
    }

    #[test]
    fn clear_active_pack_works() {
        let _home = crate::paths::ScopedTestHome::new();

        set_active_pack("my-pack").unwrap();
        assert!(get_active_pack_from_config().is_some());

        clear_active_pack().unwrap();
        assert!(get_active_pack_from_config().is_none());
    }

    #[test]
    fn get_active_pack_when_no_config() {
        let _home = crate::paths::ScopedTestHome::new();

        // No config file exists
        assert!(get_active_pack_from_config().is_none());
    }

    #[test]
    fn set_active_pack_preserves_other_fields() {
        let _home = crate::paths::ScopedTestHome::new();

        // Write a config with an extra field
        let config_path = home_config_path();
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(
            &config_path,
            r#"{"other_field": "value", "schema_pack": "old-pack"}"#,
        )
        .unwrap();

        // Set new pack
        set_active_pack("new-pack").unwrap();

        // Read back and verify
        let content = std::fs::read_to_string(&config_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["schema_pack"], "new-pack");
        assert_eq!(json["other_field"], "value");
    }

    #[test]
    fn reload_clears_locks() {
        let _home = crate::paths::ScopedTestHome::new();

        // Create a lock dir with some lock files
        let lock_dir = _home.zbrain_dir().join("schema-packs").join(".locks");
        std::fs::create_dir_all(&lock_dir).unwrap();
        std::fs::write(lock_dir.join("my-pack.lock"), "{}").unwrap();
        std::fs::write(lock_dir.join("other-pack.lock"), "{}").unwrap();

        // Clear all
        let cleared = reload_pack_cache(None);
        assert_eq!(cleared.len(), 2);

        // Clear specific
        std::fs::write(lock_dir.join("my-pack.lock"), "{}").unwrap();
        std::fs::write(lock_dir.join("other-pack.lock"), "{}").unwrap();
        let cleared = reload_pack_cache(Some("my-pack"));
        assert_eq!(cleared.len(), 1);
        assert!(cleared[0].contains("my-pack.lock"));
    }

    #[test]
    fn reload_no_lock_dir_returns_empty() {
        let _home = crate::paths::ScopedTestHome::new();

        let cleared = reload_pack_cache(None);
        assert!(cleared.is_empty());
    }
}
