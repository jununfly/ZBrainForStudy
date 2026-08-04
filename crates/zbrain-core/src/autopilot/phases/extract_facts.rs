//! `extract_facts` cycle phase — Rust port of `src/core/cycle/extract-facts.ts`.
//!
//! Reconciles the facts DB index from the `## Facts` fence on each entity
//! page. The fence is canonical: for every affected page this phase
//! parses the fence, wipes the page's DB index (`delete_facts_for_page`)
//! and re-inserts from the parsed rows (`insert_fact`). After the phase the
//! DB index for every affected page byte-matches the fence (modulo
//! embeddings + runtime-derived fields).
//!
//! This is a **fence-parsing + DB reconciliation** phase — it does NOT call
//! an LLM for generation. The only AI touch is optional batch embedding,
//! which is skipped in this port (facts insert with `NULL` embedding; the
//! TS fallback path is identical — `drift_score` gracefully returns null and
//! consolidation falls back to recency). Embeddings are
//! registered in docs/plans/KNOWN-GAPS.md (G60).
//!
//! The phantom-redirect pre-pass (TS v0.35.5) is now ported — see
//! `super::phantom_redirect::run_phantom_redirect_pass`, wired into
//! `run_extract_facts` after the legacy-row guard and before the main
//! reconcile. It reuses the cycle-level advisory file lock held by the
//! orchestrator (no separate lock; `phantom_lock_busy` is effectively false)
//! and is bounded at 50 redirects/cycle (`ZBRAIN_PHANTOM_REDIRECT_LIMIT`).
//! G61 is closed (2026-08-03).

use crate::engine::BrainEngine;
use crate::error::Result as ZbResult;
use crate::facts_fence::{parse_facts_fence, FenceFact};
use crate::types::{FactInsertStatus, NewFact};
use crate::GetPageOpts;
use super::phantom_redirect::run_phantom_redirect_pass;

/// Default `source` value when a fence row doesn't carry one. Mirrors TS
/// `FENCE_SOURCE_DEFAULT`.
pub const FENCE_SOURCE_DEFAULT: &str = "fence:reconcile";

/// Options for [`extract_facts_from_fence_text`].
#[derive(Debug, Clone, Default)]
pub struct ExtractFromFenceOpts {
    /// Override for "today" (UTC `YYYY-MM-DD`) used by the forgotten-row
    /// `valid_until` derivation. Only set by tests for determinism.
    pub now_override: Option<String>,
    /// v0.35.4 (D-ENG-1 + D-CDX-5): fallback `valid_from` when the fence row
    /// lacks an explicit `validFrom:`. Threaded from `page.effective_date`.
    pub page_effective_date: Option<String>,
}

/// Options for [`run_extract_facts`].
#[derive(Debug, Clone, Default)]
pub struct ExtractFactsOpts {
    /// Subset of slugs to reconcile. `None` = walk every page in the brain.
    pub slugs: Option<Vec<String>>,
    /// Dry-run: parse + count, no DB writes.
    pub dry_run: bool,
    /// Optional source_id override for multi-source brains. Default `"default"`.
    pub source_id: Option<String>,
    /// v0.35.5: brain directory for the phantom-redirect pre-pass. When
    /// `None` the pre-pass is skipped.
    pub brain_dir: Option<String>,
}

/// Result envelope for [`run_extract_facts`]. Status mapping (ok / warn /
/// fail) happens in the `execute_phase` caller.
#[derive(Debug, Clone, Default)]
pub struct ExtractFactsResult {
    pub pages_scanned: u64,
    pub pages_with_facts: u64,
    pub facts_inserted: u64,
    pub facts_deleted: u64,
    pub legacy_rows_pending: i64,
    pub guard_triggered: bool,
    pub warnings: Vec<String>,
    // v0.35.5 phantom-redirect pre-pass counts (deferred in this port).
    pub phantoms_scanned: u64,
    pub phantoms_redirected: u64,
    pub phantoms_ambiguous: u64,
    pub phantoms_skipped_drift: u64,
    pub phantoms_lock_busy: bool,
    pub phantoms_more_pending: bool,
}

