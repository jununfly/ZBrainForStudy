# 残留 TS 构成盘点（TS→Rust 迁移，Phase 11 收官后）

> 生成于 2026-07-15，Phase 11（package cutover + src/core 安全删除）收官后。
> 目的：厘清剩余 TS 的真实构成，区分「只是还没删」与「真正待迁移」，为下一轮迁移选目标。
> 数据来源：`find` 量化 + Rust↔TS 覆盖交叉盘点 + `UNMIGRATED_TS_*` 硬锚点 + KNOWN-GAPS。

## 1. 总量

| 位置 | 数量 | 说明 |
|---|---|---|
| `src/**/*.ts`（非测试） | 529 | 全是源文件（src 下已无 .test.ts） |
| `tests/unit/**/*.test.ts` | 765 | 活的单测，仍经 `bun test` 运行 |
| `tests/unit/**/*.ts`（helper/fixture） | 18 | 测试辅助 |
| `tests/heavy/**/*.ts` | 2 | |
| **合计** | **~1314** | 由 `scripts/typecheck-baseline.sh` gate 守护（64 既有 error 冻结基线） |

src 源文件分布：`src/core` 412 + `src/commands` 105（约 87 命令 + 18 migrations）+ `src/eval` 7 + 其它 5。

## 2. 三类残留 TS

### A. 「只是还没删」——Rust 已有对应实现（数据面 + 自动化流水线）

Rust 侧已成型，TS 端是迁移残留，理论上可随验证逐步删除（同 Phase 11 手法）。

| TS 子系统 | Rust 对应 |
|---|---|
| `src/core/minions`(37) | `crates/zbrain-core/src/minions/`（handler/queue/tools + 28 handlers）✅ |
| `src/core/ai`(26) | `crates/zbrain-core/src/ai/`（9 文件）✅ |
| `src/core/ingestion`(9) | `ingestion.rs` + `sync/import.rs` ✅ |
| `src/core/cycle`(21) | `autopilot/cycle.rs` ✅ |
| 命令：`takes`/`search`(→query)/`facts`/`sync`/`orphans`/`embed`/`extract`/`lint`/`integrity`/`backlinks`/`reindex*`/`jobs-watch`/`report`/`recall`/`init-mode-picker`/`storage`/`migrate-engine` | Rust CLI 27 命令 + minion handlers ✅ |

Rust CLI 27 顶层命令（`crates/zbrain-cli/src/lib.rs` `enum Commands` L344）：
`init doctor config schema-sql get-page think query put-page delete-page restore-page purge-deleted-pages list-pages serve-mcp serve sync sources capture facts links takes salience orphans graph-query autopilot remote jobs agent`

### B. 「真正待迁移」——Rust 完全没有（评估与治理层）

这是迁移主战场，也是 ts_keep_seeds 钉住的 KEEP 核心。

| TS 子系统/命令 | 体量 | 硬锚点 / KNOWN-GAPS |
|---|---|---|
| `schema-pack`（32 verb Schema Cathedral） | 26 | **G4** + `UNMIGRATED_TS_SCHEMA_PACK_VERBS`（lib.rs:2990）+ 锚点测试 |
| `skillpack`(27) + `skillify` | 27+ | ts_keep_seeds 钉住 |
| eval 一族（约 20 个 `eval-*` 命令 + `src/eval` + `src/core/eval*`） | 大 | ts_keep_seeds Phase10 段钉住 |
| `eval-contradictions`(15) | 15 | Rust 无 |
| `takes-quality-eval`(10) | 10 | Rust 无 |
| `calibration`(10) | 10 | 仅 `calibration_queries.rs` DB 层 + web admin，无算法 |
| `src/core/output`(9) | 9 | Rust 无对应模块 |
| doctor 11 项健康检查 | — | **G5** + `UNMIGRATED_TS_DOCTOR_CHECKS`（lib.rs:86）+ 锚点测试 |
| 其它命令：`whoknows`/`routing-eval`/`notability-eval`/`frontmatter*`/`publish`/`bench-publish`/`brainstorm`/`dream`/`models`/`providers`/`mounts`/`integrations`/`auth`/`upgrade`/`check-update`/`book-mirror`/`founder-scorecard`/`friction`/`anomalies`/`code-callees·callers·def·refs`/`resolvers`/`transcripts`/`pages`/`files`/`backfill`/`edges-backfill`/`apply-migrations`/`reconcile-links`/`repair-jsonb`/`features`/`lsd`/`ze-switch`/`claw-test` | — | Rust 无 |

### C. 「部分覆盖」——有入口或 DB 层，无完整 core 模块

迁移时需补齐 core 逻辑，非从零。

| TS 子系统 | Rust 现状 |
|---|---|
| `src/core/search`(23) | 无 `search/` 模块，检索散在 `engine.rs`/`libsql.rs`/`rerank_*` |
| `src/core/facts`(13) | 仅 `facts_fence.rs` + facts CLI/minion |
| `src/core/think`(7) | 仅 CLI `think` 入口，无独立 core 模块 |
| `src/core/calibration`(10) | 仅 DB 层（同 B） |

### D. 「有意保留」——KNOWN-GAPS 登记，暂不迁

| Gap | 内容 |
|---|---|
| G36 | `src/core/minions/plugin-loader.ts`（`ZBRAIN_PLUGIN_PATH` subagent 插件发现）+ test，Rust 无等价 |

## 3. crates 结构

- **zbrain-cli**：CLI 入口，clap 命令树、doctor 报告
- **zbrain-core**：核心库（引擎/DB libsql+postgres、ai、minions、sync、autopilot、ingestion、embedding）
- **zbrain-web**：HTTP/admin SPA、auth、mcp、webhook
- **zbrain-worker**：后台 job worker、supervisor、rss
- **zbrain-mcp**：MCP stdio server
- **zbrain-chunking**：文本分块
- **zbrain-svg**：SVG 渲染

## 4. 下一轮迁移选目标建议

按「体量大 + 有硬锚点追踪 + 内聚边界清晰」优先：

1. **schema-pack（G4，32 verb）** — 体量大、有 `UNMIGRATED_TS_SCHEMA_PACK_VERBS` 硬锚点 + `schema-sql` 已腾名，边界清晰，迁移完成即可从常量移除 + 删 TS。
2. **skillpack / skillify** — 体量大（27+），自成子系统。
3. **eval 一族** — 数量多但同质（约 20 个 eval 命令 + core/eval），适合整体作为一个独立 Phase 推进。
4. **doctor 11 检查（G5）** — 有 `UNMIGRATED_TS_DOCTOR_CHECKS` 锚点，逐项迁出（`reranker_health` 已示范迁出模式）。
5. 部分覆盖类（search/facts/think）可穿插补齐 core 逻辑。

> 注意：B/C 类都是 ts_keep_seeds 的 KEEP 核心，删除前必须先在 Rust 补齐能力（区别于 Phase 11「已迁移才删」）。每迁一块 → 补 Rust + 删对应 TS + `typecheck:update-baseline` 收缩基线（64 既有 error 应只减不增）。
