# 残留 TS 活性审计（1-14 交付物）

日期：2026-08-06 ｜ 方法：目录级 Rust 对应模块存在性检查
（`crates/zbrain-core/src/<mod>` kebab↔snake 兼容；core 根级文件查 `<snake>.rs`）。

说明：本审计按**目录**判定「该模块是否已有 Rust 对应实现」。MIGRATED = Rust 拥有
逻辑、TS 仅胶水/类型/入口；UNMIGRATED = 无 1:1 Rust 模块，属真前沿（需 port 或验证后可删）。

> ⚠️ **计数已失效（2026-08-07）**：commit `bcafcafd` 删除了 **`src/core` 整棵 TS 树**
> （338 文件 / -92,427 行）。下方「总体」与各模块文件数是 **2026-08-06 的历史快照**，
> 不再反映工作区现状。当前真实残留：`src/**/*.ts` 仅 **79** 个（commands 70 + eval 7
> + types + version.ts），全仓 TS 共 **640** 个（tests 552 / src 79 / admin 4 / 其他 5），
> 且其中 **579 个文件 / 1,436 处 import** 仍**悬空引用已删的 `src/core`**
> （tests 510 文件 1078 refs ｜ src/commands 63 文件 347 refs ｜ src/eval 5 文件 8 refs ｜
> evals 1 文件 3 refs）。本表的**模块级 MIGRATED/UNMIGRATED 判定仍然有效**（那是「Rust 是否
> 拥有该模块逻辑」的结论），失效的只是文件计数。重新计数需在 glue 拆除后做一次全量 re-audit。

## 总体

- 扫描 `src/**/*.ts` 共 **414** 个文件。（历史快照，见上方失效说明）
- 已 MIGRATED（Rust 接管逻辑）：**142** 个（分布在已迁模块）。
- 仍 UNMIGRATED（真前沿）：**272** 个。

## 已 MIGRATED 模块（Rust 拥有逻辑，TS 为胶水）

| 模块 | 文件数 |
|---|---|
| core/skillpack | 27 |
| core/schema-pack | 25 |
| core/search | 22 |
| core (根级已迁文件) | 17 |
| core/calibration | 11 |
| core/ai | 8 |
| core/think | 7 |
| core/chunkers | 5 |
| core/resolvers | 3 |
| core/code-intel | 2 |
| core/budget | 1 |
| core/facts | 11 |
| core/skillify | 3 |

> 注：`core/skillpack` 已迁；`core/skillify` 全量迁 Rust（scaffold + check 12 项审计，1-1 + 1-1-1 完成），TS 源 `src/core/skillify/*.ts`、`src/commands/skillify.ts`、`src/commands/skillify-check.ts` 已删。
> `core/code-intel` 已迁但 `core/code-intel/sinks`(3) 仍 UNMIGRATED。

## 真前沿（UNMIGRATED，按文件数）

| 模块 | 文件数 | 性质 / 对应 roadmap |
|---|---|---|
| core (根级混合胶水+小helper) | 105 | 多 shell out 到 Rust operations；清理/验证后可删 |
| commands | 70 | 入口/视图，多经 bin/zbrain-rs.js → Rust；验证后可删 |
| core/ai/recipes | 18 | TS recipes，未 port |
| core/eval-contradictions | 15 | 评估逻辑，未 port（G66 提及 calibration-join） |
| core/takes-quality-eval | 10 | 评估逻辑，未 port |
| core/brainstorm | 5 | 未 port |
| core/claw-test | 5 | 测试harness，未 port |
| core/cross-modal-eval | 5 | 评估逻辑，未 port |
| core/output/validators | 5 | output 子集，**1-4-2 BLOCKED** |
| eval/longmemeval | 5 | 评估逻辑，未 port |
| core/output | 4 | **1-4-2 BLOCKED**（逃生舱禁令） |
| core/bench | 3 | 基准，未 port |
| core/code-intel/sinks | 3 | 代码图 sink，未 port |
| core/storage | 3 | 存储，未 port |
| commands/migrations | 2 | 迁移脚本，未 port |
| core/audit | 2 | 审计，未 port |
| core/enrichment | 2 | 富化，未 port |
| eval/code-retrieval | 2 | 评估逻辑，未 port |
| core/artifact | 1 | 未 port |
| core/claw-test/runners | 1 | 测试harness，未 port |
| core/diarize | 1 | 未 port |
| core/entities | 1 | 未 port |
| core/eval | 1 | 评估，未 port |
| core/eval-shared | 1 | 评估，未 port |
| core/resolvers/builtin | 1 | resolver 内置，未 port |
| core/resolvers/builtin/x-api | 1 | resolver x-api，未 port |

## 真·未启动的核心端口（下一步候选）

1. ~~**facts (1-8)** — 已迁 Rust（完成）。~~ ~~**skillify check (1-1-1)** — 11 项审计已迁 Rust（完成），TS 孤儿 `skillify-check.ts` 已删。~~
2. ~~**search 图像 (1-7-3)**~~ — 已迁 Rust（完成 2026-08-07）：`SearchByImageOperation`（operation.rs:2240）+ CLI verb `SearchByImage` + `image_loader.rs`（SSRF，14 test）+ `search_pages_by_embedding`（libsql/postgres/InMemory）。**search 观测 (1-7-4)** — 2/4：`dedup`（search/dedup.rs）与 `explain-formatter`（explain_formatter.rs，已接 `query --explain`）已迁；`telemetry` → **G72**、`eval`（IR 指标 P@K/R@K/MRR/nDCG）→ **G73**，两者 TS 源已随 `bcafcafd` 删除，属「先删后补」缺口（纯本地 IO / 纯数学，无基建阻塞，可直接补迁）。
3. **output infra (1-4-2)** — BLOCKED（BrainWriter 撞逃生舱禁令），需先决策。
4. **eval-* 族**（eval-contradictions/takes-quality-eval/cross-modal-eval/longmemeval/code-retrieval）— 评估逻辑，可并行。

## 已验证「删 TS 执行层」里程碑完成（与地图 stale 矛盾已纠正）

- `cli.ts` / `operations.ts` / `mcp/dispatch.ts` / `mcp/tool-defs.ts` / `src/core/minions/*` 均已删且 commit。
- `bin.zbrain` → `bin/zbrain-rs.js`（JS→Rust shim）。
- 验证：`cargo build -p zbrain-cli` EXIT=0；`bun` typecheck 基线 **0 新增错误**（注意 `scripts/tsc-baseline.txt` 未排序致 gate 误报，另议修复）。
- `runCycle` 2057 行已迁 `autopilot/cycle.rs`（run_cycle + 48 phase arms，零 stub），Part12 JSON 48/48 completed。
