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
3. **`put_page` 全列 upsert** — 从 4 列 INSERT 升级到完整列
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

## 3. PG Migration 同步

### 3.1 现状

- `migrations/0001_init.sql` — 仅 7 列 pages 表 (Slice 4a)
- `migrations-sqlite/0002_pages_full_columns.sql` — 19 个新列 + 4 索引 + 2 trigger (Slice 6a)
- `migrations-sqlite/0003_salience_and_full_generation_trigger.sql` — salience_score 列 + 重建 trigger (Slice 6a S4)

### 3.2 新增文件

| 文件 | 内容 | 方言适配 |
|---|---|---|
| `migrations/0002_pages_full_columns.sql` | 19 个 ALTER TABLE ADD COLUMN + 索引 + trigger | TEXT→JSONB, TEXT→TIMESTAMPTZ, INTEGER→BIGINT, BLOB→BYTEA, REAL→DOUBLE PRECISION; trigger 用 PG BEFORE INSERT/UPDATE 语法 |
| `migrations/0003_salience_and_full_generation_trigger.sql` | salience_score + 重建 generation trigger | REAL→DOUBLE PRECISION; trigger 10 列 allow-list |

### 3.3 0002 PG 方言要点

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

### 3.4 0003 PG 方言要点

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

#### 5.2 `put_page(slug, input) → Page`

**现状**: 4 列 INSERT (slug, type, title, compiled_truth)，ON CONFLICT UPDATE 3 列。

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
    $21, $22, 'default'
)
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

#### 5.4 `list_pages(filters) → Vec<Page>`

**现状**: 仅 `type + limit` 过滤，7 列投影。

**PG SQL — 9 项过滤**:

```sql
SELECT {全列}
FROM pages
WHERE TRUE
  AND ($1 IS NULL OR type = $1)                        -- page_type
  AND ($2 IS NULL OR source_id = ANY($2::text[]))      -- source_ids
  AND ($3 IS NULL OR slug LIKE $3 || '%')              -- slug_prefix → LIKE
  AND ($4 IS NULL OR source_id = $4)                   -- source_id
  AND ($5::boolean IS NULL OR deleted_at IS NULL)      -- include_deleted
  AND ($6 IS NULL OR updated_at > $6)                  -- updated_after
  AND ($7 IS NULL OR frontmatter->>'tags' ? $7)       -- tag (JSONB ? operator)
  AND ($8 IS NULL OR title ILIKE '%' || $8 || '%')     -- tag fallback → title search (待定)
ORDER BY
  CASE WHEN $9 = 'updated_at ASC' THEN updated_at END ASC,
  CASE WHEN $9 = 'updated_at DESC' THEN updated_at END DESC,
  CASE WHEN $9 = 'title ASC' THEN title END ASC,
  CASE WHEN $9 = 'title DESC' THEN title END DESC,
  id ASC  -- default
LIMIT $10  -- COALESCE(filters.limit, ALL)
OFFSET $11 -- filters.offset
```

**注意**:
- `PageFilters.tag` 是 `Option<String>` — PG 中用 JSONB `?` 操作符检查 `frontmatter->>'tags'` 是否包含该 tag。如果 tag 搜索需要 FTS/GIN 索引，则锁定为 `unsupported` 等待 slice 6e。
- `slug_prefix` 用 `LIKE $3 || '%'` (PG 字符串连接) 而非 `LIKE '%...'`。
- `source_ids` (Vec) 用 `ANY($2::text[])` — 需 sqlx `Vec<String>` 绑定支持。
- `sort` 字段映射到 `ORDER BY` — 简单实现用 `match` 构建 ORDER BY 子句。

**tag filter 锁定决策 (C7 关联)**:
- 如果 `frontmatter` 列为 JSONB 且有 GIN 索引 → 可用 `frontmatter->'tags' ? $tag`
- 如果无 GIN 索引 → 仍可运行但性能差 → 功能正确但标记为 "needs GIN index (slice 6e)"
- **推荐**: 实现 `frontmatter->'tags' ? $tag` 语义，不加 GIN 索引，注释标明 "needs GIN index for production use (slice 6e)"

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

#### 5.8 `restore_page(slug) → Option<Page>`

