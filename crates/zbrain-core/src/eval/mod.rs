//! Evaluation harnesses for ZBrain.
//!
//! These are pure, side-effect-free evaluators (no DB, no LLM). The
//! TS-era command modules under `src/commands/eval-*.ts` were largely
//! stubs; only the pure aggregators were real and are ported here.

pub mod schema_authoring;
pub mod gate;
pub mod replay;
pub mod whoknows;
pub mod run_all;
pub mod compare;
pub mod code_retrieval;
pub mod cross_modal;
