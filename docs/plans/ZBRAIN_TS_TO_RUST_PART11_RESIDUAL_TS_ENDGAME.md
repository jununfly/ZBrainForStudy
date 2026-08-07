<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-08-07 23:50:35

[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [x][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
│   └── [x][X+] 1-1-1. skillify check 子命令迁移 (skillify-check.ts 11 项审计 → Rust zbrain skillify check)
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
├── [x][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
│   ├── [x][X+] 1-7-1. search 融合核心 port (hybrid+expansion+mode+sql-ranking+graph-signals+intent-weights+rerank+source-boost+token-budget+recency-decay+two-pass)
│   ├── [x][X+] 1-7-2. search 语义检索 port (query-intent+llm-intent+query-cache+query-cache-gate+embedding-column)
│   ├── [x][X+] 1-7-3. search 图像检索 port (by-image+image-loader, NET_NEW 1-6-7-11)
│   └── [x][X+] 1-7-4. search 工具/观测 port (eval+telemetry+dedup+explain-formatter)
├── [x][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [x][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
│   ├── [x][X+] 1-9-1. think 纯逻辑模块 port (intent/sanitize/entity/cite-render/prompt/fuseRRF)
│   ├── [x][X+] 1-9-3. think 检索融合 port (gather 4 流 + rerank + renderPages/Takes)
│   ├── [x][X+] 1-9-4. think 合成编排 port (index runThink: prompt→chat→parse→citations→persist)
│   └── [x][X+] 1-9-5. think LLM 接缝对齐 (ChatProvider/Anthropic + schema citations/gaps)
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [x][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [x][X+] 1-11-2. minions 纯删除探查 [部分完成: worker/handler-runtime/测试已被 1-13-1-3-5 吃掉; 剩 queue.ts(活) + ~31 孤立 src, 待 1-13-1-6 删命令后清]
├── [x][X+] 1-12. cycle 大迁移 (runCycle 2057行→Rust autopilot/cycle.rs: run_cycle + 48 phase arms, 详见 Part12)
│   └── [x][X+] 1-12-1. cycle Phase 路线图起草(独立 Part12 草案: 拆 runCycle 主循环 + 20 phase 为可执行切片)
├── [x][X+] 1-13. cutover 执行层: Rust CLI clap 层补全(映射 cli.ts 全部命令到 run_operation) + 退役 cli.ts + 删 operations.ts
│   └── [x][X+] 1-13-1. Phase C 退役 cli.ts + mcp legacy + 删 operations.ts — 计划与决策
└── [x][X+] 1-14. 残留 TS 活性审计（415 文件 live/orphan 分类 + 依赖测试面）
    └── [ ][X+] 1-14-1. 删除17个孤儿src + 收口1-13-1-3-2 (零风险，与引擎port并行)

### 当前施工：1. Part11 — 残留 TS 收尾 (综合容器)

**决策：**
- Q: Part11 边界与内部结构? → 综合容器，按子系统切片。收容 B+C+G38尾+A类删除，排除 D类(G36 有意保留)。每个待迁子系统一个 top-level node，G38尾与 A类删除各单列。 (用户 2026-07-15 决策)
- Q: Part11 纠缠破局策略? → 每切片自带解缠：沿用 schema_pack 模式（先迁 Rust 替代 → 消除对本子系统 TS 的 import → 再删 TS）。G38 尾(1-10)不独立硬关，随消费方切片(import/sync/whoknows/artifact/calibration/eval-schema-authoring 等)各自端口而自然消解。 (用户 2026-07-15 决策)
- Q: 下一刀选哪个子系统 (grill 2026-07-16)? → 1-4 output 模块 (纠缠最低(全仓仅1个外部 importer integrity.ts)；可复用 calibration 分层打法(纯子集+engine-read+infra Phase2)；不碰 LLM/execute_raw。更正: 1-6 孤儿命令节点非过时——6 命令 cli.ts 全有 live dispatch, Rust 全无, 是真实重迁移工作。)
- Q: 下一阶段主攻方向? → 推 grind 到终局(1-6-7 各切片→1-6-7-9 删 operations.ts + 1-3-3 calibration 收尾),并并行起草 1-12 cycle 独立 Part12 路线图备用;grind 不暂停 (用户选推荐项(Q1)。grind 关键路径最高杠杆=删3855行 operations.ts 巨石+解锁1-4-2 output;cycle 维持执行延后但 now 起并行起草独立 Part12 路线图零摩擦备用。1-4-2/1-10/1-11 删除尾均挂在 grind 这把刀上。)
- Q: operations.ts 删库(cutover)路径? → 先补 Rust CLI clap 层(把 cli.ts 全部命令映射到 Rust run_operation),clap 覆盖完整后再退役 cli.ts + 删 operations.ts。不采用'只补 registry 保留 TS 双轨'或'加通用 op 转发命令'的捷径。 (用户选推荐项(Q-cutover)。事实底座:生产入口=Rust zbrain binary(bin wrapper transparent,零TS fallback);Rust CLI 用 OperationRegistry+register_all+run_operation 但只暴露少数 clap 子命令;TS cli.ts 通过 operations.ts 暴露几十命令且主流命令仍走TS dispatch;删operations.ts 会断产品。故 cutover 前置=Rust CLI clap 层全量覆盖 cli.ts 命令。)

**当前子树：**
├── [x][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
│   ... 1 more child nodes; run tree 1-1 --depth 2 for full view
├── [x][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [x][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ... 3 more child nodes; run tree 1-3 --depth 2 for full view
├── [ ][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
│   ... 2 more child nodes; run tree 1-4 --depth 2 for full view
├── [x][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ... 6 more child nodes; run tree 1-5 --depth 2 for full view
├── [x][X+] 1-6. 孤儿命令迁移 (审计: 83 唯一活命令 = RUST_OWNED 17 / TRIVIAL_DELETE 27 / REAL_MIGRATE 33 / PARITY_REVIEW 6)
│   ... 7 more child nodes; run tree 1-6 --depth 2 for full view
├── [x][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
│   ... 4 more child nodes; run tree 1-7 --depth 2 for full view
├── [x][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [x][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
│   ... 4 more child nodes; run tree 1-9 --depth 2 for full view
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [x][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ... 2 more child nodes; run tree 1-11 --depth 2 for full view
├── [x][X+] 1-12. cycle 大迁移 (runCycle 2057行→Rust autopilot/cycle.rs: run_cycle + 48 phase arms, 详见 Part12)
│   ... 1 more child nodes; run tree 1-12 --depth 2 for full view
├── [x][X+] 1-13. cutover 执行层: Rust CLI clap 层补全(映射 cli.ts 全部命令到 run_operation) + 退役 cli.ts + 删 operations.ts
│   ... 1 more child nodes; run tree 1-13 --depth 2 for full view
└── [x][X+] 1-14. 残留 TS 活性审计（415 文件 live/orphan 分类 + 依赖测试面）
    ... 1 more child nodes; run tree 1-14 --depth 2 for full view
<!-- ROADMAP_SECTION_END -->
