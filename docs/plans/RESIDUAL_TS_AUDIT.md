# 残留 TS 活性审计（1-14 交付物）

日期：2026-08-06 ｜ 方法：目录级 Rust 对应模块存在性检查
（`crates/zbrain-core/src/<mod>` kebab↔snake 兼容；core 根级文件查 `<snake>.rs`）。

说明：本审计按**目录**判定「该模块是否已有 Rust 对应实现」。MIGRATED = Rust 拥有
逻辑、TS 仅胶水/类型/入口；UNMIGRATED = 无 1:1 Rust 模块，属真前沿（需 port 或验证后可删）。

## 总体

- 扫描 `src/**/*.ts` 共 **414** 个文件。
- 已 MIGRATED（Rust 接管逻辑）：**128** 个（分布在已迁模块）。
- 仍 UNMIGRATED（真前沿）：**286** 个。

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

> 注：`core/skillpack` 已迁但 `core/skillify`(2) 仍 UNMIGRATED → 1-1 仅 skillpack 部分完成。
> `core/code-intel` 已迁但 `core/code-intel/sinks`(3) 仍 UNMIGRATED。

## 真前沿（UNMIGRATED，按文件数）

| 模块 | 文件数 | 性质 / 对应 roadmap |
|---|---|---|
| core (根级混合胶水+小helper) | 105 | 多 shell out 到 Rust operations；清理/验证后可删 |
| commands | 70 | 入口/视图，多经 bin/zbrain-rs.js → Rust；验证后可删 |
| core/ai/recipes | 18 | TS recipes，未 port |
| core/eval-contradictions | 15 | 评估逻辑，未 port（G66 提及 calibration-join） |
| core/facts | 11 | **1-8** 未启动 |
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
| core/skillify | 2 | 1-1 余下 |
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

1. **facts (1-8)** — 11 文件，brain 中枢，多处被依赖。最高杠杆核心端口。
2. **search 图像 (1-7-3) / 观测 (1-7-4)** — NET_NEW，中工作量。
3. **output infra (1-4-2)** — BLOCKED（BrainWriter 撞逃生舱禁令），需先决策。
4. **eval-* 族**（eval-contradictions/takes-quality-eval/cross-modal-eval/longmemeval/code-retrieval）— 评估逻辑，可并行。

## 已验证「删 TS 执行层」里程碑完成（与地图 stale 矛盾已纠正）

- `cli.ts` / `operations.ts` / `mcp/dispatch.ts` / `mcp/tool-defs.ts` / `src/core/minions/*` 均已删且 commit。
- `bin.zbrain` → `bin/zbrain-rs.js`（JS→Rust shim）。
- 验证：`cargo build -p zbrain-cli` EXIT=0；`bun` typecheck 基线 **0 新增错误**（注意 `scripts/tsc-baseline.txt` 未排序致 gate 误报，另议修复）。
- `runCycle` 2057 行已迁 `autopilot/cycle.rs`（run_cycle + 48 phase arms，零 stub），Part12 JSON 48/48 completed。
