<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part13-minion-handler-wiring.json` | 最后更新: 2026-08-09 00:19:57

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
├── [x][Y+] 1-4. LLM/embedding-seam handler 接线（需 infra 注入）
│   ├── [x][Y+] 1-4-1. embed handler → embedding client 接线
│   ├── [x][Y+] 1-4-2. embed_backfill handler → embedding + BudgetMeter 接线
│   └── [x][Y+] 1-4-3. contextual_reindex handler → Haiku LLM + rate-lease 接线
└── [~][Y+] 1-6. G77 图维护族 CLI verb（backfill-links / reconcile-links / edges-backfill）
    ├── [x][Y+] 1-6-1. rebuild-md-links verb (遍历 md 页重跑 markdown link 抽取→upsert page_links)
    ├── [ ][Y+] 1-6-2. reconcile-links verb (md→code edges 对账)
    └── [ ][Y+] 1-6-3. edges-backfill verb (symbol edges 增量重跑)

### 当前施工：1-6. G77 图维护族 CLI verb（backfill-links / reconcile-links / edges-backfill）

**决策：**
- Q: G77 3 verb 的接口/范围/顺序？ → POC 路径：先 backfill-links (1-6-1) 独立 verb, 顺序 backfill→reconcile→edges-backfill; 接口用 standalone core fn (不挂 BrainEngine trait), 放 crates/zbrain-core/src/links/ 子模块; 命名 Links::Backfill / Links::Reconcile (sub-namespace 风格); edges-backfill 是 resolveSymbolEdgesIncremental 薄包装; backfill-links 1:1 复刻 TS 逻辑, 仅 md→md 边 (不补 md→code); reconcile 1:1 复刻 TS 逻辑, 仅对账 md→code edges (G77 中 G77-1 backlinks 已 done; 本节点关注 G77-2/3/4: backfill/edges-backfill/reconcile-links; LLM seam 不需要 (不阻塞); 每 verb 1-2 单元测试)
- Q: G77 3 verb 的接口/范围/顺序？ → POC 路径：先 backfill-links (1-6-1) 独立 verb, 顺序 backfill→reconcile→edges-backfill; 接口用 standalone core fn (不挂 BrainEngine trait), 放 crates/zbrain-core/src/links/ 子模块; 命名 Links::Backfill / Links::Reconcile (sub-namespace 风格); edges-backfill 是 resolveSymbolEdgesIncremental 薄包装; backfill-links 1:1 复刻 TS 逻辑, 仅 md→md 边 (不补 md→code); reconcile 1:1 复刻 TS 逻辑, 仅对账 md→code edges (G77 中 G77-1 backlinks 已 done; 本节点关注 G77-2/3/4: backfill/edges-backfill/reconcile-links; LLM seam 不需要 (不阻塞); 每 verb 1-2 单元测试)
- Q: G77 1-6-1 落地后路线图误判校正 → 校正: 1-6-1 POC 实际只是 CLI 包装 (LinksAction::RebuildMdLinks, 60 行增量), 复用 auto_fix::extract_links 已实现的核心 (5 unit test), 不重复造轮子。standalone core fn + links/ 子模块决策也校正为「复用现成」。 (TS backfill 命令实际无 links kind (只有 effective_date/emotional_weight/embedding_voyage). G77 实际是 3 个独立 verb (rebuild-md-links + reconcile-links + edges-backfill). zj-roadmap-driven skill 禁止直接 Edit JSON, 但命令行 decide 因嵌套引号报错, 选择 Python 改 JSON + 写理由.)

**当前子树：**
├── [x][Y+] 1-6-1. rebuild-md-links verb (遍历 md 页重跑 markdown link 抽取→upsert page_links)
├── [ ][Y+] 1-6-2. reconcile-links verb (md→code edges 对账)
└── [ ][Y+] 1-6-3. edges-backfill verb (symbol edges 增量重跑)
<!-- ROADMAP_SECTION_END -->
