//! facts forget — markdown-first fence-rewrite (v0.32.2 contract).
//!
//! Port of `src/core/facts/forget.ts`.
//!
//! Behaviour mirrors the TS `forgetFactInFence`:
//!   - Look up the fact row (needs v51 columns: `row_num` +
//!     `source_markdown_slug` + `entity_slug`) and the owning source's
//!     `local_path`.
//!   - If those are present AND the markdown file exists, do a fence rewrite:
//!     strike the `claim` cell, set `valid_until = today`, append
//!     `forgotten: <reason>` to `context`. Render + atomic `.tmp` + parse
//!     validate + rename, then stamp `valid_until`/`expired_at` in the DB.
//!   - Otherwise fall through to the legacy `expire_fact` DB-only path (which
//!     does NOT survive `zbrain rebuild` — named as the explicit degraded mode
//!     for pre-v51 rows / thin-client / missing file).
//!
//! The engine glue calls `BrainEngine::execute_raw` / `expire_fact` directly
//! (not through a wrapper trait) so this `async fn` stays free of the
//! closure-returning-a-future-with-local-lifetime pattern that the borrow
//! checker rejects as an escape.

use std::path::{Path, PathBuf};

use erased_serde::Serialize;

use crate::engine::BrainEngine;
use crate::error::StructuredError;
use crate::facts_fence::{
    parse_facts_fence, render_facts_fence, FenceFact, FACTS_FENCE_BEGIN, FACTS_FENCE_END,
};
use crate::page_lock::page_lock_for;

/// Discriminator on the path that handled the forget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgetPath {
    /// Fence rewrite succeeded — the forget survives `zbrain rebuild`.
    Fence,
    /// Legacy DB-only `expire_fact` fallback (degraded mode).
    LegacyDb,
    /// Fact id not found in the DB.
    NotFound,
    /// Fact already expired — no-op.
    AlreadyExpired,
}

impl ForgetPath {
    pub fn as_str(self) -> &'static str {
        match self {
            ForgetPath::Fence => "fence",
            ForgetPath::LegacyDb => "legacy_db",
            ForgetPath::NotFound => "not_found",
            ForgetPath::AlreadyExpired => "already_expired",
        }
    }
}

/// Result of a forget operation.
#[derive(Debug, Clone)]
pub struct ForgetFactResult {
    /// True iff the row was found AND a forget was applied (fence or DB).
    pub ok: bool,
    /// Which path handled the forget.
    pub path: ForgetPath,
    /// Human-readable reason (echoes what was written / requested).
    pub reason: String,
}

/// Options for [`forget_fact_in_fence`].
#[derive(Debug, Clone, Default)]
pub struct ForgetFactOpts {
    /// Reason written into `context`. Defaults to `"forgotten"`.
    pub reason: Option<String>,
}

/// Row shape read from the `facts` table for a forget.
#[derive(Debug, Clone)]
pub struct ForgetFactRow {
    pub id: i64,
    pub source_id: String,
    pub entity_slug: Option<String>,
    pub row_num: Option<i32>,
    pub source_markdown_slug: Option<String>,
    /// Present (non-null) iff the fact is already expired.
    pub expired_at: Option<String>,
}

/// Decision tree for which forget path to take, given the looked-up fact row
/// and (optionally) the owning source's `local_path`. Pure — no IO, no engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForgetPlan {
    /// Fact id not present in the DB.
    NotFound,
    /// Fact already expired — no-op.
    AlreadyExpired,
    /// Cannot fence (missing v51 columns or `local_path`) → legacy `expire_fact`.
    Legacy,
    /// Fence rewrite is viable; carry the resolved slug / dir / today.
    Fence {
        slug: String,
        local_path: String,
        today: String,
    },
}

