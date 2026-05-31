# Slice 6a-pg: PostgresEngine 镜像切片计划

> 契约来源: `13-slice-6a-gap-checklist.md` §10 C7、§11 item 7、§12 S6-T5  
> 优先级: P1 — 允许延后但不允许遗漏  
> 前置依赖: Slice 6a 主切片 (libsql + InMemory 完整实现)  
> 预计影响文件: `postgres.rs`, `migrations/0002_*.sql`, `migrations/0003_*.sql`

---

## 1. 目标

将 Slice 6a 已在 `LibsqlEngine` + `InMemoryEngine` 上落地的 13 个 Page 方法镜像到 `PostgresEngine`，使 PG 引擎与 libsql 引擎观测等价。具体包括：

1. **13 方法真实语义实现** — 替换 `Err(Error::unsupported("pending slice 6a-pg"))` 占位
2. **全列投影** — `row_to_page` 从 7 列扩展到 27+ 列，消除硬编码 default
3. **`put_page` 全列 upsert** — 从 4 列 INSERT 升级到完整列，并按 S6-T8 契约使用参数化 `source_id`
4. **`get_page` deleted_at 过滤** — 支持 `include_deleted` 选项
5. **`list_pages` 9 项过滤** — 完整实现 `PageFilters` 的 PG 方言
6. **`resolve_slugs` ILIKE** — 从精确匹配改为模糊匹配
7. **PG migration 同步** — 将 SQLite 0002/0003 翻译为 PG 方言
8. **三连绿** — `cargo build` / `cargo test` / `cargo clippy` 全通过
9. **Git tag** — `slice-6a-pg`

---

## 2. PG 方言反向适配总则

6a 主切片做的是 PG → SQLite 方向；6a-pg 做反向 (SQLite → PG)：

| # | SQLite / libsql | PostgreSQL | 说明 |
|---|---|---|---|
| R1 | `?1`, `?2`, `?3`… | `$1`, `$2`, `$3`… | 位置参数语法 |
| R2 | `CURRENT_TIMESTAMP` | `now()` | 当前时间函数 |
| R3 | `json_extract(col, '$.key')` | `col->>'key'` (text) / `col->'key'` (jsonb) | JSON 提取 |
| R4 | `LIKE '%partial%'` | `ILIKE '%partial%'` | 大小写不敏感匹配 |
| R5 | `TEXT` (JSONB) | `JSONB` | JSON 列类型 |
| R6 | `TEXT` (ISO-8601) | `TIMESTAMPTZ` | 时间列类型 |
| R7 | `INTEGER` | `BIGINT` / `BIGSERIAL` | 大整数 |
| R8 | `BLOB` | `BYTEA` | 二进制数据 (embedding) |
| R9 | `REAL` | `DOUBLE PRECISION` | 浮点数 |
| R10 | `LIMIT -1` | `LIMIT ALL` 或省略 | 无上限 |
| R11 | `ON CONFLICT(col1, col2) DO UPDATE` | `ON CONFLICT (col1, col2) DO UPDATE` | 语法一致，PG 可用约束名 |
| R12 | 多 `?` 展开 (ANY 模拟) | `ANY($N::text[])` | 数组包含 |
| R13 | `datetime(col, '+N days')` | `col + INTERVAL 'N days'` | 日期运算 |
| R14 | 客户端循环 (unnest 模拟) | `unnest()` | 数组展开 |

---

## 3. PG Migration 同步 ✅ 已完成(实际形态与原方案不同)

### 3.1 真实落地形态(修订)

PG migration 目录 (`crates/zbrain-core/migrations/`) 当前 **5 个文件**,而非原方案设计的 3 个;切片演进过程中按"小切片"原则拆得更细:

| 文件 | 切片来源 | 内容 |
|---|---|---|
| `0001_init.sql` | Slice 4a | 7 列 pages 表(基础) |
| `0002_pages_deleted_at.sql` | Slice 5x | 仅追加 `deleted_at TIMESTAMPTZ` + purge 索引 |
| `0003_pages_full_columns.sql` | Slice 6a-pg 主体 | 19 个 ALTER TABLE ADD COLUMN + 索引 + generation trigger |
| `0004_pages_pg_align_ts.sql` | Slice 6a-pg 收尾 | TIMESTAMPTZ 对齐 / now() 默认值 / trigger 微调 |
| `0005_page_tags.sql` | PG-tag (5ca9131) | `page_tags(page_id, source_id, tag)` 关联表 + 索引,与 libsql `0004_page_tags.sql` parity |

> **不再有 `migrations-sqlite/` 共享路径**: libsql 的 SQLite 方言 migration 走 `migrations-sqlite/` 目录, PG 走 `migrations/`, 两条物理路径完全独立, 仅通过"等价 schema 契约"对齐。

### 3.2 与原方案的偏差(已接受)

| 原方案 | 实际 | 接受理由 |
|---|---|---|
| 单文件 `0002_pages_full_columns.sql` | 拆为 `0002_*deleted_at*` + `0003_*full_columns*` + `0004_*align_ts*` | 演进过程中按"一事一议"自然拆分; 单文件回放风险更高 |
| 单文件 `0003_salience_and_full_generation_trigger.sql` | salience_score 列合并进 `0003_pages_full_columns.sql`; trigger 在 `0003` + `0004` 中分两次调整 | 与 libsql `0003_salience_...` 在 schema 终态等价, 物理拆分不影响 parity |
| 未规划 page_tags | 由独立切片 PG-tag (`5ca9131`) 落入 `0005_page_tags.sql` | tag 是独立特性, 拆切更稳定; 已与 libsql `0004_page_tags.sql` parity |

### 3.3 0002 PG 方言要点 (📜 历史设计参考 — 实际见 `0003_pages_full_columns.sql`)

