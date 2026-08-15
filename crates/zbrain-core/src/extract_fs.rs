//! Filesystem-source extraction for `zbrain extract --source fs`.
//!
//! Rust port of the TS `extract` command's `--source fs` path
//! (`src/commands/extract.ts`): walk a markdown directory, derive slugs from
//! file paths, extract markdown/wikilinks + timeline entries, and reconcile
//! them into the engine's `links` / `pages.timeline` tables.
//!
//! Unlike the db-source path (`auto_fix::extract_links`), fs-source resolves
//! relative link targets (e.g. `../people/bob.md`) against the on-disk slug
//! set with the same `join` + ancestor-search rules as TS `resolveSlug`.
//!
//! Pages are created on demand when a walked file's slug is absent from the
//! engine (create-if-missing, never overwriting an existing page body), so a
//! single `extract --source fs --dir <vault>` invocation turns a markdown
//! vault into a linked graph without a prior `sync`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::auto_fix::{extract_timeline_entries, ExtractLinksResult, ExtractTimelineResult};
use crate::engine::{BrainEngine, GetPageOpts, PageInput};
use crate::markdown_links::extract_markdown_links;
use crate::types::LinkBatchInput;

/// Default link batch size, mirroring TS `BATCH_SIZE` / `LINK_BATCH_SIZE`.
const FS_LINK_BATCH_SIZE: usize = 200;

/// Recursively collect `.md` files under `dir`, skipping vendor/dot
/// directories and non-content files. Mirrors TS `walkMarkdownFiles` pruning
/// (`.git`, `node_modules`) and the `_`-prefixed-file exclusion that closes
/// the "walk 28K junk files" footgun.
pub fn walk_markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let file_name_os = entry.file_name();
        let file_name = match file_name_os.to_str() {
            Some(n) => n,
            None => continue,
        };
        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_dir() {
            // Prune vendor / dot directories before recursion (saves IO).
            if file_name.starts_with('.') || file_name == "node_modules" {
                continue;
            }
            walk(root, &entry.path(), out);
        } else if ft.is_file() {
            if !file_name.ends_with(".md") {
                continue;
            }
            if file_name.starts_with('_') {
                continue;
            }
            let rel = match entry.path().strip_prefix(root) {
                Ok(r) => r.to_path_buf(),
                Err(_) => continue,
            };
            out.push(rel);
        }
    }
}

/// Normalize a slash-separated path string: collapse `.` and `..`
/// components. Node's `path.join` performs this normalization, so TS
/// `resolveSlug` relies on it; Rust `Path::join` does not, hence the manual
/// pass.
fn normalize_slug(s: &str) -> String {
    let mut comps: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !comps.is_empty() {
                    comps.pop();
                }
            }
            other => comps.push(other),
        }
    }
    comps.join("/")
}

/// Derive a brain slug from a relative file path: forward-slash separators,
/// strip the `.md` suffix, normalize. Mirrors TS `pathToSlug`.
pub fn path_to_slug(rel_path: &str) -> String {
    let rel = rel_path.replace('\\', "/");
    let no_ext = rel.strip_suffix(".md").unwrap_or(&rel);
    normalize_slug(no_ext)
}

