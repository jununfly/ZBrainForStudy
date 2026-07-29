//! Patterns phase (v0.23) — cross-session theme detection.
//!
//! Port of TS `src/core/cycle/patterns.ts`. Reads recent reflections (within
//! `lookback_days`), enqueues a single Sonnet subagent to surface themes that
//! recur across ≥`min_evidence` distinct reflections, and (in TS) writes one
//! pattern page per theme.
//!
//! ## Rust deviations (documented so the port stays honest)
//!
//! - **Config**: TS reads `dream.patterns.*` from the engine config store via
//!   `engine.getConfig`. Rust has **no `get_config` / config table** yet, so
//!   `load_patterns_config` returns the TS *defaults* (enabled=true,
//!   lookback_days=30, min_evidence=3, model="sonnet"). Wire real config
//!   lookup when the engine config store lands.
//! - **Model resolution**: TS uses `resolveModel(..., tier:"reasoning",
//!   fallback:"sonnet")`. Rust's `resolve_model` needs a `ConfigLookup`
//!   snapshot we don't have in a cycle phase, so we pass the literal
//!   `"sonnet"` (the TS fallback for that tier).
//! - **Subagent execution**: the phase enqueues a `"subagent"` minion job and
//!   polls it to completion via [`wait_for_completion`]. The subagent itself
//!   runs in the minion worker (wired with a chat provider), so the cycle
//!   phase does **not** need a chat provider — matching the TS design where
//!   the phase enqueues rather than calling the LLM directly.
//! - **Disk reverse-write**: TS reverse-writes the subagent's pattern pages
//!   from the DB back to `wiki/personal/patterns/*.md`. Rust is DB-canonical
//!   (no on-disk markdown dual-write in this path), so `reverse_write_refs`
//!   is omitted; `patterns_written` is best-effort (harvested from subagent
//!   `brain_put_page` tool-exec rows, which aren't mirrored in Rust → 0).

use chrono::Utc;
use serde_json::{json, Value};

use crate::engine::{BrainEngine, PageFilters, PageSort};
use crate::minions::queue::MinionQueue;
use crate::minions::types::{MinionJob, MinionJobInput};
use crate::minions::wait_for_completion::{wait_for_completion, WaitError, WaitOpts};

/// Options for [`run_phase_patterns`]. Mirrors TS `PatternsPhaseOpts`.
pub struct PatternsPhaseOpts {
    /// Brain directory for disk reverse-write (unused in Rust — DB-canonical).
    pub brain_dir: Option<String>,
    /// If true, detect patterns but write nothing.
    pub dry_run: bool,
    /// Override the wait timeout (ms). Default 35min. Test seam.
    pub wait_timeout_ms: Option<u64>,
}

/// Result of a `patterns` run. Mirrors the TS `PhaseResult` summary/details.
#[derive(Debug, Clone, Default)]
pub struct PatternsPhaseResult {
    /// `"ok"`, `"warn"` or `"skipped"`.
    pub status: String,
    pub summary: String,
    pub reason: Option<String>,
    pub reflections_considered: u64,
    pub patterns_written: u64,
    pub reverse_write_count: u64,
    pub child_outcome: Option<String>,
    pub job_id: Option<i64>,
    pub dry_run: bool,
}

impl PatternsPhaseResult {
    fn skipped(reason: &str, summary: &str) -> Self {
        Self {
            status: "skipped".into(),
            summary: summary.into(),
            reason: Some(reason.into()),
            ..Default::default()
        }
    }
}

struct PatternsConfig {
    enabled: bool,
    lookback_days: u32,
    min_evidence: u32,
    model: String,
}

struct ReflectionRef {
    slug: String,
    title: String,
    excerpt: String,
}

/// Load patterns config.
///
/// Rust has no engine config store yet (see module docs) — returns TS
/// defaults. This is the documented seam to wire real `getConfig` later.
async fn load_patterns_config(_engine: &dyn BrainEngine) -> PatternsConfig {
    PatternsConfig {
        enabled: true,
        lookback_days: 30,
        min_evidence: 3,
        model: "sonnet".to_string(),
    }
}

