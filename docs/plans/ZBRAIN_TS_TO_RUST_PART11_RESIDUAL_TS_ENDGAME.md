<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-08-05 21:10:41

[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [~][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [x][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [x][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
│   ├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
│   └── [x][Y+] 1-3-3. calibration Phase 2 引擎/LLM 基建 (5 函数 + schema + cycle + op，见 1-3-3-1..7)
├── [ ][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
│   ├── [x][Y+] 1-4-1. output page validators port (citation + triple-hr 纯字符串 + link + back-link engine-read)
│   └── [!][X+] 1-4-2. output infra port + TS 删除 [BLOCKED: BrainWriter 撞逃生舱禁令 + 消费者 integrity.ts/operations.ts 未迁]
├── [x][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ├── [x][X+] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
│   ├── [x][Y+] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
│   ├── [x][Y+] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
│   ├── [x][Y+] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
│   ├── [x][Y+] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
│   └── [x][Y+] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
├── [x][X+] 1-6. 孤儿命令迁移 (审计: 83 唯一活命令 = RUST_OWNED 17 / TRIVIAL_DELETE 27 / REAL_MIGRATE 33 / PARITY_REVIEW 6)
│   ├── [x][X+] 1-6-1. 孤儿命令审计 (TS 活 dispatch ~50 vs Rust 已注册, 分类 trivial-delete / real-migrate)
│   ├── [x][Y+] 1-6-2. RUST_OWNED 壳清理 (删TS副本, 过1-6-5对等闸门: config/query/search/get-page/list-pages/sync/takes/orphans/import/reconcile-links/skillpack/schema/init/doctor)
│   ├── [x][Y+] 1-6-3. TRIVIAL_DELETE 批 [已收口: 真零依赖仅3个 cache/claw-test/report 已整删; 原审计宣称27为过度分类, 20个带test_refs命令归1-6-4, discovery/network/parse非命令+call幽灵条目已从审计剔除]
│   ├── [x][X+] 1-6-4. REAL_MIGRATE 孤儿命令批 [去重后: 移出 skillify->1-1 / eval族->1-2 / calibration->1-3 / dream->1-12 / extract·export·integrity->1-4; 真孤儿=code-intel(code-*·reindex*·edges-backfill·backfill) + memory(recall·forget) + models·providers + whoknows·brainstorm·auth·features·storage·migrate·publish·extract-conversation-facts·resolvers·check-resolvable + 20个1-6-3归入带test命令]
│   ├── [x][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
│   ├── [x] 1-6-6. skill/resolver 校验子系统全量迁 Rust (check-resolvable 全轨道): 覆盖 resolver-filenames / skill-frontmatter / skill-manifest / trigger-index(+parseResolverEntries) / check-resolvable core(checks 1-4) / repo-root / CLI / routing-eval(Check5) / filing-audit(Check6) / dry-fix(--fix) / 重接 doctor+skillify-check。非孤儿命令——是整条 skill 树校验栈，耦合 doctor/skillify-check 共享核心。
│   └── [x][Y+] 1-6-7. operations.ts 替换式迁移 (Rust OperationRegistry 为继任者): 107 op 逐一对齐, 随迁随删 TS; 覆盖审计见 docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md
├── [~][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
│   ├── [x][X+] 1-7-1. search 融合核心 port (hybrid+expansion+mode+sql-ranking+graph-signals+intent-weights+rerank+source-boost+token-budget+recency-decay+two-pass)
│   ├── [ ][X+] 1-7-2. search 语义检索 port (query-intent+llm-intent+query-cache+query-cache-gate+embedding-column)
│   ├── [ ][X+] 1-7-3. search 图像检索 port (by-image+image-loader, NET_NEW 1-6-7-11)
│   └── [ ][X+] 1-7-4. search 工具/观测 port (eval+telemetry+dedup+explain-formatter)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [~][X+] 1-11-2. minions 纯删除探查 [部分完成: worker/handler-runtime/测试已被 1-13-1-3-5 吃掉; 剩 queue.ts(活) + ~31 孤立 src, 待 1-13-1-6 删命令后清]
├── [x][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
│   └── [x][X+] 1-12-1. cycle Phase 路线图起草(独立 Part12 草案: 拆 runCycle 主循环 + 20 phase 为可执行切片)
├── [~][X+] 1-13. cutover 执行层: Rust CLI clap 层补全(映射 cli.ts 全部命令到 run_operation) + 退役 cli.ts + 删 operations.ts
│   └── [~][X+] 1-13-1. Phase C 退役 cli.ts + mcp legacy + 删 operations.ts — 计划与决策
└── [~][X+] 1-14. 残留 TS 活性审计（415 文件 live/orphan 分类 + 依赖测试面）
    └── [ ][X+] 1-14-1. 删除17个孤儿src + 收口1-13-1-3-2 (零风险，与引擎port并行)

### 当前施工：1-11-2. minions 纯删除探查 [部分完成: worker/handler-runtime/测试已被 1-13-1-3-5 吃掉; 剩 queue.ts(活) + ~31 孤立 src, 待 1-13-1-6 删命令后清]

2026-07-24 更新: 原 BLOCKED(测试 100% 耦合)已被 1-13-1-3-5 解决——该节点全量删了 4 src(worker.ts/subagent.ts/brain-allowlist.ts/tool-defs.ts) + 27 测试(整 TS minions 测试套件 + 部分更广覆盖测试)。剩余: queue.ts(活, book-mirror/jobs-watch/cycle/synthesize/embed-backfill-submit/search 引用) + ~31 孤立 minions src(shell.ts/embed.ts/supervisor.ts/rate-leases.ts 等, 无测试无活 importer)。queue.ts 必须在 1-13-1-6 删 cli.ts 命令(含 book-mirror/jobs-watch)后再删, 否则破坏活命令。故 1-11-2 现仅剩 '队列+孤立 handlers 收尾', 状态改 in_progress

**决策：**
- Q: minions 闭合簇边界与安全性? → 5文件闭合可删簇(1487行):worker.ts(848,0引用)+backoff.ts(26,仅worker引)+quiet-hours.ts(94,仅worker引)+handlers/shell.ts(350,0引用)+handlers/subagent-aggregator.ts(169,0引用)。Rust孪生全真实现(worker.rs659/backoff.rs120/quiet_hours.rs199/shell.rs184/subagent_aggregator.rs102,stub标记0)。无跨目录消费者、无桶再导出、无测试直接import。排除项:embed-backfill.ts虽0引用但Rust embed_backfill.rs是v1 skeleton(not_implemented, BudgetTracker未迁),删TS会丢真实现→保留;attachments/exit-classification/spawn-helpers被非删文件引用→unsafe保留;ai/recipes 14个无Rust孪生→B类保留;plugin-loader=G36保留。 (同1-11-1手法:零外部消费者+真Rust覆盖=安全纯删。选刀教训再验:名称孪生≠真覆盖,embed_backfill.rs实测是stub,必须查stub标记。)
- Q: minions 能否作为干净纯删除切片? → 否。初探(只扫src)误报worker/backoff/quiet-hours/shell/subagent-aggregator为零外部引用,但补扫tests/后发现minions每个文件都被测试引用(queue29/worker14/types14/shell3/embed-backfill2...),无一零引用叶子。删任一文件需连带删/重写整套minions测试套件(失去对仍TS的queue/types覆盖率),违背'干净可完成切片'。已git restore回退误删的5文件。结论:A类纯删除对minions/ai/commands均已失效(cycle=B类假A, ai recipes无Rust孪生, 命令全被wiring)。ingestion(1-11-1)是最后一个真干净切片。剩余工作=B类真迁移(schema-pack G4 / doctor G5 / cycle 1-12)。 (选刀方法论升级:import探针必须扫src+tests且用可靠grep(自写正则bug漏匹配import{X}from);A类判定=零src引用+零test引用+真Rust覆盖(非stub)三者同时满足。)
<!-- ROADMAP_SECTION_END -->