```sql
-- TEXT → JSONB
ALTER TABLE pages ADD COLUMN frontmatter JSONB NOT NULL DEFAULT '{}';

-- TEXT → TIMESTAMPTZ (无 DEFAULT now() 因为仅 6a 新增列允许 NULL)
ALTER TABLE pages ADD COLUMN deleted_at TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN effective_date TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN ingested_at TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN salience_touched_at TIMESTAMPTZ;
ALTER TABLE pages ADD COLUMN last_retrieved_at TIMESTAMPTZ;

-- REAL → DOUBLE PRECISION
ALTER TABLE pages ADD COLUMN emotional_weight DOUBLE PRECISION NOT NULL DEFAULT 0.0;

-- BLOB → BYTEA
ALTER TABLE pages ADD COLUMN embedding BYTEA;

-- INTEGER → BIGINT (generation)
ALTER TABLE pages ADD COLUMN generation BIGINT NOT NULL DEFAULT 1;

-- 索引
CREATE INDEX IF NOT EXISTS idx_pages_source_id ON pages(source_id);
CREATE INDEX IF NOT EXISTS pages_deleted_at_purge_idx ON pages(deleted_at) WHERE deleted_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS pages_coalesce_date_idx ON pages (COALESCE(effective_date, updated_at));
CREATE INDEX IF NOT EXISTS pages_last_retrieved_at_idx ON pages(last_retrieved_at);

-- Generation bump trigger (PG BEFORE INSERT/UPDATE 语法 — 可以直接赋值 NEW.generation)
CREATE OR REPLACE FUNCTION bump_page_generation_fn() RETURNS TRIGGER AS $$
BEGIN
  IF TG_OP = 'INSERT' THEN
    NEW.generation := COALESCE((SELECT MAX(generation) FROM pages WHERE id <> NEW.id), 0) + 1;
  ELSIF TG_OP = 'UPDATE' THEN
    IF NEW.compiled_truth IS DISTINCT FROM OLD.compiled_truth
       OR NEW.timeline IS DISTINCT FROM OLD.timeline
       OR NEW.frontmatter IS DISTINCT FROM OLD.frontmatter
       OR NEW.deleted_at IS DISTINCT FROM OLD.deleted_at
       OR NEW.contextual_retrieval_mode IS DISTINCT FROM OLD.contextual_retrieval_mode
       OR NEW.title IS DISTINCT FROM OLD.title
       OR NEW.type IS DISTINCT FROM OLD.type
       OR NEW.page_kind IS DISTINCT FROM OLD.page_kind
       OR NEW.corpus_generation IS DISTINCT FROM OLD.corpus_generation
       OR NEW.content_hash IS DISTINCT FROM OLD.content_hash
    THEN
      NEW.generation := OLD.generation + 1;
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS bump_page_generation_trg ON pages;
CREATE TRIGGER bump_page_generation_trg
  BEFORE INSERT OR UPDATE ON pages
  FOR EACH ROW
  EXECUTE FUNCTION bump_page_generation_fn();
```

### 3.4 0003 PG 方言要点 (📜 历史设计参考 — 实际拆入 `0003`/`0004`)

```sql
ALTER TABLE pages ADD COLUMN salience_score DOUBLE PRECISION;

-- 重建 trigger 用完整 10 列 allow-list (与 0002 相同，这里需 DROP + CREATE 确保幂等)
-- 内容与 0002 的 bump_page_generation_fn 一致
```

---

## 4. `row_to_page` 全列投影升级

### 4.1 现状

`postgres.rs` `row_to_page` (行 235-303) 仅解码 7 列，其余硬编码 default：

```rust
// S2 placeholder: generation = 1, chunker_version = 1, 其余 None
```

### 4.2 目标

投影全部 27+ 列，与 `libsql.rs` `full_row_to_page` 对齐。列顺序必须与 SELECT 投影一致：

| # | 列名 | PG 类型 | Rust 类型 | 备注 |
|---|---|---|---|---|
| 0 | id | BIGINT | i64 | |
| 1 | slug | TEXT | String | |
| 2 | type | TEXT | String | `page_type` |
| 3 | page_kind | TEXT | String | |
| 4 | title | TEXT | String | |
| 5 | compiled_truth | TEXT | String | |
| 6 | timeline | TEXT | String | |
| 7 | frontmatter | JSONB | String | serde_json at app layer |
| 8 | content_hash | TEXT | Option<String> | nullable |
| 9 | emotional_weight | DOUBLE PRECISION | f64 | |
| 10 | created_at | TIMESTAMPTZ | String | ISO-8601 |
| 11 | updated_at | TIMESTAMPTZ | String | ISO-8601 |
| 12 | deleted_at | TIMESTAMPTZ | Option<String> | nullable |
| 13 | last_retrieved_at | TIMESTAMPTZ | Option<String> | nullable |
| 14 | effective_date | TIMESTAMPTZ | Option<String> | nullable |
| 15 | effective_date_source | TEXT | Option<String> | nullable |
| 16 | import_filename | TEXT | Option<String> | nullable |
| 17 | salience_touched_at | TIMESTAMPTZ | Option<String> | nullable |
| 18 | salience_score | DOUBLE PRECISION | Option<f64> | nullable |
| 19 | generation | BIGINT | i64 | |
| 20 | embedding | BYTEA | Option<Vec<u8>> | nullable |
| 21 | chunker_version | INTEGER | Option<i32> | nullable |
| 22 | source_path | TEXT | Option<String> | nullable |
| 23 | source_id | TEXT | String | |
| 24 | source_kind | TEXT | Option<String> | nullable |
| 25 | source_uri | TEXT | Option<String> | nullable |
| 26 | ingested_via | TEXT | Option<String> | nullable |
| 27 | ingested_at | TIMESTAMPTZ | Option<String> | nullable |
| 28 | contextual_retrieval_mode | TEXT | Option<String> | nullable |
| 29 | corpus_generation | TEXT | Option<String> | nullable |

### 4.3 实现

替换 `row_to_page` 为 `full_row_to_page`（或扩展原函数），使用 `sqlx::Row::try_get` 按列名取值。保留旧 7 列函数作为内部辅助（仅 `get_page` 简单路径可用），但所有对外方法一律使用全列投影。

**注意**: `sqlx` 的 `FromRow` derive 或手动 `try_get` 对 `JSONB` 列返回 `String`（serde_json 处理），对 `BYTEA` 返回 `Vec<u8>`。需确认 `Page.embedding` 字段类型为 `Option<Vec<u8>>`。

---

## 5. 13 方法逐一 PG SQL 实现

### 基础 CRUD (5 方法 — 已有占位，需升级)

#### 5.1 `get_page(slug, opts) → Option<Page>`

**现状**: 7 列投影，不支持 `include_deleted`。

**PG SQL**:
```sql
SELECT {全列}
FROM pages
WHERE slug = $1
  AND ($2 IS NULL OR deleted_at IS NULL)
```