/// Gather recent reflections within the lookback window.
async fn gather_reflections(
    engine: &dyn BrainEngine,
    lookback_days: u32,
) -> crate::Result<Vec<ReflectionRef>> {
    let since = Utc::now() - chrono::Duration::days(lookback_days as i64);
    let since_iso = since.to_rfc3339();
    let pages = engine
        .list_pages(&PageFilters {
            slug_prefix: Some("wiki/personal/reflections/".to_string()),
            updated_after: Some(since_iso),
            limit: Some(100),
            sort: Some(PageSort::UpdatedDesc),
            ..Default::default()
        })
        .await?;
    Ok(pages
        .into_iter()
        .map(|p| ReflectionRef {
            slug: p.slug.clone(),
            title: if p.title.is_empty() {
                p.slug.clone()
            } else {
                p.title.clone()
            },
            excerpt: p
                .compiled_truth
                .chars()
                .take(600)
                .collect(),
        })
        .collect())
}

/// Build the subagent prompt. Port of TS `buildPatternsPrompt`.
fn build_patterns_prompt(reflections: &[ReflectionRef], min_evidence: u32) -> String {
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let corpus = reflections
        .iter()
        .enumerate()
        .map(|(i, r)| format!("### {}. [[{}]] — {}\n{}", i + 1, r.slug, r.title, r.excerpt))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    format!(
        "You are surfacing recurring themes across the user's recent reflections.\n\n\
         OUTPUT POLICY\n\
         - Only name a pattern if it appears in at least {min_evidence} DISTINCT reflections.\n\
         - Each pattern page MUST cite the reflections that constitute its evidence (use [[wiki/personal/reflections/...]] wikilinks).\n\
         - Use `search` to check whether a similar pattern page already exists; if yes, update it (use the same slug). If no, create a new one.\n\
         - Pattern slug format: `wiki/personal/patterns/<topic-slug>` (lowercase alphanumeric + hyphens; no underscores, no extension, no date).\n\
         - A \"pattern\" is a recurring theme, anxiety, decision pattern, relationship dynamic, or self-knowledge motif. NOT a single insight. NOT a list of unrelated topics.\n\n\
         DO NOT WRITE\n\
         - A \"patterns from today\" digest (that's the dream-cycle-summaries page; not your job).\n\
         - Patterns with <{min_evidence} reflections cited.\n\
         - Anything outside wiki/personal/patterns/.\n\n\
         CONTEXT\n\
         - Today: {today}\n\
         - Reflections in scope: {n}\n\n\
         REFLECTIONS\n\
         {corpus}\n\n\
         When done, briefly list the pattern slugs you wrote/updated in your final message.",
        min_evidence = min_evidence,
        today = today,
        n = reflections.len(),
        corpus = corpus,
    )
}

/// Enqueue the patterns-detection subagent job. Returns `None` when the phase
/// should be skipped (disabled or insufficient evidence) and `Some(job)` once
/// enqueued. Shared by the cycle phase and the standalone `patterns` minion
/// handler.
pub async fn enqueue_patterns_subagent(
    engine: &dyn BrainEngine,
) -> crate::Result<Option<MinionJob>> {
    let config = load_patterns_config(engine).await;
    if !config.enabled {
        return Ok(None);
    }
    let reflections = gather_reflections(engine, config.lookback_days).await?;
    if reflections.len() < config.min_evidence as usize {
        return Ok(None);
    }
    let prompt = build_patterns_prompt(&reflections, config.min_evidence);
    let queue = MinionQueue::new(engine);
    let input = MinionJobInput {
        name: "subagent".to_string(),
        data: Some(json!({
            "prompt": prompt,
            "model": config.model,
            "max_turns": 30,
            "allowed_slug_prefixes": ["wiki/personal/patterns/"],
        })),
        max_stalled: Some(3),
        timeout_ms: Some(30 * 60 * 1000),
        ..Default::default()
    };
    let job = queue.add(&input).await?;
    Ok(Some(job))
}

