<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part12-cycle-migration.json` | 最后更新: 2026-07-29 14:41:32

[~][X+] 1. Part12 - cycle 大迁移 (按能力簇切)
├── [~][Y+] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
│   ├── [x][Y+] 1-1-1. extract-facts phase 实现 (port extract-facts.ts → autopilot/phases/extract_facts.rs; 接 execute_phase match 臂; ChatProvider trait DI)
│   ├── [x][Y+] 1-1-2. extract-atoms phase 实现 (port extract-atoms.ts → autopilot/phases/extract_atoms.rs; 接 execute_phase)
│   ├── [ ][Y+] 1-1-3. extract-takes phase 实现 (port extract-takes.ts → autopilot/phases/extract_takes.rs; 接 execute_phase; 消费者 v0_28_0→extract-takes 映射归 1-6)
│   ├── [ ][Y+] 1-1-4. propose-takes phase 实现 (port propose-takes.ts → autopilot/phases/propose_takes.rs; 接 execute_phase)
│   ├── [ ][Y+] 1-1-5. grade-takes phase 实现 (port grade-takes.ts → autopilot/phases/grade_takes.rs; 接 execute_phase)
│   └── [ ][Y+] 1-1-6. conversation-facts-backfill phase 实现 (port conversation-facts-backfill.ts → autopilot/phases/conversation_facts_backfill.rs; 接 execute_phase)
├── [ ][X+] 1-2. emotional-calibration 簇迁移 (emotional-weight/recompute-emotional-weight/calibration-profile; 消费者 calibration->calibration-profile, backfill-registry->emotional-weight)
├── [ ][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
├── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
└── [x][Y+] 1-7. 验证线打通：rust-tests.yml修复 + pack_lock提交 + 5测试失败清算

### 当前施工：1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)

**决策：**
- Q: Q1 分解粒度 → 拆 6 sub-node（extract-facts/atoms/takes/propose-takes/grade-takes/conversation-facts-backfill），每 phase 独立可测、独立提交 (mirrors 1-3-3-x cadence；父节点 1-1 仅作簇容器)
- Q: Q2 实现形态 → 每 phase = execute_phase 真实 match 臂，委托 autopilot/phases/<name>.rs 模块函数；对齐 Orphans/Purge 真实实现范式 (非 operation（cycle 内部 phase，非用户态 API）)
- Q: Q3 LLM 集成 → 走 Arc<dyn ChatProvider> trait DI（instantiate_chat），复用 1-3-3-7 同款抽象；phase 函数接收 chat 入参 (ChatProvider 已在 1-3-3 基建就绪)
- Q: Q4 实施顺序 → 按依赖从底向上：extract-facts(1-1-1)→extract-atoms(1-1-2)→extract-takes(1-1-3)→propose-takes(1-1-4)→grade-takes(1-1-5)→conversation-facts-backfill(1-1-6) (extract-facts 无跨 phase 依赖，先行)
- Q: Q5 测试策略 → tests/ 集成测试，ChatProvider stub 无真实 LLM；覆盖 happy path + 解析/错误分支 (对齐 1-3-3-7 calibration_profile 测试范式)
- Q: Q6 范围边界 → 1-1 仅覆盖标注的 6 函数；通用 Extract（HTML→text）阶段不在 1-1，留待独立节点 (节点 label 未含通用 extract)
- Q: Q7 消费者注册表 v0_28_0→extract-takes → 注册表重映射归 1-6 orchestration 节点统一接线；1-1 只暴露 phase 函数与 label (不在此节点改 engine 消费者注册)

**当前子树：**
├── [x][Y+] 1-1-1. extract-facts phase 实现 (port extract-facts.ts → autopilot/phases/extract_facts.rs; 接 execute_phase match 臂; ChatProvider trait DI)
├── [x][Y+] 1-1-2. extract-atoms phase 实现 (port extract-atoms.ts → autopilot/phases/extract_atoms.rs; 接 execute_phase)
├── [ ][Y+] 1-1-3. extract-takes phase 实现 (port extract-takes.ts → autopilot/phases/extract_takes.rs; 接 execute_phase; 消费者 v0_28_0→extract-takes 映射归 1-6)
├── [ ][Y+] 1-1-4. propose-takes phase 实现 (port propose-takes.ts → autopilot/phases/propose_takes.rs; 接 execute_phase)
├── [ ][Y+] 1-1-5. grade-takes phase 实现 (port grade-takes.ts → autopilot/phases/grade_takes.rs; 接 execute_phase)
└── [ ][Y+] 1-1-6. conversation-facts-backfill phase 实现 (port conversation-facts-backfill.ts → autopilot/phases/conversation_facts_backfill.rs; 接 execute_phase)
<!-- ROADMAP_SECTION_END -->

<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part12 - cycle 大迁移 (按能力簇切)

### 树形视图 (depth=2)

```
[~][X+] 1. Part12 - cycle 大迁移 (按能力簇切)
├── [~][Y+] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
│   ├── [x][Y+] 1-1-1. extract-facts phase 实现 (port extract-facts.ts → autopilot/phases/extract_facts.rs; 接 execute_phase match 臂; ChatProvider trait DI)
│   ├── [x][Y+] 1-1-2. extract-atoms phase 实现 (port extract-atoms.ts → autopilot/phases/extract_atoms.rs; 接 execute_phase)
│   ├── [ ][Y+] 1-1-3. extract-takes phase 实现 (port extract-takes.ts → autopilot/phases/extract_takes.rs; 接 execute_phase; 消费者 v0_28_0→extract-takes 映射归 1-6)
│   ├── [ ][Y+] 1-1-4. propose-takes phase 实现 (port propose-takes.ts → autopilot/phases/propose_takes.rs; 接 execute_phase)
│   ├── [ ][Y+] 1-1-5. grade-takes phase 实现 (port grade-takes.ts → autopilot/phases/grade_takes.rs; 接 execute_phase)
│   └── [ ][Y+] 1-1-6. conversation-facts-backfill phase 实现 (port conversation-facts-backfill.ts → autopilot/phases/conversation_facts_backfill.rs; 接 execute_phase)
├── [ ][X+] 1-2. emotional-calibration 簇迁移 (emotional-weight/recompute-emotional-weight/calibration-profile; 消费者 calibration->calibration-profile, backfill-registry->emotional-weight)
├── [ ][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
└── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
```

### 🔨 当前施工: 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
**Status:** `in_progress` | **Mode:** `exploit`

**决策记录:**
- Q: Q1 分解粒度
  A: 拆 6 sub-node（extract-facts/atoms/takes/propose-takes/grade-takes/conversation-facts-backfill），每 phase 独立可测、独立提交
  > mirrors 1-3-3-x cadence；父节点 1-1 仅作簇容器
- Q: Q2 实现形态
  A: 每 phase = execute_phase 真实 match 臂，委托 autopilot/phases/<name>.rs 模块函数；对齐 Orphans/Purge 真实实现范式
  > 非 operation（cycle 内部 phase，非用户态 API）
- Q: Q3 LLM 集成
  A: 走 Arc<dyn ChatProvider> trait DI（instantiate_chat），复用 1-3-3-7 同款抽象；phase 函数接收 chat 入参
  > ChatProvider 已在 1-3-3 基建就绪
- Q: Q4 实施顺序
  A: 按依赖从底向上：extract-facts(1-1-1)→extract-atoms(1-1-2)→extract-takes(1-1-3)→propose-takes(1-1-4)→grade-takes(1-1-5)→conversation-facts-backfill(1-1-6)
  > extract-facts 无跨 phase 依赖，先行
- Q: Q5 测试策略
  A: tests/ 集成测试，ChatProvider stub 无真实 LLM；覆盖 happy path + 解析/错误分支
  > 对齐 1-3-3-7 calibration_profile 测试范式
- Q: Q6 范围边界
  A: 1-1 仅覆盖标注的 6 函数；通用 Extract（HTML→text）阶段不在 1-1，留待独立节点
  > 节点 label 未含通用 extract
- Q: Q7 消费者注册表 v0_28_0→extract-takes
  A: 注册表重映射归 1-6 orchestration 节点统一接线；1-1 只暴露 phase 函数与 label
  > 不在此节点改 engine 消费者注册

**子节点:**
- [x] 1-1-1. extract-facts phase 实现 (port extract-facts.ts → autopilot/phases/extract_facts.rs; 接 execute_phase match 臂; ChatProvider trait DI)
- [x] 1-1-2. extract-atoms phase 实现 (port extract-atoms.ts → autopilot/phases/extract_atoms.rs; 接 execute_phase)
- [ ] 1-1-3. extract-takes phase 实现 (port extract-takes.ts → autopilot/phases/extract_takes.rs; 接 execute_phase; 消费者 v0_28_0→extract-takes 映射归 1-6)
- [ ] 1-1-4. propose-takes phase 实现 (port propose-takes.ts → autopilot/phases/propose_takes.rs; 接 execute_phase)
- [ ] 1-1-5. grade-takes phase 实现 (port grade-takes.ts → autopilot/phases/grade_takes.rs; 接 execute_phase)
- [ ] 1-1-6. conversation-facts-backfill phase 实现 (port conversation-facts-backfill.ts → autopilot/phases/conversation_facts_backfill.rs; 接 execute_phase)
<!-- ⚠️ ROADMAP_SECTION_END -->
