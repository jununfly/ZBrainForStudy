<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part11 — 残留 TS 收尾 (综合容器)

### 树形视图 (depth=2)

```
[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [ ][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [ ][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [~][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
│   ├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
│   └── [!][X+] 1-3-3. calibration Phase 2 engine/LLM 支撑（queryAcrossBrains/aggregateDomainScorecards/undoWave/gateVoice/runAbTrial）
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
├── [~][X+] 1-6. 孤儿命令迁移 (审计: 83 唯一活命令 = RUST_OWNED 17 / TRIVIAL_DELETE 27 / REAL_MIGRATE 33 / PARITY_REVIEW 6)
│   ├── [x][X+] 1-6-1. 孤儿命令审计 (TS 活 dispatch ~50 vs Rust 已注册, 分类 trivial-delete / real-migrate)
│   ├── [x][Y+] 1-6-2. RUST_OWNED 壳清理 (删TS副本, 过1-6-5对等闸门: config/query/search/get-page/list-pages/sync/takes/orphans/import/reconcile-links/skillpack/schema/init/doctor)
│   ├── [x][Y+] 1-6-3. TRIVIAL_DELETE 批 [已收口: 真零依赖仅3个 cache/claw-test/report 已整删; 原审计宣称27为过度分类, 20个带test_refs命令归1-6-4, discovery/network/parse非命令+call幽灵条目已从审计剔除]
│   ├── [~][X+] 1-6-4. REAL_MIGRATE 孤儿命令批 [去重后: 移出 skillify->1-1 / eval族->1-2 / calibration->1-3 / dream->1-12 / extract·export·integrity->1-4; 真孤儿=code-intel(code-*·reindex*·edges-backfill·backfill) + memory(recall·forget) + models·providers + whoknows·brainstorm·auth·features·storage·migrate·publish·extract-conversation-facts·resolvers·check-resolvable + 20个1-6-3归入带test命令]
│   └── [ ][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
```

### 🔨 当前施工: 1-6-4-5. get_health 生产后端 (libsql + postgres): 补 BrainEngine::get_health override — page/embed_coverage/dead_links/orphan/timeline_coverage/link_coverage/most_connected + 5分量brain_score(0-100), SQL 查询照 get_brain_stats 基建(scalar_count/links/timeline). 解 G48: 硬阻塞 features 死命令 + doctor brain_score 软降级 + autopilot targeted-submit. 三后端对齐单测钉 wire, 空brain=100/100 边界
**Status:** `in_progress` | **Mode:** `build`
<!-- ⚠️ ROADMAP_SECTION_END -->
