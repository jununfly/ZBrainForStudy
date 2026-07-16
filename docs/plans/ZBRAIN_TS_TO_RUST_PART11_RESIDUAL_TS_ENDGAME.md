<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-16 16:12:59

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
│   ├── [ ][Y+] 1-6-2. RUST_OWNED 壳清理 (删TS副本, 过1-6-5对等闸门: config/query/search/get-page/list-pages/sync/takes/orphans/import/reconcile-links/skillpack/schema/init/doctor)
│   ├── [ ][Y+] 1-6-3. TRIVIAL_DELETE 批 (纯dev/诊断工具整删: lint/upgrade/reinit-pglite/apply-migrations/repair-jsonb/report/smoke-test/claw-test/ze-switch/check-backlinks/cache/friction/lsd/founder/files/parse/network/discovery/anomalies/transcripts/book-mirror/call/integrations/frontmatter/mounts)
│   ├── [ ][X+] 1-6-4. REAL_MIGRATE 批 (33真实功能待port, 按域拆sub-slice: memory=recall/forget; model/provider=models/providers; content=extract/embed/export; resolver=resolvers/integrity/routing-eval/check-resolvable/skillify; code-intel=code-*/reindex-*/backfill; misc=whoknows/brainstorm/dream/auth/eval/features/migrate/storage/calibration/notability-eval)
│   └── [ ][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场

### 当前施工：1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)

calibration 算法补齐：Rust 已有 calibration_queries.rs(DB 层) + web admin；待补 TS src/core/calibration 10 文件算法。2026-07-15 pivot 自 doctor 封顶后选此——领域自包含、边界清晰、不与 doctor 基建阻塞重叠。先探查 TS 算法边界与 Rust 缺口，再定整体 port 或按函数切片。

**决策：**
- Q: calibration 10 文件(1802 行)怎么切？ → 分阶段：Phase 1 先 port 纯函数(templates 5 builder / recall-footer / 纯解析器 parseJudgeOutput / 纯数学 computeForecast+resolveDomainPrefix / 纯规则 takeDomainHint+evaluateNudgeRule+buildLearningEntry / formatAbReport)，自包含可单测；Phase 2 再啃 engine/LLM 支撑(async 读引擎 + LLM 调用)，重 LLM 项(voice-gate gateVoice / think-ab runAbTrial)留 G-gap。 (与 doctor 切片同构：纯函数子集是便宜镜像，engine/LLM 子集是基建。不整体 port 避免大爆炸。)
- Q: Phase 2 calibration 怎么切？全子集都卡在 engine trait 扩展或 LLM，非干净切片 → 不开大 Phase 2；开 1-3-2 = engine-read 子集（forecastForTake+batchForecast+get_scorecard domain_prefix），其余（mount 解析/execute_raw/LLM）留后续节点或登记 gap

**当前子树：**
├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
└── [!][X+] 1-3-3. calibration Phase 2 engine/LLM 支撑（queryAcrossBrains/aggregateDomainScorecards/undoWave/gateVoice/runAbTrial）
<!-- ROADMAP_SECTION_END -->

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
├── [~][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ├── [x][X+] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
│   ├── [!][Y+] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
│   ├── [!][Y+] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
│   ├── [x][Y+] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
│   ├── [x][Y+] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
│   └── [!][Y+] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
├── [ ][X+] 1-6. 孤儿命令迁移 (~20 命令：whoknows/brainstorm/dream/publish/models/providers/...)
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
