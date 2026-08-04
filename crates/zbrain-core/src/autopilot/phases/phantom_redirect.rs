//! Phantom-page redirect pre-pass (1-6-6-4). Port of
//! `src/core/cycle/phantom-redirect.ts` (v0.35.5).
//!
//! Runs at the top of `extract_facts` after the legacy-row guard, BEFORE the
//! main reconcile loop. Walks unprefixed-slug pages in the source (e.g.
//! `alice.md` at brain root), tries to resolve each to a canonical prefixed
//! slug (`people/alice-example`), migrates fact rows + disk fence, soft-
//! deletes the phantom, unlinks the `.md`. Bounded at 50 phantoms per cycle
//! (configurable via `ZBRAIN_PHANTOM_REDIRECT_LIMIT`).
//!
//! Lock contract: this pass does NOT acquire its own lock. The cycle
//! orchestrator (`autopilot::cycle`) already holds the per-source advisory
//! file lock (`autopilot::cycle_lock`) for the whole cycle when any mutating
//! phase is in scope (1-6-2). Re-acquiring the same lock here would
//! self-deadlock (the file is held by the current process). The
//! `pass_skipped_lock_busy` audit outcome is retained for schema completeness
//! and a future standalone invocation path, but the in-cycle pass expects the
//! caller to have serialized access.
//!
//! Idempotency: re-run on a half-redirected phantom is safe — the migration
//! UPDATE matches no rows (already migrated), the disk-side fence append
//! dedups by (claim, valid_from), and every other step is idempotent
//! (softDelete is no-op on already-deleted, unlink is ENOENT-safe).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::engine::{BrainEngine, Page};
use crate::facts_fence::{parse_facts_fence, render_facts_fence, FenceFact, FACTS_FENCE_BEGIN, FACTS_FENCE_END};
use crate::markdown::{parse_markdown, serialize_markdown};
use crate::schema_pack::candidate_audit::resolve_audit_dir;
use crate::types::RefreshPageBodyArgs;
use crate::GetPageOpts;

use super::phantom_audit::{log_phantom_event, PhantomCandidate, PhantomEventInput, PhantomOutcome};
use super::resolve::{find_prefix_candidates, resolve_phantom_canonical, PrefixCandidate};

/// Default per-cycle cap on redirected phantoms.
pub const DEFAULT_PHANTOM_LIMIT: usize = 50;
/// Env var override for [`DEFAULT_PHANTOM_LIMIT`].
pub const ENV_PHANTOM_LIMIT: &str = "ZBRAIN_PHANTOM_REDIRECT_LIMIT";

/// Tagged-union outcome of a single phantom-redirect attempt (mirrors TS
/// `RedirectOutcome` — the 5 user-facing variants; `not_phantom_has_residue`
/// is folded into `NotPhantom` at the redirect layer and logged separately as
/// an audit variant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectOutcome {
    NotPhantom,
    Redirected,
    Ambiguous,
    Drift,
    NoCanonical,
}

/// Result of a single-phantom redirect handler.
#[derive(Debug, Clone)]
pub struct RedirectResult {
    pub outcome: RedirectOutcome,
    /// Canonical slug, populated only on `Redirected`.
    pub canonical: Option<String>,
}

/// Aggregate result of the per-cycle pass.
#[derive(Debug, Clone, Default)]
pub struct PhantomPassResult {
    pub scanned: u64,
    pub redirected: u64,
    pub ambiguous: u64,
    pub skipped_drift: u64,
    pub no_canonical: u64,
    pub not_phantom: u64,
    /// True iff the pass was skipped wholesale because the writer lock was busy.
    pub lock_busy: bool,
    /// True iff more phantoms exist than the per-cycle cap — caller surfaces to operator.
    pub more_pending: bool,
    /// Canonical slugs whose disk fence was merged with phantom rows this pass.
    pub touched_canonicals: Vec<String>,
}

