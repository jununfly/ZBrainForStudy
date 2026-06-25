# Advanced Page Writes Contract Audit

Date: 2026-06-24
Roadmap node: `1-2-2-1 Write advanced Page writes audit plan`
Parent roadmap node: `1-2-2 Port missing advanced Page writes to Rust`

## Scope

This is a narrow contract audit for the remaining advanced Page-owned write capabilities that TS has and Rust currently lacks. It defines the parity target, public Rust shape proposal, schema/migration proposal, and TDD slice plan only; implementation is intentionally deferred.

In scope:

- Raw sidecar data:
  - `putRawData`
  - `getRawData`
- Page snapshots / versions:
  - `createVersion`
  - `getVersions`
  - `revertToVersion`
- Slug/link rewrite:
  - `updateSlug`
  - `rewriteLinks`

Out of scope:

- Already-covered Page parity from `1-2-1`: unscoped `getPage`, `findDuplicatePage`, `getPageTimestamps`, `getEffectiveDates`, `resolveSlugs`, soft-delete, tags, `refresh_page_body`, and contextual retrieval state.
- Facts, takes, search, chunks, embeddings, graph traversal, ingest flows, and upper-layer sync/import orchestration.
- Implementing migrations or Rust trait methods during this audit slice.
- Changing TS behavior for bare-slug multi-source compatibility hazards.

## TS source-of-truth facts

### Public contract

Sources: `src/core/engine.ts`, `src/core/types.ts`

```ts
export interface RawData {
  source: string;
  data: Record<string, unknown>;
  fetched_at: Date;
}

export interface PageVersion {
  id: number;
  page_id: number;
  compiled_truth: string;
  frontmatter: Record<string, unknown>;
  snapshot_at: Date;
}

putRawData(slug: string, source: string, data: object, opts?: { sourceId?: string }): Promise<void>;
getRawData(slug: string, source?: string, opts?: { sourceId?: string }): Promise<RawData[]>;

createVersion(slug: string, opts?: { sourceId?: string }): Promise<PageVersion>;
getVersions(slug: string, opts?: { sourceId?: string }): Promise<PageVersion[]>;
revertToVersion(slug: string, versionId: number, opts?: { sourceId?: string }): Promise<void>;

updateSlug(oldSlug: string, newSlug: string, opts?: { sourceId?: string }): Promise<void>;
rewriteLinks(oldSlug: string, newSlug: string): Promise<void>;
```

### Raw data semantics

Sources: `src/core/pglite-engine.ts`, `src/core/postgres-engine.ts`, `src/schema.sql`

Schema:

```sql
CREATE TABLE IF NOT EXISTS raw_data (
  id         SERIAL PRIMARY KEY,
  page_id    INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  source     TEXT    NOT NULL,
  data       JSONB   NOT NULL,
  fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(page_id, source)
);

CREATE INDEX IF NOT EXISTS idx_raw_data_page ON raw_data(page_id);
```

Behavior:

- `putRawData(slug, source, data, opts)` inserts sidecar JSON for a page and upserts by `(page_id, source)`.
- On conflict, it updates `data` and refreshes `fetched_at`.
- `getRawData(slug, source?, opts?)` returns `{ source, data, fetched_at }` rows joined through `pages`.
- If `source` is provided, reads filter `rd.source = source`.
- If `opts.sourceId` is provided, reads/writes scope the page lookup to `pages.source_id = opts.sourceId`.
- If `opts.sourceId` is omitted, TS preserves pre-v0.31.8 bare-slug behavior: same-slug rows across sources may all be read, and unscoped writes rely on the bare `slug` lookup.
- Postgres throws when `putRawData` matches no page row; PGLite's insert-select path does not explicitly throw on the no-row case.

### Page version semantics

Sources: `src/core/pglite-engine.ts`, `src/core/postgres-engine.ts`, `src/schema.sql`

Schema:

```sql
CREATE TABLE IF NOT EXISTS page_versions (
  id             SERIAL PRIMARY KEY,
  page_id        INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
  compiled_truth TEXT    NOT NULL,
  frontmatter    JSONB   NOT NULL DEFAULT '{}',
  snapshot_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_versions_page ON page_versions(page_id);
```

Behavior:

- `createVersion(slug, opts)` snapshots `pages.compiled_truth` and `pages.frontmatter` into `page_versions`.
- `createVersion` defaults `sourceId` to `default` when omitted.
- `createVersion` throws when no page is found for `(slug, sourceId)`.
- `getVersions(slug, opts)` joins versions to pages and returns rows ordered by `snapshot_at DESC`.
- `getVersions` uses a source-scoped branch when `opts.sourceId` is present and a bare-slug cross-source branch when omitted.
- `revertToVersion(slug, versionId, opts)` restores `compiled_truth` and `frontmatter` from the selected version row and sets `updated_at = now()`.
- `revertToVersion` scopes both the page lookup and the version relation when `opts.sourceId` is present.
- Without `opts.sourceId`, TS preserves historical same-slug cross-source behavior and can revert the wrong same-slug row if callers do not pass source identity.

### Slug/link rewrite semantics

Sources: `src/core/pglite-engine.ts`, `src/core/postgres-engine.ts`

Behavior:

- `updateSlug(oldSlug, newSlug, opts)` validates `newSlug` through `validateSlug(newSlug)`.
- It defaults `sourceId` to `default` when omitted.
- It updates only `pages.slug` and `pages.updated_at` for `(oldSlug, sourceId)`.
- Related rows that use stable `page_id` foreign keys remain attached naturally.
- `rewriteLinks(oldSlug, newSlug)` is intentionally a no-op because the links table uses integer page-id foreign keys.
- TS Postgres explicitly does not rewrite textual `[[wiki-links]]` in `compiled_truth`; the maintain/dead-link flow surfaces stale text references separately.

## Rust current gap

Current Rust `crates/zbrain-core` has no public or backend surface for these three capability groups:

- No `RawData` type.
- No `PageVersion` type.
- No `put_raw_data` / `get_raw_data` trait methods.
- No `create_version` / `get_versions` / `revert_to_version` trait methods.
- No `update_slug` / `rewrite_links` trait methods.
- No libsql or Postgres schema/migrations for `raw_data` or `page_versions`.
- No InMemory, libsql, or Postgres tests covering raw data, page versions, or slug/link rewrite.

Confirmed by searching:

- `crates/zbrain-core/src`
- `crates/zbrain-core/migrations-sqlite`
- `crates/zbrain-core/migrations`
- `crates/zbrain-core/tests`

for:

```text
RawData
PageVersion
put_raw_data
get_raw_data
create_version
get_versions
revert_to_version
update_slug
rewrite_links
raw_data
page_versions
```

No matches were found.

## Rust public interface proposal

Add complete public shapes matching the TS contract while using Rust-native naming and existing `zbrain-core` conventions.

Types, likely in `crates/zbrain-core/src/types.rs`:

```rust
pub struct RawData {
    pub source: String,
    pub data: serde_json::Value,
    pub fetched_at: String,
}

pub struct PageVersion {
    pub id: u64,
    pub page_id: u64,
    pub compiled_truth: String,
    pub frontmatter: serde_json::Value,
    pub snapshot_at: String,
}
```

Trait methods, likely in `BrainEngine`:

```rust
async fn put_raw_data(
    &self,
    slug: &str,
    source: &str,
    data: &serde_json::Value,
    source_id: Option<&str>,
) -> crate::Result<()>;

async fn get_raw_data(
    &self,
    slug: &str,
    source: Option<&str>,
    source_id: Option<&str>,
) -> crate::Result<Vec<RawData>>;

async fn create_version(
    &self,
    slug: &str,
    source_id: Option<&str>,
) -> crate::Result<PageVersion>;

async fn get_versions(
    &self,
    slug: &str,
    source_id: Option<&str>,
) -> crate::Result<Vec<PageVersion>>;

async fn revert_to_version(
    &self,
    slug: &str,
    version_id: u64,
    source_id: Option<&str>,
) -> crate::Result<()>;

async fn update_slug(
    &self,
    old_slug: &str,
    new_slug: &str,
    source_id: Option<&str>,
) -> crate::Result<()>;

async fn rewrite_links(&self, old_slug: &str, new_slug: &str) -> crate::Result<()>;
```

Notes:

- `source_id: Option<&str>` represents TS `opts?: { sourceId?: string }` without introducing an options struct in the first parity slice.
- `serde_json::Value` is the closest existing public representation for TS `Record<string, unknown>` / JSONB.
- Timestamp fields can follow current Rust string-based row conventions used by existing DB row types; avoid inventing a chrono dependency in this slice.
- `rewrite_links` should exist for TS parity even though implementation is a no-op.

