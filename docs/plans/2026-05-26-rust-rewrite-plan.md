# 2026-05-26 Rust Rewrite Plan Consolidation

> 本文档提炼自 `docs/plans/20260526/` 的连续过程文档。它保留仍有效的目标、切片、决策、状态与后续项；原目录中的过程性 handoff、临时审计与过期计划在提炼后可删除。

## 1. 背景与目标

原计划的目标是把 TypeScript 时代的 GBrain 代码库手术式迁移到 Rust-first 的 ZBrain：先建立 Rust core/CLI/存储后端的可验证闭环，再逐步替代 TS 侧实现。

当前仓库策略已更新为：

- 项目语言统一为 ZBrain。
- 当前主线是 TS -> Rust 迁移。
- TS 代码不在品牌迁移阶段机械删除；Rust slice 成功替代一部分，再对应削减 TS。
- 领域词 `brain` / `source` 保持，不因品牌迁移改名。
- 品牌、配置、命令、环境变量、dotfile 与公开文档统一迁到 ZBrain 命名，不保留 GBrain 兼容 alias/fallback。

## 2. 原计划范围

原计划覆盖：

- Rust workspace 与 crate 拆分。
- core types、错误模型与 `BrainEngine` trait。
- Page CRUD、schema migration、generation trigger、tag 与 link 相关能力。
- PostgreSQL 后端。
- libsql/SQLite 本地后端，用于替代 PGLite 路线。
- CLI 框架。
- Web admin 与 MCP 服务的后续接入。
- 测试策略、PG fixture、libsql flake 修复与跨后端 contract mirror。

原计划中较早的“单 crate 草图”“一次性完整重写”“PGLite 保留为 Rust 嵌入式后端”等内容已被后续决策覆盖。

## 3. 仍有效的核心技术决策

### 3.1 Rust workspace 多 crate

采用多 crate workspace，而不是单 crate：

```text
crates/
├── zbrain-core/
├── zbrain-cli/
├── zbrain-web/
└── zbrain-mcp/
```

理由：模块边界清晰、编译期约束更强、CI/cache 可按 crate 切分。

### 3.2 libsql/SQLite 替代 PGLite 本地后端

Rust 本地嵌入式后端选择 libsql/SQLite。PGLite 保留为 TypeScript 历史 truth 与行为参考，但不作为 Rust 重写线的嵌入式数据库目标。

### 3.3 PostgreSQL 与 libsql 双后端 contract mirror

PostgreSQL 与 libsql 应维持行为镜像：schema、filter、projection、tag CRUD、soft delete、advanced page methods 等关键 surface 需要双侧对齐。

### 3.4 `source_id` 默认值契约

跨 PG / libsql / InMemory 统一：

```rust
source_id.unwrap_or("default")
```

任意 backend 修改此契约时，必须同步另外两个 backend，并增加 round-trip / mirror test。

### 3.5 `put_page` 写入边界

仍有效的写入边界：

- `put_page` 不写 `embedding`。
- `put_page` 不写 `last_retrieved_at`。
- `ingested_at` 只在 ingestion metadata 存在时由 server stamp。

### 3.6 `bump_generation` watched allow-list

`generation` 只由 truth 相关列触发更新。有效 allow-list：

```text
compiled_truth, timeline, frontmatter, deleted_at,
contextual_retrieval_mode, title, type, page_kind,
corpus_generation, content_hash
```

显式排除：

```text
salience_score, last_retrieved_at, salience_touched_at, embedding
```

### 3.7 libsql cold-start FFI race 的最终结论

早期“给 libsql 测试加 `serial_test`”只是过程性 workaround，已废弃。

最终决策：并发安全边界下沉到业务层 `LibsqlEngine::init_schema()`，用 process-wide lock 包住整个 cold-start path，包括首次连接与 `PRAGMA foreign_keys = ON`。

代表形态：

```rust
SCHEMA_INIT_LOCK: LazyLock<tokio::sync::Mutex<()>>
```

不要再用测试层串行化来掩盖此类初始化 race。

### 3.8 Web 与 MCP 的定位

- Web 前端继续复用 React + TS，Rust 后端通过 Axum 托管/API 接入。
- MCP 服务进入后续 crate/slice，不属于 core + CLI MVP 的首要闭环。

## 4. 主线 slice 索引

### 4.1 已完成或已有明确历史记录的 slice