**PG SQL**:
```sql
UPDATE pages
SET deleted_at = NULL
WHERE slug = $1
  AND deleted_at IS NOT NULL
RETURNING {全列}
```

- 注意: 6a 主切片 InMemory 实现中 `restore_page` 不需要 SQL，PG 需要。

#### 5.9 `purge_page(slug) → Option<String>`

**PG SQL**:
```sql
DELETE FROM pages
WHERE slug = $1
  AND deleted_at IS NOT NULL
RETURNING slug
```

- 只删除已 soft-delete 的行

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

#### 5.12 `get_all_slugs() → Vec<String>`

**PG SQL**:
```sql
SELECT slug FROM pages
WHERE deleted_at IS NULL
ORDER BY slug ASC
```

#### 5.13 `list_all_page_refs() → Vec<PageRef>`

**PG SQL**:
```sql
SELECT id, slug, type, title, source_id
FROM pages
WHERE deleted_at IS NULL
ORDER BY slug ASC
```

- `PageRef` = 轻量引用结构 (id, slug, type, title, source_id)

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

#### 5.15 `get_page_timestamps(source_id) → Vec<(String, String, String)>`

**PG SQL**:
```sql
SELECT slug, created_at::text, updated_at::text
FROM pages
WHERE source_id = $1
  AND deleted_at IS NULL
ORDER BY slug ASC
```

- `::text` 显式转换 TIMESTAMPTZ → String
- 返回类型: `Vec<(slug, created_at, updated_at)>`

#### 5.16 `get_effective_dates(source_id) → Vec<(String, Option<String>)>`

**PG SQL**:
```sql
SELECT slug, effective_date::text
FROM pages
WHERE source_id = $1
  AND deleted_at IS NULL
ORDER BY slug ASC
```

#### 5.17 `get_salience_scores() → Vec<(String, Option<f64>)>`

**PG SQL**:
```sql
SELECT slug, salience_score
FROM pages
WHERE deleted_at IS NULL
ORDER BY slug ASC
```

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

Slice 6a 已创建 13 个 placeholder-lock 测试文件，验证 `PostgresEngine` 各方法返回 `Err(Error::unsupported("pending slice 6a"))`。

6a-pg 完成后，这些测试**必须全部删除或改写**：

| 文件 | 当前断言 | 6a-pg 后 |
|---|---|---|
| `postgres_get_page_test.rs` | assert unsupported | assert 返回正确 Page 或 None |
| `postgres_put_page_test.rs` | assert unsupported | assert 返回正确 upsert Page |
| `postgres_delete_page_test.rs` | assert unsupported | assert 返回 () |
| `postgres_list_pages_test.rs` | assert unsupported | assert 返回正确 Vec<Page> |
| `postgres_resolve_slugs_test.rs` | assert unsupported | assert 返回正确 Vec<String> |
| `postgres_find_duplicate_page_test.rs` | assert unsupported | assert 返回正确 Option<Page> |
| `postgres_soft_delete_page_test.rs` | assert unsupported | assert 返回正确 Option<String> |
| `postgres_restore_page_test.rs` | assert unsupported | assert 返回正确 Option<Page> |
| `postgres_purge_page_test.rs` | assert unsupported | assert 返回正确 Option<String> |
| `postgres_refresh_page_body_test.rs` | assert unsupported | assert 返回正确 Option<Page> |
| `postgres_update_contextual_retrieval_test.rs` | assert unsupported | assert 返回正确 Option<Page> |
| `postgres_get_all_slugs_test.rs` | assert unsupported | assert 返回正确 Vec<String> |
| `postgres_list_all_page_refs_test.rs` | assert unsupported | assert 返回正确 Vec<PageRef> |

**注意**: PG 引擎测试需要真实的 PostgreSQL 实例。如果 CI 无 PG 实例，可采用以下策略之一：
- (A) `#[cfg(feature = "pg-tests")]` 条件编译 + 环境变量 `DATABASE_URL`
- (B) 使用 `testcontainers` 启动临时 PG 容器
- (C) 先只确保编译通过 + libsql 测试全绿，PG 集成测试留给后续 CI 配置切片

**推荐方案 C** — 6a-pg 只确保 `cargo build` + `cargo clippy` 通过 + libsql/InMemory 测试全绿。PG 集成测试单独切片处理。

---

## 7. 实施步骤 (建议顺序)

