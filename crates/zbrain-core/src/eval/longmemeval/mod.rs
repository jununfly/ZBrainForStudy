//! LongMemEval benchmark harness.
//!
//! Port of the TS `src/commands/eval-longmemeval.ts` command plus its
//! `src/eval/longmemeval/` support package (adapter / sanitize / intent /
//! extract). Runs the public LongMemEval benchmark against ZBrain's hybrid
//! retrieval: per question it spins up an isolated in-memory brain, imports
//! that question's haystack, searches, optionally generates an answer through
//! a [`crate::ai::chat::ChatProvider`], and emits hypothesis JSONL for the
//! downstream `evaluate_qa.py` scorer.
//!
//! Hermetic by design: the benchmark brain is a fresh [`crate::engine::InMemoryEngine`]
//! per question, so the user's real brain is never opened or mutated.
//!
//! Dataset: <https://huggingface.co/datasets/xiaowu0162/longmemeval>

pub mod adapter;
pub mod extract;
pub mod intent;
pub mod runner;
pub mod sanitize;
pub mod summary;

pub use adapter::{
    haystack_to_pages, session_id_from_slug, LongMemEvalQuestion, LongMemEvalSession,
    LongMemEvalTurn, PageInputForImport,
};
pub use extract::{
    extract_and_insert_claims, AliasMap, CacheStats, ExtractOpts, ExtractResult, ExtractedClaim,
    ExtractorCache,
};
pub use intent::classify_intent;
pub use sanitize::{render_chat_block, sanitize_chat_content, ChatSessionForPrompt, RenderResult};
pub use summary::{
    build_by_type_summary, emit_by_type_summary, load_resume_set, seed_recall_by_type_from_file,
    AggregateRecall, ByTypeSummary, JsonlEmitter, RecallBucket, RecallByType, RecallByTypeEntry,
};

/// Where the dataset comes from — surfaced in `--help` and in the
/// "dataset not found" error so an operator can self-serve.
pub const HUGGINGFACE_URL: &str = "https://huggingface.co/datasets/xiaowu0162/longmemeval";
