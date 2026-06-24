# Page Contract Parity Audit

> Roadmap node: `1-2-1 Finish Page contract parity across storage backends`  
> Current slice: `1-2-1-1 Write Page contract parity audit plan`  
> Scope: audit only. This document records Page/storage contract parity gaps and follow-up classification; it does not implement Rust changes.

## Goal

Create a canonical, source-backed audit of Page/storage contract parity between the TypeScript BrainEngine contract/backends and the Rust ZBrain core contract/backends.

The output should make the next implementation slices obvious without absorbing unrelated upper-layer migration work.

## Scope

This audit is limited to the Page/storage contract layer:

- Page CRUD and source-scoped lookup behavior.
- Page listing, slug resolution, duplicate detection, soft-delete/restore/purge behavior.
- Page metadata/body update helpers that are exposed through the engine contract.
- Page-derived reads such as timestamps, effective dates, salience scores, orphan pages, and page refs.
- Page tags and file storage methods when they are part of the TS storage contract.
- Rust backend parity across Postgres and libsql when the Rust trait already exposes the method.

## Non-goals

- Do not migrate ingestion, search, facts, takes, agents, or product-level flows in this slice.
- Do not treat this audit as a Rust implementation commitment.
- Do not change TypeScript code as part of this audit.
- Do not update DB schema or runtime SQL in this document.
- Do not preserve GBrain compatibility aliases or fallback behavior; this remains aligned with the ZBrain migration rule that first-stage renames may be breaking.

If a semantic drift has follow-up value for cutover, it should be split into a roadmap sub-node rather than silently absorbed into this audit.

## Source Categories

The audit evidence is fixed to four source categories.

| Category | Files | Purpose |
| --- | --- | --- |
| TS contract | `src/core/engine.ts` | Defines the TypeScript `BrainEngine` Page/storage contract shape. |
| TS backend semantics | `src/core/pglite-engine.ts`, `src/core/postgres-engine.ts` | Confirms runtime behavior for PGLite/Postgres implementations. |
| Rust contract | `crates/zbrain-core/src/engine.rs`, `crates/zbrain-core/src/types.rs` | Defines Rust `BrainEngine` trait and supporting Page/storage types. |
| Rust backend/tests | `crates/zbrain-core/src/postgres.rs`, `crates/zbrain-core/src/libsql.rs`, `crates/zbrain-core/tests/*.rs` | Confirms Rust implementation and test coverage. |

## Status Rules

| Status | Meaning |
| --- | --- |
| `covered` | Rust trait has an equivalent entry point, Rust backends implement it, and Rust tests cover the key TS semantics. |
| `missing` | TS Page/storage contract has behavior but Rust trait or backends have no equivalent. |
| `semantic-drift` | Rust has an entry point or implementation, but semantics differ from TS in a way that may affect future cutover. |
| `deferred` | Behavior belongs to Page/storage contract but depends on a later slice such as facts/takes/search schema migration. |
| `out-of-scope` | Behavior belongs to upper-layer flows rather than the Page/storage contract. |

## Follow-up Classification

Allowed values:

- `none`
- `sub-node candidate`
- `issue candidate`
- `implementation slice candidate`
- `defer to node 1-2-2`
- `defer to node 1-2-3`
- `defer to upper-layer roadmap node`

## Method / Behavior Audit Table

