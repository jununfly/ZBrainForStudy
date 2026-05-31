# Handoff - 2026-05-31

## 会话目标
> 继续推进 ZBrain Rust rewrite 的 PostgreSQL Page 能力迁移，当前重点是 PG `list_pages(tag)` 独立切片：补齐 PostgreSQL `page_tags` migration、tag CRUD 行为，以及 `list_pages(PageFilters { tag })` 与 libsql backend 的行为对齐。
>
> 本次 handoff 的直接目标是把当前会话状态写入 `docs/plans/20260526/`，让下一个会话能从未提交的 PG tag slice 继续。

## 已完成
- 已完成并提交 PG `list_pages` source filters：`07f6f86 slice #74: add PG list_pages source filters`。
- 已完成并提交 PG `list_pages` follow-up filters：`defdf04 slice: add PG list_pages follow-up filters`。
  - 已覆盖：`slug_prefix` / `updated_after` / `include_deleted` / `offset` / `sort`。
- 已明确 `tag` filter 需要独立切片处理，原因是它依赖 PG `page_tags` schema 和 tag CRUD 行为，不能只作为 `list_pages` 条件追加。
- 当前 PG tag slice 已完成实现并通过验证，但尚未提交：
  - 新增 PostgreSQL migration：`crates/zbrain-core/migrations/0005_page_tags.sql`。
  - 修改 PostgreSQL backend：`crates/zbrain-core/src/postgres.rs`。
  - 新增 PostgreSQL integration tests：`crates/zbrain-core/tests/postgres_engine_page_crud.rs`。
  - `crates/zbrain-core/tests/libsql_engine_full_columns.rs` 仅有 rustfmt 产生的格式变化，无业务语义变化。
- 当前验证已跑通：
  - `cargo fmt --all --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml -- --check`
  - `cargo build --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml`
  - `cargo test --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace`
  - `cargo clippy --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace --all-targets -- -D warnings`

## 关键决策
- `list_pages(tag)` 拆成独立切片，而不是混入 PG follow-up filters。
  - 理由：`tag` 需要 `page_tags` 表和 `add_tag` / `remove_tag` / `get_tags` 行为支撑，属于 schema + behavior slice。
- PostgreSQL `page_tags.page_id` 使用 `BIGINT`。
  - 理由：PG `pages.id` 在 `crates/zbrain-core/migrations/0001_init.sql` 中是 `BIGSERIAL PRIMARY KEY`；需与 FK target 类型一致。
- PG tag 行为对齐 libsql backend：
  - `add_tag`：live page 存在时成功；重复 tag idempotent；missing / soft-deleted page 返回 `PageNotFound`；`source_id=None` 映射为 `default`。
  - `remove_tag`：删除已存在 tag；missing page/tag/source mismatch 静默 `Ok(())`；`source_id=None` 映射为 `default`。
  - `get_tags`：按 tag 升序返回；missing/source mismatch 返回空数组；`source_id=None` 映射为 `default`。
- PG `list_pages` 动态 SQL bind order 明确为：
  - `page_type → source_id → source_ids → slug_prefix → updated_after → tag → limit → offset`。
  - 注意：SQL builder 的 `param_idx` 推进顺序必须与 bind 顺序严格一致，否则会静默 misbind PG `$N`。

## 待办 / 下一步
- [ ] 提交当前 PG tag slice。
  - 当前未提交内容包含：
    - `M crates/zbrain-core/src/postgres.rs`
    - `M crates/zbrain-core/tests/libsql_engine_full_columns.rs`
    - `M crates/zbrain-core/tests/postgres_engine_page_crud.rs`
    - `?? crates/zbrain-core/migrations/0005_page_tags.sql`
- [ ] 提交前再次检查 diff 边界。
  - 特别确认 `crates/zbrain-core/tests/libsql_engine_full_columns.rs` 只是 rustfmt 格式化，不应被误解为业务改动。