/// Pure routing decision for [`forget_fact_in_fence`].
pub fn plan_forget(row: &ForgetFactRow, local_path: Option<String>) -> ForgetPlan {
    if row.expired_at.is_some() {
        return ForgetPlan::AlreadyExpired;
    }
    let can_fence = row.row_num.is_some()
        && row.source_markdown_slug.is_some()
        && row.entity_slug.is_some();
    if !can_fence {
        return ForgetPlan::Legacy;
    }
    let slug = row
        .source_markdown_slug
        .clone()
        .expect("can_fence guarantees source_markdown_slug");
    let local_path = match local_path {
        Some(p) => p,
        None => return ForgetPlan::Legacy,
    };
    ForgetPlan::Fence {
        slug,
        local_path,
        today: today_utc(),
    }
}

/// Outcome of a fence file read-modify-write (see [`apply_fence_rewrite`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FenceRewriteOutcome {
    /// Fence rewritten + committed atomically.
    Fenced,
    /// The target `row_num` is absent from the fence (DB drifted from markdown).
    ForgottenMissingRow,
    /// IO / validation failure (caller should fall back to legacy `expire_fact`).
    IoError,
}

/// Read the markdown file at `file_path`, strike the target row's `claim`,
/// set `valid_until = today`, append `forgotten: <reason>` to `context`, then
/// write atomically via `<file>.tmp` + parse-validate + rename.
///
/// Returns [`FenceRewriteOutcome::Fenced`] on success, or a non-fatal outcome
/// that the caller maps to the legacy `expire_fact` fallback. Idempotent:
/// already-struck rows stay struck.
pub async fn apply_fence_rewrite(
    file_path: &Path,
    tmp_path: &Path,
    target_row_num: i32,
    reason: &str,
    today: &str,
) -> FenceRewriteOutcome {
    let body = match tokio::fs::read_to_string(file_path).await {
        Ok(b) => b,
        Err(_) => return FenceRewriteOutcome::IoError,
    };

    let new_body = match rewrite_fence_for_forget(&body, target_row_num, reason, today) {
        Some(b) => b,
        None => return FenceRewriteOutcome::ForgottenMissingRow,
    };

    if tokio::fs::write(tmp_path, &new_body).await.is_err() {
        return FenceRewriteOutcome::IoError;
    }

    // Validate by re-parsing the .tmp.
    let tmp_body = match tokio::fs::read_to_string(tmp_path).await {
        Ok(b) => b,
        Err(_) => return FenceRewriteOutcome::IoError,
    };
    let validate = parse_facts_fence(&tmp_body);
    if !validate.warnings.is_empty() {
        let _ = tokio::fs::remove_file(tmp_path).await;
        return FenceRewriteOutcome::IoError;
    }

    if tokio::fs::rename(tmp_path, file_path).await.is_err() {
        let _ = tokio::fs::remove_file(tmp_path).await;
        return FenceRewriteOutcome::IoError;
    }

    FenceRewriteOutcome::Fenced
}

