# File Storage Contract Parity Audit

Date: 2026-06-24
Roadmap node: `1-2-1-6 Add Rust file storage contract parity`
GitHub issue: `#11 Add Rust file storage contract parity`

## Scope

This is a narrow contract audit for Rust file-storage parity. It defines the public shape and TDD plan only; implementation is deferred to the next `/zj-tdd` pass.

In scope:

- TS `BrainEngine` file metadata contract.
- Rust `zbrain-core` public trait/types proposal.
- `files` table schema needed by the three parity methods.
- InMemory, libsql, and Postgres behavior slices.

Out of scope:

- `deleteFile` / `delete_file`.
- Generic `listFiles` / `list_files`.
- `file_migration_ledger`.
- CLI upload flow.
- Binary/blob storage; only metadata rows are stored.
- Changing file identity to `(source_id, storage_path)`.

## TS source-of-truth facts

### Public contract

Source: `src/core/engine.ts`

```ts
export interface FileRow {
  id: number;
  source_id: string;
  page_slug: string | null;
  page_id: number | null;
  filename: string;
  storage_path: string;
  mime_type: string | null;
  size_bytes: number | null;
  content_hash: string;
  metadata: Record<string, unknown>;
  created_at: Date;
}

export interface FileSpec {
  source_id?: string;
  page_slug?: string | null;
  page_id?: number | null;
  filename: string;
  storage_path: string;
  mime_type?: string | null;
  size_bytes?: number | null;
  content_hash: string;
  metadata?: Record<string, unknown>;
}

upsertFile(spec: FileSpec): Promise<{ id: number; created: boolean }>;
getFile(sourceId: string, storagePath: string): Promise<FileRow | null>;
listFilesForPage(pageId: number): Promise<FileRow[]>;
```

### Backend semantics

Sources: `src/core/pglite-engine.ts`, `src/core/postgres-engine.ts`

- `upsertFile` defaults `source_id` to `default`.
- It inserts metadata into `files` and upserts on `storage_path`.
- Re-upsert with the same `storage_path` updates row fields in place and returns the same `id` with `created=false`.
- `getFile(sourceId, storagePath)` filters by both `source_id` and `storage_path`.
- `listFilesForPage(pageId)` filters by `page_id`.
- File bytes never enter the DB; `storage_path` is the pointer to external/repo storage.

### Schema semantics

Source: `src/schema.sql`

```sql
CREATE TABLE IF NOT EXISTS files (
  id           SERIAL PRIMARY KEY,
  source_id    TEXT   NOT NULL DEFAULT 'default'
               REFERENCES sources(id) ON DELETE CASCADE,
  page_slug    TEXT,
  page_id      INTEGER REFERENCES pages(id) ON DELETE SET NULL,
  filename     TEXT   NOT NULL,
  storage_path TEXT   NOT NULL,
  mime_type    TEXT,
  size_bytes   BIGINT,
  content_hash TEXT   NOT NULL,
  metadata     JSONB  NOT NULL DEFAULT '{}',
  created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE(storage_path)
);
```

Important decision: even though TS comments mention identity `(source_id, storage_path)`, the actual schema and backend conflict target are `UNIQUE(storage_path)`. Rust parity must follow the running TS implementation.

### Existing TS tests

Source: `tests/unit/engine-upsertFile.test.ts`

Covered behavior:

- Insert returns positive `id` and `created=true`.
- `getFile` round-trips `filename`, `mime_type`, `size_bytes`, `content_hash`, and `source_id`.
- Same `storage_path` upsert returns same `id` and `created=false`.
- Changed `content_hash` updates row metadata in place.
- `listFilesForPage(page_id)` returns page-linked files.
- Unknown path returns `null`.
- API returns `source_id` for multi-source rows.

## Rust gap

Current `crates/zbrain-core` has no file-storage parity surface:

- No `FileRow`.
- No `FileSpec`.
- No `UpsertFileResult`.
- No `upsert_file` / `get_file` / `list_files_for_page` trait methods.
- No Rust `files` schema/migration in libsql or Postgres setup.
- No InMemory/libsql/Postgres tests for file metadata behavior.

## Rust public interface proposal

Add complete public shapes matching the TS contract:

```rust
pub struct FileRow {
    pub id: u64,
    pub source_id: String,
    pub page_slug: Option<String>,
    pub page_id: Option<u64>,
    pub filename: String,
    pub storage_path: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub content_hash: String,
    pub metadata: serde_json::Value,
    pub created_at: String,
}

pub struct FileSpec {
    pub source_id: Option<String>,
    pub page_slug: Option<String>,
    pub page_id: Option<u64>,
    pub filename: String,
    pub storage_path: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub content_hash: String,
    pub metadata: Option<serde_json::Value>,
}

pub struct UpsertFileResult {
    pub id: u64,
    pub created: bool,
}
```

Add only the TS parity methods to `BrainEngine`:

```rust
async fn upsert_file(&self, spec: &FileSpec) -> crate::Result<UpsertFileResult>;
async fn get_file(&self, source_id: &str, storage_path: &str) -> crate::Result<Option<FileRow>>;
async fn list_files_for_page(&self, page_id: u64) -> crate::Result<Vec<FileRow>>;
```

Do not add `delete_file` or generic `list_files` in this slice.

## TDD slice plan

1. Contract compile slice
   - Add `FileSpec`, `FileRow`, `UpsertFileResult`.
   - Add the three trait methods.
   - Update object-safety coverage.

2. InMemory behavior slice
   - Insert returns `created=true` and a stable positive id.
   - Same `storage_path` updates in place and returns `created=false` with same id.
   - `get_file` filters by `source_id` and `storage_path`.
   - `list_files_for_page` filters by `page_id`.

3. libsql DB parity slice
   - Add `files` table to Rust libsql schema/migration path.
   - Use `UNIQUE(storage_path)`.
   - Implement three methods with metadata JSON roundtrip.
   - Run focused libsql tests.

4. Postgres DB parity slice
   - Add `files` table to Rust Postgres schema/migration path.
   - Use `UNIQUE(storage_path)`.
   - Implement three methods with metadata JSON roundtrip.
   - Run focused Postgres tests via `PgFixture`.

## Validation commands

Use the known Windows GNU override in this workspace:

```bash
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test engine_object_safety
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test in_memory_engine_contract file
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test libsql_engine_file_storage
PATH="/c/msys64/mingw64/bin:$PATH" cargo test -p zbrain-core --target x86_64-pc-windows-gnu --test postgres_engine_file_storage
cargo fmt -p zbrain-core --check
git diff --check
```