/// Strip the leading H1 heading + the entire `## Facts` fenced block, return
/// the whitespace-trimmed residue. Zero-length residue is the phantom stub
/// gate (codex #2). Mirrors TS `stripFenceAndFrontmatterAndLeadingH1`.
fn strip_fence_and_leading_h1(body: &str) -> String {
    if body.is_empty() {
        return String::new();
    }
    let mut working = body.to_string();

    // 1. Locate the fence markers.
    let begin = working.find(FACTS_FENCE_BEGIN);
    let end = begin.and_then(|b| {
        let e = working[FACTS_FENCE_BEGIN.len() + b..]
            .find(FACTS_FENCE_END)
            .map(|rel| FACTS_FENCE_BEGIN.len() + b + rel)?;
        Some(e)
    });
    if let (Some(b), Some(e)) = (begin, end) {
        if b < e {
            // Walk backward from b to swallow a leading `## Facts\n\n` heading.
            let mut heading_start = b;
            while heading_start > 0 && working.as_bytes()[heading_start - 1] != b'\n' {
                heading_start -= 1;
            }
            while heading_start > 0 {
                let prev_line_end = heading_start - 1;
                let prev_line_start = working[..prev_line_end]
                    .rfind('\n')
                    .map_or(0, |i| i + 1);
                let prev_line = &working[prev_line_start..prev_line_end];
                if prev_line.trim().is_empty() {
                    heading_start = prev_line_start;
                    continue;
                }
                // Is it a markdown heading whose text is "facts" (case-insensitive)?
                let tl = prev_line.trim_start();
                let n_hashes = tl.bytes().take_while(|&c| c == b'#').count();
                if (1..=6).contains(&n_hashes) {
                    let after = &tl[n_hashes..];
                    let after_sp = after.trim_start();
                    if after_sp.to_ascii_lowercase().starts_with("facts") {
                        heading_start = prev_line_start;
                    }
                }
                break;
            }
            working = format!(
                "{}{}",
                &working[..heading_start],
                &working[e + FACTS_FENCE_END.len()..]
            );
        }
    }

    // 2. Strip the leading H1 (`# text` at the very top).
    working = strip_leading_h1(&working);

    // 3. Whitespace-trim.
    working.trim().to_string()
}

/// Strip a single leading `# …` line at the very start of `s` (without a
/// regex dependency). Mirrors `s.replace(/^\s*#\s+[^\n]*\n?/, '')`.
fn strip_leading_h1(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'#' {
        return s.to_string();
    }
    i += 1;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
    }
    s[i..].to_string()
}

/// Compute the canonical content_hash for a page, matching the shape
/// `src/core/import-file.ts:241` uses so `zbrain sync`'s idempotency check
/// sees the redirected canonical as unchanged.
fn compute_page_content_hash(
    title: &str,
    type_: &str,
    compiled_truth: &str,
    timeline: &str,
    frontmatter: &serde_json::Value,
    tags: &[String],
) -> String {
    let mut tags_sorted = tags.to_vec();
    tags_sorted.sort();
    let payload = serde_json::json!({
        "title": title,
        "type": type_,
        "compiled_truth": compiled_truth,
        "timeline": timeline,
        "frontmatter": frontmatter,
        "tags": tags_sorted,
    });
    let canonical = serde_json::to_string(&payload).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    format!("sha256:{}", hex::encode(digest))
}