fn parse_forget_row(v: serde_json::Value) -> ForgetFactRow {
    let expired_at = v
        .get("expired_at")
        .filter(|x| !x.is_null())
        .map(|x| x.as_str().map(|s| s.to_string()).unwrap_or_default());
    ForgetFactRow {
        id: v.get("id").and_then(|x| x.as_i64()).unwrap_or(0),
        source_id: v
            .get("source_id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        entity_slug: v
            .get("entity_slug")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        row_num: v.get("row_num").and_then(|x| x.as_i64()).map(|n| n as i32),
        source_markdown_slug: v
            .get("source_markdown_slug")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        expired_at,
    }
}

/// Format today's date as `YYYY-MM-DD` UTC. Mirrors `forget.ts#todayUtc`.
pub fn today_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Pure fence rewrite for a forget. Returns the new file body, or `None`
/// when the target `row_num` isn't present in the fence (caller then
/// falls back to legacy `expire_fact`).
///
/// Mutates the target row: strikes the `claim` (idempotent — already struck
/// rows stay struck), sets `valid_until = today`, and appends
/// `forgotten: <reason>` to `context` (preserving any existing context).
pub fn rewrite_fence_for_forget(
    body: &str,
    target_row_num: i32,
    reason: &str,
    today: &str,
) -> Option<String> {
    let parsed = parse_facts_fence(body);
    let target = parsed.facts.iter().find(|f| f.row_num == target_row_num)?;

    let existing_context = target.context.as_deref().unwrap_or("").trim();
    let new_context = if existing_context.is_empty() {
        format!("forgotten: {reason}")
    } else {
        format!("{existing_context} | forgotten: {reason}")
    };

    let updated: Vec<FenceFact> = parsed
        .facts
        .iter()
        .map(|f| {
            if f.row_num == target_row_num {
                FenceFact {
                    active: false,
                    valid_until: Some(today.to_string()),
                    context: Some(new_context.clone()),
                    forgotten: true,
                    ..f.clone()
                }
            } else {
                f.clone()
            }
        })
        .collect();

    let new_fence = render_facts_fence(&updated);
    let begin = body.find(FACTS_FENCE_BEGIN)?;
    let end_rel = body[begin..].find(FACTS_FENCE_END)?;
    let end_end = begin + end_rel + FACTS_FENCE_END.len();
    Some(format!("{}{}{}", &body[..begin], new_fence, &body[end_end..]))
}

/// Forget a fact by id using the markdown-first fence-rewrite contract.
///
/// Routes through the fence when the row carries v51 columns + the source has
/// a `local_path` + the markdown file exists; otherwise falls through to the
/// legacy `expire_fact` DB-only path. Idempotent: returns `AlreadyExpired`
/// when the row is already expired.
pub async fn forget_fact_in_fence(
    engine: &dyn BrainEngine,
    source_id: &str,
    fact_id: i64,
    opts: ForgetFactOpts,
) -> crate::Result<ForgetFactResult> {
    let reason = opts.reason.unwrap_or_else(|| "forgotten".to_string());

    // 1) Look up the fact row.
    let row = match forget_lookup_fact(engine, fact_id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            return Ok(ForgetFactResult {
                ok: false,
                path: ForgetPath::NotFound,
                reason,
            });
        }
        Err(e) if is_unsupported(&e) => {
            // Engine lacks raw-SQL support (e.g. InMemoryEngine): best-effort
            // legacy expire so the user's intent still succeeds where possible.
            let ok = engine.expire_fact(source_id, fact_id).await.unwrap_or(false);
            return Ok(ForgetFactResult {
                ok,
                path: ForgetPath::LegacyDb,
                reason,
            });
        }
        Err(e) => return Err(e),
    };

    // 2) Plan the path.
    let local_path = forget_lookup_local_path(engine, &row.source_id)
        .await
        .unwrap_or(None);
    match plan_forget(&row, local_path) {
        ForgetPlan::NotFound => Ok(ForgetFactResult {
            ok: false,
            path: ForgetPath::NotFound,
            reason,
        }),
        ForgetPlan::AlreadyExpired => Ok(ForgetFactResult {
            ok: false,
            path: ForgetPath::AlreadyExpired,
            reason,
        }),
        ForgetPlan::Legacy => {
            let ok = engine.expire_fact(&row.source_id, fact_id).await.unwrap_or(false);
            Ok(ForgetFactResult {
                ok,
                path: ForgetPath::LegacyDb,
                reason,
            })
        }
        ForgetPlan::Fence {
            slug,
            local_path,
            today,
        } => {
            let file_path = PathBuf::from(&local_path).join(format!("{slug}.md"));
            if !file_path.exists() {
                let ok = engine.expire_fact(&row.source_id, fact_id).await.unwrap_or(false);
                return Ok(ForgetFactResult {
                    ok,
                    path: ForgetPath::LegacyDb,
                    reason,
                });
            }
            let tmp_path = PathBuf::from(&local_path).join(format!("{slug}.md.tmp"));

            // Serialize fence writes to this page within the process.
            let lock = page_lock_for(&slug);
            let _guard = lock.lock().await;

            let outcome =
                apply_fence_rewrite(&file_path, &tmp_path, row.row_num.unwrap(), &reason, &today)
                    .await;
            match outcome {
                FenceRewriteOutcome::Fenced => {
                    // Stamp the DB to match (keeps active-fact queries accurate
                    // immediately). The fence rewrite is the system of record; a
                    // stamp failure is non-fatal (the next reconcile fixes it).
                    let _ = forget_stamp(engine, fact_id, &today).await;
                    Ok(ForgetFactResult {
                        ok: true,
                        path: ForgetPath::Fence,
                        reason,
                    })
                }
                _ => Ok(legacy_expire(engine, &row.source_id, fact_id, &reason).await),
            }
        }
    }
}

