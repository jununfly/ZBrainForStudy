<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-09 13:22:00

[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [~][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
│   ├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
│   ├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
│   ├── [x] 1-1-3. 判废真空壳 + 重分类 2 非空壳（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 phase 已在 Rust）
│   ├── [~][X+] 1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 1-1-5）
│   └── [ ][X+] 1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）
└── [~][X+] 1-2. G76 extract 族命令 Rust 重实现
    ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
    ├── [ ][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
    ├── [ ][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，blocked by G35）
    └── [ ][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb

### 当前施工：1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 1-1-5）

2026-08-09 侦察对账再修正（与 1-1-3 同批）：原「10 个非 LLM 需新基建」失准——eval-brainstorm 实为 LLM 命令（core/brainstorm/orchestrator.ts 经 gateway.chat + judges.ts 调 LLM），已移 1-1-5（D 类现 5 个）；C 类实为 9 个非 LLM。另：eval-run-all/eval-schema-authoring TS 侧即 stub（直译=搬未完成态，后者只需移植 aggregateVerdict 纯函数）；eval-compare 落地即空表；eval-whoknows 的 find_experts 已在 Rust（whoknows.rs:189）；qrels-file 非缺口（search/eval.rs 已有 parse_qrels）；支撑 core/ 模块须从 bcafcafd^ 取回（src/core/ 在更早 bcafcafd 已删）。先决 eval_candidates 表完全不存在，需 0030 双 dialect migration。分阶段：阶段0 migration + BrainEngine 方法 → 阶段1 eval-export/prune → 阶段2 schema-authoring → 阶段3 gate qrels → 阶段4 replay → 阶段5 whoknows → 阶段6 run-all/compare（依赖 core/search/mode，宜重设计）→ 阶段7 code-retrieval。

**阶段0/1/2/3/4/5 已 ship（2026-08-09）**：0030 双 dialect migration（`eval_candidates` 表，26 列；sqlite 数组列降级 JSON TEXT）+ include_str! 注册 + EXPECTED_VERSION=30；`BrainEngine` 新增 `list_eval_candidates`/`delete_eval_candidates_before`（libsql/postgres 实现 + InMemory 默认空 impl）；顶层 `zbrain eval-export`/`zbrain eval-prune`/`zbrain eval-gate`/`zbrain eval-replay`/`zbrain eval-whoknows` verbs；阶段2 `zbrain_core::eval::schema_authoring::aggregate_verdict` 纯函数 + 6 单测；阶段3 `zbrain_core::eval::gate`（parse_qrels_file 双 shape + run_correctness_gate + evaluate_correctness_gate + assemble_gate_result）+ 11 core 单测；阶段4 `zbrain_core::eval::replay`（parse_ndjson + jaccard_slugs + replay_core 泛型 query_fn 注入）+ 22 core 单测 + 顶层 eval-replay verb；阶段5 `zbrain_core::eval::whoknows`（read_fixture JSONL 跳过注释/校验必填 + jaccard_at_k + top_k_hit + run_quality_gate 阈值0.8 + run_regression_gate 阈值0.4 稀疏<20行自动 skip + assemble_report）+ 9 core 单测 + 顶层 eval-whoknows verb；UNPORTED_EVAL_SUBCOMMANDS 已移除 export/prune/gate/replay/whoknows。校验：cargo check -p zbrain-cli --all-targets 绿、core eval:: 84 passed（22 replay + 11 gate + 9 whoknows + 6 schema_authoring + 36 其它）、CLI eval 18 passed、migration 版本 10 passed、e2e 全绿。诚实缺口：捕获侧写入未接线，eval_candidates 表初为空（export/prune 真实数据待捕获层；replay 消费 export 的 NDJSON 不直接依赖表数据；whoknows L2 因表空实际走 skip）；schema-authoring hermetic harness 与 gate baseline 半边均 pending。下一步：阶段6 eval-run-all + eval-compare（依赖 core/search/mode；TS 为 stub，宜重设计而非直译）。
<!-- ROADMAP_SECTION_END -->
