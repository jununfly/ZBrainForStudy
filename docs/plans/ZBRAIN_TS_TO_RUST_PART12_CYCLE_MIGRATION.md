<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part12-cycle-migration.json` | 最后更新: 2026-07-29 21:38:06

[~][X+] 1. Part12 - cycle 大迁移 (按能力簇切)
├── [~][Y+] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
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
├── [~][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
│   ├── [x] 1-3-1. synthesize-concepts phase 迁移 (port synthesize-concepts.ts → autopilot/phases/synthesize_concepts.rs; gatewayChat+execute_raw 查 atom 页+put_page 写 concept 页+deterministic 兜底; cycle 真实臂无 chat→Skipped)
│   ├── [x] 1-3-2. schema-suggest phase 迁移 (先移植 schema-pack detect.ts+suggest.ts → schema_pack::detect/suggest; phase 层接 cycle 真实臂; 无 LLM heuristics 兜底; 不写 brain DB 只写 audit jsonl)
│   ├── [x] 1-3-3. patterns phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/patterns.ts 351行; 先移植 minions wait_for_completion(94行); 单 subagent job 经 MinionQueue; cycle 真实臂 + 补 handlers/patterns.rs)
│   └── [ ][X+] 1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 需 dream_verdicts migration+transcript-discovery+磁盘双写+模型上下文预算; 待展开拆 sub-sub)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
├── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
└── [x][Y+] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算

### 当前施工：1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)

**决策：**
- Q: Q1 1-3 如何拆分？ → 勘察发现 synthesize.ts(1247行)/patterns.ts(351行) 已在 part11 TS minions teardown (45fe955) 被删（依赖 TS MinionQueue/subagent），TS 源需从 git 45fe955~1 取；synthesize-concepts.ts/schema-suggest.ts 仍在。拆 4 叶子：1-3-1 synthesize-concepts（直接 LLM，套 extract_atoms 范式）→ 1-3-2 schema-suggest（无 LLM，先移植 detect/suggest 进 schema_pack）→ 1-3-3 patterns（单 subagent，先补 wait_for_completion）→ 1-3-4 synthesize（最大件，独立 sub-node 待展开）。用户确认由易到难顺序。
- Q: Q2 synthesize（fan-out subagent + dream_verdicts 表缺 migration + 磁盘双写 + 模型上下文预算）本簇是否做完？ → 用户决策：拆细分批做。本簇先完成另三叶子；1-3-4 synthesize 保留 explore 待展开（届时再拆 sub-sub：dream_verdicts migration / wait_for_completion 复用 / transcript-discovery / fan-out 编排 / 双写）。不做降级简化版（保持 TS 语义）。

**当前子树：**
├── [x] 1-3-1. synthesize-concepts phase 迁移 (port synthesize-concepts.ts → autopilot/phases/synthesize_concepts.rs; gatewayChat+execute_raw 查 atom 页+put_page 写 concept 页+deterministic 兜底; cycle 真实臂无 chat→Skipped)
├── [x] 1-3-2. schema-suggest phase 迁移 (先移植 schema-pack detect.ts+suggest.ts → schema_pack::detect/suggest; phase 层接 cycle 真实臂; 无 LLM heuristics 兜底; 不写 brain DB 只写 audit jsonl)
├── [x] 1-3-3. patterns phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/patterns.ts 351行; 先移植 minions wait_for_completion(94行); 单 subagent job 经 MinionQueue; cycle 真实臂 + 补 handlers/patterns.rs)
└── [ ][X+] 1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 需 dream_verdicts migration+transcript-discovery+磁盘双写+模型上下文预算; 待展开拆 sub-sub)
<!-- ROADMAP_SECTION_END -->

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
├── [~][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
│   ├── [x] 1-3-1. synthesize-concepts phase 迁移 (port synthesize-concepts.ts → autopilot/phases/synthesize_concepts.rs; gatewayChat+execute_raw 查 atom 页+put_page 写 concept 页+deterministic 兜底; cycle 真实臂无 chat→Skipped)
│   ├── [x] 1-3-2. schema-suggest phase 迁移 (先移植 schema-pack detect.ts+suggest.ts → schema_pack::detect/suggest; phase 层接 cycle 真实臂; 无 LLM heuristics 兜底; 不写 brain DB 只写 audit jsonl)
│   ├── [x] 1-3-3. patterns phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/patterns.ts 351行; 先移植 minions wait_for_completion(94行); 单 subagent job 经 MinionQueue; cycle 真实臂 + 补 handlers/patterns.rs)
│   └── [ ][X+] 1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 需 dream_verdicts migration+transcript-discovery+磁盘双写+模型上下文预算; 待展开拆 sub-sub)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
├── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
└── [x][Y+] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算
```

### 🔨 当前施工: 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
**Status:** `in_progress` | **Mode:** `explore`

**决策记录:**
- Q: Q1: 1-3 如何拆分？勘察发现 synthesize.ts(1247行)/patterns.ts(351行) 已在 part11 TS minions teardown (45fe955) 被删（依赖 TS MinionQueue/subagent），TS 源需从 git 45fe955~1 取；synthesize-concepts.ts/schema-suggest.ts 仍在。
  A: 拆 4 叶子：1-3-1 synthesize-concepts（直接 LLM，套 extract_atoms 范式）→ 1-3-2 schema-suggest（无 LLM，先移植 detect/suggest 进 schema_pack）→ 1-3-3 patterns（单 subagent，先补 wait_for_completion）→ 1-3-4 synthesize（最大件，独立 sub-node 待展开）。用户确认由易到难顺序。
- Q: Q2: synthesize（fan-out subagent + dream_verdicts 表缺 migration + 磁盘双写 + 模型上下文预算）本簇是否做完？
  A: 用户决策：拆细分批做。本簇先完成另三叶子；1-3-4 synthesize 保留 explore 待展开（届时再拆 sub-sub：dream_verdicts migration / wait_for_completion 复用 / transcript-discovery / fan-out 编排 / 双写）。不做降级简化版（保持 TS 语义）。

**子节点:**
- [x] 1-3-1. synthesize-concepts phase 迁移 (port synthesize-concepts.ts → autopilot/phases/synthesize_concepts.rs; gatewayChat+execute_raw 查 atom 页+put_page 写 concept 页+deterministic 兜底; cycle 真实臂无 chat→Skipped)
- [x] 1-3-2. schema-suggest phase 迁移 (先移植 schema-pack detect.ts+suggest.ts → schema_pack::detect/suggest; phase 层接 cycle 真实臂; 无 LLM heuristics 兜底; 不写 brain DB 只写 audit jsonl)
- [x] 1-3-3. patterns phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/patterns.ts 351行; 先移植 minions wait_for_completion(94行); 单 subagent job 经 MinionQueue; cycle 真实臂 + 补 handlers/patterns.rs)
- [ ] 1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 需 dream_verdicts migration+transcript-discovery+磁盘双写+模型上下文预算; 待展开拆 sub-sub)
<!-- ⚠️ ROADMAP_SECTION_END -->
