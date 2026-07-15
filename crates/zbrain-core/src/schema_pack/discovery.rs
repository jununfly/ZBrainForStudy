//! Schema-pack discovery & repair — SQL-free, engine-backed implementation.
//!
//! Port of `src/core/schema-pack/{detect,suggest,review,sync}.ts`.
//!
//! The TS originals run raw SQL via `engine.execute_raw` (per-prefix
//! `substring(slug from '^[^/]+/')` clustering and a chunked `pages.type`
//! backfill CTE). The Rust `BrainEngine` trait deliberately has **no**
//! `execute_raw` escape hatch (see `engine.rs`), so this module performs the
//! same work in memory: it pulls pages through the typed `list_pages` API,
//! clusters by slug prefix in Rust, and backfills `page_type` with the typed
//! `put_page` (patch) API. This is correct and respects the engine's
//! abstraction; for very large corpora a later optimization may add typed
//! engine methods, but the tracer-bullet behavior is identical.

use crate::engine::{BrainEngine, Page, PageFilters, PageInput};
use crate::schema_pack::manifest::{PageTypeDefinition, PackPrimitive, SchemaPackManifest};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Pure data types
// ---------------------------------------------------------------------------

/// Lightweight page view consumed by the pure clustering logic. Decouples the
/// clustering algorithm from the 20-field [`Page`] struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageView {
    pub slug: String,
    pub page_type: String,
}

/// A discovered prefix cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefixCluster {
    pub prefix: String,
    pub page_count: usize,
    pub sample_types: Vec<String>,
    /// Suggested type name (prefix minus trailing slash).
    pub suggested_type: String,
}

/// The candidate manifest subset derived by `detect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectCandidate {
    pub api_version: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub page_types: Vec<PageTypeDefinition>,
    pub takes_kinds: Vec<String>,
}

/// Full result of [`run_detect`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectResult {
    pub total_pages: usize,
    pub typed_pages: usize,
    pub untyped_pages: usize,
    pub candidate: DetectCandidate,
    pub prefixes: Vec<PrefixCluster>,
}

/// A single discovery suggestion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suggestion {
    pub kind: String,
    pub summary: String,
    pub confidence: f64,
    pub evidence: Vec<String>,
}

/// Result of [`run_suggest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SuggestResult {
    pub suggestions: Vec<Suggestion>,
    pub notes: Vec<String>,
    pub source_id: String,
}

/// One row of [`ReviewCandidatesResult`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateReview {
    pub prefix: String,
    pub page_count: usize,
    pub suggested_type: String,
    pub in_active_pack: bool,
}

/// Result of [`run_review_candidates`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCandidatesResult {
    pub candidates: Vec<CandidateReview>,
    pub applied: Option<String>,
    pub source_id: String,
}

/// An orphaned (untyped) page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrphanRef {
    pub slug: String,
    pub source_id: String,
}

/// Result of [`run_review_orphans`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewOrphansResult {
    pub orphans: Vec<OrphanRef>,
    pub orphan_count: usize,
    pub source_id: String,
}

/// Per-prefix outcome of [`run_sync_core`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerPrefixResult {
    pub type_name: String,
    pub prefix: String,
    pub would_apply: usize,
    pub sample_slugs: Vec<String>,
    pub dead_prefix: bool,
    pub applied: usize,
}

/// Result of [`run_sync_core`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResult {
    pub schema_version: u8,
    pub apply: bool,
    pub pack_identity: Option<String>,
    pub per_prefix: Vec<PerPrefixResult>,
    pub total_would_apply: usize,
    pub total_applied: usize,
}

/// Detection options.
#[derive(Debug, Clone, Copy)]
pub struct DetectOpts {
    pub min_pages_per_prefix: usize,
    pub max_types: usize,
}

