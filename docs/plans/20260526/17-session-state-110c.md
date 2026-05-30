# 会话状态快照 — slice #110-c 收口（2026-05-30）

> 由助手 zjj 写入，作为跨会话可见的项目状态文档。
> 配套：参考 `16-slice-index-and-conventions.md`（跨切片约定）、`15-session-state-110a.md`（#110-a 上文）。

## 当前 git 状态

- **分支**：`rust-rewrite`（worktree：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust`）
- **HEAD**：`7efd83a` — `slice #110-c: align PG put_page with TS source-of-truth (28 cols, server-stamp ingested_at)`
- **工作树**：clean（除 `.codegraph/` 目录，与本切片无关，下方"后续待办"已列入清单）
- **前置 commit 链**：
  - `7efd83a` slice #110-c: align PG put_page with TS source-of-truth ← **本切片**
  - `fb23d0d` slice #110-b: PG put_page/row_to_page widen to full 30-column Page
  - `7c55a90` docs: cross-slice index + project conventions snapshot
  - `b2314b6` slice #110-a: PG pages full-column migration + bump_generation trigger (DDL only)

## #110-c 完成范围

**单一职责**：把 #110-b 落地的"30 列 PG put_page"对齐到 TS source-of-truth（`/Users/bilibili/Documents/workspace/jununfly/zbrain/src/schema.sql` + `postgres-engine.ts` + `pglite-engine.ts`），消除契约偏差。

### 偏差识别（来自上一会话契约比对）

| # | TS canonical | #110-b 现状（PG Rust） | 决议 |
|---|---|---|---|
| 1 | `putPage` **不**写 `embedding` / `last_retrieved_at`（embed 在 vector index 独立写；last_retrieved_at 由 retrieval tracker 写） | INSERT 显式写两列（embedding=null, last_retrieved_at=null） | **删两列** |
| 2 | `putPage` server-stamps `ingested_at = (sourceKind \|\| sourceUri \|\| ingestedVia) ? new Date() : null` | 直接透传 input.ingested_at | **改 server-stamp** |
| 3 | `corpus_generation TEXT`（schema.sql:131） | `INTEGER` | **改 TEXT** |
| 4 | `frontmatter JSONB NOT NULL DEFAULT '{}'`（schema.sql:93） | `JSONB`（nullable） | **改 NOT NULL + DEFAULT** |

### 交付物

| 类型 | 路径 | 说明 |
|---|---|---|
| Migration | `crates/zbrain-core/migrations/0004_pages_pg_align_ts.sql` | `corpus_generation TYPE TEXT USING ::text` + `frontmatter SET DEFAULT '{}'::jsonb` + `SET NOT NULL`（先 UPDATE 兜底 NULL） |
| Code | `crates/zbrain-core/src/postgres.rs` | FULL_PAGE_PROJECTION 30→28 列；put_page INSERT 20→19 列；row_to_page 同步去除 embedding/last_retrieved_at 解码、corpus_generation 直接 String、frontmatter 非 Option 直接解码；新增 server-stamp ingested_at 逻辑；模块 doc-comment 重写 |
| RED→GREEN tests | `crates/zbrain-core/tests/postgres_engine_full_columns.rs` | 1 个改写（roundtrip 不再覆盖 embedding/last_retrieved_at）+ 4 新增（见下）+ 2 carried trigger 测试 |

### 新增 4 个契约测试（postgres_engine_full_columns.rs）

1. `ingested_at_server_stamped_when_any_ingestion_metadata_present` — 不传 ingested_at 但传 source_kind/source_uri/ingested_via 之一 → DB 应回写 now()
2. `ingested_at_remains_none_without_ingestion_metadata` — 三个 ingestion 字段都为 None → ingested_at 保持 NULL
3. `frontmatter_defaults_to_empty_object_when_omitted` — input.frontmatter=None → DB 应返回 `{}`
4. `corpus_generation_column_is_text` — 通过 `pg_typeof(corpus_generation)` 直查列类型 = `text`

### 本地 vs CI 行为

