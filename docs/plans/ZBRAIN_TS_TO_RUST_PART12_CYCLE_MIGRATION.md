
<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part12 - cycle 大迁移 (按能力簇切)

### 树形视图 (depth=2)

```
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
├── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
│   ├── [ ][X+] 1-6-1. 编排骨架强化 (CycleOpts 补 signal/yield/synth 透传 + no_database 守卫 + pack 门控 extract_atoms/synthesize_concepts + resolveSourceForDir + extractTotals 回填 + makeErrorFromException 错误信封 hint/docs_url + deriveStatus 空列表→failed + last_full_cycle_at)
│   ├── [ ][X+] 1-6-2. 周期锁 (per-source cycle lock; busy→skipped/cycle_already_running, 失败→failed/lock_acquisition_error; 复用 sync/lock.rs 基建)
│   ├── [ ][X+] 1-6-3. BudgetMeter 共享模块 (port budget-meter.ts 188行 → autopilot/budget_meter.rs; check/estimateMaxCostUsd/unpriced warn-once/审计 jsonl ~/.zbrain/audit/dream-budget-*.jsonl; auto_think/drift/calibration 三处消费)
│   ├── [ ][X+] 1-6-4. 简单 stub 臂接线 Sync/Lint/Backlinks/Extract/Embed (复用 sync/core.rs perform_sync、links backlinks、embedding.rs、ingestion; TS runPhaseSync/Extract/Embed/Lint/Backlinks 语义对齐)
│   ├── [ ][X+] 1-6-5. consolidate phase 迁移 (port phases/consolidate.ts 297行 → autopilot/phases/consolidate.rs; (source_id,entity_slug) 桶 + 余弦0.85贪心聚类 + takes(kind=fact) + consolidated_at 标记不删除 + bitemporal valid_until + semantic upsert 去重)
│   ├── [ ][X+] 1-6-6. phantom-redirect pre-pass (port phantom-redirect.ts 606行; extract_facts 顶部; syncLockId 单锁30s重试 + 上限50 + body-shape gate + resolvePhantomCanonical + 歧义检查 + fenceDbDrift + 8步提交链; 依赖 entities/resolve + facts-fence + phantom-audit)
│   ├── [ ][X+] 1-6-7. drift phase 迁移 (port drift.ts 168行 → autopilot/phases/drift.rs; findDriftCandidates 软带0.3-0.85 + timeline 证据 takes 候选报告; 默认关闭 dream.drift.enabled; TS 无调用者但决策 q-0 定迁移)
│   └── [ ][X+] 1-6-8. 消费者切换 (CLI dream 子命令 → Rust run_cycle; --json/--dry-run/--pull/--phase/--dir/--input/--date/--from/--to/--unsafe-bypass-dream-guard; printHuman totals; failed→exit 1; TS cycle.ts+cycle/ 目录+dream.ts 删除)
└── [x][Y+] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算
```

### 🔨 当前施工: 1. Part12 - cycle 大迁移 (按能力簇切)
**Status:** `in_progress` | **Mode:** `explore`

**决策记录:**
- Q: Part12 簇执行顺序?
  A: 消费者解锁优先: emotional-calibration(1-2) -> facts-extraction(1-1) -> anomaly-transcript(1-4) -> synthesis(1-3) -> auto-think(1-5) -> orchestration-main-loop(1-6) 最后
  > 用户选推荐项(Q3)。每簇解锁对应消费者,主循环留最后降 big-bang 风险。执行仍延后,此为路线图预排;grind(1-6-7 + 1-3-3)走完删 operations.ts 后再启动 Part12。

**子节点:**
- [x] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
- [x] 1-2. emotional-calibration 簇迁移 (emotional-weight/recompute-emotional-weight/calibration-profile; 消费者 calibration->calibration-profile, backfill-registry->emotional-weight)
- [x] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
- [x] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
- [x] 1-5. auto-think 簇迁移 (auto-think phase)
- [ ] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
- [x] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算
<!-- ⚠️ ROADMAP_SECTION_END -->
