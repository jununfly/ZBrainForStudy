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
