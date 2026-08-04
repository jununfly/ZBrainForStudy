//! Port of `src/core/entities/resolve.ts` (entities/resolve) — v0.40.2.0
//! resolution chain, used by the phantom-redirect pre-pass (1-6-6) and any
//! general entity-slug resolution.
//!
//! Faithful to TS except `try_fuzzy_match`, which uses a Rust-side trigram
//! Jaccard similarity (threshold 0.4) instead of Postgres `pg_trgm`
//! `similarity()` — libsql has no pg_trgm, and this keeps the resolver
//! engine-agnostic. Page lookups go through `execute_raw`; engines without
//! raw SQL (InMemory) degrade each `try_*` helper to `None` (fail-open), so
//! the resolver falls back to `slugify` and phantom resolution returns
//! `None` (no canonical).
//!
//! SQL uses `?N` positional markers (libsql-primary, matching
//! `consolidate.rs`). Postgres-backed callers route through `execute_raw`
//! with the same markers and would need `$N` translation — that is a
//! pre-existing gap shared with `consolidate.rs`, not introduced here.

use std::collections::HashSet;

use erased_serde::Serialize;
use serde_json::Value;

use crate::engine::BrainEngine;

/// Directories considered when expanding a bare-name reference into a
/// prefixed canonical slug. Mirrors TS `PREFIX_EXPANSION_DIRS`.
const PREFIX_EXPANSION_DIRS: &[&str] = &["people", "companies"];

/// Tagged resolution source (mirrors TS `ResolutionSource`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionSource {
    ExactPage,
    FuzzyMatch,
    FallbackSlugify,
}

/// Tagged resolution result (mirrors TS `ResolveResult`).
#[derive(Debug, Clone)]
pub struct ResolveResult {
    pub slug: String,
    pub source: ResolutionSource,
}

/// A prefix-expansion candidate with its connection count (mirrors the
/// `findPrefixCandidates` row shape).
#[derive(Debug, Clone)]
pub struct PrefixCandidate {
    pub slug: String,
    pub connection_count: i64,
}

/// Deterministic slugify: lowercase, replace runs of non-alphanumeric with
/// hyphens, collapse + trim. Mirrors TS `slugify` (accent stripping via NFKD
/// is approximated by dropping non-ASCII alphanumerics — the slugify path is
/// only a fallback, so the minor divergence is immaterial).
pub fn slugify(raw: &str) -> String {
    let lowered = raw.to_lowercase();
    let mut out = String::new();
    let mut last_hyphen = false;
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_hyphen = false;
        } else if matches!(c, ' ' | '_' | '/' | '.' | ',') {
            if !last_hyphen && !out.is_empty() {
                out.push('-');
                last_hyphen = true;
            }
        }
        // other chars (accents, symbols) are dropped
    }
    while out.ends_with('-') {
        out.pop();
    }
    while out.starts_with('-') {
        out.pop();
    }
    out
}

/// True when the input looks like an already-canonical slug. Mirrors TS
/// `looksLikeSlug`.
fn looks_like_slug(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    if s != s.to_lowercase() {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '/' | '_' | '-'))
}

/// True when the input is a single bare first-name token (no slash, no
/// embedded prefix, slugifies to a single non-hyphenated token). Mirrors TS
/// `isBareName`.
fn is_bare_name(raw: &str) -> bool {
    if raw.contains('/') {
        return false;
    }
    let tokens: Vec<&str> = raw.split_whitespace().filter(|t| !t.is_empty()).collect();
    if tokens.len() != 1 {
        return false;
    }
    let slug = slugify(raw);
    if slug.is_empty() || slug.contains('-') {
        return false;
    }
    true
}

/// Trigrams of a string, space-padded (mirrors pg_trgm's 2-space padding).
fn trigrams(s: &str) -> HashSet<String> {
    let padded: Vec<char> = format!("  {}  ", s.to_lowercase()).chars().collect();
    if padded.len() < 3 {
        return HashSet::new();
    }
    padded.windows(3).map(|w| w.iter().collect()).collect()
}