- [ ] 提交前按项目纪律重新跑 verification 三连/四连绿，至少包含：
  - `cargo fmt --all --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml -- --check`
  - `cargo build --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml`
  - `cargo test --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace`
  - `cargo clippy --manifest-path /Users/bilibili/Documents/workspace/jununfly/zbrain-rust/Cargo.toml --workspace --all-targets -- -D warnings`
- [ ] 视提交后状态更新计划/索引文档：
  - `docs/plans/20260526/14-slice-6a-pg-plan.md`
  - `docs/plans/20260526/16-slice-index-and-conventions.md`
- [ ] 更新当前 TaskList 状态。
  - `#51 运行验证并记录 tag slice` 仍是 `in_progress`，虽然验证已通过；提交或记录后再收口更稳。

## 已知问题
- 当前 PG tag slice 已通过验证，但尚未提交。
- 本机会话中 `git status` / `git diff` 曾触发 sandbox warning，主要涉及 worktree index lock 或 pager 临时文件；stdout 可读。后续建议使用：
  - `GIT_PAGER=cat GIT_OPTIONAL_LOCKS=0 git -C "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" --no-pager ...`
- PG integration tests 依赖 `ZBRAIN_TEST_PG_URL`。
  - 未设置时 PostgreSQL tests 会输出 `skipping: ZBRAIN_TEST_PG_URL unset`，这**不算**有效 RED/GREEN，也**不算 pass**——只代表 shell 未加载 env。
  - **本机已有可用的 Homebrew PostgreSQL 16.14**（监听 `localhost:5434`，库 `zbrain_test` 已存在），URL 持久化在 `<repo-root>/.env`（gitignored）：
    `ZBRAIN_TEST_PG_URL=postgres://postgres:postgres@localhost:5434/zbrain_test`
  - 跑 PG 集成测试前**必须**激活：`set -a; source .env; set +a`（或 `direnv allow`）。
  - 权威源：`docs/plans/20260526/17-session-state-110c.md` L139-150（#110-c, 2026-05-30 首次启用）。
- 当前环境没有 Docker（`docker compose` 失败为 `command not found: docker`），但**无需 Docker**：直接用上述本机 Homebrew PG 即可。
- zsh 中 `status` 是只读变量；临时脚本不要写 `status=$?`，改用 `test_status=$?`。

## 相关产物
- 项目根目录：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust`
- 计划目录：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/docs/plans/20260526/`
- 本 handoff：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/docs/plans/20260526/handoff-260531.md`
- PG migration：`crates/zbrain-core/migrations/0005_page_tags.sql`
- PG backend：`crates/zbrain-core/src/postgres.rs`
- PG integration tests：`crates/zbrain-core/tests/postgres_engine_page_crud.rs`
- libsql formatting-only diff：`crates/zbrain-core/tests/libsql_engine_full_columns.rs`
- 参考计划：`docs/plans/20260526/14-slice-6a-pg-plan.md`
- 索引/约定：`docs/plans/20260526/16-slice-index-and-conventions.md`
- 最新 commit：`defdf04 slice: add PG list_pages follow-up filters`
- 当前 diff 摘要：
  - `crates/zbrain-core/src/postgres.rs`: 约 `96` 行变更
  - `crates/zbrain-core/tests/libsql_engine_full_columns.rs`: 约 `10` 行格式变更
  - `crates/zbrain-core/tests/postgres_engine_page_crud.rs`: 约 `175` 行新增/变更
  - `crates/zbrain-core/migrations/0005_page_tags.sql`: untracked 新文件，约 `16` 行

## 建议下一个会话使用的技能
- `test-driven-development`: 当前项目要求严格 TDD；继续推进或修正 PG tag slice 时必须保持 RED/GREEN/REFACTOR 证据链。
- `verification-before-completion`: 提交/宣称完成前必须重新跑 fmt/build/test/clippy，并以输出作为证据。
- `executing-plans`: 后续若继续根据 `docs/plans/20260526/` 推进切片，应按计划 checkpoint 执行而不是发散改动。
- `session-handoff`: 若下个会话结束前仍未提交/仍有未完成切片，继续生成更新版 handoff。

## 注意事项
- 不要把关键技术偏差静默归入“已接受偏差”；如果发现 PG tag 行为与 libsql 不一致，应另开 checklist/切片追踪。
- 不要把 `tag` filter 当作普通 WHERE 条件孤立处理；它依赖 `page_tags` schema、tag CRUD 和 bind order contract。
- 不要在 `list_pages` 中调整 bind 顺序而不同步 `build_list_pages_sql` 的 `param_idx` 推进顺序。
- 不要把 `ZBRAIN_TEST_PG_URL` 未设置时的 skipped tests 当成有效 PG 验证；本机有可用 PG（见“已知问题”段 + `.env`），先 `set -a; source .env; set +a` 再 `cargo test`。
- 当前 handoff 未保存任何 API key、密码或 PII；只引用路径、commit、状态和可执行待办。

## 2026-05-31 收口（追加）

- PG tag slice 已按 diff 边界拆分提交：
  - `d4bf032 chore(libsql-tests): apply rustfmt to libsql_engine_full_columns`
  - `5ca9131 slice 6a-pg: add PG page_tags + tag CRUD + list_pages(tag)`
- 提交前已在隔离 `CARGO_HOME` / `CARGO_TARGET_DIR` 下重新跑过 fmt/build/test/clippy 四件套，全部 `EXIT_CODE=0`；test 包含 live PG suite（已设置 `ZBRAIN_TEST_PG_URL`）。
- 上文“待办”里的提交项与验证项均已完成；剩余未做：
  - 刷新 `docs/plans/20260526/14-slice-6a-pg-plan.md` 与 `16-slice-index-and-conventions.md`（PG tag slice 标记完成 + 接续下一个切片定义）。
  - 收口 TaskList `#51`。