/// Append the phantom's fact rows to the canonical's disk fence, dedup-guarded
/// by (claim, valid_from). Returns the count of rows actually appended.
///
/// The disk write happens BEFORE the DB migration in the redirect handler, so
/// if this throws (rename fails, disk full, parse-validation rejects) the DB
/// migration won't run and the cycle can retry next run. Mirrors TS
/// `appendPhantomFenceRowsToCanonical`.
fn append_phantom_fence_rows_to_canonical(
    canonical_path: &Path,
    phantom_facts: &[FenceFact],
) -> std::io::Result<usize> {
    if phantom_facts.is_empty() {
        return Ok(0);
    }
    let body = std::fs::read_to_string(canonical_path)?;
    let parsed = parse_facts_fence(&body);

    let existing_keys: HashSet<String> = parsed
        .facts
        .iter()
        .map(|f| format!("{}|{}", f.claim, f.valid_from.as_deref().unwrap_or("")))
        .collect();
    let next_row_num = parsed
        .facts
        .iter()
        .map(|f| f.row_num)
        .max()
        .map_or(1, |m| m + 1);

    let mut merged: Vec<FenceFact> = parsed.facts;
    let mut appended = 0usize;
    for pf in phantom_facts {
        let key = format!("{}|{}", pf.claim, pf.valid_from.as_deref().unwrap_or(""));
        if existing_keys.contains(&key) {
            continue;
        }
        let mut row = pf.clone();
        row.row_num = next_row_num + appended as i32;
        merged.push(row);
        appended += 1;
    }

    if appended == 0 {
        return Ok(0);
    }

    let new_fence = render_facts_fence(&merged);
    let begin = body.find(FACTS_FENCE_BEGIN);
    let end = begin.and_then(|b| {
        let e = body[FACTS_FENCE_BEGIN.len() + b..]
            .find(FACTS_FENCE_END)
            .map(|rel| FACTS_FENCE_BEGIN.len() + b + rel)?;
        Some(e)
    });
    let new_body = match (begin, end) {
        (Some(b), Some(e)) if b < e => format!(
            "{}{}{}",
            &body[..b],
            new_fence,
            &body[e + FACTS_FENCE_END.len()..]
        ),
        _ => {
            let sep = if body.ends_with('\n') { "\n" } else { "\n\n" };
            format!("{}{}## Facts\n\n{}\n", body, sep, new_fence)
        }
    };

    // Atomic write: .tmp first, parse-validate, rename.
    let tmp_path = canonical_path.with_extension("md.tmp");
    std::fs::write(&tmp_path, &new_body)?;
    let reparsed = parse_facts_fence(&new_body);
    if !reparsed.warnings.is_empty() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "phantom-redirect: rendered fence failed re-parse: {}",
                reparsed.warnings.join("; ")
            ),
        ));
    }
    std::fs::rename(&tmp_path, canonical_path)?;
    Ok(appended)
}

/// Bi-directional drift between phantom's DB body and its disk file. When both
/// exist and disagree on the parsed fence row set (by claim + valid_from),
/// classify as `drift` — operator triages manually. When the disk file is
/// absent, the DB body is the truth; not drift. Mirrors TS `fenceDbDrift`.
fn fence_db_drift(page: &Page, brain_dir: &Path) -> bool {
    let phantom_path = brain_dir.join(format!("{}.md", page.slug));
    if !phantom_path.exists() {
        return false;
    }
    let db_parse = parse_facts_fence(&page.compiled_truth);
    let db_keys: HashSet<String> = db_parse
        .facts
        .iter()
        .map(|f| format!("{}|{}", f.claim, f.valid_from.as_deref().unwrap_or("")))
        .collect();

    let disk_body = match std::fs::read_to_string(&phantom_path) {
        Ok(b) => b,
        Err(_) => return false, // vanished between exists + read → DB-only, no drift
    };
    let disk_compiled = parse_markdown(&disk_body, &format!("{}.md", page.slug), None).compiled_truth;
    let disk_parse = parse_facts_fence(&disk_compiled);
    let disk_keys: HashSet<String> = disk_parse
        .facts
        .iter()
        .map(|f| format!("{}|{}", f.claim, f.valid_from.as_deref().unwrap_or("")))
        .collect();

    if db_keys.len() != disk_keys.len() {
        return true;
    }
    for k in &db_keys {
        if !disk_keys.contains(k) {
            return true;
        }
    }
    false
}

/// Materialize a DB-only canonical page to disk by serializing its full page
/// state (frontmatter + body + timeline). Reuses [`serialize_markdown`] so the
/// output round-trips through `parse_markdown` cleanly. Mirrors TS
/// `materializeCanonicalToDisk`.
async fn materialize_canonical_to_disk(
    engine: &dyn BrainEngine,
    canonical_slug: &str,
    source_id: &str,
    canonical_path: &Path,
) -> std::io::Result<()> {
    if canonical_path.exists() {
        return Ok(());
    }
    let canonical_page = match engine
        .get_page(
            canonical_slug,
            &GetPageOpts {
                source_id: Some(source_id.to_string()),
                include_deleted: false,
            },
        )
        .await
    {
        Ok(Some(p)) => Some(p),
        _ => None,
    };

    let body = if let Some(page) = canonical_page {
        let tags = engine
            .get_tags(canonical_slug, Some(source_id))
            .await
            .unwrap_or_default();
        serialize_markdown(
            &page.frontmatter,
            &page.compiled_truth,
            &page.timeline,
            &tags,
        )
    } else {
        // Canonical doesn't exist in DB either — materialize a minimal stub so
        // the subsequent fence append has somewhere to land.
        let title_from_slug = canonical_slug.split('/').next_back().unwrap_or(canonical_slug);
        serialize_markdown(
            &serde_json::Value::Object(serde_json::Map::new()),
            &format!("# {title_from_slug}\n"),
            "",
            &[],
        )
    };

    if let Some(parent) = canonical_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(canonical_path, body)?;
    Ok(())
}

