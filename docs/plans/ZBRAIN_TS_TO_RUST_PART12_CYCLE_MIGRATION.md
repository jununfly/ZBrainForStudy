<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part12-cycle-migration.json` | 最后更新: 2026-07-23 17:15:13

[~][X+] 1. Part12 - cycle 大迁移 (按能力簇切)
├── [ ][X+] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
├── [ ][X+] 1-2. emotional-calibration 簇迁移 (emotional-weight/recompute-emotional-weight/calibration-profile; 消费者 calibration->calibration-profile, backfill-registry->emotional-weight)
├── [ ][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
└── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)

### 当前施工：1. Part12 - cycle 大迁移 (按能力簇切)

**决策：**
- Q: Part12 簇执行顺序? → 消费者解锁优先: emotional-calibration(1-2) -> facts-extraction(1-1) -> anomaly-transcript(1-4) -> synthesis(1-3) -> auto-think(1-5) -> orchestration-main-loop(1-6) 最后 (用户选推荐项(Q3)。每簇解锁对应消费者,主循环留最后降 big-bang 风险。执行仍延后,此为路线图预排;grind(1-6-7 + 1-3-3)走完删 operations.ts 后再启动 Part12。)

**当前子树：**
├── [ ][X+] 1-1. facts-extraction 簇迁移 (extract-facts/atoms/takes + propose/grade-takes + conversation-facts-backfill; 消费者 v0_28_0->extract-takes)
├── [ ][X+] 1-2. emotional-calibration 簇迁移 (emotional-weight/recompute-emotional-weight/calibration-profile; 消费者 calibration->calibration-profile, backfill-registry->emotional-weight)
├── [ ][X+] 1-3. synthesis 簇迁移 (synthesize/synthesize-concepts/patterns/schema-suggest)
├── [ ][X+] 1-4. anomaly-transcript 簇迁移 (anomaly/transcript-discovery; 消费者 transcripts->transcript-discovery, pglite/postgres-engine->anomaly)
├── [ ][X+] 1-5. auto-think 簇迁移 (auto-think phase)
└── [ ][X+] 1-6. orchestration 主循环迁移 (runCycle 2057行 + base-phase/budget-meter/drift/phantom-redirect/phases/; 消费者 dream->runCycle; Rust cycle.rs 仅745行 dispatch 骨架)
<!-- ROADMAP_SECTION_END -->
