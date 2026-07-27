
<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part11 — 残留 TS 收尾 (综合容器)

### 树形视图 (depth=2)

```
[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [~][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [x][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [~][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
│   ├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
│   └── [~][Y+] 1-3-3. calibration Phase 2 引擎/LLM 基建 (5 函数 + schema + cycle + op，见 1-3-3-1..7)
├── [~][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
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
├── [x][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [x][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [~][X+] 1-11-2. minions 纯删除探查 [部分完成: worker/handler-runtime/测试已被 1-13-1-3-5 吃掉; 剩 queue.ts(活) + ~31 孤立 src, 待 1-13-1-6 删命令后清]
├── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
│   └── [ ][X+] 1-12-1. cycle Phase 路线图起草(独立 Part12 草案: 拆 runCycle 主循环 + 20 phase 为可执行切片)
└── [~][X+] 1-13. cutover 执行层: Rust CLI clap 层补全(映射 cli.ts 全部命令到 run_operation) + 退役 cli.ts + 删 operations.ts
    └── [~][X+] 1-13-1. Phase C 退役 cli.ts + mcp legacy + 删 operations.ts — 计划与决策
```

### 🔨 当前施工: 1-3-3. calibration Phase 2 引擎/LLM 基建 (5 函数 + schema + cycle + op，见 1-3-3-1..7)
**Status:** `in_progress` | **Mode:** `exploit`

已 grill 完成，8 分叉全定。序列：先 schema(1-3-3-1) 再函数。execute_raw 禁止(走 bespoke typed CalibrationQueries 方法)；LLM 走 trait DI(VoiceGenerator/VoiceJudge via instantiate_chat)；queryAcrossBrains 走 DI mountResolver + 全 4 规则逻辑；undoWave gstack scrub 记 KNOWN-GAP；takes_calibration op 实际只依赖 getCalibrationCurve(已存在但语义过时)，仍按用户决按全做。三道门 gate 工作流：lib test / cli build / mcp build。

**决策记录:**
- Q: 1-3-3 第一刀先动哪块?
  A: 先补 calibration schema 迁移 (独立 precursor 1-3-3-1)
  > 5 张 calibration 表在 Rust migrations 全不存在；3/5 函数 + takes_calibration op 硬前置；execute_raw 架构决策延后到真要写时。
- Q: calibration schema 保真度?
  A: 全量 parity，去不可满足 FK
  > take_nudge_log.proposal_id 去 REFERENCES take_proposals(Rust 无该表)改 nullable BIGINT + 保留 XOR CHECK；take_domain_assignments 先定位 TS 真实定义；版本 0023 双后端。
- Q: 3 函数依赖 raw SQL，BrainEngine 故意不放 execute_raw，怎么给 DB 访问?
  A: bespoke typed 方法挂 CalibrationQueries trait
  > 尊重 no-escape-hatch 设计；三后端各写 SQL；InMemory 读聚合走内存迭代、admin 写 Unsupported(对齐 minion queue/get_scorecard 模式)。
- Q: gateVoice/runAbTrial 的 LLM 依赖注入怎么接?
  A: trait-based DI (VoiceGenerator+VoiceJudge async trait，生产 impl 走 instantiate_chat)
  > 复用已有 parse_judge_output；对齐 Rust 既有 DI 约定(NightlyProbeDeps)；测试注入 stub。
- Q: queryAcrossBrains 的 mount 依赖怎么处理?
  A: DI mountResolver trait + 全 4 规则逻辑 port
  > mountResolver 返回 Vec<(brain_id,engine)>；canReadMountsForCtx + attributionSuffix 全 port；生产 resolver 接 Rust mounts 配置(mounts.rs)为后续小节点。
- Q: undoWave 的 gstack-learnings-prune 外部二进制 scrub?
  A: 本刀跳过，记 KNOWN-GAP
  > DB 反转核心(Step 1-3)先 port；外部二进制 best-effort 非阻断项，后续节点补。
- Q: takes_calibration op 节点边界?
  A: 全做 (schema+5函数+cycle+op)
  > grill 发现 op 实际调 getCalibrationCurve(已存在于 Rust 但语义过时/签名不全)，不依赖 5 函数；用户决仍按全做，op 切片额外对齐 get_calibration_curve。

**子节点:**
- [x] 1-3-3-1. calibration schema 迁移 (5 表 calibration_profiles/take_nudge_log/think_ab_results/take_grade_cache/take_domain_assignments; 版本 0023 双后端 pg+sqlite; 全量 parity 去不可满足 FK; take_domain_assignments 先定位 TS 真实定义)
- [x] 1-3-3-2. undoWave (bespoke typed 写 revert_wave_resolutions/delete_calibration_profiles_for_wave/purge_nudge_log_for_wave + take_grade_cache unapply; gstack scrub 记 KNOWN-GAP 跳过)
- [x] 1-3-3-3. aggregateDomainScorecards (CalibrationQueries bespoke typed 方法; 4 aggregator 变体 scalar_brier/weighted_brier/count_based/cluster_summary; 三后端 SQL; InMemory 内存迭代/Unsupported)
- [ ] 1-3-3-4. queryAcrossBrains (DI mountResolver trait 返回 Vec<(brain_id,engine)>; 全 4 规则逻辑 local-first/mount-fallback/published/SUBAGENT 禁; canReadMountsForCtx + attributionSuffix; 生产 resolver 接 mounts 配置为后续小节点)
- [x] 1-3-3-5. takes_calibration op (对齐 get_calibration_curve 到 canonical weight/resolved_quality + bucketSize + allowList + holder 可选; 新增 op handler; 解锁 1-6-7-16)
- [ ] 1-3-3-6. gateVoice + runAbTrial (trait DI VoiceGenerator+VoiceJudge via instantiate_chat 复用 parse_judge_output; runAbTrial 编排 + think_ab_results INSERT, thinkRunner DI; Rust 无 runThink 故生产 think 接线后续)
- [ ] 1-3-3-7. calibration-profile 循环 runPhaseCalibrationProfile (port; 串联 getScorecard + gateVoice + biasTagsGenerator + aggregateDomainScorecards + 写 calibration_profiles; cold-brain<5 skip; budget gate)
<!-- ⚠️ ROADMAP_SECTION_END -->

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-24 11:52:40

[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [~][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [x][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [~][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
│   ├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
│   └── [~][Y+] 1-3-3. calibration Phase 2 引擎/LLM 基建 (5 函数 + schema + cycle + op，见 1-3-3-1..7)
├── [~][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
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
├── [x][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [x][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
├── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
│   └── [ ][X+] 1-12-1. cycle Phase 路线图起草(独立 Part12 草案: 拆 runCycle 主循环 + 20 phase 为可执行切片)
└── [~][X+] 1-13. cutover 执行层: Rust CLI clap 层补全(映射 cli.ts 全部命令到 run_operation) + 退役 cli.ts + 删 operations.ts
    └── [~][X+] 1-13-1. Phase C 退役 cli.ts + mcp legacy + 删 operations.ts — 计划与决策

### 当前施工：1-3-3-6. gateVoice + runAbTrial (trait DI VoiceGenerator+VoiceJudge via instantiate_chat 复用 parse_judge_output; runAbTrial 编排 + think_ab_results INSERT; Rust 无 runThink 故生产 think 接线后续) — 1-3-3-5 已 completed
<!-- ROADMAP_SECTION_END -->