/// Run the patterns phase. Mirrors TS `runPhasePatterns`.
pub async fn run_phase_patterns(
    engine: &dyn BrainEngine,
    opts: &PatternsPhaseOpts,
) -> crate::Result<PatternsPhaseResult> {
    let config = load_patterns_config(engine).await;
    if !config.enabled {
        return Ok(PatternsPhaseResult::skipped(
            "disabled",
            "dream.patterns.enabled is false",
        ));
    }
    let reflections = gather_reflections(engine, config.lookback_days).await?;
    if reflections.len() < config.min_evidence as usize {
        return Ok(PatternsPhaseResult::skipped(
            "insufficient_evidence",
            &format!(
                "{} reflections in last {}d (need ≥{})",
                reflections.len(),
                config.lookback_days,
                config.min_evidence
            ),
        ));
    }
    if opts.dry_run {
        return Ok(PatternsPhaseResult {
            status: "ok".into(),
            summary: format!(
                "dry-run: would detect patterns over {} reflections",
                reflections.len()
            ),
            reflections_considered: reflections.len() as u64,
            dry_run: true,
            ..Default::default()
        });
    }

    let job = match enqueue_patterns_subagent(engine).await? {
        None => {
            return Ok(PatternsPhaseResult::skipped(
                "insufficient_evidence",
                &format!(
                    "{} reflections in last {}d (need ≥{})",
                    reflections.len(),
                    config.lookback_days,
                    config.min_evidence
                ),
            ));
        }
        Some(job) => job,
    };

    let timeout_ms = opts.wait_timeout_ms.unwrap_or(35 * 60 * 1000);
    let outcome = match wait_for_completion(
        &MinionQueue::new(engine),
        job.id,
        WaitOpts {
            timeout_ms: Some(timeout_ms),
            poll_ms: Some(5_000),
        },
    )
    .await
    {
        Ok(final_job) => final_job.status.as_str().to_string(),
        Err(WaitError::Timeout { .. }) => "timeout".to_string(),
        Err(e) => {
            return Ok(PatternsPhaseResult {
                status: "warn".into(),
                summary: format!("patterns: waiting for subagent job {} failed: {e}", job.id),
                reason: Some("wait_error".into()),
                reflections_considered: reflections.len() as u64,
                job_id: Some(job.id),
                ..Default::default()
            });
        }
    };

    // Harvest the slugs the subagent wrote. Fail-soft: the
    // `subagent_tool_executions` table (TS) is not mirrored in Rust, so this
    // is best-effort and returns empty on Unsupported / missing table. The
    // subagent writes pattern pages to the engine DB directly (canonical), so
    // `patterns_written` is best-effort 0 in Rust.
    let written_refs = collect_child_put_page_slugs(engine, job.id).await;

    Ok(PatternsPhaseResult {
        status: "ok".into(),
        summary: format!(
            "{} pattern candidate(s) from subagent job {} ({})",
            written_refs.len(),
            job.id,
            outcome
        ),
        reflections_considered: reflections.len() as u64,
        patterns_written: written_refs.len() as u64,
        child_outcome: Some(outcome),
        job_id: Some(job.id),
        ..Default::default()
    })
}

