//! Pure-logic + retrieval-fusion modules for the `zbrain think` subsystem.
//!
//! Ported from `src/core/think/*.ts`. The leaf pure-logic modules (intent /
//! sanitize / entity-extract / cite-render / prompt / gather.fuseRanked) are
//! engine-free; [`gather`] adds the 4-stream retrieval fusion that wires
//! against the live `BrainEngine` (node 1-9-3).
//!
//! Port map:
//!   * [`intent`]      ← `intent.ts`          (trajectory-routing intent classifier)
//!   * [`sanitize`]    ← `sanitize.ts`        (prompt-injection defense + `<take>` render)
//!   * [`entity`]      ← `entity-extract.ts`  (candidate-entity extraction)
//!   * [`cite_render`] ← `cite-render.ts`     (structured/inline citation resolution)
//!   * [`prompt`]      ← `prompt.ts`          (system + user message builders)
//!   * [`fusion`]      ← `gather.ts` fuseRanked (generic RRF over two ranked lists)
//!   * [`gather`]      ← `gather.ts` runGather + renderPagesBlock + takesHitToTakeForPrompt
//!                      (4-stream retrieval fusion; stream 3 vector-takes blocked → G71)
//!
//! NOTE: [`fusion::fuse_ranked`] is intentionally generic (operates on any `T`
//! via a key closure) and distinct from `crate::search::fusion::rrf_fusion`,
//! which fuses `FusionRow`s. The think gather fuses heterogeneous take/page
//! lists by `(slug, row)` keys, so a generic helper is the faithful port.

pub mod cite_render;
pub mod entity;
pub mod fusion;
pub mod gather;
pub mod intent;
pub mod prompt;
pub mod sanitize;

pub use cite_render::*;
pub use entity::*;
pub use fusion::*;
pub use gather::*;
pub use intent::*;
pub use prompt::*;
pub use sanitize::*;