impl Default for DetectOpts {
    fn default() -> Self {
        Self {
            min_pages_per_prefix: 5,
            max_types: 50,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------

/// Extract the clustering prefix from a slug: first path segment + `/`.
/// `"people/alice.md"` -> `"people/"`; `"standalone"` -> `""` (no directory).
pub fn slug_prefix(slug: &str) -> String {
    match slug.split_once('/') {
        Some((head, _)) => format!("{head}/"),
        None => String::new(),
    }
}

/// Cluster pages by slug prefix. Pure: takes lightweight [`PageView`]s.
///
/// - Groups by prefix, counting pages and collecting distinct non-empty types.
/// - Drops prefixes with fewer than `min_pages_per_prefix` pages.
/// - Sorts by page count descending and caps at `max_types`.
pub fn cluster_pages(pages: &[PageView], min_pages_per_prefix: usize, max_types: usize) -> Vec<PrefixCluster> {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, (usize, std::collections::BTreeSet<String>)> = BTreeMap::new();
    for p in pages {
        let prefix = slug_prefix(&p.slug);
        if prefix.is_empty() {
            continue; // top-level slugs carry no prefix signal
        }
        let entry = groups.entry(prefix.clone()).or_insert((0, std::collections::BTreeSet::new()));
        entry.0 += 1;
        if !p.page_type.is_empty() {
            entry.1.insert(p.page_type.clone());
        }
    }

    let mut clusters: Vec<PrefixCluster> = groups
        .into_iter()
        .filter(|(_, (cnt, _))| *cnt >= min_pages_per_prefix)
        .map(|(prefix, (cnt, types))| PrefixCluster {
            prefix: prefix.clone(),
            page_count: cnt,
            sample_types: types.into_iter().collect(),
            suggested_type: prefix.trim_end_matches('/').to_string(),
        })
        .collect();

    // Most popular prefixes first.
    clusters.sort_by(|a, b| b.page_count.cmp(&a.page_count));
    clusters.truncate(max_types);
    clusters
}

/// Build a candidate manifest from prefix clusters.
pub fn build_candidate(prefixes: &[PrefixCluster], max_types: usize) -> DetectCandidate {
    let page_types: Vec<PageTypeDefinition> = prefixes
        .iter()
        .take(max_types)
        .map(|c| PageTypeDefinition {
            name: c.suggested_type.clone(),
            primitive: PackPrimitive::Entity,
            path_prefixes: vec![c.prefix.clone()],
            aliases: vec![],
            extractable: false,
            expert_routing: false,
        })
        .collect();

    DetectCandidate {
        api_version: "zbrain-schema-pack-v1".to_string(),
        name: "detected-candidate".to_string(),
        version: "0.0.0".to_string(),
        description: "Auto-detected candidate schema pack".to_string(),
        page_types,
        takes_kinds: vec![],
    }
}

/// Determine a descriptive type name from a prefix (singularized-ish stub).
/// Keeps it simple: the prefix without the trailing slash.
pub fn suggested_type_name(prefix: &str) -> String {
    prefix.trim_end_matches('/').to_string()
}

/// Pure heuristic suggestions: one `add_type` per prefix at confidence 0.5.
pub fn heuristic_suggestions(detected: &DetectResult) -> Vec<Suggestion> {
    detected
        .prefixes
        .iter()
        .map(|c| Suggestion {
            kind: "add_type".to_string(),
            summary: format!("Add type `{}`", c.suggested_type),
            confidence: 0.5,
            evidence: vec![format!("{}: {} pages", c.prefix, c.page_count)],
        })
        .collect()
}

/// Build a full [`DetectResult`] from raw page views. Pure.
pub fn detect_from_views(pages: &[PageView], opts: DetectOpts) -> DetectResult {
    let total = pages.len();
    let typed = pages.iter().filter(|p| !p.page_type.is_empty()).count();
    let untyped = total - typed;
    let prefixes = cluster_pages(pages, opts.min_pages_per_prefix, opts.max_types);
    let candidate = build_candidate(&prefixes, opts.max_types);
    DetectResult {
        total_pages: total,
        typed_pages: typed,
        untyped_pages: untyped,
        candidate,
        prefixes,
    }
}

// ---------------------------------------------------------------------------
// Engine-backed verbs (memory-based)
// ---------------------------------------------------------------------------

/// Fetch all live pages for a source, paginating through `list_pages`.
async fn fetch_all_pages(engine: &dyn BrainEngine, source_id: &str) -> crate::Result<Vec<Page>> {
    let mut out = Vec::new();
    let limit = 1000usize;
    let mut offset = 0usize;
    loop {
        let batch = engine
            .list_pages(&PageFilters {
                source_id: Some(source_id.to_string()),
                include_deleted: false,
                limit: Some(limit),
                offset: Some(offset),
                ..Default::default()
            })
            .await?;
        let n = batch.len();
        out.extend(batch);
        if n < limit {
            break;
        }
        offset += n;
    }
    Ok(out)
}

/// Run detection over a source: cluster its pages and derive a candidate pack.
pub async fn run_detect(
    engine: &dyn BrainEngine,
    source_id: &str,
    opts: DetectOpts,
) -> crate::Result<DetectResult> {
    let pages = fetch_all_pages(engine, source_id).await?;
    let views: Vec<PageView> = pages
        .into_iter()
        .map(|p| PageView {
            slug: p.slug,
            page_type: p.page_type,
        })
        .collect();
    Ok(detect_from_views(&views, opts))
}

/// Run detection then produce suggestions. Hermetic by default: when no
/// `suggest_fn` is supplied, returns the deterministic heuristic fallback and
/// never calls an LLM. The `suggest_fn` seam lets callers inject an LLM
/// refiner without changing the signature.
pub async fn run_suggest<F>(
    engine: &dyn BrainEngine,
    source_id: &str,
    opts: DetectOpts,
    suggest_fn: Option<F>,
) -> crate::Result<SuggestResult>
where
    F: FnOnce(&DetectResult) -> Vec<Suggestion>,
{
    let detected = run_detect(engine, source_id, opts).await?;
    let used_fn = suggest_fn.is_some();
    let suggestions = match suggest_fn {
        Some(f) => {
            let mut s = f(&detected);
            // Clamp confidence to [0,1], sort desc, dedup by summary.
            for sug in &mut s {
                sug.confidence = sug.confidence.clamp(0.0, 1.0);
            }
            s.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));
            s.dedup_by(|a, b| a.summary == b.summary);
            s
        }
        None => heuristic_suggestions(&detected),
    };
    let notes = if used_fn {
        vec!["used provided suggest_fn".to_string()]
    } else {
        vec!["hermetic heuristic fallback (no LLM)".to_string()]
    };
    Ok(SuggestResult {
        suggestions,
        notes,
        source_id: source_id.to_string(),
    })
}

/// Review detected candidates against the active pack (injected). If
/// `apply_slug` is set, writes a delta JSON file under
/// `~/.zbrain/schema-pack-deltas/` and records it as `applied`.
pub async fn run_review_candidates(
    engine: &dyn BrainEngine,
    source_id: &str,
    active_pack: Option<&SchemaPackManifest>,
    apply_slug: Option<&str>,
    opts: DetectOpts,
) -> crate::Result<ReviewCandidatesResult> {
    let detected = run_detect(engine, source_id, opts).await?;
    let active_types: Vec<String> = active_pack
        .map(|p| p.page_types.iter().map(|pt| pt.name.clone()).collect())
        .unwrap_or_default();

    let candidates: Vec<CandidateReview> = detected
        .prefixes
        .iter()
        .map(|c| CandidateReview {
            prefix: c.prefix.clone(),
            page_count: c.page_count,
            suggested_type: c.suggested_type.clone(),
            in_active_pack: active_types.iter().any(|t| t == &c.suggested_type),
        })
        .collect();

    let applied = match apply_slug {
        Some(slug) => {
            write_candidate_delta(&detected.candidate, active_pack, slug)?;
            Some(slug.to_string())
        }
        None => None,
    };

    Ok(ReviewCandidatesResult {
        candidates,
        applied,
        source_id: source_id.to_string(),
    })
}

/// Write a candidate delta JSON file for `apply_slug`. Best-effort: errors are
/// returned (the CLI decides whether to fail loud), but the file write is the
/// only side effect.
fn write_candidate_delta(
    candidate: &DetectCandidate,
    active_pack: Option<&SchemaPackManifest>,
    apply_slug: &str,
) -> crate::Result<()> {
    let pack_identity = active_pack.map(|p| p.name.clone()).unwrap_or_else(|| "none".to_string());
    let dir = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".zbrain").join("schema-pack-deltas"))
        .unwrap_or_else(|_| PathBuf::from(".zbrain").join("schema-pack-deltas"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| crate::Error::new("Io", "io_error", format!("cannot create deltas dir: {e}")))?;
    let ts = chrono::Utc::now().timestamp();
    let path = dir.join(format!("{pack_identity}-{ts}.json"));
    let payload = serde_json::json!({
        "apply_slug": apply_slug,
        "candidate": candidate,
    });
    let bytes = serde_json::to_string_pretty(&payload)
        .map_err(|e| crate::Error::new("SchemaPack", "json_error", format!("cannot serialize candidate: {e}")))?;
    std::fs::write(&path, bytes)
        .map_err(|e| crate::Error::new("Io", "io_error", format!("cannot write candidate delta: {e}")))?;
    Ok(())
}