| TS method / behavior | TS backend semantic notes | Rust trait equivalent | Rust backend coverage | Test coverage | Status | Follow-up classification |
| --- | --- | --- | --- | --- | --- | --- |
| `getPage(slug, opts?)` | TS PGLite/Postgres only add `source_id` predicate when `opts.sourceId` is provided. Without `sourceId`, lookup is unscoped across sources; `includeDeleted` controls `deleted_at` filtering. | `get_page(slug, &GetPageOpts)` exists, but `GetPageOpts.source_id = None` is documented as normalized to `"default"`, not unscoped. | Postgres/libsql both use `opts.source_id.unwrap_or("default")` and require `source_id = default` when no source is supplied. | CRUD tests cover basic get/put behavior; current evidence does not show a backend test for TS-style unscoped no-source lookup. | `semantic-drift` | `sub-node candidate` |
| `putPage(slug, page, opts?)` | TS defaults `sourceId` to `"default"` and upserts on `(source_id, slug)`. | `put_page(slug, source_id, input)` exists and defaults absent source to `"default"`. | Postgres/libsql implement upsert semantics with source defaulting. | `libsql_engine_put_page_source_id.rs`, `inmemory_engine_put_page_source_id.rs`, CRUD/lifecycle tests. | `covered` | `none` |
| `findDuplicatePage(sourceId, { hash, frontmatterId })` | TS returns `{ slug, id } | null`, filters by source, excludes deleted rows, matches `content_hash` or non-null `frontmatter.id`, orders by `id`, limit 1. | `find_duplicate_page(source_id, &FindDuplicatePageOpts)` exists but returns `Option<Page>`. | Postgres/libsql implement equivalent source/deleted/hash/frontmatter query, but return a full `Page`. | `page_methods_find_duplicate_page.rs`. | `semantic-drift` | `sub-node candidate` |
| `deletePage(slug, opts?)` | TS defaults absent `sourceId` to `"default"` and hard-deletes that source-scoped row. | `delete_page(slug, source_id)` exists. | Postgres/libsql use default source when absent and delete by `(slug, source_id)`. | CRUD/lifecycle tests cover delete behavior. | `covered` | `none` |
| `softDeletePage(slug, opts?)` | TS Postgres uses optional source guard: absent `sourceId` is unscoped across sources; returns `{ slug } | null`. PGLite behavior should be treated as source-backed evidence for equivalent semantics. | `soft_delete_page(slug, source_id)` returns `Option<String>`. | Postgres uses `($2::text IS NULL OR source_id = $2)`; libsql mirrors optional source semantics. | `page_methods_soft_delete_page.rs`. | `covered` | `none` |
| `restorePage(slug, opts?)` | TS Postgres uses optional source guard: absent `sourceId` is unscoped across deleted rows; returns boolean. | `restore_page(slug, source_id)` returns `bool`. | Postgres uses `($2::text IS NULL OR source_id = $2)`; libsql mirrors optional source semantics. | `page_methods_restore_page.rs`. | `covered` | `none` |
| `purgeDeletedPages(olderThanHours)` | TS purges deleted rows older than threshold and returns purged slugs plus count. | `purge_deleted_pages(older_than_hours)` returns `PurgeResult { slugs, count }`. | Postgres/libsql implement purge result shape. | `page_methods_purge_deleted_pages.rs`. | `covered` | `none` |
| `listPages(filters?)` | TS supports filter-driven page listing, including source scoping and deleted-row visibility behavior. | `list_pages(&PageFilters)` exists. | libsql explicitly documents TS precedence for source scope: non-empty `sourceIds` wins over `sourceId`; Postgres has corresponding filter implementation. | `libsql_engine_list_pages.rs`, object-safety/list tests. | `covered` | `none` |
| `resolveSlugs(partial, opts?)` | TS supports exact-first/fuzzy matching, optional `sourceId` / `sourceIds`, and result limiting behavior. | `resolve_slugs(partial)` exists but has no source scope opts. | Postgres/libsql are exact-only (`WHERE slug = ?`) and comments explicitly defer fuzzy matching. | Object-safety/in-memory test evidence includes `resolve_slugs`; backend comments show Postgres/libsql exact-only. | `semantic-drift` | `implementation slice candidate` |
| `getAllSlugs(opts?)` | TS returns all slugs, optionally source-scoped; PGLite evidence does not filter `deleted_at`. | `get_all_slugs(source_id)` returns `HashSet<String>`. | Postgres/libsql expose equivalent method. | `page_methods_get_all_slugs.rs`. | `covered` | `none` |
| `listAllPageRefs()` | TS returns live `{ slug, source_id }` refs ordered by source and slug, filtering `deleted_at IS NULL`. | `list_all_page_refs()` returns `Vec<PageRef>`. | Postgres/libsql expose equivalent method. | `page_methods_list_all_page_refs.rs`. | `covered` | `none` |
| `refreshPageBody(args)` | TS updates compiled page body / timeline / content hash style Page-body fields through the storage contract. | `refresh_page_body(&RefreshPageBodyArgs)` exists. | Postgres/libsql expose equivalent update helper. | `page_methods_refresh_page_body.rs`. | `covered` | `none` |
| `updatePageContextualRetrievalState(slug, sourceId, mode, corpusGeneration)` | TS updates contextual retrieval state fields for a source-scoped page. | `update_page_contextual_retrieval_state(slug, source_id, mode, corpus_generation)` exists. | Postgres/libsql expose equivalent update helper. | `page_methods_update_cr_state.rs`. | `covered` | `none` |
| `getPageTimestamps(slugs)` | TS query evidence does not filter `deleted_at`; returns timestamp map for requested slugs. | `get_page_timestamps(slugs)` exists. | Rust Postgres evidence filters `deleted_at IS NULL`; libsql should be checked/kept in lockstep. | `page_methods_get_page_timestamps.rs`. | `semantic-drift` | `sub-node candidate` |
| `getEffectiveDates(refs)` | TS uses `COALESCE(p.effective_date, p.updated_at, p.created_at)`. | `get_effective_dates(refs)` exists. | Rust Postgres/libsql use `COALESCE(updated_at, created_at)` and comments state `effective_date` is intentionally not consulted, contradicting TS evidence. | `page_methods_get_effective_dates.rs`. | `semantic-drift` | `sub-node candidate` |
| `getSalienceScores(refs)` | TS computes salience from page/take-related fields for source-scoped refs. | `get_salience_scores(refs)` exists. | Postgres/libsql implement salience score reads; libsql performs `ln()` in Rust because math functions may be unavailable. | `page_methods_get_salience_scores.rs`, `page_methods_salience_scores_with_takes.rs`. | `covered` | `none` |
| `findOrphanPages()` | TS returns orphan page candidates as `{ slug, title, domain }`. | `find_orphan_pages()` returns `Vec<OrphanPage>`. | Postgres/libsql expose equivalent method. | `page_methods_find_orphan_pages.rs`. | `covered` | `none` |
| Page tag methods | TS contract includes tag behavior as Page-adjacent storage operations. | `add_tag`, `remove_tag`, `get_tags` exist. | libsql/Postgres tag implementations exist; evidence includes libsql tag CRUD tests. | `libsql_engine_tag_crud.rs`. | `covered` | `none` |
| File storage methods: `upsertFile`, `getFile`, `listFilesForPage`; types `FileRow`, `FileSpec` | TS contract and PGLite/Postgres backends expose file storage operations with source/page linkage, storage path, MIME/size/hash, and metadata. | No Rust trait equivalents found in `BrainEngine`; no corresponding `FileRow` / `FileSpec` Rust types found. | No Postgres/libsql backend implementation evidence found. | No Rust tests found for file storage contract. | `missing` | `sub-node candidate` |