## Schema/migration proposal

Future implementation should add both DB tables to Rust-owned schema/migration paths. This audit does not implement them.

### libsql / SQLite proposal

Add next migration under `crates/zbrain-core/migrations-sqlite/` and bump the libsql schema version.

```sql
CREATE TABLE IF NOT EXISTS raw_data (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id    INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    source     TEXT NOT NULL,
    data       TEXT NOT NULL,
    fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(page_id, source)
);

CREATE INDEX IF NOT EXISTS idx_raw_data_page ON raw_data(page_id);

CREATE TABLE IF NOT EXISTS page_versions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    page_id        INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    compiled_truth TEXT NOT NULL,
    frontmatter    TEXT NOT NULL DEFAULT '{}',
    snapshot_at    TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_versions_page ON page_versions(page_id);
```

Use JSON text round-trips for `data` and `frontmatter`, matching existing libsql JSON handling conventions.

### Postgres proposal

Add next migration under `crates/zbrain-core/migrations/`.

```sql
CREATE TABLE IF NOT EXISTS raw_data (
    id         BIGSERIAL PRIMARY KEY,
    page_id    INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    source     TEXT NOT NULL,
    data       JSONB NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(page_id, source)
);

CREATE INDEX IF NOT EXISTS idx_raw_data_page ON raw_data(page_id);

CREATE TABLE IF NOT EXISTS page_versions (
    id             BIGSERIAL PRIMARY KEY,
    page_id        INTEGER NOT NULL REFERENCES pages(id) ON DELETE CASCADE,
    compiled_truth TEXT NOT NULL,
    frontmatter    JSONB NOT NULL DEFAULT '{}'::jsonb,
    snapshot_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_versions_page ON page_versions(page_id);
```

## TDD slice plan

1. Contract compile slice
   - Add `RawData` and `PageVersion` public types.
   - Add the seven trait methods.
   - Update object-safety coverage.
   - Keep backend methods minimally compiling or use focused tests to drive each implementation in sequence.

2. InMemory raw data slice
   - `put_raw_data` inserts and upserts by `(page_id, source)` after resolving page by slug/source scope.
   - `get_raw_data` filters by slug, optional raw source, and optional source id.
   - Verify unscoped reads preserve cross-source same-slug behavior.

3. InMemory page versions and slug/link slice
   - `create_version` snapshots compiled truth/frontmatter for `(slug, source_id.unwrap_or("default"))`.
   - `get_versions` orders newest first.
   - `revert_to_version` restores compiled truth/frontmatter and bumps update timestamp.
   - `update_slug` validates slug and scopes by `source_id.unwrap_or("default")`.
   - `rewrite_links` is a no-op that returns success.

4. libsql schema/backend slice
   - Add SQLite migration and schema-version bump.
   - Implement raw data methods with JSON text round-trip and `(page_id, source)` upsert.
   - Implement page versions methods and slug/link methods.
   - Cover missing-page behavior intentionally; decide whether to mirror Postgres throw semantics or preserve PGLite no-op behavior for raw data no-row writes.

5. Postgres schema/backend slice
   - Add Postgres migration.
   - Implement raw data methods with `JSONB`, upsert, and Postgres no-page error behavior.
   - Implement page versions methods and slug/link methods.
   - Confirm `rewrite_links` remains no-op and textual wiki links are not modified.

6. Final validation / closure slice
   - Run focused tests for all three backends.
   - Run object-safety, formatting, and whitespace checks.
   - Update roadmap and close follow-up issues if created.

## Validation commands

Use the known Windows GNU target override in this workspace:

```bash
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test engine_object_safety
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test in_memory_engine_contract raw_data
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test in_memory_engine_contract version
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test in_memory_engine_contract slug
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test libsql_engine_advanced_page_writes
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test postgres_engine_advanced_page_writes
cargo fmt -p zbrain-core --check
git diff --check
```

## Open decision for implementation

The only behavior that needs an explicit implementation-time decision is raw-data no-page behavior:

- TS Postgres throws if no page row is found.
- TS PGLite currently performs an insert-select that can no-op when no page row is selected.

Recommendation: prefer explicit errors in Rust DB backends for missing pages unless a compatibility test requires the PGLite no-op behavior. Record the final decision on the implementation child node before coding that slice.