/// Resolve a relative link target to a canonical slug, given the directory of
/// the containing page and the set of all known slugs. Mirrors TS
/// `resolveSlug`: exact `join(file_dir, target)` first, then strip leading
/// path components from `file_dir` (ancestor search) so authors who omit a
/// `../` still resolve.
pub fn resolve_slug(
    file_dir: &str,
    rel_target: &str,
    all_slugs: &HashSet<String>,
) -> Option<String> {
    let target_no_ext = rel_target.strip_suffix(".md").unwrap_or(rel_target);
    let s1 = normalize_slug(&format!("{}/{}", file_dir, target_no_ext));
    if all_slugs.contains(&s1) {
        return Some(s1);
    }
    let parts: Vec<&str> = file_dir.split('/').filter(|p| !p.is_empty()).collect();
    for strip in 1..=parts.len() {
        let ancestor = &parts[..parts.len() - strip];
        let candidate = if ancestor.is_empty() {
            target_no_ext.to_string()
        } else {
            normalize_slug(&format!("{}/{}", ancestor.join("/"), target_no_ext))
        };
        if all_slugs.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Directory-based link-type inference for the fs-source path. Mirrors TS
/// `inferTypeByDir` (calibrated verb-based inference for db-source lives in
/// `link-extraction.ts`; this is the fs analog).
pub fn infer_link_type_by_dir(from_dir: &str, to_dir: &str) -> String {
    let from = from_dir.split('/').next().unwrap_or("");
    let to = to_dir.split('/').next().unwrap_or("");
    match (from, to) {
        ("people", "companies") => "works_at".to_string(),
        ("people", "deals") => "involved_in".to_string(),
        ("deals", "companies") => "deal_for".to_string(),
        ("meetings", "people") => "attended".to_string(),
        _ => "mentions".to_string(),
    }
}

/// Infer a page type from the top-level directory of a slug (mirrors the TS
/// frontmatter-free heuristic so `put_page` rows are typed sensibly).
fn infer_page_type(top_dir: &str) -> String {
    match top_dir {
        "people" => "person",
        "companies" => "company",
        "deals" => "deal",
        "meetings" => "meeting",
        _ => "concept",
    }
    .to_string()
}

/// Extract a page title from the first `# ` heading, falling back to the file
/// stem.
fn extract_title(content: &str, stem: &str) -> String {
    for line in content.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("# ") {
            let r = rest.trim();
            if !r.is_empty() {
                return r.to_string();
            }
        }
    }
    stem.to_string()
}

/// Ensure a page exists for `slug`: if absent from the engine, create it from
/// the on-disk `content` (title + body). Never overwrites an existing page
/// body — extraction only reconciles links/timeline, matching TS behavior.
async fn ensure_page(
    engine: &dyn BrainEngine,
    slug: &str,
    content: &str,
) -> crate::Result<()> {
    if engine.get_page(slug, &GetPageOpts::default()).await?.is_some() {
        return Ok(());
    }
    let top = slug.split('/').next().unwrap_or("");
    let stem = Path::new(slug)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(slug);
    let input = PageInput {
        page_type: infer_page_type(top),
        title: extract_title(content, stem),
        compiled_truth: content.to_string(),
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
    };
    engine.put_page(slug, None, &input).await?;
    Ok(())
}

/// Scan markdown files under `dir` for links, resolve each target against the
/// on-disk slug set, and write them via `add_links_batch`. Page-level analog
/// of TS `extractLinksFromDir`.
pub async fn extract_links_from_dir(
    engine: &dyn BrainEngine,
    dir: &Path,
) -> crate::Result<ExtractLinksResult> {
    let files = walk_markdown_files(dir);
    let all_slugs: HashSet<String> = files
        .iter()
        .map(|p| path_to_slug(&p.to_string_lossy().replace('\\', "/")))
        .collect();

    let mut result = ExtractLinksResult::default();
    let mut batch: Vec<LinkBatchInput> = Vec::new();

    for rel in &files {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let slug = path_to_slug(&rel_str);
        let full = dir.join(rel);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(_) => continue,
        };
        ensure_page(engine, &slug, &content).await?;

        let file_dir = rel
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .replace('\\', "/");

        for (name, rel_target) in extract_markdown_links(&content) {
            match resolve_slug(&file_dir, &rel_target, &all_slugs) {
                None => {
                    result.dangling += 1;
                }
                Some(target) if target == slug => {
                    // Self-link — never written.
                }
                Some(target) => {
                    let to_dir = target.split('/').next().unwrap_or("").to_string();
                    let link_type = infer_link_type_by_dir(&file_dir, &to_dir);
                    batch.push(LinkBatchInput {
                        from_slug: slug.clone(),
                        to_slug: target,
                        link_type: Some(link_type),
                        context: Some(format!("markdown link: [{name}]")),
                        link_source: Some("markdown".to_string()),
                        origin_slug: None,
                        origin_field: None,
                        from_source_id: Some("default".to_string()),
                        to_source_id: Some("default".to_string()),
                        origin_source_id: None,
                    });
                    if batch.len() >= FS_LINK_BATCH_SIZE {
                        result.links_created += engine.add_links_batch(&batch).await?;
                        batch.clear();
                    }
                }
            }
        }
        result.pages_processed += 1;
    }
    if !batch.is_empty() {
        result.links_created += engine.add_links_batch(&batch).await?;
    }
    Ok(result)
}