### Phase 1: Migration 同步 (预估 1 个 commit)

1. 创建 `migrations/0002_pages_full_columns.sql` — 翻译 SQLite 0002 为 PG 方言
2. 创建 `migrations/0003_salience_and_full_generation_trigger.sql` — 翻译 SQLite 0003 为 PG 方言
3. 本地验证: `sqlx migrate run` (需 PG 实例) 或仅代码审查

### Phase 2: `row_to_page` 全列投影 (预估 1 个 commit)

4. 扩展 `postgres.rs` `row_to_page` 为 `full_row_to_page`，解码全部 27+ 列
5. 确认 `Page` struct 的 `embedding` 字段类型兼容 `Vec<u8>` (BYTEA)
6. 更新所有现有方法 (`get_page`, `put_page`, `list_pages`, `resolve_slugs`) 使用全列投影
7. `cargo build` + `cargo clippy` 验证

### Phase 3: 基础 CRUD 升级 (预估 1-2 个 commit)

8. `get_page` — 支持 `include_deleted` 过滤
9. `put_page` — 全列 upsert
10. `list_pages` — 9 项过滤 + ORDER BY + OFFSET
11. `resolve_slugs` — ILIKE 模糊匹配
12. `cargo build` + `cargo clippy` 验证

### Phase 4: 高级方法实现 (预估 2-3 个 commit)

13. `find_duplicate_page` — PG 方言翻译
14. `soft_delete_page` / `restore_page` / `purge_page` — soft-delete 三件套
15. `refresh_page_body` — 内容更新
16. `update_page_contextual_retrieval_state` — 检索模式更新
17. `get_all_slugs` / `list_all_page_refs` — 轻量列表
18. `update_slug` — slug 变更
19. `get_page_timestamps` / `get_effective_dates` / `get_salience_scores` / `touch_salience` — 读取方法
20. `cargo build` + `cargo clippy` 验证

### Phase 5: 测试更新 + 三连绿 (预估 1 个 commit)

21. 删除或改写 13 个 placeholder-lock 测试
22. 如采用方案 C，仅保留 `cargo build` + `cargo test --workspace` (非 PG 测试) + `cargo clippy`
23. 全量三连绿验证

### Phase 6: 收尾

24. 更新 `engine.rs` 行 266-269 注释，删除 "pending slice 6a-pg" 占位说明
25. `git add -A && git commit -m "feat(slice-6a-pg): PostgresEngine mirror — 13 methods + full projection + PG migrations"`
26. `git tag slice-6a-pg`

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

- [ ] `migrations/0002_pages_full_columns.sql` 创建且 PG 方言正确
- [ ] `migrations/0003_salience_and_full_generation_trigger.sql` 创建且 PG 方言正确
- [ ] `row_to_page` / `full_row_to_page` 解码 27+ 列，无硬编码 default
- [ ] `get_page` 支持 `include_deleted` 过滤
- [ ] `put_page` 全列 upsert (INSERT 20+ 列, ON CONFLICT UPDATE 20+ 列)
- [ ] `delete_page` 保持不变 (硬删除)
- [ ] `list_pages` 9 项过滤 + ORDER BY + OFFSET + LIMIT
- [ ] `resolve_slugs` ILIKE 模糊匹配 + deleted_at 过滤
- [ ] `find_duplicate_page` PG 方言 (frontmatter->>'id')
- [ ] `soft_delete_page` PG 方言 (now())
- [ ] `restore_page` 实现
- [ ] `purge_page` 实现
- [ ] `refresh_page_body` 实现
- [ ] `update_page_contextual_retrieval_state` 实现
- [ ] `get_all_slugs` 实现
- [ ] `list_all_page_refs` 实现
- [ ] `update_slug` 实现
- [ ] `get_page_timestamps` 实现
- [ ] `get_effective_dates` 实现
- [ ] `get_salience_scores` 实现
- [ ] `touch_salience` 实现
- [ ] `engine.rs` 注释更新 — 删除 "pending slice 6a-pg"
- [ ] 13 个 placeholder-lock 测试删除或改写
- [ ] `cargo build --workspace` ✅
- [ ] `cargo test --workspace` ✅
- [ ] `cargo clippy --workspace -- -D warnings` ✅
- [ ] `git tag slice-6a-pg`
