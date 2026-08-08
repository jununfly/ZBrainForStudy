# Part13 — minion/MCP handler 接线（接已完成 Rust cycle + CLI verbs）

路线图由 `roadmap_cli.py` 渲染，请勿手动编辑本文件下方的路线图 section。

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part13-minion-handler-wiring.json` | 最后更新: 2026-08-08 10:05:00

[~][X+] 1. Part13 - minion/MCP handler 接线（接已完成 Rust cycle + CLI verbs）
├── [x][Y+] 1-1. cycle-phase minion handler 接线（delegate 到 run_cycle / 指定 phase）
│   ├── [x][Y+] 1-1-1. autopilot_cycle handler → run_cycle（完整 cycle）
│   ├── [x][Y+] 1-1-2. consolidate handler → run_cycle phase=Consolidate
│   ├── [x][Y+] 1-1-3. extract_facts handler → run_cycle phase=ExtractFacts
│   ├── [x][Y+] 1-1-4. recompute_emotional_weight handler → run_cycle phase=RecomputeEmotionalWeight
│   ├── [x][Y+] 1-1-5. resolve_symbol_edges handler → run_cycle phase=ResolveSymbolEdges
│   ├── [x][Y+] 1-1-6. synthesize handler → run_cycle phase=Synthesize
│   └── [x][Y+] 1-1-7. extract_conversation_facts handler → run_cycle phase=ConversationFactsBackfill
├── [ ][Y+] 1-2. 现有 Rust CLI verb delegate handler 接线
│   ├── [x][Y+] 1-2-1. integrity handler → zbrain integrity verb
│   ├── [ ][Y+] 1-2-2. reindex handler → zbrain reindex verb（Reindex::Pages 已存在）
│   └── [x][Y+] 1-2-3. sync handler → zbrain sync verb
├── [ ][Y+] 1-3. 无对应 Rust verb 的 handler（命令未迁 / 死技术，需决策）
│   ├── [ ][Y+] 1-3-1. lint handler — 无 Lint verb（命令未迁，需决策）
│   ├── [ ][Y+] 1-3-2. lint_fix handler — 无 LintFix verb（命令未迁，需决策）
│   ├── [ ][Y+] 1-3-3. extract handler — 无 Extract verb（选项 C 已删 TS 命令，需决策）
│   ├── [ ][Y+] 1-3-4. integrity_auto handler — 无 IntegrityAuto verb（需决策）
│   ├── [ ][Y+] 1-3-5. sync_retry_failed handler — 无 SyncRetryFailed verb（需决策）
│   └── [ ][Y+] 1-3-6. repair_jsonb handler — pglite 死技术 → wontfix 候选
└── [ ][Y+] 1-4. LLM/embedding-seam handler 接线（需 infra 注入）
    ├── [ ][Y+] 1-4-1. embed handler → embedding client 接线
    ├── [ ][Y+] 1-4-2. embed_backfill handler → embedding + BudgetMeter 接线
    └── [ ][Y+] 1-4-3. contextual_reindex handler → Haiku LLM + rate-lease 接线

### 当前施工：1. Part13 - minion/MCP handler 接线（接已完成 Rust cycle + CLI verbs）

**决策：**
- Q: Part13 范围如何界定？minions/handlers 有 19 个 not_implemented 桩，分几类、先做什么？ → 按可接线性分四簇：cycle-phase(7,经 run_cycle)、现有 verb(3,integrity/reindex/sync)、无 verb(6,命令未迁/死技术)、LLM/embedding-seam(3,需 infra)。先机械簇(1-1+1-2)后阻塞簇(1-3+1-4)。 (核查依据：CLI verbs 仅 Integrity/Reindex/Sync 存在；extract/integrity_auto/lint/lint_fix/repair_jsonb/sync_retry_failed 无对应 Rust verb（选项 C 删除 TS 命令后无替代，部分属 KNOWN-GAPS G74/G78；repair_jsonb=pglite 死技术）。cycle-phase 七桩全部可经 run_cycle(支持 phase 过滤) 机械接线。LLM/embedding 三桩需 embedding client / ChatProvider 注入（G30/G41/G60 相关）。backlinks.rs 已实现为 delegate 模板：读 ctx.data → 调引擎 op → 返回 JSON。)

**当前子树：**
├── [x][Y+] 1-1. cycle-phase minion handler 接线（delegate 到 run_cycle / 指定 phase）
│   ... 7 more child nodes; run tree 1-1 --depth 2 for full view
├── [ ][Y+] 1-2. 现有 Rust CLI verb delegate handler 接线
│   ... 3 more child nodes; run tree 1-2 --depth 2 for full view
├── [ ][Y+] 1-3. 无对应 Rust verb 的 handler（命令未迁 / 死技术，需决策）
│   ... 6 more child nodes; run tree 1-3 --depth 2 for full view
└── [ ][Y+] 1-4. LLM/embedding-seam handler 接线（需 infra 注入）
    ... 3 more child nodes; run tree 1-4 --depth 2 for full view
<!-- ROADMAP_SECTION_END -->