/// Scan markdown files under `dir` for dated timeline entries and append each
/// as a `"{date} {summary}"` line to the page's `pages.timeline` (skipping
/// lines already present). Page-level analog of TS `extractTimelineFromDir`.
pub async fn extract_timeline_from_dir(
    engine: &dyn BrainEngine,
    dir: &Path,
) -> crate::Result<ExtractTimelineResult> {
    let files = walk_markdown_files(dir);
    let mut result = ExtractTimelineResult::default();

    for rel in &files {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let slug = path_to_slug(&rel_str);
        let full = dir.join(rel);
        let content = match std::fs::read_to_string(&full) {
            Ok(c) => c,
            Err(_) => continue,
        };
        ensure_page(engine, &slug, &content).await?;

        // De-dup against lines already present (mirrors db-source
        // `extract_timeline`, which guards before calling add_timeline_entry;
        // the engine default impl only appends).
        let existing: HashSet<String> = match engine.get_page(&slug, &GetPageOpts::default()).await? {
            Some(p) => p
                .timeline
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect(),
            None => HashSet::new(),
        };
        for entry in extract_timeline_entries(&content) {
            let line = format!("{} {}", entry.date, entry.summary);
            if existing.contains(&line) {
                continue;
            }
            engine.add_timeline_entry(&slug, "default", &line).await?;
            result.entries_added += 1;
        }
        result.pages_processed += 1;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::InMemoryEngine;

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let full = dir.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&full, content).ok();
    }

    async fn put_page(engine: &InMemoryEngine, slug: &str, title: &str) {
        let input = PageInput {
            page_type: "concept".to_string(),
            title: title.to_string(),
            compiled_truth: String::new(),
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
        };
        engine.put_page(slug, None, &input).await.ok();
    }

    #[test]
    fn path_to_slug_strips_md_and_normalizes() {
        assert_eq!(path_to_slug("people/alice.md"), "people/alice");
        assert_eq!(path_to_slug("alice.md"), "alice");
        assert_eq!(path_to_slug(r"people\alice.md"), "people/alice");
        assert_eq!(path_to_slug("a/../b.md"), "b");
    }

    #[test]
    fn resolve_slug_ancestor_search() {
        let mut set = HashSet::new();
        set.insert("people/alice".to_string());
        set.insert("people/bob".to_string());
        set.insert("companies/acme".to_string());
        // Exact join: people/alice + ../people/bob => people/bob
        assert_eq!(
            resolve_slug("people", "../people/bob.md", &set),
            Some("people/bob".to_string())
        );
        // Dangling
        assert_eq!(resolve_slug("people", "../people/ghost.md", &set), None);
    }

    #[tokio::test]
    async fn fs_links_extract_and_idempotent() {
        let tmp = std::env::temp_dir().join(format!("zb_extract_fs_{}.txt", std::process::id()));
        let dir = tmp.with_extension(""); // ensure unique-ish dir base
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();
        // guard: use a random subdir to avoid clobbering
        let dir = dir.join("vault");
        std::fs::create_dir_all(&dir).ok();

        write_file(
            &dir,
            "people/alice.md",
            "---\ntitle: Alice\n---\n\n[Bob](../people/bob.md) is a friend.\n",
        );
        write_file(
            &dir,
            "people/bob.md",
            "---\ntitle: Bob\n---\n\nWorks at [Acme](../companies/acme.md).\n",
        );
        write_file(
            &dir,
            "companies/acme.md",
            "---\ntitle: Acme\n---\n\nFounded by [Alice](../people/alice.md).\n",
        );

        let engine = InMemoryEngine::new();
        // Pre-create pages (mirrors TS test setup; fs path won't overwrite).
        put_page(&engine, "people/alice", "Alice").await;
        put_page(&engine, "people/bob", "Bob").await;
        put_page(&engine, "companies/acme", "Acme").await;

        let r1 = extract_links_from_dir(&engine, &dir).await.unwrap();
        assert!(r1.links_created >= 3, "expected >=3 links, got {}", r1.links_created);

        // Second run must be idempotent (ON CONFLICT DO NOTHING).
        let r2 = extract_links_from_dir(&engine, &dir).await.unwrap();
        assert_eq!(r2.links_created, 0, "second run must create 0 new links");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn fs_timeline_extract_and_idempotent() {
        let tmp = std::env::temp_dir().join(format!("zb_extract_fs_tl_{}.txt", std::process::id()));
        let dir = tmp.with_extension("").join("vault");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();

        write_file(
            &dir,
            "people/alice.md",
            "---\ntitle: Alice\n---\n\n## Timeline\n\n- **2024-01-15** | source — Founded NovaMind\n- **2024-06-01** | source — Raised seed round\n",
        );

        let engine = InMemoryEngine::new();
        put_page(&engine, "people/alice", "Alice").await;

        let r1 = extract_timeline_from_dir(&engine, &dir).await.unwrap();
        assert_eq!(r1.entries_added, 2);

        let r2 = extract_timeline_from_dir(&engine, &dir).await.unwrap();
        assert_eq!(r2.entries_added, 0, "second run must add 0 new entries");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn fs_creates_missing_page() {
        let tmp = std::env::temp_dir().join(format!("zb_extract_fs_mk_{}.txt", std::process::id()));
        let dir = tmp.with_extension("").join("vault");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok();

        write_file(
            &dir,
            "people/alice.md",
            "# Alice\n\n[Bob](../people/bob.md)\n",
        );
        write_file(&dir, "people/bob.md", "# Bob\n");

        let engine = InMemoryEngine::new();
        // No pages pre-created: fs path must create them on demand.
        let r = extract_links_from_dir(&engine, &dir).await.unwrap();
        assert!(r.links_created >= 1, "expected >=1 link after auto-create");

        // alice's title should be derived from the H1.
        let alice = engine.get_page("people/alice", &GetPageOpts::default()).await.unwrap();
        assert!(alice.is_some());
        assert_eq!(alice.unwrap().title, "Alice");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