- 本 handoff 文件以独立 `docs:` 提交入库，与代码切片解耦。

## 2026-05-31 收口（追加 #2）— PG-advanced-reads 切片

- **切片**：PG-advanced-reads（5 个只读方法在 PG 端的 trait override；libsql 端按 D1 决策维持 `Unsupported`，留待 6a-libsql 阶段处理）。
- **commit 链**：
  - `a747ed5 slice 6a-pg(advanced_reads): override PG for 5 read methods, mirror TS pglite-engine` — 实现 commit（9 files, +1552 / −43）。
  - `7243b4f docs(plans): backfill plan 14 §10.2 commit hash a747ed5` — doc-only follow-up，回填稳定 hash，避开 amend 自指循环。
- **覆盖方法**（plan 14 §11.1 SQL 契约）：
  - `get_all_slugs(Option<&str>)`：`$1::text IS NULL OR source_id = $1` 守卫；返回 `HashSet<String>`，无 `deleted_at` 过滤（对齐 TS 行为）。
  - `list_all_page_refs()`：`WHERE deleted_at IS NULL ORDER BY source_id ASC, slug ASC`，返回 `Vec<PageRef>`。
  - `get_page_timestamps(&[String])`：`COALESCE(updated_at, created_at)::text`，`slug = ANY($1::text[])`，HashMap key=`slug`。
  - `get_effective_dates(&[PageRef])`：`unnest($1::text[], $2::text[]) AS u(slug, source_id)` 二元 join，HashMap key=`format!("{source_id}::{slug}")`。
  - `get_salience_scores(&[PageRef])`：**6a 阶段退化** `COALESCE(emotional_weight, 0.0) * 5.0`；6c 再补 `+ ln(1 + N_tags)`；takes 项硬编 0 直至 6c 落地。key=`format!("{source_id}::{slug}")`。
