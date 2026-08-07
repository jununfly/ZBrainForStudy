//! v0.32.2 — markdown-first fact write path (Rust port of `fence-write.ts`).
//!
//! The "system of record" invariant: new facts land in the entity page's
//! `## Facts` fence FIRST, then the DB index is stamped via
//! `engine.insert_fact`. The DB single-row insert stays as the legacy /
//! thin-client fallback only (when the brain has no `sources.local_path`
//! configured, or for facts with no resolved entity page to fence onto).
//!
//! Concurrency: reuses the in-process page lock (`page_lock_for`) so two
//! writes to the same `<slug>.md` serialize. Atomicity: write the fence to
//! `<file>.tmp`, re-parse the `.tmp` body, THEN `rename` onto the canonical
//! file. If parse fails the `.tmp` stays in place as quarantine evidence and
//! the DB is NOT touched (Codex Q7 atomic-write recovery).

use std::path::{Path, PathBuf};

use tokio::fs;

use crate::engine::BrainEngine;
use crate::facts_fence::{parse_facts_fence, upsert_fact_row, FenceFactInput};
use crate::page_lock::page_lock_for;
use crate::types::{FactInsertStatus, FactKind, FactVisibility, NewFact};

/// Resolved source binding for the entity page.
#[derive(Debug, Clone)]
pub struct FenceTarget {
    /// Source primary key, e.g. "default".
    pub source_id: String,
    /// Filesystem root for this source. `None` when the brain is read-only /
    /// thin-client (no fence writes possible).
    pub local_path: Option<String>,
    /// Entity slug — also becomes `source_markdown_slug` + the file basename.
    pub slug: String,
}

/// Input fact prepared by the backstop pipeline (post-dedup).
#[derive(Debug, Clone)]
pub struct FenceInputFact {
    pub fact: String,
    pub kind: Option<FactKind>,
    pub notability: Option<String>,
    pub source: String,
    pub context: Option<String>,
    pub visibility: FactVisibility,
    /// Defaults to 1.0 when `None` (matches `engine.insert_fact` behavior).
    pub confidence: Option<f64>,
    pub valid_from: Option<String>,
    pub session_id: Option<String>,
}

/// Outcome of a [`write_facts_to_fence`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceWriteResult {
    /// Number of new rows written + indexed.
    pub inserted: usize,
    /// DB ids assigned to the inserted rows, in input order.
    ///
    /// NOTE: the Rust `BrainEngine::insert_fact` (singular) returns only a
    /// `FactInsertStatus`, not the assigned id, so the fence path cannot
    /// surface per-row ids until `BrainEngine::insert_facts` (plural,
    /// returning ids) lands. Production backstop runs in queue mode, which
    /// discards counts/ids entirely; the inline MCP op is the only consumer
    /// of `fact_ids` and tolerates empty ids today.
    pub ids: Vec<i64>,
    /// True when the path fell through to DB-only because `local_path` was
    /// unset.
    pub legacy_fallback: bool,
    /// True when fence parse-validate failed; rows were NOT inserted, `.tmp`
    /// quarantined.
    pub fence_write_failed: bool,
    /// True when the stub-creation guard refused to spawn a phantom entity
    /// page for an unprefixed bare slug (e.g. `jared` with no `people/`
    /// prefix). Rows were NOT inserted; the caller routes them to the legacy
    /// DB-only path so they aren't silently dropped.
    pub stub_guard_blocked: bool,
}

impl Default for FenceWriteResult {
    fn default() -> Self {
        FenceWriteResult {
            inserted: 0,
            ids: Vec::new(),
            legacy_fallback: false,
            fence_write_failed: false,
            stub_guard_blocked: false,
        }
    }
}

/// Look up `sources.local_path` for a given `source_id`. Returns `None`
/// (thin-client / remote-brain installs) when the source has no
/// `local_path` configured or does not exist.
pub async fn lookup_source_local_path(
    engine: &dyn BrainEngine,
    source_id: &str,
) -> crate::Result<Option<String>> {
    let row = engine.get_source(source_id).await?;
    Ok(row.and_then(|s| s.local_path))
}