/// Seed map for common founder/company metrics. Free-text labels normalize
/// to lowercase snake_case so trajectory queries don't fragment across
/// capitalization variants.
const METRIC_NORMALIZATION_MAP: &[(&str, &str)] = &[
    ("mrr", "mrr"),
    ("monthly recurring revenue", "mrr"),
    ("arr", "arr"),
    ("annual recurring revenue", "arr"),
    ("revenue", "revenue"),
    ("burn", "burn_rate"),
    ("burn rate", "burn_rate"),
    ("runway", "runway"),
    ("cash", "cash"),
    ("gross margin", "gross_margin"),
    ("fundraise", "fundraise"),
    ("raise", "fundraise"),
    ("headcount", "headcount"),
    ("team size", "team_size"),
    ("team", "team_size"),
    ("users", "users"),
    ("mau", "mau"),
    ("monthly active users", "mau"),
    ("dau", "dau"),
    ("daily active users", "dau"),
    ("churn", "churn_rate"),
    ("churn rate", "churn_rate"),
    ("cac", "cac"),
    ("ltv", "ltv"),
];

/// Normalize a free-text metric label to lowercase snake_case. Known labels
/// map to canonical names; unknown labels are lowercased + whitespace-collapsed
/// → underscores. Returns `None` for empty / whitespace-only input. Mirrors TS
/// `normalizeMetricLabel`.
pub fn normalize_metric_label(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let trimmed = raw.trim().to_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    if let Some((_, seed)) = METRIC_NORMALIZATION_MAP
        .iter()
        .find(|(k, _)| *k == trimmed.as_str())
    {
        return Some((*seed).to_string());
    }
    let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join("_");
    let cleaned: String = collapsed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Map parsed fence rows into engine-ready insert rows (`NewFact`).
///
/// Pure: no engine call, no I/O. Structural port of TS
/// `extractFactsFromFenceText` (minus the `FenceExtractedFact` superset type —
/// Rust `NewFact` now carries `row_num` + `source_markdown_slug`, so no
/// separate type is needed).
pub fn extract_facts_from_fence_text(
    facts: &[FenceFact],
    slug: &str,
    _source_id: &str,
    opts: &ExtractFromFenceOpts,
) -> Vec<NewFact> {
    let today = opts
        .now_override
        .clone()
        .unwrap_or_else(|| crate::time::today_utc_date());
    let page_date_fallback = opts.page_effective_date.clone();

    facts
        .iter()
        .map(|f| {
            // valid_from precedence: fence > pageEffectiveDate > None (engine
            // insert_fact defaults to now()).
            let valid_from = f.valid_from.clone().or_else(|| page_date_fallback.clone());

            // valid_until derivation:
            //   1. Explicit validUntil in the fence → honor as-is.
            //   2. Inactive (forgotten OR unrecognized-inactive) → today.
            //   3. Otherwise → null.
            let valid_until: Option<String> = if f.valid_until.is_some() {
                f.valid_until.clone()
            } else if !f.active && (f.forgotten || f.superseded_by.is_none()) {
                Some(today.clone())
            } else {
                None
            };

            NewFact {
                fact: f.claim.clone(),
                kind: Some(f.kind.clone()),
                entity_slug: Some(slug.to_string()),
                visibility: Some(f.visibility.clone()),
                context: f.context.clone(),
                valid_from,
                valid_until,
                source: f
                    .source
                    .clone()
                    .unwrap_or_else(|| FENCE_SOURCE_DEFAULT.to_string()),
                source_session: None,
                confidence: Some(f.confidence),
                notability: Some(f.notability.clone()),
                claim_metric: normalize_metric_label(f.claim_metric.as_deref()),
                claim_value: f.claim_value,
                claim_unit: f.claim_unit.clone(),
                claim_period: f.claim_period.clone(),
                event_type: None,
                row_num: Some(f.row_num),
                source_markdown_slug: Some(slug.to_string()),
            }
        })
        .collect()
}

/// Run the `extract_facts` phase against the current brain state. Returns an
/// [`ExtractFactsResult`] envelope; status mapping (ok / warn / fail) happens
/// in the `execute_phase` caller.
pub async fn run_extract_facts(
    engine: &dyn BrainEngine,
    opts: &ExtractFactsOpts,
) -> ZbResult<ExtractFactsResult> {
    let source_id = opts
        .source_id
        .clone()
        .unwrap_or_else(|| "default".to_string());
    let mut result = ExtractFactsResult::default();

    // ── Empty-fence guard (Codex R2-#7) ────────────────────────────────
    // Refuse the destructive reconcile pass while legacy v0.31 fact rows
    // (row_num NULL but entity_slug NOT NULL) still linger.
    let legacy = engine.count_legacy_fact_rows().await?;
    result.legacy_rows_pending = legacy;
    if legacy > 0 {
        result.guard_triggered = true;
        result.warnings.push(format!(
            "extract_facts: {legacy} legacy v0.31 fact rows pending fence backfill. \
             Run `zbrain apply-migrations --yes` to complete v0_32_2 before this phase \
             can safely reconcile fence → DB."
        ));
        return Ok(result);
    }

    // ── v0.35.5: phantom-redirect pre-pass ───────────────────────────
    // Runs BEFORE the main reconcile loop. Needs disk access to canonical
    // pages; skipped when `brain_dir` is absent (e.g. pure-DB callers). The
    // pass reuses the caller-held cycle advisory lock (1-6-2), so it does not
    // acquire its own. Scenario B (phantom had only-on-disk fence, no DB
    // facts yet) is covered by unioning the touched canonicals into the
    // reconcile slug set below.
    let mut phantom_touched: Vec<String> = Vec::new();
    if let Some(brain_dir) = &opts.brain_dir {
        match run_phantom_redirect_pass(engine, brain_dir, &source_id, opts.dry_run).await {
            Ok(pass) => {
                result.phantoms_scanned = pass.scanned;
                result.phantoms_redirected = pass.redirected;
                result.phantoms_ambiguous = pass.ambiguous;
                result.phantoms_skipped_drift = pass.skipped_drift;
                result.phantoms_lock_busy = pass.lock_busy;
                result.phantoms_more_pending = pass.more_pending;
                // `no_canonical` / `not_phantom` are informational; the result
                // envelope doesn't track them per-row but the audit log does.
                phantom_touched = pass.touched_canonicals;
            }
            Err(e) => {
                result.warnings.push(format!(
                    "extract_facts: phantom-redirect pre-pass failed ({e}); continuing with reconcile"
                ));
            }
        }
    }

    // ── Resolve target slug set ───────────────────────────────────────
    // Presence — not length — distinguishes modes: `slugs: Some(vec![])` is a
    // real incremental no-op; `None` is a full-brain walk.
    let slugs: Vec<String> = match &opts.slugs {
        Some(s) => {
            let mut base = s.clone();
            base.extend(phantom_touched.iter().cloned());
            base.sort();
            base.dedup();
            base
        }
        None => {
            let mut all: Vec<String> = engine
                .get_all_slugs(Some(&source_id))
                .await?
                .into_iter()
                .collect();
            all.extend(phantom_touched.iter().cloned());
            all.sort();
            all.dedup();
            all
        }
    };

    // ── Reconcile each page ───────────────────────────────────────────
    for slug in slugs {
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

        let body = page.compiled_truth.clone();
        let parsed = parse_facts_fence(&body);
        for w in &parsed.warnings {
            result.warnings.push(format!("{slug}: {w}"));
        }
        if !parsed.facts.is_empty() {
            result.pages_with_facts += 1;
        }

        if opts.dry_run {
            continue;
        }

        // Wipe-and-reinsert per page (scoped to source_markdown_slug = slug).
        let deleted = engine.delete_facts_for_page(&slug, &source_id).await?;
        result.facts_deleted += deleted as u64;

        if parsed.facts.is_empty() {
            continue;
        }

        // v0.35.4 (D-ENG-1) — thread page.effective_date as the fallback
        // valid_from.
        let extracted = extract_facts_from_fence_text(
            &parsed.facts,
            &slug,
            &source_id,
            &ExtractFromFenceOpts {
                page_effective_date: page.effective_date.clone(),
                ..Default::default()
            },
        );

        // Embeddings: skipped (NULL embedding fallback, matches TS fail-open
        // path); registered in docs/plans/KNOWN-GAPS.md (G60).
        for fact in &extracted {
            match engine.insert_fact(&source_id, &slug, fact).await? {
                FactInsertStatus::Duplicate => {}
                FactInsertStatus::Inserted | FactInsertStatus::Superseded => {
                    result.facts_inserted += 1;
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_metric_label_variants() {
        assert_eq!(normalize_metric_label(Some("MRR")), Some("mrr".to_string()));
        assert_eq!(
            normalize_metric_label(Some("Monthly Active Users")),
            Some("mau".to_string())
        );
        assert_eq!(
            normalize_metric_label(Some("Net Promoter Score")),
            Some("net_promoter_score".to_string())
        );
        // Unknown label → lowercased + spaces → underscores.
        assert_eq!(
            normalize_metric_label(Some("Custom Weird Metric")),
            Some("custom_weird_metric".to_string())
        );
        assert_eq!(normalize_metric_label(None), None);
        assert_eq!(normalize_metric_label(Some("   ")), None);
    }

    #[test]
    fn extract_maps_fence_row_to_new_fact() {
        let f = FenceFact {
            row_num: 2,
            claim: "Alice joined Acme".to_string(),
            kind: crate::types::FactKind::Fact,
            confidence: 0.95,
            visibility: crate::types::FactVisibility::Private,
            notability: "high".to_string(),
            valid_from: Some("2024-01-01".to_string()),
            valid_until: None,
            source: Some("cli:think".to_string()),
            context: None,
            active: true,
            superseded_by: None,
            forgotten: false,
            claim_metric: Some("MRR".to_string()),
            claim_value: Some(1000.0),
            claim_unit: Some("usd".to_string()),
            claim_period: Some("monthly".to_string()),
        };
        let out = extract_facts_from_fence_text(
            &[f],
            "alice",
            "default",
            &ExtractFromFenceOpts::default(),
        );
        assert_eq!(out.len(), 1);
        let nf = &out[0];
        assert_eq!(nf.fact, "Alice joined Acme");
        assert_eq!(nf.entity_slug.as_deref(), Some("alice"));
        assert_eq!(nf.valid_from.as_deref(), Some("2024-01-01"));
        assert_eq!(nf.source, "cli:think");
        assert_eq!(nf.row_num, Some(2));
        assert_eq!(nf.source_markdown_slug.as_deref(), Some("alice"));
        assert_eq!(nf.claim_metric.as_deref(), Some("mrr"));
        assert_eq!(nf.claim_value, Some(1000.0));
    }

    #[test]
    fn forgotten_row_derives_valid_until_today() {
        let f = FenceFact {
            row_num: 1,
            claim: "stale claim".to_string(),
            kind: crate::types::FactKind::Fact,
            confidence: 0.5,
            visibility: crate::types::FactVisibility::Private,
            notability: "low".to_string(),
            valid_from: None,
            valid_until: None,
            source: None,
            context: None,
            active: false,
            superseded_by: None,
            forgotten: true,
            claim_metric: None,
            claim_value: None,
            claim_unit: None,
            claim_period: None,
        };
        let out = extract_facts_from_fence_text(
            &[f],
            "bob",
            "default",
            &ExtractFromFenceOpts {
                now_override: Some("2026-07-27".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].valid_until.as_deref(), Some("2026-07-27"));
    }
}
