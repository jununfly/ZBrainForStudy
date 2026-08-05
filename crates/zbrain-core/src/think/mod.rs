//! Pure-logic modules for the `zbrain think` subsystem.
//!
//! Ported from `src/core/think/*.ts` (intent / sanitize / entity-extract /
//! cite-render / prompt / gather.fuseRanked). These modules are pure: no
//! engine access, no DB, no async, no LLM. They are the leaf building blocks
//! the think orchestration (later sub-nodes 1-9-3 / 1-9-4 / 1-9-5) wires
//! together.
//!
//! Pure-logic port map (node 1-9-1):
//!   * [`intent`]      ← `intent.ts`          (trajectory-routing intent classifier)
//!   * [`sanitize`]    ← `sanitize.ts`        (prompt-injection defense + `<take>` render)
//!   * [`entity`]      ← `entity-extract.ts`  (candidate-entity extraction)
//!   * [`cite_render`] ← `cite-render.ts`     (structured/inline citation resolution)
//!   * [`prompt`]      ← `prompt.ts`          (system + user message builders)
//!   * [`fusion`]      ← `gather.ts` fuseRanked (generic RRF over two ranked lists)
//!
//! NOTE: this module's RRF ([`fusion::fuse_ranked`]) is intentionally generic
//! (operates on any `T` via a key closure) and distinct from
//! `crate::search::fusion::rrf_fusion`, which fuses `FusionRow`s. The think
//! gather fuses heterogeneous take/page lists by `(slug, row)` keys, so a
//! generic helper is the faithful port.

pub mod cite_render;
pub mod entity;
pub mod fusion;
pub mod intent;
pub mod prompt;
pub mod sanitize;

pub use cite_render::*;
pub use entity::*;
pub use fusion::*;
pub use intent::*;
pub use prompt::*;
pub use sanitize::*;