fn stub_entity_page(slug: &str) -> String {
    let prefix = slug.split('/').next().unwrap_or("");
    let page_type_str = match prefix {
        "people" => "person",
        "companies" => "company",
        "deals" => "deal",
        _ => "concept",
    };
    let tail = slug.split('/').skip(1).collect::<Vec<_>>().join("/");
    let title = if tail.is_empty() {
        slug.to_string()
    } else {
        tail.replace(['-', '_', '/'], " ")
            .split_whitespace()
            .map(|w| {
                let mut c = w.chars();
                match c.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    format!(
        "---\ntype: {page_type_str}\ntitle: {title}\nslug: {slug}\n---\n\n# {title}\n"
    )
}

/// Run a markdown-first fence write for one entity. Acquires the page lock,
/// reads or stub-creates the file, appends each input fact to the `## Facts`
/// fence, atomically renames the `.tmp` into place, and stamps the DB index
/// via `engine.insert_fact` per row.
///
/// Returns `legacyFallback: true` when `target.local_path` is `None` — the
/// caller is responsible for falling through to the legacy DB-only path.
///
/// Returns `fenceWriteFailed: true` when parse-validation of the just-written
/// `.tmp` fails. The `.tmp` stays on disk as quarantine; the DB is NOT
/// touched.
pub async fn write_facts_to_fence(
    engine: &dyn BrainEngine,
    target: &FenceTarget,
    facts: &[FenceInputFact],
) -> crate::Result<FenceWriteResult> {
    if target.local_path.is_none() {
        return Ok(FenceWriteResult {
            legacy_fallback: true,
            ..Default::default()
        });
    }
    if facts.is_empty() {
        return Ok(FenceWriteResult::default());
    }

    let local_path = target.local_path.as_ref().unwrap();
    let file_path: PathBuf = Path::new(local_path).join(format!("{}.md", target.slug));
    let tmp_path: PathBuf = Path::new(local_path).join(format!("{}.md.tmp", target.slug));

    // In-process page lock (serializes concurrent writes to the same slug).
    let lock = page_lock_for(&target.slug);
    let _guard = lock.lock().await;

    // 1. Read existing body or stub-create.
    let mut body: String = if fs::try_exists(&file_path).await.unwrap_or(false) {
        match fs::read_to_string(&file_path).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    "[facts-fence-write] cannot read {}: {e}; aborting fence write",
                    file_path.display()
                );
                return Ok(FenceWriteResult::default());
            }
        }
    } else {
        // Stub-creation guard: refuse to spawn a phantom entity page whose
        // slug has no directory prefix (people/, companies/, deals/, …). The
        // caller routes these facts to the legacy DB-only path so they aren't
        // silently dropped.
        if !target.slug.contains('/') {
            tracing::warn!(
                "[facts] refusing to stub-create unprefixed entity page slug={} — \
                 routing to legacy DB-only path. Provide a directory prefix \
                 (people/, companies/, etc.) to opt into fence writes.",
                target.slug
            );
            return Ok(FenceWriteResult {
                stub_guard_blocked: true,
                ..Default::default()
            });
        }
        if let Some(parent) = file_path.parent() {
            let _ = fs::create_dir_all(parent).await;
        }
        stub_entity_page(&target.slug)
    };

    // 2. Upsert each fact onto the fence in input order; row_num increases
    //    monotonically (max-existing + 1 per call, append-only).
    let mut assigned_row_nums: Vec<i32> = Vec::with_capacity(facts.len());
    for f in facts {
        let input = FenceFactInput {
            claim: f.fact.clone(),
            kind: f.kind.clone().unwrap_or(FactKind::Fact),
            confidence: f.confidence.unwrap_or(1.0),
            visibility: f.visibility.clone(),
            notability: f
                .notability
                .clone()
                .unwrap_or_else(|| "medium".to_string()),
            valid_from: f.valid_from.clone(),
            valid_until: None,
            source: Some(f.source.clone()),
            context: f.context.clone(),
            active: None,
            row_num: None,
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
        };
        let res = upsert_fact_row(&body, &input);
        body = res.body;
        assigned_row_nums.push(res.row_num);
    }

    // 3. Atomic write: .tmp first.
    if fs::write(&tmp_path, &body).await.is_err() {
        return Ok(FenceWriteResult::default());
    }

    // 4. Parse-before-rename: re-read the .tmp and verify the fence is
    //    well-formed. Anything malformed → leave .tmp as quarantine, do NOT
    //    insert to DB.
    let tmp_body = match fs::read_to_string(&tmp_path).await {
        Ok(b) => b,
        Err(_) => return Ok(FenceWriteResult::default()),
    };
    let parsed = parse_facts_fence(&tmp_body);
    if !parsed.warnings.is_empty() {
        tracing::warn!(
            "[facts-fence-write] fence parse rejected .tmp for slug={} ({} warnings); \
             leaving .tmp as quarantine: {:?}",
            target.slug,
            parsed.warnings.len(),
            parsed.warnings
        );
        return Ok(FenceWriteResult {
            fence_write_failed: true,
            ..Default::default()
        });
    }

    // 5. Rename .tmp → file (POSIX atomic).
    if fs::rename(&tmp_path, &file_path).await.is_err() {
        let _ = fs::remove_file(&tmp_path).await;
        return Ok(FenceWriteResult::default());
    }

    // 6. Stamp the DB. The fence is the system of record; we insert each new
    //    row carrying its fence row_num + source_markdown_slug so a
    //    `zbrain rebuild` reconciles byte-identical DB state.
    let mut inserted = 0usize;
    for (i, f) in facts.iter().enumerate() {
        let new_fact = NewFact {
            fact: f.fact.clone(),
            kind: f.kind.clone(),
            entity_slug: Some(target.slug.clone()),
            visibility: Some(f.visibility.clone()),
            context: f.context.clone(),
            valid_from: f.valid_from.clone(),
            valid_until: None,
            source: f.source.clone(),
            source_session: f.session_id.clone(),
            confidence: f.confidence,
            notability: f.notability.clone(),
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
            event_type: None,
            row_num: assigned_row_nums.get(i).copied(),
            source_markdown_slug: Some(target.slug.clone()),
        };
        match engine
            .insert_fact(&target.source_id, &target.slug, &new_fact)
            .await?
        {
            FactInsertStatus::Inserted => inserted += 1,
            FactInsertStatus::Duplicate | FactInsertStatus::Superseded => {}
        }
    }

    Ok(FenceWriteResult {
        inserted,
        ids: Vec::new(),
        ..Default::default()
    })
}