- `$2` = `if !opts.include_deleted { Some(false) } else { None }` — 即 `include_deleted=false` 时过滤 deleted_at IS NULL
- **注意**: 需调整逻辑。当 `include_deleted = false` (默认): 加 `AND deleted_at IS NULL`; 当 `include_deleted = true`: 不过滤。

**Rust 伪代码**:
```rust
if opts.include_deleted {
    // 不过滤 deleted_at
    query("... WHERE slug = $1")
} else {
    query("... WHERE slug = $1 AND deleted_at IS NULL")
}
```

#### 5.2 `put_page(slug, source_id, input) → Page`

**现状**: 4 列 INSERT (slug, type, title, compiled_truth)，ON CONFLICT UPDATE 3 列；S6-T8 后 trait 已接受 `source_id: Option<&str>`，但当前 `PostgresEngine` 仍以 `_source_id` 占位且未接入 SQL，实际依赖 schema default `'default'`。

**S6-T8 契约**:
```rust
let source_id = source_id.unwrap_or("default");
```

- `None` 必须归一为 `"default"`。
- `Some(id)` 必须写入指定 source。
- 同一 `slug` 在不同 `source_id` 下必须是独立 Page。
- 只有同一 `(source_id, slug)` 才应触发 upsert。
- 写入非 default source 前，测试 fixture 必须先 seed `sources(id)`，否则会违反 `pages.source_id -> sources(id)` 外键。

**PG SQL — 全列 upsert**:
```sql
INSERT INTO pages (
    slug, type, page_kind, title, compiled_truth, timeline,
    frontmatter, content_hash, emotional_weight,
    effective_date, effective_date_source, import_filename,
    chunker_version, source_path, source_kind, source_uri,
    ingested_via, ingested_at, contextual_retrieval_mode, corpus_generation,
    last_retrieved_at, embedding, source_id
) VALUES (
    $1, $2, $3, $4, $5, $6,
    $7, $8, $9,
    $10, $11, $12,
    $13, $14, $15, $16,
    $17, $18, $19, $20,
    $21, $22, $23
)
-- $23 = source_id.unwrap_or("default")
ON CONFLICT (source_id, slug) DO UPDATE SET
    type = EXCLUDED.type,
    page_kind = EXCLUDED.page_kind,
    title = EXCLUDED.title,
    compiled_truth = EXCLUDED.compiled_truth,
    timeline = EXCLUDED.timeline,
    frontmatter = EXCLUDED.frontmatter,
    content_hash = EXCLUDED.content_hash,
    emotional_weight = EXCLUDED.emotional_weight,
    effective_date = EXCLUDED.effective_date,
    effective_date_source = EXCLUDED.effective_date_source,
    import_filename = EXCLUDED.import_filename,
    chunker_version = EXCLUDED.chunker_version,
    source_path = EXCLUDED.source_path,
    source_kind = EXCLUDED.source_kind,
    source_uri = EXCLUDED.source_uri,
    ingested_via = EXCLUDED.ingested_via,
    ingested_at = EXCLUDED.ingested_at,
    contextual_retrieval_mode = EXCLUDED.contextual_retrieval_mode,
    corpus_generation = EXCLUDED.corpus_generation,
    last_retrieved_at = EXCLUDED.last_retrieved_at,
    embedding = EXCLUDED.embedding,
    updated_at = now()
RETURNING {全列}
```

#### 5.3 `delete_page(slug) → ()`

**现状**: 已实现，无需改动。PG `DELETE FROM pages WHERE slug = $1`。

#### 5.4 `list_pages(filters) → Vec<Page>`  ✅ 已实现 (`5ca9131` + 之前的 PG full-column 链路)

**实际实现**: 动态拼装 SQL,9 项过滤已全部上线。tag filter 走 `page_tags` JOIN(与 libsql `0004` parity),不再用 JSONB `?` + title fallback。

**bind 顺序契约 (postgres.rs `build_list_pages_sql`, 行 114-178)**:

```text
page_type → source_id → source_ids → slug_prefix → updated_after → tag → limit → offset
```

(与 libsql 引擎一致;`include_deleted` 不占位,直接拼到 `WHERE`。)

**PG SQL 真实形态**:

```sql
SELECT {全列}
FROM pages AS p
[JOIN page_tags AS pt ON pt.page_id = p.id]          -- 仅当 tag filter 启用时 JOIN
WHERE TRUE
  AND p.deleted_at IS NULL                            -- 默认隐藏 soft-deleted(include_deleted=false)
  AND ($N1::text IS NULL OR p.type = $N1)             -- page_type
  AND ($N2::text IS NULL OR p.source_id = $N2)        -- source_id
  AND ($N3::text[] IS NULL OR p.source_id = ANY($N3)) -- source_ids
  AND ($N4::text IS NULL OR p.slug LIKE $N4 || '%')   -- slug_prefix
  AND ($N5::text IS NULL OR p.updated_at > $N5)       -- updated_after
  AND ($N6::text IS NULL OR pt.tag = $N6)             -- tag (page_tags JOIN)
ORDER BY <sort 白名单映射>, p.id ASC
LIMIT $N7 OFFSET $N8
```

**注意**:
- **tag filter 已锁定**: 走 `page_tags(page_id, tag)` JOIN(与 libsql `0004` parity),无需 JSONB `?` 操作符,也无需 title fallback。schema 由 `migrations/0005_page_tags.sql` 提供。GIN 索引留给 slice 6e(只在 tag 维度需要 FTS 时再加)。
- `slug_prefix` 用 `LIKE $N4 || '%'` (PG 字符串连接) 而非 `LIKE '%...'`。
- `source_ids` (Vec) 用 `ANY($N3::text[])` — sqlx `Vec<String>` 直接绑定成功。
- `sort` 字段用 `match` 白名单映射到 ORDER BY 子句,不拼接用户输入。

**对齐链路**:
- libsql `S6-T5c` (`bb9e5bc`) — `page_tags` schema + tag CRUD + `list_pages(tag)` JOIN
- PG-tag (`5ca9131`) — 镜像上面三件套,完成 PG↔libsql parity

#### 5.5 `resolve_slugs(partial) → Vec<String>`

**现状**: 精确匹配 `WHERE slug = $1`。

**PG SQL — ILIKE 模糊匹配**:
```sql
SELECT slug FROM pages
WHERE slug ILIKE '%' || $1 || '%'
  AND deleted_at IS NULL
ORDER BY slug ASC
```

