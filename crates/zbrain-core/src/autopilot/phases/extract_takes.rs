//! Part12 1-1-3 — port of `src/core/cycle/extract-takes.ts` →
//! `autopilot/phases/extract_takes.rs`.
//!
//! This phase parses `## Takes` fenced blocks out of each page's
//! `compiled_truth` + `timeline` and upserts them into the `takes` table.
//! It is a **fence-parsing + DB reconciliation** phase — it does NOT call an
//! LLM.
//!
//! ## Port scope / divergence from TS
//!
//! The TS module exposes two source paths:
//! - `fs`: walk markdown files on disk (`walkMarkdownFiles` + `readFileSync`).
//! - `db`: iterate engine pages and re-extract from `compiled_truth` +
//!   `timeline`.
//!
//! The cycle runs against an engine, so this Rust port implements the **db
//! path only**. The `fs` path needs filesystem access to a repo checkout and
//! is out of scope for the cycle; `zbrain extract takes` (command) keeps
//! using the TS/FS path until a separate command-port lands.
//!
//! The TS `--rebuild` flag deletes existing takes via raw SQL
//! (`DELETE FROM takes WHERE page_id = $1`). The `BrainEngine` trait
//! deliberately omits an `executeRaw` escape hatch (see `takes_fence.rs`
//! `check_takes_weight_grid` note), so `--rebuild` is NOT ported. The default
//! incremental upsert (`add_takes_batch`, append-only) is the cycle behavior
//! and matches `extract_takes` without `--rebuild`.
//!
//! ### Cycle-phase taxonomy note
//!
//! TS `ALL_PHASES` (20 entries) does **not** include `extract-takes` — in TS
//! it is a `v0_28_0` orchestrator consumer (`zbrain extract takes`), not a
//! `runCycle` phase. The Rust port elevates it to a dedicated `CyclePhase`
//! for parity with `ExtractFacts`/`ExtractAtoms` (which ARE cycle phases) and
//! to expose it via `run_cycle`. This is a deliberate taxonomy divergence;
//! see the part12 roadmap notes.

use crate::engine::BrainEngine;
use crate::error::Result as ZbResult;
use crate::GetPageOpts;
use crate::takes_fence::parse_takes_fence;
use crate::takes_fence::FenceTake;
use crate::types::TakeInput;

const BATCH_SIZE: usize = 100;

/// Options for [`run_extract_takes`]. Mirrors TS `ExtractTakesOpts` (db path).
#[derive(Debug, Clone, Default)]
pub struct ExtractTakesOpts {
    /// Subset of slugs to re-extract. `None` = walk every page across all
    /// sources.
    pub slugs: Option<Vec<String>>,
    /// Dry-run: parse + count, no DB writes.
    pub dry_run: bool,
    /// Source scope for bare slugs. When `slugs` is set, each bare slug is
    /// resolved against this source (default `"default"`). When `None`, every
    /// `(slug, source_id)` pair across all sources is enumerated.
    pub source_id: Option<String>,
}

/// A single failed extraction work-item (surfaced for `recordSyncFailures`).
#[derive(Debug, Clone)]
pub struct ExtractedTakeFailure {
    /// On DB-source extraction the stable identifier is the slug (no on-disk
    /// file path to point at).
    pub path: String,
    pub error: String,
}

/// Result envelope for [`run_extract_takes`]. Mirrors TS `ExtractTakesResult`.
#[derive(Debug, Clone, Default)]
pub struct ExtractTakesResult {
    pub pages_scanned: u64,
    pub pages_with_takes: u64,
    pub takes_upserted: u64,
    pub warnings: Vec<String>,
    pub failed_files: Vec<ExtractedTakeFailure>,
}

/// Map a parsed fence take into an engine-ready [`TakeInput`] for `page_id`.
fn fence_take_to_input(page_id: u64, t: &FenceTake) -> TakeInput {
    TakeInput {
        page_id,
        row_num: Some(t.row_num),
        claim: t.claim.clone(),
        kind: t.kind.clone(),
        holder: t.holder.clone(),
        weight: t.weight,
        since_date: t.since_date.clone(),
        until_date: t.until_date.clone(),
        source: t.source.clone(),
        superseded_by: None,
        active: Some(t.active),
    }
}