- **find_orphan_pages 已摘出**：作为独立切片 **PG-find-orphan-pages**，本切片不涉及；plan 14 §10.2 中保持 `[ ]`。
- **S6-T2 形态**（`page_methods_salience_scores_takes_zero_until_6c.rs`）：strong-semantics sibling 锁测；libsql 段断言 `Unsupported`、PG 段断言 `score = emotional_weight * 5.0` 与 `takes = 0` 的代数不变式；TDD 红→绿证据链完整。
- **四连绿门禁**（S5）全过：fmt ✅、build ✅、test ✅（37 套件 / 0 failed / 0 ignored，含 23 个 PG live 测试）、clippy ✅。验证前已 `set -a; source .env; set +a`。
- **plan 14 §10.2 状态**：5 行已标 `[x]`（hash `a747ed5`），`find_orphan_pages` 行保持 `[ ]`+独立切片注。
- **流程踩坑教训（已沉淀到 SOUL/记忆）**：
  - **commit hash 自指循环**：在实现 commit 里既写代码又写 `<this commit>` 占位、然后 `git commit --amend` 回填 hash → 内容变 → SHA 变 → 文档 hash 永远落后一步（实测连环 `23ff929 → 16a563f → a747ed5`）。
  - **解药**：实现 commit 先落地、立刻拿到稳定 hash；再开**独立 doc-only follow-up commit** 回填，绝不 amend 实现 commit。
- **未尽事项 / 下一步候选切片**（按优先级）：
  - **PG-find-orphan-pages**：单方法独立切片，沿用 PG-advanced-reads 流程；TS 行为权威源 `pglite-engine.ts` 中的 `findOrphanPages`。
  - **6a-libsql advanced reads**：把 5 个方法在 libsql backend 落地，移除 `Unsupported` stub。
  - **6c 完整 salience 公式**：补 `+ ln(1 + N_tags)`、激活 takes 维度（解锁 `salience_scores_takes_zero_until_6c.rs` 中的"until 6c"约束）。
- **TodoList 状态**：本切片相关全部 `completed`（含 #25/26/28/29/30/31~36/38/39/40/41）。

---

## PG-find-orphan-pages 切片闭环（追加，commit a56c9ae + 99c9c10）

- **范围**：`crates/zbrain-core/migrations/0006_links.sql`（新增）+ `postgres.rs` 追加 `find_orphan_pages` PG override + `tests/page_methods_find_orphan_pages.rs` 重写为 5 PG case + 1 libsql Unsupported placeholder。
- **关键决策**：
  - **links migration**：移植 TS `pglite-schema.ts:209-231` 完整 DDL；FK 列 `INTEGER → BIGINT`（匹配 `pages.id BIGSERIAL`）；`page_links` VIEW **YAGNI 暂不移植**。
  - **C11 双侧 soft-delete 过滤**：候选侧 + 入链源侧均 `deleted_at IS NULL`；测试 case `treats_link_from_deleted_page_as_absent` 锁定。
  - **`COALESCE(title, slug)` 是防御性死代码**：TS `title TEXT NOT NULL` + `putPage` 直接 bind → 空 title 存为空串。Rust 实现保留 COALESCE 作未来 NULL 漂移兜底；测试断言 empty title **保持空串**（TS parity），doc 注释解释该 nuance。
- **真实契约**：`async fn find_orphan_pages(&self) -> Result<Vec<OrphanPage>>`；`OrphanPage { slug, title, domain: Option<String> }`。
- **四连绿**：fmt clean / build ok / **test 239 passed** / clippy clean。
- **commit 序**：
  - `a56c9ae` 实现 + 测试（含 migration）。
  - `99c9c10` doc-only follow-up：plan 14 §10.2 勾选 + §12 落地修订段。
- **未尽事项 / 下一步候选切片**（更新优先级）：
  - **6a-libsql advanced reads**：5 个方法 libsql 落地，移除 `Unsupported`。
  - **PG-advanced-writes**：`refresh_page_body` + `update_page_contextual_retrieval_state`（plan 14 §10.2 仅剩这两行未勾选）。
  - **6c 完整 salience**：补 `+ ln(1 + N_tags)` + takes 维度。

---

## 会话交接 — 6a-libsql advanced reads（S1 调研完成，S2 RED 待执行）

- **会话时间**：2026-05-31 17:13 CST
- **当前 HEAD**：`d6de087 docs(handoff): append PG-find-orphan-pages slice closure`
- **工作树**：clean
- **本会话实际产出**：0 代码改动；多次 context 压缩/恢复导致反复 skill 加载，未进入 S2 RED。但 S1 调研结论已充分消化，下个会话可直接跳到 S2。