- FixMe 标记: `ILIKE` 模糊匹配；生产需 `pg_trgm` GIN 索引 (slice 6e)
- 新增 `deleted_at IS NULL` 过滤 (6a 主切片也可能已加)

---

### 高级方法 (8 方法 — 当前全部 unsupported)

#### 5.6 `find_duplicate_page(source_id, opts) → Option<Page>`

**libsql SQL** (SQLite 方言):
```sql
SELECT {全列}
FROM pages
WHERE source_id = ?1
  AND deleted_at IS NULL
  AND (content_hash = ?2 OR (?3 IS NOT NULL AND json_extract(frontmatter, '$.id') = ?3))
ORDER BY id ASC
LIMIT 1
```

**PG SQL**:
```sql
SELECT {全列}
FROM pages
WHERE source_id = $1
  AND deleted_at IS NULL
  AND (content_hash = $2 OR ($3 IS NOT NULL AND frontmatter->>'id' = $3))
ORDER BY id ASC
LIMIT 1
```

- `json_extract(frontmatter, '$.id')` → `frontmatter->>'id'` (R3)

#### 5.7 `soft_delete_page(slug, source_id) → Option<String>`

**libsql SQL**:
```sql
UPDATE pages
SET deleted_at = CURRENT_TIMESTAMP
WHERE slug = ?1
  AND deleted_at IS NULL
  AND (?2 IS NULL OR source_id = ?2)
RETURNING slug
```

**PG SQL**:
```sql
UPDATE pages
SET deleted_at = now()
WHERE slug = $1
  AND deleted_at IS NULL
  AND ($2 IS NULL OR source_id = $2)
RETURNING slug
```

- `CURRENT_TIMESTAMP` → `now()` (R2)

#### 5.8 `restore_page(slug, source_id: Option<&str>) → Result<bool>`

> **签名修订 (2026-05-31 PG-soft-delete 切片落地)**：原计划写作 `restore_page(slug) → Option<Page>`，
> 与 trait 实际签名（`Result<bool>`，并需 source guard）不一致；以本节为准。

**PG SQL**:
```sql
UPDATE pages
SET deleted_at = NULL
WHERE slug = $1
  AND deleted_at IS NOT NULL
  AND ($2::text IS NULL OR source_id = $2)
```

- 返回受影响行数 > 0 即 `Ok(true)`，否则 `Ok(false)`；不返回完整 Page。
- 注意: 6a 主切片 InMemory 实现中 `restore_page` 不需要 SQL，PG 需要。

#### 5.9 `purge_deleted_pages(older_than_hours: u32, source_id: Option<&str>) → Result<PurgeResult>`

> **签名修订 (2026-05-31 PG-soft-delete 切片落地)**：原计划写作 `purge_page(slug) → Option<String>`，
> 与 trait 实际签名（按时间窗口批量、返回 `PurgeResult { slugs, count }`）不一致；以本节为准。

**PG SQL**:
```sql
DELETE FROM pages
WHERE deleted_at IS NOT NULL
  AND deleted_at < now() - ($1::text || ' hours')::interval
  AND ($2::text IS NULL OR source_id = $2)
RETURNING slug
```

- 只删除 soft-deleted 且超过 `older_than_hours` 的行；返回 `PurgeResult { slugs, count }`（`count = slugs.len() as u64`）。
- FK CASCADE 自动清理 `page_chunks` / `page_links` 子行。

#### 5.10 `refresh_page_body(slug, args) → Option<Page>`

**PG SQL**:
```sql
UPDATE pages
SET compiled_truth = $2,
    content_hash = $3,
    frontmatter = $4,
    timeline = $5,
    updated_at = now()
WHERE slug = $1
  AND deleted_at IS NULL
RETURNING {全列}
```

- `RefreshPageBodyArgs` 字段: `compiled_truth`, `content_hash`, `frontmatter`, `timeline`

#### 5.11 `update_page_contextual_retrieval_state(slug, mode) → Option<Page>`

**PG SQL**:
```sql
UPDATE pages
SET contextual_retrieval_mode = $2,
    updated_at = now()
WHERE slug = $1
  AND deleted_at IS NULL
RETURNING {全列}
```

#### 5.12 `get_all_slugs(source_id: Option<&str>) → HashSet<String>`

> ⚠️ 此处原签名与 `engine.rs` L383-388 不符，且原 SQL 与 TS 行为相反。**真实签名 + TS-aligned SQL 已在 §11.1 / §11.2 重写**；本节保留作为历史记录，落地以 §11 为准。

**PG SQL（TS-aligned，**不**过滤 `deleted_at`，见 §11.6 偏差 D11-r1）**:
```sql
-- source_id = Some
SELECT slug FROM pages WHERE source_id = $1
-- source_id = None
SELECT slug FROM pages
```

#### 5.13 `list_all_page_refs() → Vec<PageRef>`

> ⚠️ 此处原列投影与 `PageRef` 实际字段不符（`PageRef` 仅 `slug, source_id`），且 ORDER BY 与 TS `pglite-engine.ts` L1088-1098 不一致。**真实签名 + TS-aligned SQL 已在 §11.3 重写**；本节保留作为历史记录，落地以 §11 为准。

**PG SQL（TS-aligned，ORDER BY source_id, slug）**:
```sql
SELECT slug, source_id
FROM pages
WHERE deleted_at IS NULL
ORDER BY source_id, slug
```

- `PageRef` 真实结构（详见 `engine.rs`）: `{ slug: String, source_id: Option<String> }`

#### 5.14 `update_slug(old_slug, new_slug) → Option<Page>`

**PG SQL**:
```sql
UPDATE pages
SET slug = $2,
    updated_at = now()
WHERE slug = $1
  AND deleted_at IS NULL
RETURNING {全列}
```

#### 5.15 `get_page_timestamps(slugs: &[String]) → HashMap<String, String>`

> ⚠️ 此处原签名（按 `source_id` 拉所有页）与 `engine.rs` L414-419 不符（实际按 slug 数组查询，返回 `COALESCE(updated_at, created_at)` 单一时间戳）。**真实签名 + TS-aligned SQL 已在 §11.4 重写**；本节保留作为历史记录，落地以 §11 为准。

**PG SQL（TS-aligned，按 slug 数组、COALESCE 单时间戳）**:
```sql
SELECT slug, COALESCE(updated_at, created_at) AS ts
FROM pages
WHERE slug = ANY($1::text[])
  AND deleted_at IS NULL
```

