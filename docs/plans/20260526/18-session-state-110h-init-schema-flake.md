# 会话状态快照 — slice #110-h 启动（init_schema 偶发 flake）

> 由助手 zjj 写入。配套：`16-slice-index-and-conventions.md`（跨切片约定）、`17-session-state-110c.md`（前置）。

## 当前 git 状态

- **分支**：`rust-rewrite`（worktree：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust`）
- **HEAD**：`62cabc4` — `slice #110-g: isolate concurrent integration tests`
- **工作树**：非 clean；未跟踪项包括 `.codegraph/`（无关）、本文档、H1 复现器 `crates/zbrain-core/tests/libsql_init_schema_flake_reproduce.rs`
- **前置 commit 链**（近 5）：
  - `62cabc4` slice #110-g: isolate concurrent integration tests
  - `d8e042b` docs: log slice #110-g + local PG path enablement
  - `5e0bb7e` docs: session state snapshot after slice #110-c
  - `7efd83a` slice #110-c: align PG put_page with TS source-of-truth (28 cols, server-stamp ingested_at)
  - `fb23d0d` slice #110-b: PG put_page/row_to_page widen to full 30-column Page

## #110-h 单一职责

> **拿到 init_schema 偶发 flake 的具体错误信息，定位唯一根因，落最小可行隔离，使 100 轮压测 0 失败。**

**显式排除**（防 scope creep）：
- 不重写 libsql 的迁移机制
- 不引入全局 mutex 把所有 libsql 集成测试串行化（sledgehammer，且拖回归速度）
- 不改 `init_schema` 业务语义
- 除非 H3 证据明确指向业务代码，否则**只动测试侧**

## 现象画像（来自 #110-g 收尾观察）

| 项 | 状态 |
|---|---|
| 入口栈帧 | `tests/libsql_engine_list_pages.rs:45` 的 helper `init_clean_engine` |
| 失败行 | `engine.init_schema().await.expect("init_schema")` |
| 复现率 | 50 轮全 workspace 压测 0 次；65 轮总盘 1 次 ≈ 1.5% |
| 已确认不相关 | 与 #110-g 修复的 offset/tie-break 完全无关 |
| **缺失证据（关键）** | 具体错误字符串未抓到；`init_schema` 内部 6 个失败点都被 `.expect("init_schema")` 折叠成同一个 panic 消息 |

`LibsqlEngine::init_schema`（`src/libsql.rs:157~199`）内部潜在失败点：
1. `self.conn()` → libsql connect / PRAGMA foreign_keys
2. `conn.query("PRAGMA user_version")` 查询
3. `rows.next()` 行抓取
4. `row.get(0)` 列解码
5. `conn.execute_batch(MIGRATION_N)` 迁移执行
6. `conn.execute_batch("PRAGMA user_version = N")` 版本写入

## 候选根因清单（按典型概率排序，待 RED 阶段证据反证）

1. **`NamedTempFile` 路径撞库 / 同文件被多 Database 持有** → journal/WAL 锁竞争
2. **libsql `Builder::new_local().build()` 内部一次性初始化竞争**（pthread/log）
3. **`PRAGMA user_version` 与 `execute_batch` 在同连接上交替**（libsql 0.x 连接边界）
4. **OS fd 耗尽 / macOS `tempfile` cleanup race**
5. **其它未列出机制** — 由 H3 证据确认

> **设计原则**：不预先选根因，按 H1/H2 抓到的具体 msg 走。

## 切片步骤（Surgical TDD）

| 任务 | 内容 | 完成判据 |
|---|---|---|
| **H1** | 写 `tests/libsql_init_schema_flake_reproduce.rs`：N=32 并发 `tokio::spawn`，每个独立 `NamedTempFile` + connect + `init_schema`；目标把失败率从 1.5% 提到 >10%，使 5~10 轮可稳定复现 | 抓到 panic + backtrace |
| **H2** | 把复现测试与 helper 内 `.expect("init_schema")` 改为打印底层 `Error::engine(msg)` 的 `.unwrap_or_else`（**仅测试侧**） | 拿到具体根因 msg |
| **H3** | msg + 候选清单交叉 → 收敛到**唯一**根因 | 不收敛到唯一就停 |
| **H4** | 按根因落最小手术修复（候选：tempfile 加随机后缀 / Builder 预热 OnceCell / 调 `--test-threads`）| 复现脚本 100 轮 0 失败 |
| **H5** | 三连绿：`cargo build` / `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` | 全绿、零警告 |
| **H6** | commit + 更新本文档落最终结论 | message 含根因/修复点/压测证据；本文档落"完成范围" |