/// Harvest slugs the subagent wrote via `brain_put_page`. Fail-soft (see
/// module docs): returns an empty list when the engine lacks `execute_raw` or
/// the `subagent_tool_executions` table.
async fn collect_child_put_page_slugs(engine: &dyn BrainEngine, job_id: i64) -> Vec<String> {
    let sql = "SELECT DISTINCT COALESCE(input->>'slug', input->>'slug') AS slug \
               FROM subagent_tool_executions \
               WHERE job_id = ?1 AND tool_name = 'brain_put_page' AND status = 'complete' \
               ORDER BY 1";
    match engine.execute_raw(sql, &[&job_id]).await {
        Ok(rows) => rows
            .into_iter()
            .filter_map(|r| match r {
                Value::Object(map) => map
                    .get("slug")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BrainEngine, EngineConfig, InMemoryEngine, PageInput};
    use crate::minions::queue::MinionQueue;
    use crate::minions::types::MinionJobStatus;

    async fn setup() -> InMemoryEngine {
        let engine = InMemoryEngine::new();
        engine.connect(&EngineConfig::default()).await.unwrap();
        engine
    }

    async fn put_reflection(engine: &InMemoryEngine, slug: &str) {
        engine
            .put_page(
                slug,
                Some("default"),
                &PageInput {
                    page_type: "note".to_string(),
                    title: format!("Reflection {slug}"),
                    compiled_truth: format!("content of {slug}"),
                    timeline: None,
                    frontmatter: None,
                    content_hash: None,
                    page_kind: None,
                    effective_date: None,
                    effective_date_source: None,
                    import_filename: None,
                    chunker_version: None,
                    source_path: None,
                    source_kind: None,
                    source_uri: None,
                    ingested_via: None,
                    ingested_at: None,
                    last_retrieved_at: None,
                    embedding: None,
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn insufficient_evidence_skips() {
        let engine = setup().await;
        // No reflections → below min_evidence (3) → skipped.
        let r = run_phase_patterns(
            &engine,
            &PatternsPhaseOpts {
                brain_dir: None,
                dry_run: false,
                wait_timeout_ms: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "skipped");
        assert_eq!(r.reason.as_deref(), Some("insufficient_evidence"));
    }

    #[tokio::test]
    async fn dry_run_reports_with_enough_reflections() {
        let engine = setup().await;
        for i in 0..3 {
            put_reflection(&engine, &format!("wiki/personal/reflections/r{i}")).await;
        }
        let r = run_phase_patterns(
            &engine,
            &PatternsPhaseOpts {
                brain_dir: None,
                dry_run: true,
                wait_timeout_ms: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert!(r.dry_run);
        assert_eq!(r.reflections_considered, 3);
        assert_eq!(r.patterns_written, 0);
    }

    #[tokio::test]
    async fn inmemory_no_worker_times_out() {
        // InMemory implements both enqueue_job and get_job, but there is no
        // worker to run the subagent. So run_phase_patterns enqueues a Waiting
        // job, then wait_for_completion polls until its (short) timeout and
        // reports `ok` with `child_outcome = "timeout"` — not a hard fail.
        let engine = setup().await;
        for i in 0..3 {
            put_reflection(&engine, &format!("wiki/personal/reflections/r{i}")).await;
        }
        let r = run_phase_patterns(
            &engine,
            &PatternsPhaseOpts {
                brain_dir: None,
                dry_run: false,
                wait_timeout_ms: Some(200),
            },
        )
        .await
        .unwrap();
        assert_eq!(r.status, "ok");
        assert_eq!(r.child_outcome.as_deref(), Some("timeout"));
        assert!(r.job_id.is_some(), "enqueued job id should be reported");
        assert_eq!(r.reflections_considered, 3);
    }

    #[tokio::test]
    async fn enqueue_subagent_job_when_evidence_present_libsql() {
        // On libsql the subagent job is actually enqueued (no worker, so it
        // stays Waiting — we just assert it was created with the right shape).
        let _g = libsql_guard();
        let (_temp, engine) = libsql_engine().await;
        for i in 0..3 {
            engine
                .put_page(
                    &format!("wiki/personal/reflections/r{i}"),
                    Some("default"),
                    &PageInput {
                        page_type: "note".to_string(),
                        title: format!("Reflection r{i}"),
                        compiled_truth: format!("content of r{i}"),
                        timeline: None,
                        frontmatter: None,
                        content_hash: None,
                        page_kind: None,
                        effective_date: None,
                        effective_date_source: None,
                        import_filename: None,
                        chunker_version: None,
                        source_path: None,
                        source_kind: None,
                        source_uri: None,
                        ingested_via: None,
                        ingested_at: None,
                        last_retrieved_at: None,
                        embedding: None,
                    },
                )
                .await
                .unwrap();
        }
        let job = enqueue_patterns_subagent(&engine).await.unwrap().expect("should enqueue");
        assert_eq!(job.name, "subagent");
        assert_eq!(job.status, MinionJobStatus::Waiting);
        let queue = MinionQueue::new(&engine);
        let fetched = queue.get_job(job.id).await.unwrap().unwrap();
        assert_eq!(fetched.id, job.id);
    }

    // ── libsql fixtures (job-capable engine) ──

    static LIBSQL_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> =
        std::sync::OnceLock::new();
    fn libsql_guard() -> std::sync::MutexGuard<'static, ()> {
        LIBSQL_TEST_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    async fn libsql_engine() -> (tempfile::NamedTempFile, crate::libsql::LibsqlEngine) {
        let temp = tempfile::NamedTempFile::new().expect("temp db");
        let path = temp.path().to_string_lossy().to_string();
        let config = EngineConfig {
            database_path: Some(path),
            database_url: None,
        };
        let engine = crate::libsql::LibsqlEngine::new();
        engine.connect(&config).await.unwrap();
        engine.init_schema().await.unwrap();
        (temp, engine)
    }
}