/// Single-phantom redirect. The caller (the pass) is responsible for the outer
/// audit cap + counting. Mirrors TS `tryRedirectPhantom`.
async fn try_redirect_phantom(
    engine: &dyn BrainEngine,
    page: &Page,
    source_id: &str,
    brain_dir: &Path,
    dry_run: bool,
) -> RedirectResult {
    // Predicate (D2): unprefixed AND alive.
    if page.slug.contains('/') {
        return RedirectResult {
            outcome: RedirectOutcome::NotPhantom,
            canonical: None,
        };
    }

    // A3 + codex #2: strict zero-residue body-shape gate.
    let residue = strip_fence_and_leading_h1(&page.compiled_truth);
    if !residue.is_empty() {
        log_phantom_event(
            &resolve_audit_dir(),
            PhantomEventInput {
                phantom_slug: Some(page.slug.clone()),
                outcome: PhantomOutcome::NotPhantomHasResidue,
                source_id: source_id.to_string(),
                ..Default::default()
            },
        );
        return RedirectResult {
            outcome: RedirectOutcome::NotPhantom,
            canonical: None,
        };
    }

    // Codex #1: phantom-specific resolver bypasses exact-self-match.
    let canonical = match resolve_phantom_canonical(engine, source_id, &page.slug).await {
        Some(c) => c,
        None => {
            log_phantom_event(
                &resolve_audit_dir(),
                PhantomEventInput {
                    phantom_slug: Some(page.slug.clone()),
                    outcome: PhantomOutcome::NoCanonical,
                    source_id: source_id.to_string(),
                    ..Default::default()
                },
            );
            return RedirectResult {
                outcome: RedirectOutcome::NoCanonical,
                canonical: None,
            };
        }
    };

    // D5 + codex #11: standalone ambiguity query.
    let candidates: Vec<PrefixCandidate> = find_prefix_candidates(engine, source_id, &page.slug).await;
    if candidates.len() > 1 {
        log_phantom_event(
            &resolve_audit_dir(),
            PhantomEventInput {
                phantom_slug: Some(page.slug.clone()),
                canonical_slug: Some(canonical.clone()),
                outcome: PhantomOutcome::Ambiguous,
                source_id: source_id.to_string(),
                candidates: Some(
                    candidates
                        .into_iter()
                        .map(|c| PhantomCandidate {
                            slug: c.slug,
                            connection_count: c.connection_count,
                        })
                        .collect(),
                ),
                ..Default::default()
            },
        );
        return RedirectResult {
            outcome: RedirectOutcome::Ambiguous,
            canonical: Some(canonical),
        };
    }

    // Round 27/29/30: bi-directional drift check.
    if fence_db_drift(page, brain_dir) {
        log_phantom_event(
            &resolve_audit_dir(),
            PhantomEventInput {
                phantom_slug: Some(page.slug.clone()),
                canonical_slug: Some(canonical.clone()),
                outcome: PhantomOutcome::Drift,
                source_id: source_id.to_string(),
                ..Default::default()
            },
        );
        return RedirectResult {
            outcome: RedirectOutcome::Drift,
            canonical: Some(canonical),
        };
    }

    // D10: dry-run preview — no FS / DB / audit writes.
    if dry_run {
        return RedirectResult {
            outcome: RedirectOutcome::Redirected,
            canonical: Some(canonical),
        };
    }

    // ─── Commit phase ───────────────────────────────────────────────────
    let canonical_path: PathBuf = brain_dir.join(format!("{canonical}.md"));
    if let Err(e) = materialize_canonical_to_disk(engine, &canonical, source_id, &canonical_path).await {
        log_phantom_event(
            &resolve_audit_dir(),
            PhantomEventInput {
                phantom_slug: Some(page.slug.clone()),
                canonical_slug: Some(canonical.clone()),
                outcome: PhantomOutcome::Drift,
                source_id: source_id.to_string(),
                reason: Some(format!("materialize failed: {e}")),
                ..Default::default()
            },
        );
        return RedirectResult {
            outcome: RedirectOutcome::Drift,
            canonical: Some(canonical),
        };
    }

    // Disk-side first: parse phantom's fence and append to canonical's disk
    // fence (dedup-guarded). If this throws, no DB state has moved.
    let phantom_fence = parse_facts_fence(&page.compiled_truth);
    if let Err(e) = append_phantom_fence_rows_to_canonical(&canonical_path, &phantom_fence.facts) {
        log_phantom_event(
            &resolve_audit_dir(),
            PhantomEventInput {
                phantom_slug: Some(page.slug.clone()),
                canonical_slug: Some(canonical.clone()),
                outcome: PhantomOutcome::Drift,
                source_id: source_id.to_string(),
                reason: Some(format!("fence merge failed: {e}")),
                ..Default::default()
            },
        );
        return RedirectResult {
            outcome: RedirectOutcome::Drift,
            canonical: Some(canonical),
        };
    }

    // Refresh canonical's compiled_truth + content_hash so the next `zbrain
    // sync` sees the canonical as unchanged.
    let new_canonical_body = match std::fs::read_to_string(&canonical_path) {
        Ok(b) => b,
        Err(e) => {
            log_phantom_event(
                &resolve_audit_dir(),
                PhantomEventInput {
                    phantom_slug: Some(page.slug.clone()),
                    canonical_slug: Some(canonical.clone()),
                    outcome: PhantomOutcome::Drift,
                    source_id: source_id.to_string(),
                    reason: Some(format!("canonical read failed: {e}")),
                    ..Default::default()
                },
            );
            return RedirectResult {
                outcome: RedirectOutcome::Drift,
                canonical: Some(canonical),
            };
        }
    };
    let reparsed = parse_markdown(&new_canonical_body, &format!("{canonical}.md"), None);
    let canonical_tags = engine.get_tags(&canonical, Some(source_id)).await.unwrap_or_default();
    let new_content_hash = compute_page_content_hash(
        &reparsed.title,
        &reparsed.type_,
        &reparsed.compiled_truth,
        &reparsed.timeline,
        &reparsed.frontmatter,
        &canonical_tags,
    );
    if let Err(e) = engine
        .refresh_page_body(&RefreshPageBodyArgs {
            slug: canonical.clone(),
            source_id: source_id.to_string(),
            compiled_truth: reparsed.compiled_truth.clone(),
            timeline: serde_json::Value::String(reparsed.timeline.clone()),
            content_hash: new_content_hash,
        })
        .await
    {
        // Non-fatal: the redirect still proceeds (DB facts migrated below).
        eprintln!(
            "[zbrain] phantom-redirect: refresh canonical {canonical} failed ({e}); continuing"
        );
    }

    // Lossless DB migration. Re-runs return migrated=0.
    let migrated = match engine.migrate_facts_to_canonical(&page.slug, &canonical, source_id).await {
        Ok(n) => n,
        Err(e) => {
            log_phantom_event(
                &resolve_audit_dir(),
                PhantomEventInput {
                    phantom_slug: Some(page.slug.clone()),
                    canonical_slug: Some(canonical.clone()),
                    outcome: PhantomOutcome::Drift,
                    source_id: source_id.to_string(),
                    reason: Some(format!("migrate failed: {e}")),
                    ..Default::default()
                },
            );
            return RedirectResult {
                outcome: RedirectOutcome::Drift,
                canonical: Some(canonical),
            };
        }
    };

    // DB FK rewrite for the links table (wiki-link text rewrite is a known
    // follow-up — codex #5). Rust `rewrite_links` is currently a no-op on
    // libsql/postgres (registered in KNOWN-GAPS).
    let _ = engine.rewrite_links(&page.slug, &canonical).await;

    // Soft-delete + unlink. Order matters — softDelete first so a concurrent
    // sync that observes the phantom .md gone treats it as a normal deletion.
    let _ = engine.soft_delete_page(&page.slug, Some(source_id)).await;
    let _ = engine.delete_facts_for_page(&page.slug, source_id).await;
    let phantom_path = brain_dir.join(format!("{}.md", page.slug));
    if phantom_path.exists() {
        if let Err(e) = std::fs::remove_file(&phantom_path) {
            eprintln!(
                "[zbrain] phantom-redirect: unlink {} failed ({e}); cycle continues",
                phantom_path.display()
            );
        }
    }

    log_phantom_event(
        &resolve_audit_dir(),
        PhantomEventInput {
            phantom_slug: Some(page.slug.clone()),
            canonical_slug: Some(canonical.clone()),
            outcome: PhantomOutcome::Redirected,
            fact_count: Some(migrated),
            source_id: source_id.to_string(),
            ..Default::default()
        },
    );
    RedirectResult {
        outcome: RedirectOutcome::Redirected,
        canonical: Some(canonical),
    }
}

