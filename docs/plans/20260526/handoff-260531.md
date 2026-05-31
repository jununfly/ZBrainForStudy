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
