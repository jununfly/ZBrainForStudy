//! Extract handler — vault filesystem extraction, wired to `extract_fs`.
//!
//! Faithful Rust port of the TS `jobs.ts` `extract` minion job
//! (see `docs/plans/zbrain-g74-g76-reimpl.json` node 1-2-4):
//!
//! ```js
//! worker.register('extract', async (job) => {
//!   const { runExtractCore } = await import('./extract.ts');
//!   const mode = (['links','timeline','all'].includes(job.data.mode))
//!     ? job.data.mode : 'all';
//!   const dir = job.data.dir ?? (await engine.getConfig('sync.repo_path')) ?? '.';
//!   return await runExtractCore(engine, { mode, dir, dryRun: !!job.data.dryRun });
//! });
//! ```
//!
//! The TS `runExtractCore` with a `dir` argument is the filesystem-extraction
//! path — the same `--source fs` core the CLI `extract` verb uses
//! (`crates/zbrain-core/src/extract_fs.rs`). So this handler maps the job
//! payload onto `extract_links_from_dir` / `extract_timeline_from_dir`:
//!
//! - `mode` ∈ {links, timeline, all} (default `all`)
//! - `dir`  optional vault directory; TS defaulted to `sync.repo_path`, but the
//!   Rust engine has no such getter, so we fall back to `.` (cwd). Enqueuers
//!   (autopilot / agents) are expected to pass `dir` explicitly.
//! - `dry_run` boolean — when set, we report the file count that *would* be
//!   scanned without writing any pages/links/timeline rows.
//!
//! Scope is a directory (vault), bounded and deterministic — no LLM. Distinct
//! from `extract-conversation-facts` (LLM, run via `run_cycle`). Closes KNOWN-
//! GAPS G76 minion-job wiring for the links/timeline family.

use std::path::Path;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::engine::BrainEngine;
use crate::extract_fs::{extract_links_from_dir, extract_timeline_from_dir, walk_markdown_files};
use crate::minions::handler::{MinionHandler, MinionJobContext};
use crate::Result;

pub struct ExtractHandler;

#[async_trait]
impl MinionHandler for ExtractHandler {
    async fn handle(&self, ctx: &MinionJobContext) -> Result<Value> {
        let data = &ctx.data;
        let mode = data
            .get("mode")
            .and_then(|v| v.as_str())
            .filter(|m| matches!(*m, "links" | "timeline" | "all"))
            .unwrap_or("all");
        let dir = data
            .get("dir")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| ".".to_string());
        let dry_run = data.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false);

        // `ctx.engine()` is `&Arc<dyn BrainEngine>`; `Arc::as_ref` yields the
        // `&dyn BrainEngine` the extract_fs core ops expect (mirrors the
        // extract-conversation-facts handler).
        let engine: &dyn BrainEngine = ctx.engine().as_ref();

        let mut out = serde_json::Map::new();
        out.insert("mode".into(), json!(mode));
        out.insert("dir".into(), json!(dir));
        out.insert("dry_run".into(), json!(dry_run));

        if dry_run {
            // Contract parity with TS `dryRun`: report what would be scanned
            // without mutating the graph.
            let files = walk_markdown_files(Path::new(&dir));
            out.insert("files_would_scan".into(), json!(files.len()));
            return Ok(Value::Object(out));
        }

        if mode == "links" || mode == "all" {
            let r = extract_links_from_dir(engine, Path::new(&dir)).await?;
            out.insert(
                "links".into(),
                json!({
                    "pages_processed": r.pages_processed,
                    "links_created": r.links_created,
                    "dangling": r.dangling,
                }),
            );
        }
        if mode == "timeline" || mode == "all" {
            let r = extract_timeline_from_dir(engine, Path::new(&dir)).await?;
            out.insert(
                "timeline".into(),
                json!({
                    "pages_processed": r.pages_processed,
                    "entries_added": r.entries_added,
                }),
            );
        }

        Ok(Value::Object(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::BrainEngine;
    use crate::InMemoryEngine;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn engine() -> Arc<dyn BrainEngine> {
        Arc::new(InMemoryEngine::new())
    }

    fn ctx_for(engine: Arc<dyn BrainEngine>, data: Value) -> MinionJobContext {
        MinionJobContext::new(
            engine,
            1,
            "extract".into(),
            data,
            0,
            "tok".into(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn extract_runs_fs_dir() {
        let dir = std::env::temp_dir().join(format!("zb_extract_job_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("note-a.md"), "# A\n\nSee [[note-b]].\n").ok();
        std::fs::write(dir.join("note-b.md"), "# B\n\n2024-01-01 did a thing.\n").ok();

        let eng = engine();
        let ctx = ctx_for(Arc::clone(&eng), serde_json::json!({ "mode": "all", "dir": dir.to_string_lossy() }));
        let result = ExtractHandler.handle(&ctx).await.expect("extract job should run");

        assert_eq!(result.get("mode").and_then(|v| v.as_str()), Some("all"));
        let links = result.get("links").expect("links result present");
        assert_eq!(links.get("links_created").and_then(|v| v.as_u64()), Some(1));
        assert!(result.get("timeline").is_some(), "timeline result present");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn extract_defaults_mode_all_and_runs() {
        let dir = std::env::temp_dir().join(format!("zb_extract_job2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();

        let eng = engine();
        let ctx = ctx_for(Arc::clone(&eng), serde_json::json!({ "dir": dir.to_string_lossy() }));
        let result = ExtractHandler.handle(&ctx).await.expect("extract job runs with default mode");
        assert_eq!(result.get("mode").and_then(|v| v.as_str()), Some("all"));
        assert!(result.get("links").is_some());
        assert!(result.get("timeline").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn extract_dry_run_scans_without_writing() {
        let dir = std::env::temp_dir().join(format!("zb_extract_job3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("x.md"), "# X\n").ok();

        let eng = engine();
        let ctx = ctx_for(Arc::clone(&eng), serde_json::json!({ "dir": dir.to_string_lossy(), "dry_run": true }));
        let result = ExtractHandler.handle(&ctx).await.expect("extract dry_run runs");
        assert_eq!(result.get("dry_run").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(result.get("files_would_scan").and_then(|v| v.as_u64()), Some(1));
        // No graph writes: links/timeline keys must be absent in dry-run.
        assert!(result.get("links").is_none());
        assert!(result.get("timeline").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