/// Trigram Jaccard similarity in [0, 1]. Used as the Rust-side stand-in for
/// Postgres `pg_trgm` `similarity()` (threshold 0.4, matching TS).
fn trigram_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        return 0.0;
    }
    inter as f64 / union as f64
}

async fn try_exact_slug(
    engine: &dyn BrainEngine,
    source_id: &str,
    candidate: &str,
) -> Option<String> {
    let sql = "SELECT slug FROM pages WHERE source_id = ?1 AND slug = ?2 AND deleted_at IS NULL LIMIT 1";
    let params: &[&(dyn Serialize + Sync)] = &[&source_id, &candidate];
    let rows = match engine.execute_raw(sql, params).await {
        Ok(r) => r,
        Err(_) => return None,
    };
    rows.first()
        .and_then(|v| v.get("slug"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

async fn try_fuzzy_match(
    engine: &dyn BrainEngine,
    source_id: &str,
    raw: &str,
) -> Option<String> {
    let lc = raw.to_lowercase();
    let fragment = slugify(raw);
    // Pull candidate pages whose title or slug contains the fragment, then
    // rank in Rust by trigram similarity (stand-in for pg_trgm). Libsql has
    // no pg_trgm, so the scoring is done client-side.
    let sql = "SELECT slug, title FROM pages \
               WHERE source_id = ?1 AND deleted_at IS NULL \
                 AND (lower(title) LIKE '%' || ?2 || '%' OR slug LIKE '%' || ?3 || '%') \
               ORDER BY slug ASC LIMIT 50";
    let params: &[&(dyn Serialize + Sync)] = &[&source_id, &lc, &fragment];
    let rows = match engine.execute_raw(sql, params).await {
        Ok(r) => r,
        Err(_) => return None,
    };
    let mut best: Option<(String, f64)> = None;
    for row in &rows {
        let slug = row.get("slug").and_then(|s| s.as_str()).unwrap_or("");
        let title = row.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let title_score = trigram_similarity(&title.to_lowercase(), &lc);
        let slug_score = trigram_similarity(slug, &fragment);
        let score = title_score.max(slug_score);
        if score < 0.4 {
            continue;
        }
        let better = match &best {
            None => true,
            Some((_, b)) => score > *b,
        };
        if better {
            best = Some((slug.to_string(), score));
        }
    }
    best.map(|(slug, _)| slug)
}

/// Query prefix-expansion candidates for a token across every configured
/// directory, returning them ordered by connection_count DESC, slug ASC.
async fn prefix_candidate_rows(
    engine: &dyn BrainEngine,
    source_id: &str,
    token: &str,
) -> Vec<PrefixCandidate> {
    let mut patterns: Vec<String> = Vec::new();
    for dir in PREFIX_EXPANSION_DIRS {
        patterns.push(format!("{dir}/{token}"));
        patterns.push(format!("{dir}/{token}-%"));
    }
    let mut conds = Vec::new();
    let mut param_refs: Vec<&(dyn Serialize + Sync)> = Vec::with_capacity(patterns.len() + 1);
    param_refs.push(&source_id);
    for (i, p) in patterns.iter().enumerate() {
        conds.push(format!("p.slug LIKE ?{}", i + 2));
        param_refs.push(p);
    }
    let sql = format!(
        "SELECT p.slug, \
                ((SELECT COUNT(*) FROM links WHERE to_page_id = p.id) \
                 + (SELECT COUNT(*) FROM links WHERE from_page_id = p.id) \
                 + (SELECT COUNT(*) FROM content_chunks WHERE page_id = p.id)) \
                 AS connection_count \
         FROM pages p \
         WHERE p.source_id = ?1 AND p.deleted_at IS NULL \
           AND ({}) \
         ORDER BY connection_count DESC, p.slug ASC \
         LIMIT 10",
        conds.join(" OR ")
    );
    let rows = match engine.execute_raw(&sql, &param_refs).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.iter()
        .filter_map(|v| {
            let slug = v.get("slug")?.as_str()?.to_string();
            let cc = v
                .get("connection_count")
                .and_then(|c| c.as_i64().or_else(|| c.as_f64().map(|f| f as i64)))
                .unwrap_or(0);
            Some(PrefixCandidate {
                slug,
                connection_count: cc,
            })
        })
        .collect()
}

async fn try_prefix_expansion(
    engine: &dyn BrainEngine,
    source_id: &str,
    token: &str,
) -> Option<String> {
    // Top-1 by connection_count wins; slug-ASC secondary key makes ties
    // deterministic (mirrors TS). `prefix_candidate_rows` already orders
    // DESC, slug ASC, so the first row is the winner.
    prefix_candidate_rows(engine, source_id, token)
        .await
        .into_iter()
        .next()
        .map(|c| c.slug)
}

/// Resolve a raw entity reference to a canonical slug. Mirrors TS
/// `resolveEntitySlug`: exact → fuzzy → prefix (bare name) → fallback slugify.
pub async fn resolve_entity_slug(
    engine: &dyn BrainEngine,
    source_id: &str,
    raw: &str,
) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if looks_like_slug(trimmed) {
        if let Some(exact) = try_exact_slug(engine, source_id, trimmed).await {
            return Some(exact);
        }
    }
    if let Some(fuzzy) = try_fuzzy_match(engine, source_id, trimmed).await {
        return Some(fuzzy);
    }
    if is_bare_name(trimmed) {
        if let Some(exp) = try_prefix_expansion(engine, source_id, &slugify(trimmed)).await {
            return Some(exp);
        }
    }
    Some(slugify(trimmed))
}

/// Resolution-source-tagged variant of [`resolve_entity_slug`]. Mirrors TS
/// `resolveEntitySlugWithSource`.
pub async fn resolve_entity_slug_with_source(
    engine: &dyn BrainEngine,
    source_id: &str,
    raw: &str,
) -> Option<ResolveResult> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if looks_like_slug(trimmed) {
        if let Some(exact) = try_exact_slug(engine, source_id, trimmed).await {
            return Some(ResolveResult {
                slug: exact,
                source: ResolutionSource::ExactPage,
            });
        }
    }
    if let Some(fuzzy) = try_fuzzy_match(engine, source_id, trimmed).await {
        return Some(ResolveResult {
            slug: fuzzy,
            source: ResolutionSource::FuzzyMatch,
        });
    }
    if is_bare_name(trimmed) {
        if let Some(exp) = try_prefix_expansion(engine, source_id, &slugify(trimmed)).await {
            return Some(ResolveResult {
                slug: exp,
                source: ResolutionSource::FuzzyMatch,
            });
        }
    }
    Some(ResolveResult {
        slug: slugify(trimmed),
        source: ResolutionSource::FallbackSlugify,
    })
}

/// Phantom-canonical resolver. Variant of `resolve_entity_slug` that SKIPS
/// the exact-slug step (a phantom slug would exact-match itself and make the
/// pass a no-op). Mirrors TS `resolvePhantomCanonical`.
///
/// Output is filtered to require `result != phantomSlug AND result.includes('/')`
/// so a fuzzy match bouncing back to the phantom itself doesn't trigger a
/// self-redirect. Caller treats `None` as `'no_canonical'`.
pub async fn resolve_phantom_canonical(
    engine: &dyn BrainEngine,
    source_id: &str,
    phantom_slug: &str,
) -> Option<String> {
    let trimmed = phantom_slug.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(fuzzy) = try_fuzzy_match(engine, source_id, trimmed).await {
        if fuzzy != phantom_slug && fuzzy.contains('/') {
            return Some(fuzzy);
        }
    }
    if let Some(exp) = try_prefix_expansion(engine, source_id, &slugify(trimmed)).await {
        if exp != phantom_slug && exp.contains('/') {
            return Some(exp);
        }
    }
    None
}

/// Standalone candidate query for ambiguity detection (codex #11). Mirrors TS
/// `findPrefixCandidates`: returns every prefixed page matching
/// `<dir>/<token>` OR `<dir>/<token>-*` so the caller can count candidates and
/// refuse to redirect when ambiguous. Cap of 10.
pub async fn find_prefix_candidates(
    engine: &dyn BrainEngine,
    source_id: &str,
    token: &str,
) -> Vec<PrefixCandidate> {
    if token.is_empty() {
        return Vec::new();
    }
    prefix_candidate_rows(engine, source_id, token).await
}
