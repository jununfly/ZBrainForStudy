//! takes-quality-eval — faithful A2 port of TS `takes-quality-eval` (1-1-5-3 / #319).
//!
//! Six pieces, mirroring the TS module layout:
//! - [`rubric`]: the 5-dimension scoring rubric (single source of truth).
//! - [`receipt`]: the 4-sha receipt-name contract + DB-authoritative persistence.
//! - [`runner`]: drives the shared cross-modal judge panel and produces a receipt.
//! - [`replay`]: load a prior receipt without running models.
//! - [`regress`]: compare a fresh receipt against a prior one (CI gate).
//! - [`trend`]: DB-backed quality-over-time view.
//!
//! The runner deliberately reuses the cross-modal judge panel (3-model
//! parallel) so every eval family shares one verdict/aggregation semantics;
//! only the rubric (5 takes-quality dimensions) and the receipt shape differ.

pub mod rubric;
pub mod receipt;
pub mod runner;
pub mod replay;
pub mod regress;
pub mod trend;

pub use rubric::{
    default_dimensions, rubric_sha8, RUBRIC_VERSION, RUBRIC_DIMENSIONS, PASS_MEAN_THRESHOLD,
    PASS_FLOOR_THRESHOLD, MIN_SUCCESSES_FOR_VERDICT, render_judge_prompt,
};
pub use receipt::{
    build_receipt_filename, corpus_sha8, model_set_sha8, parse_receipt_filename, receipt_filename,
    write_receipt_artifact, write_takes_quality_run, ReceiptIdentity, TakesQualityReceipt,
    TakesQualityRunRow, DimensionRoll, CorpusMeta, ReceiptError, RECEIPT_SCHEMA_VERSION,
    verdict_to_string,
};
pub use runner::{run, TakesQualityRunOpts, TakesQualityRunOutput, render_takes};
pub use replay::{load_receipt_from_disk, load_receipt_from_db};
pub use regress::{compare_receipts, RegressionDelta, RegressOpts};
pub use trend::{load_trend, render_trend_table, TrendOpts, TrendRow};
