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
