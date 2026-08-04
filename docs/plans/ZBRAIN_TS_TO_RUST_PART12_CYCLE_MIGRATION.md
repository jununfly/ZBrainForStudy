<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part12-cycle-migration.json` | 最后更新: 2026-08-03 12:06:01

[~][X+] 1. Part12 - cycle 大迁移 (按能力簇切)
├── [x][Y+] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
│   ├── [x][Y+] 1-1-1. extract-facts phase 实现 (port extract-facts.ts → autopilot/phases/extract_facts.rs; 接 execute_phase match 臂; ChatProvider trait DI)
│   ├── [x][Y+] 1-1-2. extract-atoms phase 实现 (port extract-atoms.ts → autopilot/phases/extract_atoms.rs; 接 execute_phase)
│   ├── [x][Y+] 1-1-3. extract-takes phase 实现 (port extract-takes.ts → autopilot/phases/extract_takes.rs; 接 execute_phase; 消费者 v0_28_0→extract-takes 映射归 1-6)
│   ├── [x][Y+] 1-1-4. propose-takes phase 实现 (port propose-takes.ts → autopilot/phases/propose_takes.rs; 接 execute_phase)
│   ├── [x][Y+] 1-1-5. grade-takes phase 实现 (port grade-takes.ts → autopilot/phases/grade_takes.rs; 接 execute_phase)
│   └── [x][Y+] 1-1-6. conversation-facts-backfill phase 实现 (port conversation-facts-backfill.ts → autopilot/phases/conversation_facts_backfill.rs; 接 execute_phase)
├── [x][Y+] 1-2. emotional-calibration 簇迁移 (emotional-weight/recompute-emotional-weight/calibration-profile; 消费者 calibration->calibration-profile, backfill-registry->emotional-weight)
│   ├── [x][Y+] 1-2-1. compute_emotional_weight 纯函数移植 (port emotional-weight.ts computeEmotionalWeight → autopilot/phases/emotional_weight.rs; 无引擎依赖)
│   ├── [x][Y+] 1-2-2. 补引擎方法 batch_load_emotional_inputs + set_emotional_weight_batch (trait + InMemory + libsql + postgres; get_config 走 opts override)
│   ├── [x][Y+] 1-2-3. recompute_emotional_weight phase 实现 (port recompute-emotional-weight.ts → autopilot/phases/recompute_emotional_weight.rs; 接 execute_phase)
│   └── [x][Y+] 1-2-4. run_calibration_profile 接 cycle 真实臂 (calibration/calibration_profile.rs 已存在; cycle.rs CyclePhase::CalibrationProfile 真实臂 + 单测)
├── [x][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
│   ├── [x] 1-3-1. synthesize-concepts phase 迁移 (port synthesize-concepts.ts → autopilot/phases/synthesize_concepts.rs; gatewayChat+execute_raw 查 atom 页+put_page 写 concept 页+deterministic 兜底; cycle 真实臂无 chat→Skipped)
│   ├── [x] 1-3-2. schema-suggest phase 迁移 (先移植 schema-pack detect.ts+suggest.ts → schema_pack::detect/suggest; phase 层接 cycle 真实臂; 无 LLM heuristics 兜底; 不写 brain DB 只写 audit jsonl)
│   ├── [x] 1-3-3. patterns phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/patterns.ts 351行; 先移植 minions wait_for_completion(94行); 单 subagent job 经 MinionQueue; cycle 真实臂 + 补 handlers/patterns.rs)
│   └── [x] 1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 拆 6 子节点分批做)
├── [x][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [x][X+] 1-5. auto-think 簇迁移 (auto-think phase)
├── [~][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
│   ├── [x][X+] 1-6-1. 编排骨架剩余缺口收口 (signal 透传 + resolveSourceForDir + makeErrorFromException 错误信封 + no_database 守卫决策; yield/synth/extractTotals/last_full_cycle_at/deriveStatus/chat门控 已交付)
│   ├── [ ][X+] 1-6-2. 周期锁 (per-source cycle lock; busy→skipped/cycle_already_running, 失败→failed/lock_acquisition_error; 复用 sync/lock.rs 基建)
│   ├── [x][X+] 1-6-3. BudgetMeter 共享模块 (port budget-meter.ts → autopilot/budget_meter.rs; BaseCyclePhase 注入 meter; 两处消费 auto_think+drift; calibration 已剔除)
│   ├── [x] 1-6-4. 简单 stub 臂接线 Sync/Lint/Backlinks/Extract/Embed (复用 sync/core.rs perform_sync、links backlinks、embedding.rs、ingestion; TS runPhaseSync/Extract/Embed/Lint/Backlinks 语义对齐)
│   ├── [x][X+] 1-6-5. consolidate phase 迁移 (port src/core/cycle/phases/consolidate.ts 297行 → autopilot/phases/consolidate.rs; 确定性 v0.35.4 确定性无 LLM（含 semantic upsert + bitemporal valid_until writeback）: (source_id,entity_slug) 桶扫描 + age gate 24h + 余弦0.85贪心聚类(通过 execute_raw 读 facts.embedding) + 取最高confidence文本写 takes(kind=fact) + UPDATE facts.consolidated_at/consolidated_into 不删; InMemory 无 embedding 列→聚类退化单例、phase 返回 Ok/Skipped 不 fail; 用 add_takes_batch + execute_raw)
│   ├── [x] 1-6-6. phantom-redirect pre-pass (TS v0.35.5, 606行, extract_facts 内预通行). 全量保真移植，拆 4 sub-node：1-6-6-1 entities/resolve 全量移植、1-6-6-2 migrateFactsToCanonical 引擎 primitive、1-6-6-3 phantom_audit 模块、1-6-6-4 extract_facts 内主逻辑接线+测试+收口 G61。复用 cycle 锁；rewrite_links 在 libsql/postgres 实为 no-op（已知降级，记 G-gap）。
│   ├── [x][X+] 1-6-7. drift phase 迁移 (port drift.ts 168行 → autopilot/phases/drift.rs; findDriftCandidates 软带0.3-0.85 + timeline 证据 takes 候选报告; 默认关闭 dream.drift.enabled; TS 无调用者但决策 q-0 定迁移)
│   └── [ ][X+] 1-6-8. 消费者切换 (CLI dream 子命令 → Rust run_cycle; --json/--dry-run/--pull/--phase/--dir/--input/--date/--from/--to/--unsafe-bypass-dream-guard; printHuman totals; failed→exit 1; TS cycle.ts+cycle/ 目录+dream.ts 删除)
└── [x][Y+] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算

### 当前施工：1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)

**决策：**
- Q: drift.ts 无 TS 调用者且 CyclePhase 不含 drift，是否迁移？ → 一并迁移 (用户决策：作为 1-6-7 子节点移植 findDriftCandidates；默认关闭 dream.drift.enabled 保持休眠语义)
- Q: phantom-redirect pre-pass (606行, 此前 extract_facts port 显式 DEFER) 是否纳入 1-6？ → 纳入 1-6-6 子节点 (Part12 收官需 extract_facts 达 TS 等价，TS cycle/ 目录才能全删)
- Q: 1-6 剩余节点（1-6-1/1-6-2/1-6-5/1-6-6/1-6-8）在 1-6-3-4 之后的执行顺序？ → 1-6-3-4 → 1-6-1（编排骨架强化）→ 1-6-2（周期锁）→ 1-6-5（consolidate）→ 1-6-6（phantom-redirect）→ 1-6-8（消费者切换，最后集成删 TS） (依赖最干净：骨架与锁先于真实 phase 与集成)

**当前子树：**
├── [x][X+] 1-6-1. 编排骨架剩余缺口收口 (signal 透传 + resolveSourceForDir + makeErrorFromException 错误信封 + no_database 守卫决策; yield/synth/extractTotals/last_full_cycle_at/deriveStatus/chat门控 已交付)
│   ... 4 more child nodes; run tree 1-6-1 --depth 2 for full view
├── [ ][X+] 1-6-2. 周期锁 (per-source cycle lock; busy→skipped/cycle_already_running, 失败→failed/lock_acquisition_error; 复用 sync/lock.rs 基建)
├── [x][X+] 1-6-3. BudgetMeter 共享模块 (port budget-meter.ts → autopilot/budget_meter.rs; BaseCyclePhase 注入 meter; 两处消费 auto_think+drift; calibration 已剔除)
│   ... 4 more child nodes; run tree 1-6-3 --depth 2 for full view
├── [x] 1-6-4. 简单 stub 臂接线 Sync/Lint/Backlinks/Extract/Embed (复用 sync/core.rs perform_sync、links backlinks、embedding.rs、ingestion; TS runPhaseSync/Extract/Embed/Lint/Backlinks 语义对齐)
├── [x][X+] 1-6-5. consolidate phase 迁移 (port src/core/cycle/phases/consolidate.ts 297行 → autopilot/phases/consolidate.rs; 确定性 v0.35.4 确定性无 LLM（含 semantic upsert + bitemporal valid_until writeback）: (source_id,entity_slug) 桶扫描 + age gate 24h + 余弦0.85贪心聚类(通过 execute_raw 读 facts.embedding) + 取最高confidence文本写 takes(kind=fact) + UPDATE facts.consolidated_at/consolidated_into 不删; InMemory 无 embedding 列→聚类退化单例、phase 返回 Ok/Skipped 不 fail; 用 add_takes_batch + execute_raw)
├── [x] 1-6-6. phantom-redirect pre-pass (TS v0.35.5, 606行, extract_facts 内预通行). 全量保真移植，拆 4 sub-node：1-6-6-1 entities/resolve 全量移植、1-6-6-2 migrateFactsToCanonical 引擎 primitive、1-6-6-3 phantom_audit 模块、1-6-6-4 extract_facts 内主逻辑接线+测试+收口 G61。复用 cycle 锁；rewrite_links 在 libsql/postgres 实为 no-op（已知降级，记 G-gap）。
│   ... 4 more child nodes; run tree 1-6-6 --depth 2 for full view
├── [x][X+] 1-6-7. drift phase 迁移 (port drift.ts 168行 → autopilot/phases/drift.rs; findDriftCandidates 软带0.3-0.85 + timeline 证据 takes 候选报告; 默认关闭 dream.drift.enabled; TS 无调用者但决策 q-0 定迁移)
└── [ ][X+] 1-6-8. 消费者切换 (CLI dream 子命令 → Rust run_cycle; --json/--dry-run/--pull/--phase/--dir/--input/--date/--from/--to/--unsafe-bypass-dream-guard; printHuman totals; failed→exit 1; TS cycle.ts+cycle/ 目录+dream.ts 删除)
<!-- ROADMAP_SECTION_END -->
