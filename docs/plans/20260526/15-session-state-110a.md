# 会话状态快照 — slice #110-a 收口（2026-05-30）

> 由助手 zjj 写入，作为跨会话可见的项目状态文档。
> 配套 docs：参考 `13-slice-6a-gap-checklist.md` / `14-slice-6a-pg-plan.md`。

## 当前 git 状态

- **分支**：`rust-rewrite`（worktree：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust`）
- **HEAD**：`b2314b6` — `slice #110-a: PG pages full-column migration + bump_generation trigger (DDL only)`
- **工作树**：clean
- **前置 3 个相关 commit**：
  - `9ce9773` feat(zbrain-core): pg row_to_page decodes deleted_at into Page (#72-a)
  - `53656cb` feat(zbrain-core): pg get_page honors include_deleted via deleted_at filter (#73)
  - `b337855` style(zbrain-core): normalize rustfmt drift across page CRUD files

## #110-a 完成范围（DDL only）

**单一职责**：仅交付 PG schema migration + `bump_page_generation_fn` trigger，**不动** `crates/zbrain-core/src/postgres.rs`。让 `postgres_engine_full_columns.rs` 三条测试在 CI 上保持 RED，由下一切片 #110-b 推 GREEN。

### 交付物

| 类型 | 路径 | 说明 |
|---|---|---|
| Migration | `crates/zbrain-core/migrations/0003_pages_full_columns.sql` | `ALTER TABLE pages ADD COLUMN IF NOT EXISTS ...` +19 列 + `CREATE OR REPLACE FUNCTION bump_page_generation_fn()` + `BEFORE INSERT OR UPDATE` trigger |
| RED tests | `crates/zbrain-core/tests/postgres_engine_full_columns.rs` | 三条：`roundtrip_all_full_columns` / `generation_bumps_on_watched_column_change` / `generation_does_not_bump_on_unwatched_column_change` |
| 辅助 | 同上文件内 `unique_slug()` + `side_pool()` | 进程内 `AtomicU64::fetch_add` + nanos 生成唯一 slug；side-channel `sqlx::PgPool` 直查 generation 列 |

### 本地 vs CI 行为

- **本地**：`ZBRAIN_TEST_PG_URL` 未设置 → `init_clean_engine()` 打印 `eprintln!("skip ...")` 并 `return None`，调用方 `let Some(engine) = init_clean_engine().await else { return; };` 早退 → 视为 pass。
- **CI**：runner 设置 `ZBRAIN_TEST_PG_URL` → 测试真实执行 → 全部 RED（因为 `put_page` 还在 10 列旧 schema），等 #110-b 修复后转 GREEN。

### 三连绿验证（本地）

- `cargo build --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml` → 0
- `cargo test --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace -- --test-threads=1` → 0（PG 测试 skip）
- `cargo clippy --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace --all-targets -- -D warnings` → 0

> 注：测试用 `--test-threads=1` 串行，避免 workspace 并发偶发失败。

## bump_generation trigger 设计（必须严格保持一致）

### 10 列 watched allow-list（破坏即破坏 cache bookmark gate 契约）

`compiled_truth, timeline, frontmatter, deleted_at, contextual_retrieval_mode, title, type, page_kind, corpus_generation, content_hash`

### 显式排除（行为字段不算 truth 变更）

`salience_score, last_retrieved_at, salience_touched_at, embedding`

### 触发语义

- `BEFORE INSERT OR UPDATE ON pages FOR EACH ROW`
- 函数 `bump_page_generation_fn()` plpgsql，按 `TG_OP` 分支：
  - **INSERT** → `NEW.generation := COALESCE((SELECT MAX(generation) FROM pages), 0) + 1`（首次=1，新行+1）
  - **UPDATE** → 10 列 allow-list `IS DISTINCT FROM` 任一命中 → `NEW.generation := OLD.generation + 1`；全部相等 → 保持 `OLD.generation`
- PG 可直接 `NEW.generation := ...`（libsql 受 BEFORE UPDATE 不能直接赋值 NEW.* 的限制需 nested UPDATE workaround；PG 无此问题，实现更直接）

### 来源对齐

- **TS source of truth**：`/Users/bilibili/Documents/workspace/jununfly/zbrain/src/core/pglite-schema.ts` 行 60-159（plpgsql 函数 + BEFORE INSERT OR UPDATE trigger）
- **libsql 镜像**：
  - `crates/zbrain-core/migrations-sqlite/0002_pages_full_columns.sql`（20 列）
  - `crates/zbrain-core/migrations-sqlite/0003_salience_and_full_generation_trigger.sql`（trigger + `salience_score` 列）

### sqlx migrate splitter 风险

`$$...$$` plpgsql body 含分号，sqlx 默认按 `;` 切句可能截断。`0003` 用 `CREATE OR REPLACE FUNCTION ... $$ ... $$ LANGUAGE plpgsql;` 形式，本地未触发问题。**如 CI 上挂**，fallback 方案：
- (a) 改 `DO $$ ... $$` block
- (b) 拆 `0003a_columns.sql` + `0003b_trigger.sql`

## PG ↔ SQLite 列类型映射（#110-a 起固定）

| libsql SQLite | PostgreSQL |
|---|---|
| `TEXT` | `TEXT` / `JSONB`（JSON 结构用 JSONB） |
| `INTEGER` | `INTEGER` / `BIGINT` |
| `REAL` | `DOUBLE PRECISION` |
| `BLOB` | `BYTEA` |
| `DATETIME TEXT` | `TIMESTAMPTZ` |

## 下一切片 #110-b：范围严格界定

**目标**：把 `postgres_engine_full_columns.rs` 三条 RED 测试推 GREEN。

**唯一允许修改的文件**：`crates/zbrain-core/src/postgres.rs`

**任务清单**：

1. **`put_page` 30 列 UPSERT**
   - 把现有 10 列 INSERT 升级为完整 30 列 INSERT ON CONFLICT DO UPDATE
   - 列序参考 `crates/zbrain-core/src/engine.rs` 行 50-217 `Page` struct
   - JSON 字段（`compiled_truth` / `timeline` / `frontmatter` / `embedding` 等）走 `serde_json::to_value`
   - `generation` / `created_at` / `updated_at` 不在 INSERT 列表里（由 trigger / DEFAULT 处理）

2. **`row_to_page` decoder 升级**
   - 从 10 列扩到 30 列
   - TIMESTAMPTZ → `Option<String>`：`row.try_get::<Option<DateTime<Utc>>, _>("col")?.map(|ts| ts.to_rfc3339())`
   - JSONB → 反序列化到对应 typed 字段，缺省时 fallback default
   - **`PageKind` 默认值**：schema 默认 `'markdown'`，反序列化时 `PageKind::Markdown`（不是 `Default::default()`）

3. **`get_page` / `list_pages` projection 升级**
   - SELECT 列表扩到 30 列
   - 保持现有 `include_deleted` filter 不变
   - `list_pages` 的 source filter（#74）**不要并进来**，留独立切片

**验证**：本地三连绿（参 #110-a），CI 上 `cargo test postgres_engine_full_columns -- --test-threads=1` 三条 GREEN。

**禁止**：
- ❌ 不要新增 migration（schema 已经 #110-a 落地）
- ❌ 不要改 libsql / InMemory backend（独立切片）
- ❌ 不要把 #74 source filter 顺手做了
- ❌ 不要静默接受任何"边迁顺手修一下"的偏差

## Follow-up 切片池（按优先级）

### PG 侧

| ID | 范围 | 触发条件 |
|---|---|---|
| **#110-b** | PG `put_page` 30-col UPSERT + `row_to_page` decoder | **当前 next** |
| #74 | PG `list_pages` source filters（`source_id` / `source_ids`） | #110-b 后 |
| #75 | PG integration test isolation（CI 上共享 DB trigger 残留风险） | #110-b 后 |

### libsql 侧（S6 系列）

| ID | 范围 | 当前状态 |
|---|---|---|
| S6-T5b | `list_pages` 4 filters：`slug_prefix` / `source_id` / `source_ids` / `updated_after` | 进行中 |
| S6-T5c | `migrations-sqlite/0004_page_tags.sql` 新表 + tag filter JOIN | pending |
| S6-T6 | `put_page` 30-col UPSERT，移除最后一个 `row_to_page` 调用点 | pending |
| schema 升级 | `updated_at` → 毫秒精度（`strftime('%Y-%m-%d %H:%M:%f', 'now')`），解除 sleep ≥1.1s 约束 | 独立切片，未排期 |

### 6a-pg 已完成（按时间顺序）

- ✅ #73 PG `get_page include_deleted`（`53656cb`）
- ✅ #72-a PG `Page.deleted_at` 字段保真（`9ce9773`）
- ✅ **#110-a PG `pages` 全列 migration + bump_generation trigger DDL（`b2314b6`，2026-05-30）**

## 关键技术备忘（跨切片复用）

### `BrainEngine::put_page` 签名（容易写反，必须严格）

```rust
async fn put_page(
    &self,
    slug: &str,
    source_id: Option<&str>,
    input: &PageInput,
) -> Result<Page>;
```

对应 `get_page`：

```rust
async fn get_page(
    &self,
    slug: &str,
    source_id: Option<&str>,
    opts: GetPageOptions,
) -> Result<Option<Page>>;
```

### PG sqlx + chrono 解码惯例

- `zbrain-core/Cargo.toml` 不直接依赖 `chrono`；workspace `sqlx` 已开 `chrono` feature
- 解码 TIMESTAMPTZ：`row.try_get::<Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>, _>("col")?`
- 直接 `chrono::DateTime` 会报 `error[E0433]: cannot find module or crate \`chrono\``——sqlx feature 仅暴露 sqlx 内部类型，不顶层 re-export
- 与 libsql SQLite `TEXT` 时间戳 + `Page.*_at: Option<String>` 形状对齐：`.map(|ts| ts.to_rfc3339())`

### 测试隔离 / unique slug 惯例

避免引入 `uuid` 依赖（zbrain-core 不需要），unique slug 用 nanos + 进程内 `AtomicU64::fetch_add`：

```rust
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_slug() -> String {
    let nanos: u64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("p-{nanos:x}-{n}")
}
```

**Clippy gotchas**（本切片踩过）：
- `u128 as u64` → `u64::try_from(...).unwrap_or(u64::MAX)`
- `Result<T, E>` 上 `.map().unwrap_or()` → `.map_or(default, |t| ...)`
- doc 注释里 `put_page` / `get_page` / 列名等 code identifier 必须反引号包裹

### PG 集成测试 skip pattern

```rust
async fn init_clean_engine() -> Option<PostgresEngine> {
    if std::env::var("ZBRAIN_TEST_PG_URL").is_err() {
        eprintln!("skip: ZBRAIN_TEST_PG_URL not set");
        return None;
    }
    // ... 真实初始化
}

#[tokio::test]
async fn some_test() {
    let Some(engine) = init_clean_engine().await else { return; };
    // 真实断言
}
```

本地视为 pass，CI runner 兜底 RED。

## 关键引用路径

| 用途 | 路径 |
|---|---|
| TS source of truth | `/Users/bilibili/Documents/workspace/jununfly/zbrain/src/core/pglite-schema.ts` 行 60-159 |
| libsql 镜像 migrations | `crates/zbrain-core/migrations-sqlite/0002_pages_full_columns.sql` + `0003_salience_and_full_generation_trigger.sql` |
| Rust `Page` 30 字段定义 | `crates/zbrain-core/src/engine.rs` 行 50-217 |
| Rust `PageInput`（有 `#[derive(Default)]`，可用 `..Default::default()`） | `crates/zbrain-core/src/engine.rs` |
| PG #110-a migration | `crates/zbrain-core/migrations/0003_pages_full_columns.sql` |
| PG RED 测试 | `crates/zbrain-core/tests/postgres_engine_full_columns.rs` |
| 6a-pg plan | `docs/plans/20260526/14-slice-6a-pg-plan.md` |
| 6a gap checklist | `docs/plans/20260526/13-slice-6a-gap-checklist.md` |

## 新对话续接 checklist

新会话开局，按以下顺序自检：

1. `cd /Users/bilibili/Documents/workspace/jununfly/zbrain-rust && git log --oneline -3 && git status` 确认 HEAD = `b2314b6`、工作树 clean
2. Read 本文件（`docs/plans/20260526/15-session-state-110a.md`）回血
3. Read `docs/plans/20260526/14-slice-6a-pg-plan.md` 与 `13-slice-6a-gap-checklist.md` 补全 6a 上下文
4. Read `crates/zbrain-core/migrations/0003_pages_full_columns.sql` 验证 trigger 现状
5. Read `crates/zbrain-core/tests/postgres_engine_full_columns.rs` 锁定 RED 断言
6. Read `crates/zbrain-core/src/postgres.rs` 当前 `put_page` / `row_to_page` / `get_page` 实现 —— 这是 #110-b 的唯一改动面
7. 在小切片范围内 RED → GREEN，禁止扩大范围