- 与 #110-a 同模式：本地 `ZBRAIN_TEST_PG_URL` 未设置 → 测试 skip（return None 早退视为 pass）；CI 设置 env 后真实执行。
- 本地三连绿（本次执行）：
  - `cargo build` → 0
  - `cargo test --workspace` → 全绿（PG 测试 skip 路径）
  - `cargo clippy --workspace --all-targets -- -D warnings` → 0

## 必须保持的契约（破坏即破坏 source-of-truth）

### PG put_page 19 列 INSERT 列表（固定顺序）

```
slug, type, page_kind, title, compiled_truth, timeline, frontmatter,
content_hash, emotional_weight, effective_date, effective_date_source,
import_filename, salience_touched_at, salience_score, source_path,
source_id, source_kind, source_uri, ingested_via, ingested_at,
contextual_retrieval_mode, corpus_generation
```

> 实际 INSERT 19 列 + `ingested_at` 用 server-stamp 分支注入，所以源码里 binds 数量保持稳定。

### 不持久化的列（PG/TS 两端一致）

- `embedding` — 由 vector index 模块独立写
- `last_retrieved_at` — 由 retrieval tracker 模块独立写
- `id, created_at, updated_at, deleted_at, salience_touched_at`（部分由 schema default / trigger 管理）

### server-stamp ingested_at 规则（TS source-of-truth，必须与 putPage 一致）

```ts
const ingestedAt =
  (sourceKind || sourceUri || ingestedVia) ? new Date() : null;
```

Rust 端等价：

```rust
let ingested_at_ts = match input.ingested_at.as_deref() {
    Some(ts) => parse_rfc3339_opt(Some(ts), "ingested_at")?,
    None => {
        if input.source_kind.is_some()
            || input.source_uri.is_some()
            || input.ingested_via.is_some()
        {
            Some(sqlx::types::chrono::Utc::now())
        } else {
            None
        }
    }
};
```

### corpus_generation 类型

`TEXT` — 不是 INTEGER。Rust 解码用 `Option<String>` 直接 sqlx::Row::try_get。

### frontmatter 默认值

`JSONB NOT NULL DEFAULT '{}'::jsonb` — Rust 解码用非 Option `sqlx::types::Json<serde_json::Value>` 直接 try_get；input.frontmatter=None 不再 bind NULL，DB default 兜底。

## 后续切片清单（用户嘱「待处理细节新建切片放后面避免遗漏」）

按优先级：

1. **#110-d** — libsql 端同步对齐
   - 删除 put_page 中 embedding / last_retrieved_at 列与 bind
   - 加 server-stamp ingested_at 分支
   - 评估 libsql 是否需要等价的 corpus_generation TEXT / frontmatter NOT NULL 处理（libsql 当前 schema 状态待查）
   - 复用 #110-c 同款 4 个契约测试模板（libsql 版本）
   - 三连绿 + 单 commit

2. **#110-e（候选）** — `chunker_version` TEXT 历史债评估
   - 当前 PG/libsql/TS 三方该列类型差异未审计
   - 应单独开切片对齐（不混入 page CRUD 切片）

3. **#110-f（候选）** — embedder / retrieval-tracker 端到端契约
   - 由于 #110-c 已确认 put_page **不**写 embedding/last_retrieved_at，需要为 vector index 和 retrieval tracker 各自的 trait 接口写契约测试
   - 目标：明确"谁写"的边界，避免后续切片误把这两列塞回 put_page

4. **杂物切片（非阻塞）** — `.codegraph/` 目录加入 `.gitignore`
   - 当前 codegraph.db 已 ignore 但目录本身 untracked
   - 一行改动，与 #110 系列无关，可任意时机收口

## 三连绿命令（复制即用）

```bash
cd /Users/bilibili/Documents/workspace/jununfly/zbrain-rust
cargo build
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## 关键 TS source-of-truth 文件锚点

- 标准 schema：`/Users/bilibili/Documents/workspace/jununfly/zbrain/src/schema.sql`
- PG putPage 实现：`/Users/bilibili/Documents/workspace/jununfly/zbrain/src/postgres-engine.ts`
- PGLite putPage 实现：`/Users/bilibili/Documents/workspace/jununfly/zbrain/src/pglite-engine.ts`

下一会话开局可直接读本文件 + `16-slice-index-and-conventions.md` 取得完整跨切片视野。