/// Flush a per-page buffer of takes. The Rust `add_takes_batch` takes a single
/// `page_id`, so the buffer must contain takes for exactly one page (unlike
/// the TS version, which carries `page_id` inside each `TakeBatchInput` and
/// can batch across pages). All buffered takes share the same `page_id`.
async fn flush_batch(
    engine: &dyn BrainEngine,
    buffer: &mut Vec<TakeInput>,
    result: &mut ExtractTakesResult,
    dry_run: bool,
) -> ZbResult<()> {
    if buffer.is_empty() {
        return Ok(());
    }
    if dry_run {
        result.takes_upserted += buffer.len() as u64;
    } else {
        let page_id = buffer[0].page_id;
        let inserted = engine.add_takes_batch(page_id, buffer).await?;
        result.takes_upserted += inserted.upserted as u64;
    }
    buffer.clear();
    Ok(())
}

/// Run the `extract_takes` cycle phase against the current brain state.
///
/// Enumerates `(slug, source_id)` pairs via [`BrainEngine::list_all_page_refs`]
/// (or the `opts.slugs` subset), parses the `## Takes` fence from each page's
/// `compiled_truth` + `timeline`, and upserts the takes via
/// [`BrainEngine::add_takes_batch`] (append-only, incremental).
pub async fn run_extract_takes(
    engine: &dyn BrainEngine,
    opts: &ExtractTakesOpts,
) -> ZbResult<ExtractTakesResult> {
    let mut result = ExtractTakesResult::default();

    // Resolve target (slug, source_id) pairs.
    let refs: Vec<(String, String)> = match &opts.slugs {
        Some(slugs) => {
            let source_id = opts.source_id.clone().unwrap_or_else(|| "default".to_string());
            slugs.iter().map(|s| (s.clone(), source_id.clone())).collect()
        }
        None => engine
            .list_all_page_refs()
            .await?
            .into_iter()
            .map(|r| (r.slug, r.source_id))
            .collect(),
    };

    let dry_run = opts.dry_run;

    for (slug, source_id) in refs {
        result.pages_scanned += 1;

        let page = match engine
            .get_page(
                &slug,
                &GetPageOpts {
                    source_id: Some(source_id.clone()),
                    include_deleted: false,
                },
            )
            .await?
        {
            Some(p) => p,
            None => continue,
        };

        let body = format!("{}\n{}", page.compiled_truth, page.timeline);
        let parsed = parse_takes_fence(&body);
        for w in &parsed.warnings {
            result.warnings.push(format!("{slug}: {w}"));
            if w.starts_with("TAKES_HOLDER_INVALID") {
                result.failed_files.push(ExtractedTakeFailure {
                    path: slug.clone(),
                    error: w.clone(),
                });
            }
        }
        if parsed.takes.is_empty() {
            continue;
        }

        result.pages_with_takes += 1;

        // Per-page buffer: all takes belong to `page.id`.
        let mut buffer: Vec<TakeInput> = Vec::new();
        for t in &parsed.takes {
            buffer.push(fence_take_to_input(page.id, t));
            if buffer.len() >= BATCH_SIZE {
                flush_batch(engine, &mut buffer, &mut result, dry_run).await?;
            }
        }
        flush_batch(engine, &mut buffer, &mut result, dry_run).await?;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};
    use crate::types::{PageType, TakesListOpts};

    fn fence_body() -> String {
        "some text\n\n## Takes\n\n<!--- zbrain:takes:begin -->\n\
         | # | claim | kind | who | weight | since | source |\n\
         |---|---|-------|------|-----|--------|-------|--------|\n\
         | 1 | CEO of Acme | fact | world | 1.0 | 2017-01 | Crustdata |\n\
         | 2 | Strong technical founder | take | people/garry | 0.85 | 2026-04-29 | OH |\n\
         <!--- zbrain:takes:end -->"
            .to_string()
    }

    #[tokio::test]
    async fn extract_takes_from_db_upserts() {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        engine
            .put_page(
                "company/acme",
                Some("default"),
                &PageInput {
                    page_type: PageType::from("note"),
                    title: "Acme".into(),
                    compiled_truth: fence_body(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let result = run_extract_takes(&engine, &ExtractTakesOpts::default())
            .await
            .unwrap();

        assert_eq!(result.pages_scanned, 1);
        assert_eq!(result.pages_with_takes, 1);
        assert_eq!(result.takes_upserted, 2);
        assert!(result.warnings.is_empty());

        let takes = engine.list_takes(&TakesListOpts::default()).await.unwrap();
        assert_eq!(takes.len(), 2);
    }

    #[tokio::test]
    async fn extract_takes_empty_brain() {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        let result = run_extract_takes(&engine, &ExtractTakesOpts::default())
            .await
            .unwrap();
        assert_eq!(result.pages_scanned, 0);
        assert_eq!(result.pages_with_takes, 0);
        assert_eq!(result.takes_upserted, 0);
    }
}