### S1 调研结论（5 个 advanced read 方法 libsql 落地方案）

#### 目标方法 + SQL 方言适配

| 方法 | PG SQL 关键 | libsql 适配 |
|------|------------|-------------|
| `get_all_slugs(Option<&str>)` | `$1::text IS NULL OR source_id = $1` | `?1 IS NULL OR source_id = ?1`；bind `Option<&str>` → NULL 自动 |
| `list_all_page_refs()` | `WHERE deleted_at IS NULL ORDER BY source_id ASC, slug ASC` | 同 PG，无参数绑定 |
| `get_page_timestamps(&[String])` | `slug = ANY($1::text[])` | 动态展开 `IN (?1, ?2, …)` + 逐个 bind |
| `get_effective_dates(&[PageRef])` | `unnest($1::text[], $2::text[]) AS u(slug, source_id)` | 用 `json_each` 或循环逐条 SELECT；key=`"{source_id}::{slug}"` |
| `get_salience_scores(&[PageRef])` | 同 get_effective_dates 的 unnest | 同上；公式 `COALESCE(emotional_weight,0.0)*5.0` |

#### libsql.rs 实施要点

1. **imports 补充**：需补 `use std::collections::{HashMap, HashSet}` + `use crate::engine::PageRef`（当前 L1-28 缺失）。
2. **插入位置**：在 `impl BrainEngine for LibsqlEngine` 块末尾（L829 `}` 之前）追加 5 个 async fn override。
3. **conn() 可见性**：`conn()` 是 `async fn` 私有方法（L686 模式），override 内直接调用即可，无需改可见性。
4. **streaming 模板**：参考 `soft_delete_page`（L686-718）和 `get_tags`（L799-829）的 `conn.query(...) → rows.next() → row.get::<T>(idx)` 循环。
5. **unnest 替代**：`get_effective_dates` 和 `get_salience_scores` 的 PG `unnest($1::text[], $2::text[])` 在 libsql 中不可用。推荐方案：**逐条拼接 WHERE OR** 或用 `json_each`——逐条更简单可靠：
   ```rust
   // 伪代码
   for pref in page_refs {
     let row = conn.query(
       "SELECT COALESCE(updated_at, created_at) AS ts FROM pages WHERE slug = ?1 AND source_id = ?2 AND deleted_at IS NULL",
       ::libsql::params![pref.slug, pref.source_id]
     ).await?;
     // 累积到 HashMap
   }
   ```
   若性能敏感可改用 `json_each` 单条查询，但 6a 阶段逐条足够。
6. **IN 动态展开**：`get_page_timestamps` 的 `slug = ANY($1::text[])` → 运行时拼 `WHERE slug IN (?1, ?2, …)` + 对应 bind 列表。
7. **module shadow**：所有 `libsql` crate 引用走 `::libsql::params![]` / `::libsql::Row` 前缀，避免与 `crate::libsql` module 冲突。

#### 测试翻转计划

| 测试文件 | 当前 libsql 段 | 翻转目标 |
|---------|--------------|---------|
| `page_methods_get_all_slugs.rs` | `assert!(matches!(err, Error::Unsupported))` | 正向行为：返回含 seeded slug 的 HashSet |
| `page_methods_list_all_page_refs.rs` | 同上 | 正向行为：返回 Vec<PageRef>，soft-deleted 排除 |
| `page_methods_get_page_timestamps.rs` | 同上 | 正向行为：返回 HashMap<slug, ISO-8601 ts> |
| `page_methods_get_effective_dates.rs` | 同上 | 正向行为：返回 HashMap<"{sid}::{slug}", ts> |
| `page_methods_get_salience_scores.rs` | 同上 | 正向行为：返回 HashMap<"{sid}::{slug}", f64> |
| `salience_scores_takes_zero_until_6c.rs` | libsql 段断 Unsupported | 翻为代数不变式 `(0.4*5.0 - score).abs() < 1e-9`；**仍保留** takes=0 不变式 |

#### salience 测试 emotional_weight 注入问题