## 风险与边界

- 100 轮 0 失败 ≠ 数学上无 flake，只是把 P(fail) 压到 <1%。本文档结论段必须明示
- 若根因落在 libsql 上游 → 本切片仅做隔离/绕开；另开 follow-up 跟踪上游 issue
- 若 H3 在合理时间内无法收敛到唯一根因 → 停下来评审，不在切片内堆"多重防御"
- TS 端**不动**（迁移项目纪律）

## 决策记录

| 决策 | 选项 | 用户裁定 |
|---|---|---|
| D1 切片编号 | `#110-h` / `#111-flake` | **#110-h**（同属 #110 测试稳定性家族） |
| D2 是否允许动业务 `init_schema` | 仅根因明确时允许 / 完全禁止 | **仅根因明确时允许**（默认禁止） |
| D3 压测轮数门槛 | 50 / 100 / 200 | **100 轮** |
| D4 是否先写盘文档 | 立刻写盘 / 写测试后再写盘 | **立刻写盘** |

裁定时间：2026-05-30 19:55（用户「都按默认」）

## 2026-05-30 P3 诊断更新

用户选择先做 **P3：锁死 `--test-threads=1`，判断 flake 是"并发"还是"次数/环境"**。

### 关键证据

| 场景 | 命令核心 | 结果 |
|---|---|---|
| 默认 test harness 并发 | `cargo test -p zbrain-core --test libsql_engine_list_pages` × 30 | **29 pass / 1 fail**；失败为 `SIGSEGV: invalid memory reference` |
| 单线程 | `cargo test -p zbrain-core --test libsql_engine_list_pages -- --test-threads=1` × 30 | **30 pass / 0 fail** |
| 2 线程 | `... -- --test-threads=2` × 30 | **30 pass / 0 fail** |
| 4 线程 | `... -- --test-threads=4` × 30 | **30 pass / 0 fail** |
| 8 线程 | `... -- --test-threads=8` × 30 | **28 pass / 2 fail**；失败为 `SIGSEGV: invalid memory reference` |

### 结论修正

- 原画像里的"`init_schema().await.expect("init_schema")` 折叠底层 `Error::engine(msg)`"不再是主线解释：新证据显示失败形态是 **进程级 native crash（SIGSEGV/SIGABRT）**，Rust 层 `.expect()` 可能根本没有机会执行。
- H1 自建 `tokio::spawn` 并发复现器（32 个 task，共享同一测试函数 runtime）30 轮 0 fail；它测的是 **task 级并发**，而原 flake 出现在 **test harness 并发运行多个 `#[tokio::test]`，每个测试各自 runtime** 的模型里。因此 H1 复现器维度不足，不能作为唯一 RED。
- P3 已把根因空间收敛到：**同一 test binary 内多个 libsql 测试在较高 `--test-threads` 并发下触发 libsql/SQLite native 层 crash**。不是单纯次数问题，也不是"只要 >1 线程就必现"；更像并发度阈值/资源压力问题。

### 下一步

进入 **H2 修正版**：不要继续等 Rust `Error::engine(msg)`；改为在测试侧最小化定位 native crash 的触发测试集合与触发阶段。

推荐顺序：
1. 用 `--test-threads=8` 对 `libsql_engine_list_pages` 按测试名二分/分组，找到最小崩溃测试集合。
2. 仅在测试侧添加诊断输出（test name / phase: temp alloc / connect / init_schema / put_page / list_pages），确认 crash 发生阶段。
3. 若最小集合仍指向 init_schema 并发，则 H4 候选修复优先是**只对 libsql 相关集成测试加 `#[serial]` 或局部 mutex**；若指向 `put_page/list_pages` 后续阶段，再继续根因追踪。

## 2026-05-30 H2 修正版结果

### 分组/阈值定位