## Follow-up Candidates

These are follow-up classifications only; this document does not commit implementation.

### Sub-node candidates

1. **Align `getPage` no-source lookup semantics**
   - Current drift: TS no-source lookup is unscoped; Rust no-source lookup normalizes to `"default"`.
   - Cutover risk: callers that rely on cross-source lookup could observe missing pages after Rust cutover.

2. **Decide `findDuplicatePage` return shape**
   - Current drift: TS returns `{ slug, id } | null`; Rust returns `Option<Page>`.
   - Cutover risk: unnecessary full `Page` materialization and contract shape mismatch.

3. **Align `getPageTimestamps` deleted-row visibility**
   - Current drift: TS evidence does not filter deleted rows; Rust filters live rows.
   - Cutover risk: timestamp-dependent maintenance or reconciliation logic may diverge.

4. **Align `getEffectiveDates` fallback chain**
   - Current drift: TS consults `effective_date`; Rust explicitly does not.
   - Cutover risk: ordering, ranking, or recency semantics may change.

5. **Add Rust file storage contract parity**
   - Current gap: TS has `FileRow`, `FileSpec`, `upsertFile`, `getFile`, and `listFilesForPage`; Rust has no equivalent evidence.
   - Cutover risk: file-backed Page features cannot move to Rust storage without a new contract slice.

### Implementation slice candidate

1. **Implement `resolveSlugs` TS parity**
   - Current drift: Rust trait lacks source-scope options; Postgres/libsql are exact-only; TS supports source scoping, fuzzy behavior, and limits.
   - Recommended treatment: a focused implementation slice because this likely requires trait signature/design changes, backend SQL updates, and backend-specific tests.

## Recommended next action

Update the roadmap from this audit:

1. Mark `1-2-1-1 Write Page contract parity audit plan` as completed.
2. Add sub-nodes for high-value semantic drift / missing contract work.
3. Render `docs/plans/ZBRAIN_TS_TO_RUST_ROADMAP.md` from the roadmap JSON.
