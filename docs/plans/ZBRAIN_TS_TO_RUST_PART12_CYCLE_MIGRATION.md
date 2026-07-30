<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part12-cycle-migration.json` | 最后更新: 2026-07-30 16:10:00

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
│   └── [~] 1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 拆 6 子节点分批做)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
├── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
└── [x][Y+] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算

### 当前施工：1-3-4. synthesize phase 迁移 (TS 源 git 45fe955~1:src/core/cycle/synthesize.ts 1247行; fan-out subagent per transcript; 拆 6 子节点分批做)

簇内最大件。Explore agent 已出结构化拆解：入口 runPhaseSynthesize(synthesize.ts:247)；扇出每 transcript/chunk 一个 subagent job(idempotency_key=dream:synth:<filePath>:<hash16>[:c<i>of<n>])；judgeSignificance 用 ChatProvider(Haiku)，合成 offload 给 worker 跑 subagent(Sonnet)。BLOCKER：dream_verdicts 表/引擎方法全缺；transcript-discovery 无；getConfig/setConfig 无；subagent_tool_executions 表无。

**决策：**
- Q: Q1 磁盘双写是否复刻？ → 待用户决策：复刻 TS 磁盘写(brainDir/<slug>.md + dream-cycle-summaries/<date>.md) 保持语义，或 DB-canonical 仅写引擎(patterns 先例) (影响 1-3-4-5)
- Q: Q2 配置/冷却与 subagent_tool_executions 表如何处理？ → 待用户决策：完整复刻(加 BrainEngine getConfig/setConfig + 建 subagent_tool_executions 表) 或 patterns 先例(硬编码 TS 默认 + 登记 KNOWN-GAP) (影响 1-3-4-6；patterns 已用此先例)

**当前子树：**
├── [x] 1-3-4-1. dream_verdicts migration + engine 方法 (get_dream_verdict/put_dream_verdict; 双 dialect migration 0026; InMemory+libsql+postgres; EXPECTED_VERSION 25->26)
├── [x] 1-3-4-2. transcript-discovery 模块 (移植 transcript-discovery.ts:214 discoverTranscripts; 纯 fs+sha256; 递归扫 corpusDir+meetingTranscriptsDir; 过滤 minChars/日期/自消费标记/excludePatterns)
├── [x] 1-3-4-3. 模型上下文预算纯函数 (computeChunkCharBudget / splitTranscriptByBudget / rewriteChunkedSlug + 单测; 无依赖可独立)
├── [ ] 1-3-4-4. 扇出编排 run_phase_synthesize (复用 patterns 的 MinionQueue+wait_for_completion; 接 cycle 真实臂; 需 1-3-4-1/2/3 + ChatProvider 裁决入参)
├── [ ] 1-3-4-5. 磁盘双写 (reverseWriteRefs 写 brainDir/<slug>.md + writeSummaryPage 写 dream-cycle-summaries/<date>.md; 受 Q1 决策影响)
└── [ ] 1-3-4-6. 配置/冷却 + subagent_tool_executions (loadSynthConfig 的 dream.synthesize.* 键 + cooldown 时间戳; 受 Q2 决策影响)
<!-- ROADMAP_SECTION_END -->