/// List untyped pages (orphans) for a source. Caps at 1000.
pub async fn run_review_orphans(
    engine: &dyn BrainEngine,
    source_id: &str,
) -> crate::Result<ReviewOrphansResult> {
    let pages = fetch_all_pages(engine, source_id).await?;
    let orphans: Vec<OrphanRef> = pages
        .into_iter()
        .filter(|p| p.page_type.is_empty())
        .take(1000)
        .map(|p| OrphanRef {
            slug: p.slug,
            source_id: p.source_id,
        })
        .collect();
    let count = orphans.len();
    Ok(ReviewOrphansResult {
        orphans,
        orphan_count: count,
        source_id: source_id.to_string(),
    })
}

/// Backfill `page_type` from the active pack's path prefixes. Dry-run by
/// default (`apply = false`); with `apply = true`, patches each matching
/// untyped page via `put_page` (carrying the existing title/compiled_truth so
/// only `page_type` changes).
pub async fn run_sync_core(
    engine: &dyn BrainEngine,
    source_id: Option<&str>,
    active_pack: Option<&SchemaPackManifest>,
    apply: bool,
    batch_size: usize,
) -> crate::Result<SyncResult> {
    let pack = match active_pack {
        Some(p) => p,
        None => {
            return Ok(SyncResult {
                schema_version: 1,
                apply,
                pack_identity: None,
                per_prefix: vec![],
                total_would_apply: 0,
                total_applied: 0,
            })
        }
    };

    // Collect (type, prefix) pairs from the active pack.
    let mut rules: Vec<(String, String)> = Vec::new();
    for pt in &pack.page_types {
        for prefix in &pt.path_prefixes {
            rules.push((pt.name.clone(), prefix.clone()));
        }
    }

    let pages = match source_id {
        Some(sid) => fetch_all_pages(engine, sid).await?,
        None => {
            // No source scope: pull default source pages.
            fetch_all_pages(engine, "default").await?
        }
    };

    let mut per_prefix = Vec::new();
    let mut total_would = 0usize;
    let mut total_applied = 0usize;

    for (type_name, prefix) in rules {
        // Match TS `source_path LIKE prefix%` on untyped pages.
        let matches: Vec<Page> = pages
            .iter()
            .filter(|p| {
                p.page_type.is_empty()
                    && p.source_path
                        .as_deref()
                        .map(|sp| sp.starts_with(&prefix))
                        .unwrap_or(false)
            })
            .cloned()
            .collect();

        let would_apply = matches.len();
        let sample_slugs: Vec<String> = matches
            .iter()
            .take(10)
            .map(|p| p.slug.clone())
            .collect();
        let dead_prefix = would_apply == 0;

        let mut applied = 0usize;
        if apply {
            // Respect batch_size as a cap on applied pages per prefix.
            let capped: Vec<&Page> = matches.iter().take(batch_size.max(1)).collect();
            for page in capped {
                let input = PageInput {
                    page_type: type_name.clone(),
                    title: page.title.clone(),
                    compiled_truth: page.compiled_truth.clone(),
                    ..Default::default()
                };
                engine
                    .put_page(&page.slug, Some(&page.source_id), &input)
                    .await?;
                applied += 1;
            }
        }

        total_would += would_apply;
        total_applied += applied;
        per_prefix.push(PerPrefixResult {
            type_name,
            prefix,
            would_apply,
            sample_slugs,
            dead_prefix,
            applied,
        });
    }

    Ok(SyncResult {
        schema_version: 1,
        apply,
        pack_identity: Some(pack.name.clone()),
        per_prefix,
        total_would_apply: total_would,
        total_applied,
    })
}