async fn legacy_expire(
    engine: &dyn BrainEngine,
    source_id: &str,
    fact_id: i64,
    reason: &str,
) -> ForgetFactResult {
    let ok = engine.expire_fact(source_id, fact_id).await.unwrap_or(false);
    ForgetFactResult {
        ok,
        path: ForgetPath::LegacyDb,
        reason: reason.to_string(),
    }
}

async fn forget_lookup_fact(
    engine: &dyn BrainEngine,
    fact_id: i64,
) -> crate::Result<Option<ForgetFactRow>> {
    let p = serde_json::json!(fact_id);
    let params: &[&(dyn Serialize + Sync)] = &[&p];
    let rows = engine
        .execute_raw(
            "SELECT id, source_id, entity_slug, row_num, source_markdown_slug, expired_at \
             FROM facts WHERE id = $1",
            params,
        )
        .await?;
    Ok(rows.into_iter().next().map(parse_forget_row))
}

async fn forget_lookup_local_path(
    engine: &dyn BrainEngine,
    source_id: &str,
) -> crate::Result<Option<String>> {
    let p = serde_json::json!(source_id);
    let params: &[&(dyn Serialize + Sync)] = &[&p];
    let rows = engine
        .execute_raw(
            "SELECT id, local_path FROM sources WHERE id = $1 LIMIT 1",
            params,
        )
        .await?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.get("local_path").and_then(|v| v.as_str()).map(|s| s.to_string())))
}

async fn forget_stamp(
    engine: &dyn BrainEngine,
    fact_id: i64,
    valid_until: &str,
) -> crate::Result<()> {
    let pid = serde_json::json!(fact_id);
    let pvu = serde_json::json!(valid_until);
    let params: &[&(dyn Serialize + Sync)] = &[&pvu, &pid];
    engine
        .execute_raw(
            "UPDATE facts SET valid_until = $1, expired_at = now() \
             WHERE id = $2 AND expired_at IS NULL",
            params,
        )
        .await?;
    Ok(())
}

