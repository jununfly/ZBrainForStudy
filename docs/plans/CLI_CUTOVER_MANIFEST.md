# CLI Cutover Manifest — operations.ts → 删库

> 路线图: Part11 残 TS 收尾 (`zbrain-ts-to-rust-part11-residual-ts-endgame.json`)
> 节点: `1-13 cutover 执行层`
> 决策: Q-cutover = 先补 Rust CLI clap 层再退役 cli.ts + 删 operations.ts
> 日期: 2026-07-23

## 1. 架构事实(探查钉死)

1. **生产 CLI 入口 = Rust `zbrain` binary**。`package.json` 的 `bin.zbrain` → `bin/zbrain-rs.js`,这是 **100% transparent pass-through**(`spawnSync` Rust binary,零 TS fallback)。
2. **Rust CLI 用 `OperationRegistry` + `register_all` + `run_operation(op_name)`** 派发 op(`crates/zbrain-cli/src/lib.rs`)。每个 clap 子命令内部调 `run_operation("think"/"get_page"/...)`。
3. **Rust CLI 命令树已覆盖 ~90 命令**(106 提取去掉内部 enum 误抓),含 `Commands` 主枚举 + Jobs/Sources/Links/Takes/Facts/Remote/Agent/Schema/Mounts/Skillpack 子枚举。
4. **TS `cli.ts` 是 legacy 入口**,但仍被 `check-resolvable` 脚本 + TS 测试引用。它两路 dispatch:
   - 第 113 行 `CLI_ONLY.has(command)` → `handleCliOnly` 自有 handler(46 命令,绕过 operations.ts)
   - 第 119 行 `cliOps.get(command)` → 由 `operations.ts` 数组构建,第 215 行 `op.handler(ctx, params)` 实时 dispatch TS op
5. **`operations.ts` 被 27 处 TS 文件引用**(见 §3),不是孤立文件。

## 2. 真实范围(为什么删 operations.ts ≠ 删几个 const)