- PG 测试用 `pg_set_emotional_weight` helper 直接 UPDATE 表设值。
- libsql 侧 `conn()` 是私有的，测试文件无法直接访问。
- **推荐方案**：在 `LibsqlEngine` 上新增 `pub(crate) async fn set_emotional_weight(&self, slug: &str, source_id: &str, weight: f64)` 辅助方法，仅供测试用；或通过 `database()` 拿 `Connection` 执行 raw SQL。
- **备选**：在测试 helper 模块中用 `init_clean_engine()` 返回的 engine 直接走 `put_page` 然后手动 raw SQL UPDATE（需确认 `database()` 能拿到可 query 的 conn）。

#### trait 默认实现清理

- `engine.rs` L383-452 的 5 个 `Err(Error::unsupported("pending slice 6a"))` 默认体在 libsql override 落地后**可保留**（作为未来新 backend 的兜底），无需删除。
- `postgres.rs` L685-688 注释 "libsql side intentionally keeps the default Unsupported until slice 6a-libsql" 需**删除/更新**。

#### 契约冲突已澄清

- plan 14 §11.4 写 "D1 锁定 libsql advanced-reads 等 6c+ 切片再处理" 是过期描述。
- handoff 多处把 6a-libsql 列为下一切片优先级 → handoff 是更新的 ground truth。
- 本切片完成后需在 doc-only follow-up 中清理 plan 14 §11.4。

### 待办 / 下一步（6a-libsql 切片）

- [ ] S2 RED：翻转 6 个测试文件 libsql 段为正向行为测试（含 salience 代数不变式）
- [ ] S3 GREEN：在 libsql.rs 补 imports + 追加 5 个 async fn override
- [ ] S4 REFACTOR：提取公共 unnest 替代逻辑、IN 动态展开 helper
- [ ] S5 四连绿门禁：fmt / build / test / clippy
- [ ] S6 commit：实现 commit + doc-only follow-up（回填 hash + 刷新 plan 14 + 清理 postgres.rs 注释）

### 已知问题

- libsql 并行 flake：`cargo test --workspace` 偶发 libsql SIGABRT，单独跑通过，属 pre-existing。
- `get_effective_dates` / `get_salience_scores` 的 unnest 替代方案尚未写代码验证，可能需迭代。
- `get_all_slugs` 不过滤 `deleted_at`——这是 TS parity quirk，不是 bug，但测试需显式验证。

### 相关产物

- Plan: `docs/plans/20260526/14-slice-6a-pg-plan.md`（§11 PG-advanced-reads 落地段，§11.4 需清理）
- Gap checklist: `docs/plans/20260526/13-slice-6a-gap-checklist.md`
- 本 handoff: `docs/plans/20260526/handoff-260531.md`
- 最新 commit: `d6de087`
- libsql 实现: `crates/zbrain-core/src/libsql.rs`（984 行，L829 前插入）
- PG 参照面: `crates/zbrain-core/src/postgres.rs`（L685-870）
- Trait 定义: `crates/zbrain-core/src/engine.rs`（L383-452）
- 测试: `crates/zbrain-core/tests/page_methods_*.rs`（6 文件）

### 建议下一个会话使用的技能

- `test-driven-development`：严格红绿重构，S2→S6 必备
- `executing-plans`：S1 调研已完成，按 S2-S6 步骤执行
- `codegraph-assistant`：unnest 替代方案可能需快速检索 libsql query API

### 注意事项

- **跳过 S1**：调研结论已在上文沉淀，下一个会话直接从 S2 RED 开始。
- **module shadow**：所有 libsql crate 引用走 `::libsql::` 前缀。
- **commit hash 自指循环**：实现 commit 先落地拿稳定 hash，再开独立 doc-only follow-up commit 回填，永不 amend 实现 commit。
- **PG 集成测试**：`ZBRAIN_TEST_PG_URL` env + `set -a; source .env; set +a` + `#[serial_test::serial]`。
- **types 导入**：`PageRef` 从 `crate::engine` 导入；`FindDuplicatePageOpts` 等从 `crate::types` 导入。