- 返回类型: `HashMap<slug, ts>`，ts 为 ISO-8601 字符串

#### 5.16 `get_effective_dates(refs: &[PageRef]) → HashMap<String, String>`

> ⚠️ 此处原签名（按 `source_id` 拉所有页）与 `engine.rs` L427-432 不符（实际按 `PageRef` 数组 `(slug, source_id)` 二维查询，key 为 `"{source_id}::{slug}"`）。**真实签名 + TS-aligned SQL 已在 §11.5 重写**；本节保留作为历史记录，落地以 §11 为准。

**PG SQL（TS-aligned，`unnest` 二维 join，key=`source_id::slug`）**:
```sql
SELECT p.slug,
       COALESCE(p.source_id, '') AS source_id,
       p.effective_date::text     AS effective_date
FROM pages p
JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
  ON p.slug = u.slug
  AND COALESCE(p.source_id, '') = COALESCE(u.source_id, '')
WHERE p.deleted_at IS NULL
  AND p.effective_date IS NOT NULL
```

- 返回类型: `HashMap<"{source_id}::{slug}", effective_date>`，仅含有 effective_date 的条目

#### 5.17 `get_salience_scores(refs: &[PageRef]) → HashMap<String, f64>`

> ⚠️ 此处原签名（无参数、按 `salience_score` 列读出）与 `engine.rs` L447-452 不符（实际按 `PageRef` 数组查询、返回 6a 退化公式 `emotional_weight * 5`，6c 补全为 `+ ln(1 + tag_count)`）。**真实签名 + TS-aligned SQL 已在 §11.6 重写**；本节保留作为历史记录，落地以 §11 为准。

**PG SQL（TS-aligned，6a 退化为 `emotional_weight * 5`，key=`source_id::slug`）**:
```sql
SELECT p.slug,
       COALESCE(p.source_id, '') AS source_id,
       (p.emotional_weight * 5)  AS score
FROM pages p
JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id)
  ON p.slug = u.slug
  AND COALESCE(p.source_id, '') = COALESCE(u.source_id, '')
WHERE p.deleted_at IS NULL
  AND p.emotional_weight IS NOT NULL
```

- 返回类型: `HashMap<"{source_id}::{slug}", score>`
- 6c 阶段公式补全为 `emotional_weight * 5 + ln(1 + tag_count)`（详见 6c 切片）

#### 5.18 `touch_salience(slug) → Option<String>`

**PG SQL**:
```sql
UPDATE pages
SET salience_touched_at = now()
WHERE slug = $1
  AND deleted_at IS NULL
RETURNING slug
```

---

## 6. placeholder-lock 测试更新

### 6.1 实际形态(修订)

Slice 6a 的 placeholder-lock 测试**并不在 `postgres_*` 文件里**,而是落在 `crates/zbrain-core/tests/page_methods_*.rs` 共 14 个文件,断言 **`BrainEngine` trait 的默认实现** 返回 `Err(Error::unsupported("pending slice 6a"))`。

这意味着:
- 红测锁的是 **trait 默认实现**,与具体引擎(PG / libsql / InMemory)无关。
- 任何一个引擎一旦 override 某个方法,该引擎在对应红测上就会"绿",但只要还有引擎未 override,该红测就**应继续存在**。
- libsql 已 override 了全部 13 个高级方法(`S6-T5b/T5c/T6/T7/T8` 链路);PG 当前仅 override 了 tag CRUD(`5ca9131`),其余 10 个仍走 trait 默认 `unsupported`。

### 6.2 当前 14 个 `page_methods_*.rs` 实际状态

| 文件 | 锁定语义 | 当前命中后端 | PG-tag 之后是否改造 |
|---|---|---|---|
| `page_methods_find_duplicate_page.rs` | trait 默认 unsupported | InMemory / PG / libsql | ✅ 已完成（`39a4f68`） |
| `page_methods_soft_delete_page.rs` | trait 默认 unsupported | InMemory / PG / libsql | ✅ 已完成（`2568268`） |
| `page_methods_restore_page.rs` | trait 默认 unsupported | InMemory / PG | ✅ 已完成（`2568268`） |
| `page_methods_purge_deleted_pages.rs` | trait 默认 unsupported | InMemory / PG | ✅ 已完成（`2568268`） |
| `page_methods_refresh_page_body.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-writes |
| `page_methods_update_cr_state.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-writes |
| `page_methods_get_all_slugs.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-reads |
| `page_methods_list_all_page_refs.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-reads |
| `page_methods_find_orphan_pages.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-find-orphan-pages（独立切片） |
| `page_methods_get_page_timestamps.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-reads |
| `page_methods_get_effective_dates.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-reads |
| `page_methods_get_salience_scores.rs` | trait 默认 unsupported | InMemory / PG | ❌ 留待 PG-advanced-reads(或 6c) |
| `page_methods_salience_scores_takes_zero_until_6c.rs` | 6c takes 偏差锁 | InMemory(已 override 但 takes=0) | ❌ 6c 闭合 |
| **(tag CRUD 无对应 `page_methods_*` 红测)** | — | libsql + PG 已 override | ✅ 由 `libsql_engine_tag_crud.rs` 覆盖;PG 侧暂缺独立 `postgres_engine_tag_crud.rs`(留待 PG 集成测试基础设施切片) |

### 6.3 PG-tag slice 后的处置

- **不删除任何 `page_methods_*.rs`**。
- tag 三件套不在该批红测里,无需联动调整。
- 后续每个 PG 高级方法切片(PG-find-duplicate / PG-soft-delete / PG-advanced-reads / PG-advanced-writes)完成时,对应红测可以**改写**为正向断言(返回正确语义),或**保留**为"trait 默认仍 unsupported"的负向锁(取决于届时是否还有未实现该方法的引擎)。

### 6.4 PG 集成测试基础设施

PG 引擎测试需要真实的 PostgreSQL 实例。当前采用方案 C 的混合形态:
- (C-当前) `postgres_engine_*.rs` 用 `#[ignore]` 或 feature-gated 跳过,本切片三连绿仅依赖 libsql / InMemory 测试。
- (A-未来) `#[cfg(feature = "pg-tests")]` 条件编译 + 环境变量 `DATABASE_URL`,在 CI 配置切片落地。
- (B-未来) `testcontainers` 启动临时 PG 容器,在 CI 配置切片落地。