删 `operations.ts` 的硬前置 = **所有运行时引用方不再 import 它**:
- `cli.ts` 第 17 行 `import { operations }` + 第 30 行 `for (const op of operations)` 构建 `cliOps` Map
- `mcp/dispatch.ts`、`mcp/tool-defs.ts`(legacy,已被 Rust MCP server 取代)
- `commands/book-mirror.ts`(find put_page op)、`commands/tools-json.ts`(map 所有 op 生成 tools JSON)
- 9 处 `import type { OperationContext }`(cycle/*、calibration、facts/meta-hook、whoknows)

这些 import 方大多属于 **TS legacy 层**(mcp/dispatch.ts 注释明写 "Delete in 1-6")。所以删 `operations.ts` = **TS legacy 层整体退役的一部分**,与 Part12 cycle 迁移耦合,不是孤立动作。

## 3. 依赖图(27 处 import operations.ts)

**运行时依赖(必须清理/迁移):**
- `src/cli.ts` — `import { operations, OperationError }`
- `src/mcp/dispatch.ts` — legacy dispatch(应删)
- `src/mcp/tool-defs.ts` — legacy tools(应删或改 Rust MCP)
- `src/commands/book-mirror.ts` — `operations.find(op => op.name === 'put_page')`
- `src/commands/tools-json.ts` — `operations.map(op => ...)`

**类型导入(只依赖 `OperationContext` 类型):**
- `src/commands/calibration.ts`、`src/core/cycle/base-phase.ts`、`src/core/cycle/calibration-profile.ts`、`src/core/cycle/grade-takes.ts`、`src/core/cycle/propose-takes.ts`、`src/core/facts/meta-hook.ts`、`src/commands/whoknows.ts`

**注释/字符串引用(无害,删文件后可批量清):**
- `cli.ts:730/731/734/1132`、`content-sanity.ts`、`destructive-guard.ts`、`doctor-remote.ts`、`embedding.ts`、`engine.ts`、`facts/backstop.ts`、`facts/eligibility.ts`、`cycle/patterns.ts`、`synthesize.ts`、`whoknows.ts`

## 4. 阶段划分

### 阶段 A — Rust registry 100% 完整(零 TS 风险,当前可做)
把 `operations.ts` 剩余 ~13 个未迁 op 的 Rust `TypedOperation` 包装 + 注册写完。**TS 侧不动**,Rust registry 完整度从 ~70/73 → 73/73。
- 引擎方法已就绪:code-intel ×7(`get_callers_of`/`get_callees_of`/`find_code_def`/`find_code_refs`/`recursive_walk`/`disambiguate_symbol` 三后端全覆盖)、run_doctor、find_orphans
- 零 Rust 引擎(需补或决策):`recall`(facts 引擎可迁)、`whoami`(纯 ctx 检查极简)、`find_trajectory`(Rust 零引擎方法 → 需补引擎或决策)

### 阶段 B — Rust CLI clap 层补全(覆盖 TS 46 operations 命令)
给 `operations.ts` 的 46 个 `cliHints.name` 命令建对应 Rust clap 子命令(内部 `run_operation`)。当前 Rust 已覆盖大部分,**缺失约 18 个**:
`code_blast, code_callees, code_callers, code_def, code_flow, code_refs, code_traversal_cache_clear, find-contradictions, find-trajectory, history, tag, tags, timeline, timeline-add, transcripts, revert, search-by-image, whoami`

### 阶段 C — 退役 TS legacy 层 + 删 operations.ts
1. `cli.ts` 移除 `import { operations }` + `cliOps` 构建 + `cliOps.get()` 派发路径
2. 删除 `mcp/dispatch.ts`、`mcp/tool-defs.ts`(legacy,已被 Rust MCP server 取代)
3. `book-mirror.ts`/`tools-json.ts` 改为不依赖 operations 数组(Rust MCP server 已提供 tools)
4. 9 处 `OperationContext` 类型导入改为从 Rust 绑定类型或保留 types-only 模块
5. **删除 `operations.ts`** + 缩 typecheck 基线

### 阶段 D — CLI_ONLY 命令迁 Rust(更大,独立于 C)
46 个 CLI_ONLY 命令(dream/extract/import/export/eval/sync 等)走 `handleCliOnly` 复杂 handler,部分引擎在 TS。需 Rust 侧新实现。其中 `dream→runCycle` 归 Part12 cycle 迁移;`sync` 已在 Rust(`zbrain sync`);其余独立评估。

## 5. 执行顺序(用户已烤定: A→B→C, D 独立后续)

```
A: 补 ~13 op 到 Rust registry (TS 不动)  ← 当前起点,零风险
B: Rust CLI clap 补全 ~18 缺失命令
C: 退役 cli.ts + mcp legacy + 删 operations.ts
D: CLI_ONLY 命令迁 Rust (Part12+ 独立)
```

## 6. 决策记录

- Q-cutover: 选「先补 Rust CLI clap 层再退役」(推荐)。理由:一次性干净,避免 TS/RS 双轨长期并存。不采用「只补 registry 保留 TS 双轨」或「加通用 op 转发命令」捷径。
- 关键认知修正:删 operations.ts 不是删几个 const,而是 TS legacy 层整体退役(27 处引用),故拆 A→B→C 三阶段,阶段 A 独立零风险可立即做。

## 7. 风险

- 阶段 C 若 Rust 命令覆盖不全(阶段 B 漏做),删 cli.ts 会让命令从产品消失(bin wrapper 不 fallback TS)。故 B 必须在 C 前 100% 完成。
- `find_trajectory` 在 Rust 零引擎方法,阶段 A 需先补引擎方法或显式决策延后(若延后则阶段 B/C 也需对应处理)。
- 阶段 D 与 Part12 cycle 迁移重叠(dream),需协调避免重复劳动。

## 8. 执行进度(2026-07-23 更新)

### 阶段 A — Rust registry 收口

| op | 状态 | 说明 |
|----|------|------|
| code_callers / code_callees / code_def / code_refs | ✅ 已迁 | 批1,引擎方法三后端已就绪 |
| find_trajectory | ✅ 已迁 | 卡点已破:补 trait+libsql+postgres 三后端 + `trajectory_stats.rs` 数学 + op;并修复 `insert_fact` 漏写 5 个 claim 列的真缺口 |
| whoami | ✅ 已迁 | 纯 ctx 信任边界,fail-closed remote 无 auth → unknown_transport |
| code_blast / code_flow | ✅ 已迁 | 包 `recursive_walk`(libsql/postgres 已实装);Rust 无 traversal cache,直调 drop TS 缓存层 |
| code_traversal_cache_clear | ✅ 已迁 | Rust recursive_walk 无状态 → `deleted:0`;localOnly + admin |
| find_orphans | ✅ 已迁(改名) | 早已是 `find_orphan_pages` op(operation.rs:9812 注册),无需新增 |
| run_doctor | ➡️ 映射 CLI | 非 registry op,走既有 `zbrain doctor` Rust CLI 命令 |
| **recall** | ✅ 已迁 | 新建 4 个事实引擎方法(`list_facts_by_session/list_facts_since/list_supersessions/count_unconsolidated_facts`);`list_facts_by_entity` 已存在复用；`resolve_entity_slug` 不移植(登记为 G53 已知缺口,entity 原样用 slug)。op 包装 + 8 单测全分支覆盖。三后端(InMemory/libsql/postgres)全部实装。 |

### 结论

**阶段 A — **Rust registry 100% 完整(73/73 ops) ✅ 完成。所有 operations.ts 列出的 op 均已在 Rust registry 注册,三后端引擎方法全部就位,测试覆盖完成。接下来进入阶段 B — Rust CLI clap 补全 18 个缺失命令。

### 备注

- `CliHints` struct 无 `hidden` 字段,Rust 侧无法表达 TS `cliHints.hidden:true`(code_blast/code_flow/code_traversal_cache_clear/部分 op)。阶段 B(clap 补全)时再补 hidden 语义,不影响阶段 A registry 完整性。
- 阶段 A 全程 TS 侧未动(不删 TS const),符合"零风险、TS 不动"原则。阶段 C 删 operations.ts 时再统一处理。
