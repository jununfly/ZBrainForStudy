<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part13-minion-handler-wiring.json` | 最后更新: 2026-08-08 17:50:34

[x][X+] 1. Part13 - minion/MCP handler 接线（接已完成 Rust cycle + CLI verbs）
├── [x][Y+] 1-1. cycle-phase minion handler 接线（delegate 到 run_cycle / 指定 phase）
│   ├── [x][Y+] 1-1-1. autopilot_cycle handler → run_cycle（完整 cycle）
│   ├── [x][Y+] 1-1-2. consolidate handler → run_cycle phase=Consolidate
│   ├── [x][Y+] 1-1-3. extract_facts handler → run_cycle phase=ExtractFacts
│   ├── [x][Y+] 1-1-4. recompute_emotional_weight handler → run_cycle phase=RecomputeEmotionalWeight
│   ├── [x][Y+] 1-1-5. resolve_symbol_edges handler → run_cycle phase=ResolveSymbolEdges
│   ├── [x][Y+] 1-1-6. synthesize handler → run_cycle phase=Synthesize
│   └── [x][Y+] 1-1-7. extract_conversation_facts handler → run_cycle phase=ConversationFactsBackfill
├── [x][Y+] 1-2. 现有 Rust CLI verb delegate handler 接线
│   ├── [x][Y+] 1-2-1. integrity handler → zbrain integrity verb
│   ├── [x][Y+] 1-2-2. reindex handler → zbrain reindex verb（Reindex::Pages 已存在）
│   └── [x][Y+] 1-2-3. sync handler → zbrain sync verb
├── [x][Y+] 1-3. 无对应 Rust verb 的 handler（命令未迁 / 死技术，需决策）
│   ├── [!][Y+] 1-3-1. lint handler — 无 Lint verb（命令未迁，需决策）
│   ├── [!][Y+] 1-3-2. lint_fix handler — 无 LintFix verb（命令未迁，需决策）
│   ├── [!][Y+] 1-3-3. extract handler — 无 Extract verb（选项 C 已删 TS 命令，需决策）
│   ├── [!][Y+] 1-3-4. integrity_auto handler — 无 IntegrityAuto verb（需决策）
│   ├── [!][Y+] 1-3-5. sync_retry_failed handler — 无 SyncRetryFailed verb（需决策）
│   └── [!][Y+] 1-3-6. repair_jsonb handler — pglite 死技术 → wontfix 候选
└── [x][Y+] 1-4. LLM/embedding-seam handler 接线（需 infra 注入）
    ├── [x][Y+] 1-4-1. embed handler → embedding client 接线
    ├── [x][Y+] 1-4-2. embed_backfill handler → embedding + BudgetMeter 接线
    └── [x][Y+] 1-4-3. contextual_reindex handler → Haiku LLM + rate-lease 接线
<!-- ROADMAP_SECTION_END -->