CI 上跑真实 PG 集成测试单独留 slice。

---

## 7. 实施步骤 (建议顺序 → 实际状态)

> **重大决策**: 原方案把 6a-pg 当作单一大切片(包含 13 个高级方法)。实际推进中按"一事一议"切得更细 — Phase 1-3 + tag CRUD 已完成并 tag 落地(`slice-6a-pg`, `5ca9131`); Phase 4 的 10 个高级方法**拆为多个独立后续切片**(PG-find-duplicate / PG-soft-delete / PG-advanced-reads / PG-advanced-writes), 不在 6a-pg 主切片内闭合。

### Phase 1: Migration 同步 ✅ 已完成

1. ✅ `migrations/0002_pages_deleted_at.sql` 落地 (Slice 5x)
2. ✅ `migrations/0003_pages_full_columns.sql` 落地 (19 个新列 + 索引 + generation trigger)
3. ✅ `migrations/0004_pages_pg_align_ts.sql` 落地 (TIMESTAMPTZ 对齐)
4. ✅ `migrations/0005_page_tags.sql` 落地 (PG-tag `5ca9131`, 与 libsql `0004_page_tags.sql` parity)

### Phase 2: `row_to_page` 全列投影 ✅ 已完成

5. ✅ `postgres.rs` 全列投影(27+ 列), 与 `libsql.rs` `full_row_to_page` 对齐 — 见 `postgres_engine_full_columns.rs` 测试

### Phase 3: 基础 CRUD 升级 ✅ 已完成

6. ✅ `get_page` 支持 `include_deleted` 过滤 (postgres.rs 行 251)
7. ✅ `put_page` 全列 upsert + 参数化 `source_id` (postgres.rs 行 277)
8. ✅ `delete_page` 硬删除 (postgres.rs 行 392)
9. ✅ `list_pages` 9 项过滤 + `build_list_pages_sql` bind 顺序契约 (postgres.rs 行 406, 见 §5.4)
10. ✅ `resolve_slugs` ILIKE 模糊匹配 + deleted_at 过滤 (postgres.rs 行 532)

### Phase 3.5: Tag CRUD ✅ 已完成 (PG-tag, `5ca9131`)

11. ✅ `add_tag` / `remove_tag` / `get_tags` (postgres.rs 行 450/491/513), 与 libsql `S6-T5c` (`bb9e5bc`) parity

### Phase 4: 10 个高级方法 ❌ 未完成 — 拆为后续独立切片

> 不在 6a-pg 主切片内闭合; 每个方法/方法群拆为独立小切片落地。前置占位测试见 §6.2 表。

| 后续切片(规划名) | 范围 | 命中的 placeholder-lock 测试 |
|---|---|---|
| `PG-find-duplicate` | `find_duplicate_page` | `page_methods_find_duplicate_page.rs` |
| `PG-soft-delete` | `soft_delete_page` / `restore_page` / `purge_deleted_pages` | `page_methods_soft_delete_page.rs` / `_restore_page.rs` / `_purge_deleted_pages.rs` |
| `PG-advanced-writes` | `refresh_page_body` / `update_page_contextual_retrieval_state` | `page_methods_refresh_page_body.rs` / `_update_cr_state.rs` |
| `PG-advanced-reads` | `get_all_slugs` / `list_all_page_refs` / `get_page_timestamps` / `get_effective_dates` / `get_salience_scores` (5 个) | `page_methods_get_all_slugs.rs` / `_list_all_page_refs.rs` / `_get_page_timestamps.rs` / `_get_effective_dates.rs` / `_get_salience_scores.rs` |
| `PG-find-orphan-pages` | `find_orphan_pages` (单独切片，独立小切片落地) | `page_methods_find_orphan_pages.rs` |

每个后续切片的"完成准则": (a) PG 实现 + clippy; (b) 对应红测改写为正向断言或保留为"仍有引擎未实现"的负向锁; (c) 独立 commit + git tag。

### Phase 5: 三连绿验证 ✅ 已完成 (针对当前已实现范围)

12. ✅ `cargo build --workspace` / `cargo test --workspace` / `cargo clippy --workspace -- -D warnings` 全通过
13. ✅ PG 真实集成测试基础设施: **未落地**, 当前按方案 C — `postgres_engine_*.rs` 用 `#[ignore]` 跳过; 单独留 "PG 集成测试基础设施" 切片

### Phase 6: 收尾 ✅ 已完成 (主切片层面)

14. ✅ `git tag slice-6a-pg` 已打 (主切片闭合)
15. ✅ tag CRUD 独立 commit `5ca9131`
16. ✅ 本计划文档收口编辑 (`14-slice-6a-pg-plan.md` §3 / §5.4 / §6 / §7 / §10 全部对齐现实)
17. ❌ `engine.rs` 行 266-269 "pending slice 6a-pg" 注释删除 — **不删除**, 因 10 个 trait 默认方法仍返回 `Err(Error::unsupported("pending slice 6a"))`, 等对应 PG 后续切片落地后随该切片一并清理

---

## 8. 风险与开放问题

| # | 风险 | 缓解 |
|---|---|---|
| 1 | PG 集成测试需要真实 PG 实例 | 采用方案 C，CI 配置另开切片 |
| 2 | `PageFilters.source_ids` (Vec) 绑定到 `ANY($N::text[])` 可能需要 sqlx 特殊处理 | 验证 sqlx `Vec<String>` → PG text[] 绑定；如不行，回退到客户端循环展开 |
| 3 | `PageFilters.sort` 枚举映射到 ORDER BY 子句 — 动态 SQL 注入风险 | 用 `match` 白名单映射，不拼接用户输入 |
| 4 | `frontmatter->'tags' ? $tag` 需要 JSONB 类型 — 如果 migration 未正确设置 default `'{}'::jsonb` | migration 0002 已设 `DEFAULT '{}'`，sqlx 应自动映射为 JSONB |
| 5 | `embedding` 列 BYTEA ↔ `Option<Vec<u8>>` — sqlx 类型映射需确认 | 测试编译验证 |
| 6 | `Page.embedding` 字段当前是 `Option<Vec<u8>>` — 如是 `Option<Vec<f32>>` 则需序列化/反序列化 | 检查 `engine.rs` Page struct 定义 |

---

## 9. D1/D2 不接受偏差追踪 (跨切片)

