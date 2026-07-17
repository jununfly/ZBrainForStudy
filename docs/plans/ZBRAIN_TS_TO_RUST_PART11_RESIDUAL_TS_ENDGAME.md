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
│   ├── [x][X+] 1-6-4. REAL_MIGRATE 孤儿命令批 [去重后: 移出 skillify->1-1 / eval族->1-2 / calibration->1-3 / dream->1-12 / extract·export·integrity->1-4; 真孤儿=code-intel(code-*·reindex*·edges-backfill·backfill) + memory(recall·forget) + models·providers + whoknows·brainstorm·auth·features·storage·migrate·publish·extract-conversation-facts·resolvers·check-resolvable + 20个1-6-3归入带test命令]
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

### 🔨 当前施工: 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
**Status:** `in_progress` | **Mode:** `explore`

calibration 算法补齐：Rust 已有 calibration_queries.rs(DB 层) + web admin；待补 TS src/core/calibration 10 文件算法。2026-07-15 pivot 自 doctor 封顶后选此——领域自包含、边界清晰、不与 doctor 基建阻塞重叠。先探查 TS 算法边界与 Rust 缺口，再定整体 port 或按函数切片。

**决策记录:**
- Q: calibration 10 文件(1802 行)怎么切？
  A: 分阶段：Phase 1 先 port 纯函数(templates 5 builder / recall-footer / 纯解析器 parseJudgeOutput / 纯数学 computeForecast+resolveDomainPrefix / 纯规则 takeDomainHint+evaluateNudgeRule+buildLearningEntry / formatAbReport)，自包含可单测；Phase 2 再啃 engine/LLM 支撑(async 读引擎 + LLM 调用)，重 LLM 项(voice-gate gateVoice / think-ab runAbTrial)留 G-gap。
  > 与 doctor 切片同构：纯函数子集是便宜镜像，engine/LLM 子集是基建。不整体 port 避免大爆炸。
- Q: Phase 2 calibration 怎么切？全子集都卡在 engine trait 扩展或 LLM，非干净切片
  A: 不开大 Phase 2；开 1-3-2 = engine-read 子集（forecastForTake+batchForecast+get_scorecard domain_prefix），其余（mount 解析/execute_raw/LLM）留后续节点或登记 gap

**子节点:**
- [x] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
- [x] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
- [!] 1-3-3. calibration Phase 2 engine/LLM 支撑（queryAcrossBrains/aggregateDomainScorecards/undoWave/gateVoice/runAbTrial）
<!-- ⚠️ ROADMAP_SECTION_END -->

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-17 15:21:31

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

### 当前施工：1-6-4-4. features auto-fix 引擎地基 (page-level: list_stale_pages + put_page_embedding + add_timeline_entry + link 解析器)

ENGINE FOUNDATION COMPLETE 2026-07-17 TDD. list_stale_pages + put_page_embedding(Vec<u8> surgical UPDATE) + add_timeline_entry(trait default get->append->set_page_timeline, 3-backend surgical UPDATE timeline TEXT) + extract_markdown_links pure parser(9 unit tests). Tests: inmemory(2)+libsql(4)+postgres(3)=9 integration passed. Sub-nodes 1-6-4-4-1..4 pending for library-function+CLI wiring. chunk-level modeling is a separate epic.

**决策：**
- Q: 范围：本刀(1-6-4-4)建多大？ → 引擎地基先行：本刀只补 count_stale_chunks + chunk embedding 回写 + add_timeline_entry 三后端引擎方法，库函数(embed_stale/extract_links/extract_timeline)与 features --auto-fix 接线拆到后续子节点
- Q: 建模：embed --stale/extract timeline 用哪种模型？ → Rust-native page-level: embed --stale=枚举null-embedding页→EmbeddingClient→put_page回写页面向量; extract timeline=追加pages.timeline TEXT(add_timeline_entry 三后端); extract links=现有add_links_batch; 不触发schema迁移. chunk-level(建content_chunks/timeline_entries表+chunk embed+chunk搜索重架构+health重算+存量回填)是独立epic, 不塞进本刀.
- Q: embed --stale 的枚举与写回机制？ → 专用引擎方法: 新增 list_stale_pages(SQL WHERE embedding IS NULL AND deleted_at IS NULL, 与 get_health 同源) + put_page_embedding(slug, source_id, embedding: Vec<u8>)(单条 UPDATE pages SET embedding=? WHERE (source_id,slug)). 外科手术式, 不重写整页. 三后端各实现.
- Q: add_timeline_entry 智能程度？ → 智能 trait 方法 add_timeline_entry(slug, source_id, date, summary, detail): get_page→纯函数解析现有 timeline(移植TS解析器)→插入/追加条目→put_page(timeline). 贴合 TS engine.addTimelineEntry 语义; 三后端经 get_page+put_page 实现.
- Q: 节点结构：1-6-4-4 怎么拆？ → 地基=父节点本体+4行为子节点: 1-6-4-4 本身承载引擎地基(list_stale_pages+put_page_embedding+add_timeline_entry+link解析器); 子节点 1-6-4-4-1 embed_stale库函数 / 1-6-4-4-2 extract_links库函数 / 1-6-4-4-3 extract_timeline库函数 / 1-6-4-4-4 features --auto-fix CLI接线. 不做独立embed/extract CLI.

**当前子树：**
├── [x][X+] 1-6-4-4-1. embed_stale 库函数 (枚举 stale 页→EmbeddingClient→put_page_embedding)
├── [x][X+] 1-6-4-4-2. extract_links 库函数 (link 解析器→add_links_batch)
├── [x][X+] 1-6-4-4-3. extract_timeline 库函数 (timeline 解析器→add_timeline_entry)
└── [ ][X+] 1-6-4-4-4. features --auto-fix CLI 接线 (execute_auto_fix + accepted 记账 + --json auto_fix_results)
<!-- ROADMAP_SECTION_END -->
