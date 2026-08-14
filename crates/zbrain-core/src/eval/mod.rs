//! Evaluation harnesses for ZBrain.
//!
//! Most modules here are pure, side-effect-free aggregators — the TS-era
//! command modules under `src/commands/eval-*.ts` were largely stubs, and only
//! the real aggregators were ported. The exceptions are the two live-LLM
//! benchmark harnesses, [`cross_modal`] and [`longmemeval`], which drive a
//! [`crate::ai::chat::ChatProvider`] and (for longmemeval) an isolated
//! in-memory brain.

pub mod schema_authoring;
pub mod gate;
pub mod replay;
pub mod whoknows;
pub mod run_all;
pub mod compare;
pub mod code_retrieval;
pub mod cross_modal;
pub mod longmemeval;
pub mod takes_quality;
pub mod contradictions;
pub mod brainstorm;