| 偏差 | 当前状态 | 6a-pg 影响 | 闭合切片 |
|---|---|---|---|
| D1: `list_pages` 过滤器签名 `Option<&PageFilters>` vs `&PageFilters` | 未闭合 | 6a-pg 使用 `&PageFilters` (当前 trait 签名) | S6-signature |
| D2: 时间字段是否引入 `chrono` | 未闭合 | 6a-pg 保持 `String` (ISO-8601) | S6-time-types |

6a-pg **不关闭** D1/D2，保持当前 trait 签名不变。

---

## 10. 验证 Checklist

### 10.1 6a-pg 主切片范围 ✅ 全部完成

- [x] `migrations/0002_pages_deleted_at.sql` 落地
- [x] `migrations/0003_pages_full_columns.sql` 创建且 PG 方言正确
- [x] `migrations/0004_pages_pg_align_ts.sql` 落地 (TIMESTAMPTZ 对齐)
- [x] `migrations/0005_page_tags.sql` 落地 (PG-tag, 与 libsql `0004` parity)
- [x] `row_to_page` / `full_row_to_page` 解码 27+ 列, 无硬编码 default
- [x] `get_page` 支持 `include_deleted` 过滤
- [x] `put_page` 全列 upsert (INSERT 20+ 列, ON CONFLICT UPDATE 20+ 列), 按 S6-T8 契约绑定 `source_id.unwrap_or("default")`
- [x] `delete_page` 保持硬删除
- [x] `list_pages` 9 项过滤 + ORDER BY + OFFSET + LIMIT (bind 顺序契约见 §5.4)
- [x] `resolve_slugs` ILIKE 模糊匹配 + deleted_at 过滤
- [x] `add_tag` / `remove_tag` / `get_tags` 实现 (PG-tag `5ca9131`, 与 libsql `S6-T5c` `bb9e5bc` parity)
- [x] `cargo build --workspace` ✅
- [x] `cargo test --workspace` ✅ (按方案 C, PG 真实集成测试 `#[ignore]`)
- [x] `cargo clippy --workspace -- -D warnings` ✅
- [x] `git tag slice-6a-pg`
- [x] 本计划文档收口 (§3 / §5.4 / §6 / §7 / §10 对齐现实, 不静默接受偏差)

### 10.2 拆给后续独立切片(本主切片不在范围) ❌

> 每一项落入对应 PG 后续切片时, 在该切片的 plan 中重新建 checklist; 此处仅作"未完成项的导航"。

- [x] `find_duplicate_page` PG 方言 — 切片: **PG-find-duplicate**（`39a4f68`）
- [x] `soft_delete_page` PG 方言 (now()) — 切片: **PG-soft-delete**（`2568268`）
- [x] `restore_page` 实现 — 切片: **PG-soft-delete**（`2568268`）
- [x] `purge_deleted_pages` 实现 — 切片: **PG-soft-delete**（`2568268`）
- [ ] `refresh_page_body` 实现 — 切片: **PG-advanced-writes**
- [ ] `update_page_contextual_retrieval_state` 实现 — 切片: **PG-advanced-writes**
- [x] `get_all_slugs` 实现 — 切片: **PG-advanced-reads**（`16a563f`）
- [x] `list_all_page_refs` 实现 — 切片: **PG-advanced-reads**（`16a563f`）
- [ ] `find_orphan_pages` 实现 — 切片: **PG-find-orphan-pages**（已从 PG-advanced-reads 摘出，独立小切片）
- [x] `get_page_timestamps` 实现 — 切片: **PG-advanced-reads**（`16a563f`）
- [x] `get_effective_dates` 实现 — 切片: **PG-advanced-reads**（`16a563f`）
- [x] `get_salience_scores` 实现 — 切片: **PG-advanced-reads**（`16a563f`；6a 阶段退化为 `emotional_weight * 5`，6c 再补 `+ ln(1 + N_tags)`）
- [ ] `engine.rs` "pending slice 6a" 注释 — 等最后一个 PG 后续切片完成后清理
- [ ] 13 个 `page_methods_*.rs` placeholder-lock 红测 — 跟随对应 PG 后续切片改写/保留(见 §6.2)
- [ ] PG 真实集成测试基础设施 (`postgres_engine_*.rs` 去 `#[ignore]`) — 独立切片 **PG-integration-test-infra**

### 10.3 跨切片偏差追踪 (不在 6a-pg 内闭合)

- [ ] D1: `list_pages` 签名 `&PageFilters` vs `Option<&PageFilters>` — 切片 **S6-signature**
- [ ] D2: 时间字段引入 `chrono` — 切片 **S6-time-types**

---

## §11 PG-advanced-reads 切片落地修订（来自 critical review）

> 背景: 本计划 §5.12–§5.17 起草时, 5 个只读方法的 trait 签名、返回类型、SQL 与 `engine.rs` L350-465 真实 trait 默认实现严重不符 (R1–R5)。
> 落地切片 **PG-advanced-reads** 时必须以 `engine.rs` 实际签名为权威源, 并按 TS `pglite-engine.ts` 行为反向适配 PG 方言。
> §5.12–§5.17 已加 ⚠️ 警示 + 重写; 本节作为偏差登记 + 切片落地"真"契约的单一信源。
> `find_orphan_pages` 已从本切片摘出, 归 **PG-find-orphan-pages** 独立小切片处理 (§626 / §10.2 / §6.2 已同步)。

### 11.1 5 个只读方法真实契约 (engine.rs 权威)

