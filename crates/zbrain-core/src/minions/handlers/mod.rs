//! Minion job handlers — concrete [`MinionHandler`] implementations for each
//! job type (subagent, embed-backfill, sync, lint, etc.).
//!
//! ## TS reference
//!
//! - Handler implementations live under `src/core/minions/handlers/` in TS.
//! - Each handler maps to a job `name` string in the `minion_jobs` table.
//!
//! ## Structure
//!
//! - [`registry`] — [`MinionHandlerRegistry`] + [`register_builtin_handlers`]
//! - [`subagent`] — [`SubagentHandler`] (gateway path, v1)

pub mod registry;
pub mod subagent;

// ── 1-4-2 low-complexity handlers ──────────────────────────────────────────
pub mod backlinks;
pub mod embed;
pub mod extract;
pub mod import;
pub mod integrity;
pub mod integrity_auto;
pub mod lint;
pub mod lint_fix;
pub mod orphans;
pub mod purge;
pub mod reindex;
pub mod repair_jsonb;
pub mod subagent_aggregator;
pub mod sync;
pub mod sync_retry_failed;