| 运行集合 | 并发参数 | 结果 | 结论 |
|---|---|---|---|
| 前 12 个测试（Group A） | `--test-threads=8` × 30 | 30 pass / 0 fail | 单独不足以触发 |
| 后 11 个测试（Group B） | `--test-threads=8` × 30 | 30 pass / 0 fail | 单独不足以触发 |
| 前 14 个测试 | `--test-threads=8` × 30 | 30 pass / 0 fail | 不触发 |
| 前 18 个测试 | `--test-threads=8` × 30 | 30 pass / 0 fail | 不触发 |
| 前 20 个测试 | `--test-threads=8` × 30 | 30 pass / 0 fail | 不触发 |
| 前 21 个测试 | `--test-threads=8` × 30 | 30 pass / 0 fail | 不触发 |
| 前 22 个测试 | `--test-threads=8` × 30 | 30 pass / 0 fail | 不触发 |
| 仅 3 个 schema 测试 | `--test-threads=8` × 50 | 50 pass / 0 fail | schema 测试本身不是充分条件 |
| 全集但跳过 1 个早期测试（22 个） | `--test-threads=8` × 30 | 30 pass / 0 fail | 更支持总负载/并发阈值，而非某个特定测试 |
| 全 23 个测试 | `--test-threads=8`，多轮 | 可复现 SIGSEGV/SIGABRT | 需要全集级别并发/负载才容易触发 |

### Phase 诊断

在测试侧 `init_clean_engine()` 临时加入 phase stderr：

- `temp alloc start/ok`
- `connect start/ok`
- `init_schema start/ok`

带 `--nocapture --test-threads=8` 跑全集，在 24 次 pass 后抓到一次 SIGSEGV：

- 前 8 个测试几乎同时进入 `init_schema start`，随后均打印 `init_schema ok`。
- 第 9 个测试 `list_pages_includes_soft_deleted_when_flag_set` 打印：
  - `temp alloc ok`
  - `connect ok`
  - `init_schema start`
  - **未打印 `init_schema ok`，进程 SIGSEGV**

结论：crash phase 被锁定在 **`LibsqlEngine::init_schema()` 内部**，不是 `put_page` / `soft_delete_page` / `list_pages` 后续阶段。

### H3 初步根因收敛

基于证据，候选根因重新排序：

1. **最可能 / 可行动根因**：libsql local engine 在同一 test binary 内多个独立 `#[tokio::test]` runtime 高并发执行 `init_schema()` 时触发 native 层非线程安全路径（SIGSEGV/SIGABRT）。测试侧并发模型是触发器。
2. `NamedTempFile` 路径撞库：证据弱。诊断输出显示 temp path 都不同；且单个 H1 复现器 960 次不同 temp file init 0 fail。
3. SQLite journal/WAL 同文件竞争：证据弱。不同 temp path，无同文件竞争证据。
4. Rust 层 `Error::engine(msg)`：已基本排除。进程级 signal 表明不是普通 `Result::Err`。
5. 某个具体 list/tag/schema 测试逻辑：证据弱。各子集单跑不触发，全集阈值触发。

H4 候选修复应限定在测试隔离层：对 libsql 集成测试中会触发 local engine schema 初始化的测试加局部串行化（优先 `#[serial_test::serial]`，与 #110-g PG 测试隔离一致），而不是改业务 `init_schema` 或重写 migration。

## 2026-05-30 H4 修复与局部验证

### 修复范围

- 只修改 `crates/zbrain-core/tests/libsql_engine_list_pages.rs`。
- 23 个 `#[tokio::test]` 全部追加 `#[serial_test::serial]`。
- 不修改生产文件 `crates/zbrain-core/src/libsql.rs`。
- 不修改 `Cargo.toml`：workspace 与 `zbrain-core` dev-dependencies 已有 `serial_test`。

新增测试文件说明：