| # | 方法 | trait 签名 (engine.rs L350-465) | TS 锚点 (pglite-engine.ts) | PG 方言要点 |
|---|------|----------------------------------|----------------------------|---------------|
| 1 | `get_all_slugs` | `(&self, source_id: Option<&str>) -> Result<HashSet<String>>` | L1071-1086, **不过滤** `deleted_at` | `SELECT slug FROM pages [WHERE source_id = $1]`; `$1::text IS NULL OR source_id = $1` 守卫; 收 `HashSet<String>` |
| 2 | `list_all_page_refs` | `(&self) -> Result<Vec<PageRef>>` | L1088-1098, 过滤 `deleted_at IS NULL`, `ORDER BY source_id, slug` | `SELECT slug, source_id FROM pages WHERE deleted_at IS NULL ORDER BY source_id, slug`; `PageRef { slug, source_id }` 仅 2 字段 |
| 3 | `get_page_timestamps` | `(&self, slugs: &[String]) -> Result<HashMap<String, String>>` | L2567-2575, `COALESCE(updated_at, created_at)` ISO-8601 | `SELECT slug, COALESCE(updated_at, created_at)::text AS ts FROM pages WHERE slug = ANY($1::text[]) AND deleted_at IS NULL`; key=slug |
| 4 | `get_effective_dates` | `(&self, refs: &[PageRef]) -> Result<HashMap<String, String>>` | L2577-2594, `unnest(...)` 二维 join, key=`"{source_id}::{slug}"` | `SELECT p.slug, p.source_id, COALESCE(p.updated_at, p.created_at)::text AS ts FROM pages p JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id) ON p.slug = u.slug AND p.source_id = u.source_id WHERE p.deleted_at IS NULL`; key=`format!("{source_id}::{slug}")` |
| 5 | `get_salience_scores` | `(&self, refs: &[PageRef]) -> Result<HashMap<String, f64>>` | L2596-2617, **6a 退化** `emotional_weight * 5` (6c 补 `+ ln(1 + COUNT DISTINCT tag)`) | `SELECT p.slug, p.source_id, COALESCE(p.emotional_weight, 0.0) * 5.0 AS score FROM pages p JOIN unnest($1::text[], $2::text[]) AS u(slug, source_id) ON p.slug = u.slug AND p.source_id = u.source_id WHERE p.deleted_at IS NULL`; key=`format!("{source_id}::{slug}")` |

### 11.2 偏差登记 (R1–R5)

| ID | 位置 | 原始计划错处 | 修订 | 状态 |
|----|------|--------------|------|------|
| R1 | §5.12 `get_all_slugs` | 起草时假设过滤 `deleted_at IS NULL`, 与 TS L1071-1086 实际行为不符 | 改为不过滤 `deleted_at`, 仅按 `source_id` 守卫 | ✅ §5.12 已修订 |
| R2 | §5.13 `list_all_page_refs` | 起草时假设 `PageRef` 含更多字段, 与 struct 实际 `{slug, source_id}` 不符 | 收紧为 2 字段, `ORDER BY source_id, slug` | ✅ §5.13 已修订 |
| R3 | §5.14 `find_orphan_pages` | 错归入 PG-advanced-reads, 实为 source-graph 跨表查询, 复杂度独立 | 摘出为 **PG-find-orphan-pages** 独立切片 (§626 / §10.2 / §6.2 同步) | ✅ 已摘出 |
| R4 | §5.15 `get_page_timestamps` | 起草时按 `refs` 取参, 与 trait 实际 `slugs: &[String]` 不符 | 改为按 slug 数组 `WHERE slug = ANY($1::text[])`, key=slug | ✅ §5.15 已修订 |
| R5 | §5.16 / §5.17 `get_effective_dates` / `get_salience_scores` | 起草时按单参数 / 单 array 取参, 与 `&[PageRef]` 二维结构不符 | 改为 `unnest($1::text[], $2::text[]) AS u(slug, source_id)` 二维 join, key=`"{source_id}::{slug}"`; §5.17 明示 6a 退化 + 6c 补全 | ✅ §5.16 / §5.17 已修订 |

### 11.3 切片落地工序 (S1–S6)

> 严格 RED → GREEN → REFACTOR, 不跨步, 不批量。

- **S1 文档收口**: §5.12-§5.17 + §11 已落地; commit 前确认 `git diff` 仅含 plan 14 改动。
- **S2 RED**: **扩展现有 5 个 `page_methods_*.rs`** (`get_all_slugs` / `list_all_page_refs` / `get_page_timestamps` / `get_effective_dates` / `get_salience_scores`), 在同文件追加 `#[serial_test::serial] #[tokio::test]` PG 镜像测试 (参照 `page_methods_soft_delete_page.rs` `2568268` 形态, **不**新建 `postgres_engine_advanced_reads.rs`); libsql 默认 `Unsupported` 断言保持不动; PG 侧覆盖真实场景 (含 soft-deleted 行可见性区分); 跑 `ZBRAIN_TEST_PG_URL=... cargo test -p zbrain-core --test page_methods_get_all_slugs --test page_methods_list_all_page_refs --test page_methods_get_page_timestamps --test page_methods_get_effective_dates --test page_methods_get_salience_scores` 必红 (PG override 未实现)。无 `ZBRAIN_TEST_PG_URL` 时 PG 段自动跳过, libsql 段仍绿。
- **S3 GREEN**: 在 `crates/zbrain-core/src/postgres.rs` 末尾追加 5 个 `impl BrainEngine for PostgresEngine` override (按 §11.1 SQL); libsql 不动, 维持 trait 默认 `Unsupported`。
- **S4 重写锁测**: `tests/page_methods_salience_scores_takes_zero_until_6c.rs` 改为 S6-T2 形态:
  - libsql 分支: 仍调用 trait 默认 → 断言 `Err(Unsupported)`;
  - PG 分支 (在 `ZBRAIN_TEST_PG_URL` 下): 插入 `emotional_weight=0.4` 行 → 断言 `(0.4 * 5.0 - score).abs() < 1e-9`;
  - doc-comment 同步: 6c 切片改为 `0.4*5 + ln(1+N_tags)`。
- **S5 四连绿门禁**: `cargo fmt --all -- --check` / `cargo build --workspace --all-targets` / `cargo test --workspace --all-targets` / `cargo clippy --workspace --all-targets -- -D warnings`; libsql 并行 SIGABRT flake 命中则按 plan 16 §8 line 231 重跑单 crate, 不掩盖。
- **S6 提交 + 文档**: 一次性 commit (实现 + 测试 + plan 收口); commit message 模板 `slice 6a-pg(advanced_reads): override PG for 5 read methods, mirror TS pglite-engine`; 在 §10.2 把 5 行勾选为 ✅ 并补 commit hash。

### 11.4 与 libsql 的契约边界

- 本切片 **不** 实现 libsql 的 5 个 advanced-reads, 维持 `engine.rs` trait 默认 `Err(EngineError::Unsupported(...))`;
- 现有 5 个 `page_methods_*.rs` placeholder-lock 红测除 `salience_scores_takes_zero_until_6c.rs` 外保持不动, 锁住 trait 默认;
- 决策 D1 锁定: libsql advanced-reads 等 6c+ 切片再处理, 不下放到本切片。