use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    fn view(slug: &str, page_type: &str) -> PageView {
        PageView {
            slug: slug.to_string(),
            page_type: page_type.to_string(),
        }
    }

    #[test]
    fn slug_prefix_extraction() {
        assert_eq!(slug_prefix("people/alice.md"), "people/");
        assert_eq!(slug_prefix("notes/x.md"), "notes/");
        assert_eq!(slug_prefix("standalone"), "");
    }

    #[test]
    fn cluster_groups_and_filters() {
        let pages = vec![
            view("people/a", ""),
            view("people/b", ""),
            view("people/c", ""),
            view("people/d", ""),
            view("people/e", "person"),
            view("notes/x", ""),
            view("notes/y", ""),
            view("standalone", ""), // no prefix -> ignored
        ];
        let clusters = cluster_pages(&pages, 3, 50);
        // people/ has 5 >= 3; notes/ has 2 < 3 -> dropped
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].prefix, "people/");
        assert_eq!(clusters[0].page_count, 5);
        assert_eq!(clusters[0].suggested_type, "people");
        assert_eq!(clusters[0].sample_types, vec!["person".to_string()]);
    }

    #[test]
    fn cluster_caps_max_types() {
        let pages: Vec<PageView> = (0..10)
            .flat_map(|i| {
                let p = format!("cat{i}/");
                (0..5).map(move |j| view(&format!("{p}page{j}"), "")).collect::<Vec<_>>()
            })
            .collect();
        let clusters = cluster_pages(&pages, 3, 3);
        assert_eq!(clusters.len(), 3);
    }

    #[test]
    fn build_candidate_maps_prefixes() {
        let clusters = vec![PrefixCluster {
            prefix: "people/".to_string(),
            page_count: 5,
            sample_types: vec!["person".to_string()],
            suggested_type: "people".to_string(),
        }];
        let c = build_candidate(&clusters, 50);
        assert_eq!(c.page_types.len(), 1);
        assert_eq!(c.page_types[0].name, "people");
        assert_eq!(c.page_types[0].primitive, PackPrimitive::Entity);
        assert_eq!(c.page_types[0].path_prefixes, vec!["people/".to_string()]);
        assert!(!c.page_types[0].extractable);
    }

    #[test]
    fn detect_from_views_counts() {
        let pages = vec![
            view("people/a", "person"),
            view("people/b", ""),
            view("notes/x", ""),
        ];
        let r = detect_from_views(&pages, DetectOpts::default());
        assert_eq!(r.total_pages, 3);
        assert_eq!(r.typed_pages, 1);
        assert_eq!(r.untyped_pages, 2);
        // notes/ has 1 page < default min 5, people/ has 2 < 5 -> no clusters
        assert_eq!(r.prefixes.len(), 0);
    }

    #[test]
    fn heuristic_suggestions_one_per_prefix() {
        let detected = DetectResult {
            total_pages: 0,
            typed_pages: 0,
            untyped_pages: 0,
            candidate: build_candidate(&[], 50),
            prefixes: vec![PrefixCluster {
                prefix: "people/".to_string(),
                page_count: 9,
                sample_types: vec![],
                suggested_type: "people".to_string(),
            }],
        };
        let s = heuristic_suggestions(&detected);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, "add_type");
        assert_eq!(s[0].confidence, 0.5);
        assert!(s[0].summary.contains("people"));
    }

    async fn seeded_engine() -> InMemoryEngine {
        let engine = InMemoryEngine::default();
        let cases: &[(&str, &str)] = &[
            ("people/alice", ""),
            ("people/bob", ""),
            ("people/carol", ""),
            ("people/dave", ""),
            ("people/erin", ""),
            ("notes/idea", ""),
            ("notes/draft", ""),
            ("company/acme", ""),
        ];
        for (slug, pt) in cases {
            let input = PageInput {
                page_type: pt.to_string(),
                title: slug.to_string(),
                compiled_truth: format!("# {slug}"),
                source_path: Some(slug.to_string()),
                ..Default::default()
            };
            engine.put_page(slug, Some("src1"), &input).await.unwrap();
        }
        engine
    }

    #[tokio::test]
    async fn run_detect_clusters_real_engine() {
        let engine = seeded_engine().await;
        let r = run_detect(&engine, "src1", DetectOpts::default())
            .await
            .unwrap();
        assert_eq!(r.total_pages, 8);
        assert_eq!(r.untyped_pages, 8);
        // people/ (5) qualifies; notes/ (2) and company/ (1) don't (min 5)
        assert_eq!(r.prefixes.len(), 1);
        assert_eq!(r.prefixes[0].prefix, "people/");
        assert_eq!(r.prefixes[0].page_count, 5);
    }

    #[tokio::test]
    async fn run_suggest_hermetic() {
        let engine = seeded_engine().await;
        let r = run_suggest(&engine, "src1", DetectOpts::default(), None::<fn(&DetectResult) -> Vec<Suggestion>>)
            .await
            .unwrap();
        assert_eq!(r.suggestions.len(), 1);
        assert!(r.notes.iter().any(|n| n.contains("hermetic")));
    }

    #[tokio::test]
    async fn run_review_orphans_finds_untyped() {
        let engine = seeded_engine().await;
        let r = run_review_orphans(&engine, "src1").await.unwrap();
        assert_eq!(r.orphan_count, 8);
        assert!(r.orphans.iter().all(|o| o.source_id == "src1"));
    }

    #[tokio::test]
    async fn run_review_candidates_marks_active() {
        let engine = seeded_engine().await;
        let active = SchemaPackManifest {
            name: "zbrain-base".to_string(),
            page_types: vec![PageTypeDefinition {
                name: "people".to_string(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/".to_string()],
                aliases: vec![],
                extractable: false,
                expert_routing: false,
            }],
            ..Default::default()
        };
        let r = run_review_candidates(&engine, "src1", Some(&active), None, DetectOpts::default())
            .await
            .unwrap();
        assert_eq!(r.candidates.len(), 1);
        assert!(r.candidates[0].in_active_pack);
        assert_eq!(r.applied, None);
    }

    #[tokio::test]
    async fn run_sync_core_dry_run_and_apply() {
        let engine = seeded_engine().await;
        let active = SchemaPackManifest {
            name: "zbrain-base".to_string(),
            page_types: vec![PageTypeDefinition {
                name: "person".to_string(),
                primitive: PackPrimitive::Entity,
                path_prefixes: vec!["people/".to_string()],
                aliases: vec![],
                extractable: false,
                expert_routing: false,
            }],
            ..Default::default()
        };
        // Dry-run: 5 untyped people/* pages would apply, none applied.
        let dry = run_sync_core(&engine, Some("src1"), Some(&active), false, 1000)
            .await
            .unwrap();
        assert_eq!(dry.total_would_apply, 5);
        assert_eq!(dry.total_applied, 0);

        // Apply: now they get page_type "person".
        let applied = run_sync_core(&engine, Some("src1"), Some(&active), true, 1000)
            .await
            .unwrap();
        assert_eq!(applied.total_applied, 5);

        // Verify via list_pages (all src1) that people/* got "person" and
        // other prefixes stayed untyped. Title must be preserved.
        let pages = engine
            .list_pages(&PageFilters {
                source_id: Some("src1".to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        let by_slug: std::collections::HashMap<&str, &Page> =
            pages.iter().map(|p| (p.slug.as_str(), p)).collect();
        for slug in ["people/alice", "people/bob", "people/carol", "people/dave", "people/erin"] {
            let p = by_slug[slug];
            assert_eq!(p.page_type, "person", "page_type backfilled for {slug}");
            assert!(!p.title.is_empty(), "title preserved for {slug}");
        }
        for slug in ["notes/idea", "notes/draft", "company/acme"] {
            assert_eq!(by_slug[slug].page_type, "", "{slug} left untyped");
        }
    }
}