| Slice | 范围 | 状态 |
|---|---|---|
| slice-1 | workspace 脚手架，4 crates + sanity tests | 已完成 |
| slice-2 | core types + error model | 已完成 |
| slice-3 | `BrainEngine` trait skeleton + InMemoryEngine | 已完成 |
| slice-4a | Postgres lifecycle：connect/disconnect/init_schema | 已完成 |
| slice-4b | Postgres Page CRUD | 已完成 |
| slice-5 | libsql embedded SQLite backend lifecycle + Page CRUD | 已完成 |
| 6a S1-S8 | libsql Page schema/filter/tag/UPSERT/source_id 对齐 | 已完成 |
| PG mirror | PG full-column migration、list filters、tag、soft delete、duplicate lookup | 已完成 |
| T5 | 13 个 `page_methods_*.rs` PG tests 迁移到 `PgFixture` | 已完成 |
| T6 | libsql init_schema cold-start race 修复，移除测试层 `serial_test` workaround | 已完成 |
| cleanup | `BrainEngine` S6 默认 fallback 清理为 required trait contract | 已完成 |

### 4.2 后续候选 slice

| Slice | 范围 | 备注 |
|---|---|---|
| PG integration test infra | 去 `#[ignore]`、加载 `.env`、禁止把 `ZBRAIN_TEST_PG_URL unset` skip 误判为 pass | 独立推进 |
| S6-signature | 评估 `list_pages` 签名 `&PageFilters` vs `Option<&PageFilters>` | 独立推进 |
| S6-time-types | 引入更明确的时间字段类型，例如 chrono 相关封装 | 独立推进 |
| PG advanced reads | `get_all_slugs` / `list_all_page_refs` / `get_page_timestamps` / `get_effective_dates` / `get_salience_scores` | 候选下一阶段 |
| PG advanced writes | `refresh_page_body` / `update_page_contextual_retrieval_state` / `update_slug` / `touch_salience` | 候选下一阶段 |
| libsql timestamp precision | `updated_at` 毫秒精度，减少跨秒排序测试 sleep | 独立推进 |
| Web/API/MCP 接入 | Axum Web API、React admin 复用、MCP 服务 | core + CLI 闭环之后 |
| TS 削减 | Rust slice 替代成功后对应删除 TS 侧实现 | 由 `docs/prd/complete-ts-to-rust.md` 统筹 |

## 5. 测试与验证纪律

### 5.1 Rust slice 三连绿

每个 Rust 实现切片提交前必须 fresh verification：

```bash
cargo fmt --all --manifest-path <root>/Cargo.toml -- --check
cargo build --manifest-path <root>/Cargo.toml --workspace
cargo test --manifest-path <root>/Cargo.toml --workspace
cargo clippy --manifest-path <root>/Cargo.toml --workspace --all-targets -- -D warnings
```

如果 workspace 并发测试出现已知 flake，可以补充串行复跑定位，但不能把串行复跑当作掩盖真实 race 的长期方案。

### 5.2 PG 测试不得误判

PG 测试必须明确区分：

- `ZBRAIN_TEST_PG_URL` unset 导致 skip。
- PG fixture 真正启动并执行。

不能把 unset skip 记为 PG pass。

### 5.3 TDD 与切片边界

- 新行为先写失败测试，再写最小实现。
- 每个切片只解决一个明确问题。
- 发现新问题时记录 follow-up，不把新范围静默并入当前切片。
- 实现 commit 与 doc-only follow-up commit 分离；不要 amend 已稳定实现 commit。

## 6. 已废弃或仅保留历史价值的结论

以下内容已不作为未来执行依据：

- 早期单 crate / early directory sketch。
- 早期“完整一次性重写”的大切片草图。
- 早期依赖替代清单中未被后续采用的项，例如部分 `embeddingdb` 设想。
- “libsql 测试加 `serial_test`”作为长期修复的结论。
- 旧 handoff 中“不要删除 `serial_test`”的阶段性提醒；最终状态是已由 T6 与 cleanup 删除。
- 空 handoff / 空 follow-up 文档本身不携带有效决策。

## 7. 本 consolidation 后的清理规则

`docs/plans/20260526/` 中的过程文件已被本文吸收。后续应以本文作为 2026-05-26 Rust rewrite 计划的 canonical 摘要，不再依赖连续 handoff 文件恢复上下文。

如果未来需要恢复更细历史，可从 git history 查找被删除的过程文档；当前工作上下文只保留：

- 本文：`docs/plans/20260526-rust-rewrite-plan.md`
- 当前路线图：`docs/plans/zbrain-ts-to-rust-roadmap.json`
- 路线图视图：`docs/plans/ZBRAIN_TS_TO_RUST_ROADMAP.md`
- TS -> Rust PRD：`docs/prd/complete-ts-to-rust.md`