fn is_unsupported(e: &StructuredError) -> bool {
    let m = format!("{} {}", e.class, e.message).to_lowercase();
    m.contains("unsupported") || m.contains("not implemented") || m.contains("not_yet_implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fence_body(rows: &str) -> String {
        format!(
            "# Page\n\n<!--- zbrain:facts:begin -->\n\
             | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |\n\
             |---|-------|------|------------|------------|------------|------------|-------------|--------|--------|\n\
             {rows}<!--- zbrain:facts:end -->\n"
        )
    }

    fn tempdir() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("zb_forget_test_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ── rewrite (pure) ───────────────────────────────────────────────

    #[test]
    fn rewrite_strikes_claim_and_appends_forgotten() {
        let body = fence_body("| 1 | loves coffee | preference | 0.9 | private | high |  |  |  |  |\n| 2 | hates tea | preference | 0.8 | private | high |  |  |  |  |\n");
        let out = rewrite_fence_for_forget(&body, 2, "typo", "2026-08-06").expect("rewrite");
        let parsed = parse_facts_fence(&out);
        let row2 = parsed.facts.iter().find(|f| f.row_num == 2).unwrap();
        assert!(!row2.active);
        assert!(row2.forgotten);
        assert_eq!(row2.valid_until.as_deref(), Some("2026-08-06"));
        assert!(row2.context.as_deref().unwrap().contains("forgotten: typo"));
        assert_eq!(row2.claim, "hates tea");
        // row 1 unchanged
        let row1 = parsed.facts.iter().find(|f| f.row_num == 1).unwrap();
        assert!(row1.active);
        assert!(!row1.forgotten);
    }

    #[test]
    fn rewrite_preserves_existing_context() {
        let body = fence_body("| 3 | x | fact | 0.5 | private | low |  |  |  | prior note |\n");
        let out = rewrite_fence_for_forget(&body, 3, "wrong", "2026-08-06").unwrap();
        let row = parse_facts_fence(&out)
            .facts
            .into_iter()
            .find(|f| f.row_num == 3)
            .unwrap();
        let ctx = row.context.as_deref().unwrap();
        assert!(ctx.contains("prior note"));
        assert!(ctx.contains("forgotten: wrong"));
    }

    #[test]
    fn rewrite_missing_row_returns_none() {
        let body = fence_body("| 1 | x | fact | 0.5 | private | low |  |  |  |  |\n");
        assert!(rewrite_fence_for_forget(&body, 99, "r", "2026-08-06").is_none());
    }

    // ── plan_forget (pure) ───────────────────────────────────────────

    fn base_row() -> ForgetFactRow {
        ForgetFactRow {
            id: 7,
            source_id: "default".into(),
            entity_slug: Some("alice".into()),
            row_num: Some(2),
            source_markdown_slug: Some("alice".into()),
            expired_at: None,
        }
    }

    #[test]
    fn plan_already_expired() {
        let mut r = base_row();
        r.expired_at = Some("2026-01-01".into());
        assert_eq!(plan_forget(&r, Some("/tmp".into())), ForgetPlan::AlreadyExpired);
    }

    #[test]
    fn plan_legacy_when_columns_missing() {
        let mut r = base_row();
        r.row_num = None;
        assert_eq!(plan_forget(&r, Some("/tmp".into())), ForgetPlan::Legacy);
    }

    #[test]
    fn plan_legacy_when_no_local_path() {
        assert_eq!(plan_forget(&base_row(), None), ForgetPlan::Legacy);
    }

    #[test]
    fn plan_fence_when_all_present() {
        match plan_forget(&base_row(), Some("/data".into())) {
            ForgetPlan::Fence {
                slug,
                local_path,
                today,
            } => {
                assert_eq!(slug, "alice");
                assert_eq!(local_path, "/data");
                assert_eq!(today.len(), 10);
            }
            other => panic!("expected Fence, got {other:?}"),
        }
    }

    // ── apply_fence_rewrite (file IO) ────────────────────────────────

    #[tokio::test]
    async fn apply_rewrites_file_and_leaves_no_tmp() {
        let dir = tempdir();
        let slug = "alice";
        let file = dir.join(format!("{slug}.md"));
        let body = fence_body(
            "| 1 | likes a | preference | 0.9 | private | high |  |  |  |  |\n\
             | 2 | likes b | preference | 0.8 | private | high |  |  |  |  |\n",
        );
        std::fs::write(&file, &body).unwrap();
        let tmp = dir.join(format!("{slug}.md.tmp"));

        let out = apply_fence_rewrite(&file, &tmp, 2, "typo", "2026-08-06").await;
        assert_eq!(out, FenceRewriteOutcome::Fenced);
        assert!(!tmp.exists(), "tmp must be renamed away");

        let new_body = std::fs::read_to_string(&file).unwrap();
        let row2 = parse_facts_fence(&new_body)
            .facts
            .into_iter()
            .find(|f| f.row_num == 2)
            .unwrap();
        assert!(!row2.active);
        assert!(row2.forgotten);
        assert!(row2.context.as_deref().unwrap().contains("forgotten: typo"));
    }

    #[tokio::test]
    async fn apply_missing_row_is_noop() {
        let dir = tempdir();
        let slug = "bob";
        let file = dir.join(format!("{slug}.md"));
        let body = fence_body("| 1 | only row | fact | 0.5 | private | low |  |  |  |  |\n");
        std::fs::write(&file, &body).unwrap();
        let tmp = dir.join(format!("{slug}.md.tmp"));

        let out = apply_fence_rewrite(&file, &tmp, 99, "r", "2026-08-06").await;
        assert_eq!(out, FenceRewriteOutcome::ForgottenMissingRow);
        // file unchanged, no tmp left behind
        assert_eq!(std::fs::read_to_string(&file).unwrap(), body);
        assert!(!tmp.exists());
    }
}