```rust
//! - Slice #110-h: every test is `#[serial_test::serial]` because concurrent
//!   libtest execution creates multiple independent `#[tokio::test]` runtimes
//!   that can enter libsql local `init_schema()` at the same time. On macOS,
//!   `--test-threads=8` reproduced SIGSEGV/SIGABRT inside `init_schema()` even
//!   though every test used a distinct temp DB file. Serialising this file is a
//!   test-isolation fix; it does not change engine semantics.
```

示例测试属性：

```rust
#[tokio::test]
#[serial_test::serial]
async fn list_pages_projects_all_30_columns() {
    // ...
}
```

### 验证证据

| 验证项 | 命令核心 | 结果 |
|---|---|---|
| 编译该测试 binary | `cargo test -p zbrain-core --test libsql_engine_list_pages --no-run` | pass；`Finished test profile` |
| 单轮默认线程 | `cargo test -p zbrain-core --test libsql_engine_list_pages` | pass；23 passed |
| 单轮 8 线程 | `cargo test -p zbrain-core --test libsql_engine_list_pages -- --test-threads=8` | pass；23 passed |
| 默认线程 100 轮 | 同一 test binary 循环 100 轮 | 后台任务 `y6LOFa` completed，无失败 stdout/stderr；未展示预期 PASS 文本，故仅作弱正向证据 |
| 8 线程 100 轮（第一次） | `... -- --test-threads=8` 循环 100 轮 | 后台任务 `oxpKSt` completed，无失败 stdout/stderr；未展示预期 PASS 文本，故仅作弱正向证据 |
| 8 线程 100 轮（第二次） | `... -- --test-threads=8` 循环 100 轮 | 后台任务 `q9OtF7` completed，无失败 stdout/stderr；未展示预期 PASS 文本，故仅作弱正向证据 |
| 8 线程 100 轮（foreground 状态文件确认） | `for i in $(seq 1 100); do cargo test -p zbrain-core --test libsql_engine_list_pages -- --test-threads=8 ...; done` | **pass**；stdout 明确打印 `PASS test-threads=8 100 rounds confirmed`，exit code 0 |

> 注：第一次状态文件版后台任务 `S8ovwb` completed 但状态文件只保留 START 行，未写入 PASS；原因不明，不作为强正向证据。随后使用 foreground 版本重跑同等 100 轮确认并取得 stdout PASS + exit 0。

### 当前结论

H4 局部隔离修复已取得足够的局部验证证据：原先在 `--test-threads=8` 下 30 轮可复现的 `libsql_engine_list_pages` native crash，在该测试文件级串行化后通过 100 轮确认压测。下一步进入 H5：workspace 三连绿。

## 2026-05-30 H5 workspace 三连绿

### 首次 H5 结果

- `cargo build`: pass。
- `cargo test --workspace`: pass。
- `cargo clippy --workspace --all-targets -- -D warnings`: fail。

失败原因不是生产代码，而是 #110-h H1 新增诊断测试文件的 doc comment 触发 `clippy::doc-markdown`：

```text
error: item in documentation is missing backticks
 --> crates/zbrain-core/tests/libsql_init_schema_flake_reproduce.rs:6:67
  |
6 | //! default multi-threaded runner because "each test owns its own NamedTempFile,
  |                                                                   ^^^^^^^^^^^^^

error: item in documentation is missing backticks
  --> crates/zbrain-core/tests/libsql_init_schema_flake_reproduce.rs:14:10
   |
14 | //!      OnceCell that is not concurrency-safe on cold start.
   |          ^^^^^^^^

error: item in documentation is missing backticks
  --> crates/zbrain-core/tests/libsql_init_schema_flake_reproduce.rs:15:10
   |
15 | //!   3. SQLite journal/WAL file creation race when N processes touch the
   |          ^^^^^^
```

### H5 修复

只修正文档注释 markdown，不改测试逻辑：

```rust
//! default multi-threaded runner because "each test owns its own `NamedTempFile`,
//! so no cross-test contention". Empirically that still flakes at ~1.5% on
//! `tests/libsql_engine_list_pages.rs:45` (`engine.init_schema().await.expect("init_schema")`).
//!
//! Hypothesised culprits (see `docs/plans/20260526/18-session-state-110h-init-schema-flake.md`):
//!   1. `NamedTempFile` re-uses pid-based suffix → libsql open races on the
//!      same path when several tests start in the same wall-clock millisecond.
//!   2. libsql `Builder::new_local(...).build()` lazily provisions a global
//!      `OnceCell` that is not concurrency-safe on cold start.
//!   3. `SQLite` journal/WAL file creation race when N processes touch the
```

### H5 最终验证

最终重新执行完整三连绿：

```bash
cd "/Users/bilibili/Documents/workspace/jununfly/zbrain-rust" && \
  cargo build && \
  cargo test --workspace && \
  cargo clippy --workspace --all-targets -- -D warnings
```

结果：**pass，exit code 0**。

关键输出：

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.22s
...
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.76s
...
test init_schema_survives_high_concurrency ... ok
...
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.18s
```

当前 #110-h 已满足：局部 100 轮 flake 压测通过 + workspace 三连绿通过。下一步：提交 #110-h。
