/**
 * skillpack/state.rs — machine-owned install-state file at
 * `~/.zbrain/skillpack-state.json`.
 *
 * Codex outside-voice G1 fix: the original spec put TOFU SHA, pinned
 * commits, source URLs, rename maps, and per-source receipts inside
 * markdown comments in the user's RESOLVER.md / AGENTS.md. Codex
 * pointed out that an editable markdown trust store is fragile —
 * any agent or human edit corrupts provenance. v0.36 retired the
 * managed-block model entirely, so this file becomes the single
 * source of truth for "what third-party scaffolds happened, when,
 * from where, with what verified hash."
 *
 * Atomic update via `.tmp` + `rename()`. Schema-versioned. Pure
 * function over the parsed JSON; the calling commands wrap it with
 * read/write helpers.
 */

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::paths::zbrain_path;
use crate::skillpack::remote_source::ResolvedSourceKind;

/// Schema version stamped on every state file.
pub const SKILLPACK_STATE_SCHEMA_VERSION: &str = "zbrain-skillpack-state-v1";

/// Per-pack scaffold record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillpackStateEntry {
    /// Pack name (matches skillpack.json `name`).
    pub name: String,
    /// Pack version when scaffold ran.
    pub version: String,
    /// Author display name (whatever the manifest declared).
    pub author: String,
    /// Source URL or local path scaffold pulled from.
    pub source: String,
    /// Source kind.
    pub source_kind: Option<ResolvedSourceKind>,
    /// Resolved git commit SHA when source_kind=git; None for tarball/local.
    pub pinned_commit: Option<String>,
    /// SHA-256 of the tarball that was extracted when source_kind=tarball; None otherwise.
    pub tarball_sha256: Option<String>,
    /// Tier the pack was on in the registry at scaffold time (informational only).
    pub tier: Option<crate::skillpack::trust_prompt::SkillpackTier>,
    /// ISO 8601 wall-clock timestamp of the scaffold (UTC).
    pub scaffolded_at: Option<String>,
    /// Absolute path of the workspace where files were written.
    pub workspace: Option<PathBuf>,
    /// Skill slugs the pack contributed (relative paths under skills/).
    pub skill_slugs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillpackState {
    pub schema_version: String,
    pub packs: Vec<SkillpackStateEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillpackStateError {
    #[error("skillpack-state.json is not valid JSON ({path}): {err}")]
    StateMalformedJson { path: String, err: String },

    #[error("skillpack-state.json has unknown schema_version {actual}; expected {expected}")]
    StateSchemaUnknown { actual: String, expected: &'static str },

    #[error("failed to atomically write skillpack-state.json to {path}: {err}")]
    StateAtomicWriteFailed { path: String, err: String },
}

fn empty_state() -> SkillpackState {
    SkillpackState {
        schema_version: SKILLPACK_STATE_SCHEMA_VERSION.to_string(),
        packs: Vec::new(),
    }
}

/// Default state file path. Override via `opts.state_path` in calling code.
pub fn default_state_path() -> PathBuf {
    zbrain_path("skillpack-state.json")
        .unwrap_or_else(|| PathBuf::from("skillpack-state.json"))
}

/// Load the state file. Returns an empty state on missing file (cold start).
/// Throws on malformed JSON or unknown schema version (forward-compat: a
/// future state.v2 file should not be silently downgraded).
pub fn load_state(state_path: Option<&Path>) -> SkillpackState {
    let default = default_state_path();
    let path = state_path.unwrap_or(&default);

    if !path.exists() {
        return empty_state();
    }

    let raw = match fs::read_to_string(path) {
        Ok(r) => r,
        Err(e) => {
            // If we can't read it, treat it as empty (don't fail hard)
            tracing::warn!("Failed to read skillpack-state.json: {}, starting empty", e);
            return empty_state();
        }
    };

    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("skillpack-state.json is not valid JSON: {}, starting empty", e);
            return empty_state();
        }
    };

    let schema_version = match value.get("schema_version").and_then(|v| v.as_str()) {
        Some(v) => v,
        None => {
            tracing::warn!("skillpack-state.json missing schema_version, starting empty");
            return empty_state();
        }
    };

    if schema_version != SKILLPACK_STATE_SCHEMA_VERSION {
        panic!("skillpack-state.json has unknown schema_version {schema_version}; expected {SKILLPACK_STATE_SCHEMA_VERSION}");
    }

    let packs = match value.get("packs").and_then(|v| v.as_array()) {
        Some(arr) => {
            serde_json::from_value(serde_json::Value::Array(arr.clone()))
                .unwrap_or_else(|e| {
                    tracing::warn!("Failed to deserialize packs: {}, starting with empty", e);
                    Vec::new()
                })
        }
        None => Vec::new(),
    };

    SkillpackState {
        schema_version: schema_version.to_string(),
        packs,
    }
}

/// Persist state via atomic .tmp + rename. Caller is responsible for ensuring
/// the directory exists (zbrainPath returns paths under ~/.zbrain which
/// setup-zbrain ensures, but we mkdir defensively).
pub fn save_state(state: &SkillpackState, state_path: Option<&Path>) {
    let default = default_state_path();
    let path = state_path.unwrap_or(&default);

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let tmp = path.with_extension("tmp");
    let json = serde_json::to_string_pretty(state).unwrap_or_default() + "\n";

    if let Err(e) = fs::write(&tmp, json) {
        let _ = fs::remove_file(&tmp);
        panic!("failed to atomically write skillpack-state.json to {}: {}", path.to_string_lossy(), e);
    }

    let _ = fs::rename(&tmp, path);
}

/// Find a pack entry by name. Returns None if not installed.
pub fn find_entry<'a>(state: &'a SkillpackState, name: &str) -> Option<&'a SkillpackStateEntry> {
    state.packs.iter().find(|p| p.name == name)
}

/// Upsert a pack entry. Replaces any existing entry with the same name (e.g.
/// a re-scaffold at a newer version). Returns a new state value (immutable
/// update so tests can compare references).
pub fn upsert_entry(mut state: SkillpackState, entry: SkillpackStateEntry) -> SkillpackState {
    state.packs.retain(|p| p.name != entry.name);
    state.packs.push(entry);
    state
}

/// Remove a pack entry by name. Returns a new state value.
pub fn remove_entry(mut state: SkillpackState, name: &str) -> SkillpackState {
    state.packs.retain(|p| p.name != name);
    state
}

/// Identity check for the first-install-confirm prompt (codex G4).
/// Returns true when state already has an entry with the same name AND
/// same author AND same pinned-commit-or-tarball-SHA. The TOFU prompt
/// skips when this returns true.
pub fn is_already_trusted(
    state: &SkillpackState,
    candidate: &SkillpackStateEntry,
) -> bool {
    let Some(existing) = find_entry(state, &candidate.name) else {
        return false;
    };

    if existing.author != candidate.author {
        return false;
    }

    // Either pinned commit or tarball SHA must match (whichever is non-null).
    if let Some(pinned) = &candidate.pinned_commit {
        return existing.pinned_commit.as_ref() == Some(pinned);
    }

    if let Some(sha) = &candidate.tarball_sha256 {
        return existing.tarball_sha256.as_ref() == Some(sha);
    }

    // Local-path source — no identity to pin; treat as untrusted (always re-confirm).
    false
}