/// The per-cycle phantom-redirect pass. Called from `run_extract_facts` after
/// the legacy-row guard. Single per-cycle lock is expected to be held by the
/// caller (the cycle orchestrator); this function does NOT re-acquire it.
///
/// Mirrors TS `runPhantomRedirectPass` (minus the lock-acquire branch, which
/// is delegated to the cycle orchestrator per 1-6-2 / grill decision ②).
pub async fn run_phantom_redirect_pass(
    engine: &dyn BrainEngine,
    brain_dir: &str,
    source_id: &str,
    dry_run: bool,
) -> crate::error::Result<PhantomPassResult> {
    let mut result = PhantomPassResult::default();

    let limit = std::env::var(ENV_PHANTOM_LIMIT)
        .ok()
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<usize>().ok().filter(|n| *n > 0))
        .unwrap_or(DEFAULT_PHANTOM_LIMIT);

    // Collect unprefixed (phantom) slugs for this source. `get_all_slugs`
    // works on every engine (including InMemory); the TS path used
    // `execute_raw` with `slug NOT LIKE '%/%'`, but filtering here keeps the
    // pass engine-agnostic without raw SQL.
    let all_slugs: HashSet<String> = engine.get_all_slugs(Some(source_id)).await?;
    let phantoms: Vec<String> = all_slugs.into_iter().filter(|s| !s.contains('/')).collect();

    result.more_pending = phantoms.len() > limit;

    let brain_dir_path = Path::new(brain_dir);
    let mut touched_set = HashSet::new();

    for slug in phantoms.into_iter().take(limit) {
        let page = match engine
            .get_page(
                &slug,
                &GetPageOpts {
                    source_id: Some(source_id.to_string()),
                    include_deleted: false,
                },
            )
            .await?
        {
            Some(p) => p,
            None => continue,
        };
        result.scanned += 1;

        let redirect_result = try_redirect_phantom(engine, &page, source_id, brain_dir_path, dry_run).await;

        match redirect_result.outcome {
            RedirectOutcome::Redirected => {
                result.redirected += 1;
                if !dry_run {
                    if let Some(c) = redirect_result.canonical {
                        touched_set.insert(c);
                    }
                }
            }
            RedirectOutcome::Ambiguous => result.ambiguous += 1,
            RedirectOutcome::Drift => result.skipped_drift += 1,
            RedirectOutcome::NoCanonical => result.no_canonical += 1,
            RedirectOutcome::NotPhantom => result.not_phantom += 1,
        }
    }

    result.touched_canonicals = {
        let mut v: Vec<String> = touched_set.into_iter().collect();
        v.sort();
        v
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FactKind, FactVisibility};

    #[test]
    fn strip_leaves_only_residue() {
        // Phantom stub: just an H1 + a facts fence → empty residue.
        let body = "# alice\n\n## Facts\n\n<!--- zbrain:facts:begin -->\n\
                    | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |\n\
                    |---|-------|------|------------|------------|------------|------------|-------------|--------|---------|\n\
                    | 1 | Alice founded Acme | fact | 0.9 | private | high | 2024-01-01 | | cli | | |\n\
                    <!--- zbrain:facts:end -->\n";
        let residue = strip_fence_and_leading_h1(body);
        assert!(residue.is_empty(), "phantom stub should gate empty, got: '{residue}'");

        // Real page with prose → non-empty residue → not a phantom.
        let real = "# alice\n\nAlice is the CEO of Acme.\n\n## Facts\n\n<!--- zbrain:facts:begin -->\n\
                    | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |\n\
                    |---|-------|------|------------|------------|------------|------------|-------------|--------|---------|\n\
                    | 1 | Alice founded Acme | fact | 0.9 | private | high | 2024-01-01 | | cli | | |\n\
                    <!--- zbrain:facts:end -->\n";
        assert!(!strip_fence_and_leading_h1(real).is_empty());
    }

    #[test]
    fn strip_keeps_prose_under_fence() {
        let body = "# alice\n\nSome notes here.\n\n## Facts\n\n<!--- zbrain:facts:begin -->\n\
                    | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |\n\
                    |---|-------|------|------------|------------|------------|------------|-------------|--------|---------|\n\
                    | 1 | x | fact | 0.9 | private | high | 2024-01-01 | | cli | | |\n\
                    <!--- zbrain:facts:end -->\n";
        let residue = strip_fence_and_leading_h1(body);
        assert!(residue.contains("Some notes here."), "prose should remain: '{residue}'");
    }

    #[test]
    fn content_hash_is_stable_and_ordered() {
        let a = compute_page_content_hash("Alice", "person", "x", "y", &serde_json::json!({}), &["b".to_string(), "a".to_string()]);
        let b = compute_page_content_hash("Alice", "person", "x", "y", &serde_json::json!({}), &["a".to_string(), "b".to_string()]);
        assert_eq!(a, b, "tag order must not change the hash");
        assert!(a.starts_with("sha256:"));
    }

    #[test]
    fn append_fence_dedups_and_counts() {
        let dir = tempfile::TempDir::new().unwrap();
        let canon = dir.path().join("people/alice-example.md");
        std::fs::create_dir_all(canon.parent().unwrap()).unwrap();
        std::fs::write(
            &canon,
            "---\ntype: person\ntitle: Alice\n---\n\n# Alice\n\n## Facts\n\n<!--- zbrain:facts:begin -->\n\
             | # | claim | kind | confidence | visibility | notability | valid_from | valid_until | source | context |\n\
             |---|-------|------|------------|------------|------------|------------|-------------|--------|---------|\n\
             | 1 | Alice lives in NYC | fact | 0.9 | private | high | 2024-01-01 | | cli | | |\n\
             <!--- zbrain:facts:end -->\n",
        )
        .unwrap();

        let phantom_facts = vec![
            FenceFact {
                row_num: 1,
                claim: "Alice lives in NYC".to_string(), // dup → skipped
                kind: FactKind::Fact,
                confidence: 0.9,
                visibility: FactVisibility::Private,
                notability: "high".to_string(),
                source: Some("cli".to_string()),
                context: None,
                active: true,
                superseded_by: None,
                forgotten: false,
                claim_metric: None,
                claim_value: None,
                claim_unit: None,
                claim_period: None,
                valid_from: Some("2024-01-01".to_string()),
                valid_until: None,
            },
            FenceFact {
                row_num: 2,
                claim: "Alice founded Acme".to_string(), // new → appended
                kind: FactKind::Fact,
                confidence: 0.9,
                visibility: FactVisibility::Private,
                notability: "high".to_string(),
                source: Some("cli".to_string()),
                context: None,
                active: true,
                superseded_by: None,
                forgotten: false,
                claim_metric: None,
                claim_value: None,
                claim_unit: None,
                claim_period: None,
                valid_from: Some("2024-02-01".to_string()),
                valid_until: None,
            },
        ];

        let appended = append_phantom_fence_rows_to_canonical(&canon, &phantom_facts).unwrap();
        assert_eq!(appended, 1, "only the new fact should be appended");

        let merged = parse_facts_fence(&std::fs::read_to_string(&canon).unwrap());
        assert_eq!(merged.facts.len(), 2, "canonical now has 2 rows");
        assert!(
            merged
                .facts
                .iter()
                .any(|f| f.row_num == 2 && f.claim == "Alice founded Acme")
        );
    }
}
