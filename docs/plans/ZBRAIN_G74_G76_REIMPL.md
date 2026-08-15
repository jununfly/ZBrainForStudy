<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-15 20:53:53

[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [~][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
│   ├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
│   ├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
│   ├── [x][X+] 1-1-3. 判废真空壳 + 重分类 2 非空壳 eval 命令（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 cycle phase 已在 Rust）
│   ├── [x] 1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 D 类）
│   └── [x][X+] 1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）
├── [x][X+] 1-2. G76 extract 族命令 Rust 重实现
│   ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
│   ├── [x][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径
│   ├── [x][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，unblocked — Rust ConversationFactsBackfill 已就绪）
│   └── [x][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb
└── [~][X+] 1-3. by-mention 实体提及自动链接子系统（gazetteer，TS 独立 pass，仅 DB-source，Rust 未实现）

### 当前施工：1-1. G74 eval 族命令 Rust 重实现（19 命令）

G74 能力对账表（19 命令 → 4 类）：
【A 真空壳 scaffold，建议判废】1 个：eval-markdown-greenfield(40行)。TS 侧即 return not_yet_implemented，bench 一个 OpenClaw→zbrain 的 markdown-greenfield 迁移导入器；Rust 侧该概念仅是 discovery SQL 里的 imported_from:'markdown-greenfield' frontmatter 排除标记，无命令/phase/feature 可 bench → 真空壳。
【A′ 初判空壳、复核移出：bench 的底层 feature 已在 Rust】2 个：eval-extract-atoms(39行)、eval-synthesize-concepts(36行)。二者虽也 return not_yet_implemented，但 bench 的 extract_atoms/synthesize_concepts cycle phase 已在 Rust 完整 port（Part12 1-1-2/1-3-1，autopilot/phases/*.rs 已接 cycle.rs、有 discovery SQL 与集成测试），故非空壳、不可 wontfix，重分类为可 port 的 eval 子命令（待实现：跑对应 phase + 比 OpenClaw baseline 算 precision/recall/tier-agreement）。
【B 非 LLM 且 Rust 底座已在，只缺 verb】eval.ts(421行) → 依赖 core/search/eval.ts，Rust 侧 crates/zbrain-core/src/search/eval.rs(619行, G73 已 resolved) 已完整 port 4 个 IR 指标 + run_eval，但 run_eval 零调用者（仅 search/mod.rs:27 re-export）。eval-trajectory.ts(232行) → engine.find_trajectory + CLI FindTrajectory verb 已有，缺的只是 computeTrajectoryStats 的回归/漂移统计层。
【C 非 LLM 但需新基建】10 个：eval-gate(491, 需 bench/{baseline-file,correctness-gate})、eval-whoknows(451, Rust 有 whoknows verb 但无 eval gate 层)、eval-replay(489)、eval-run-all(356)、eval-code-retrieval(260, 需 eval/code-retrieval/harness)、eval-compare(256, 需 metric-glossary)、eval-export(159)、eval-prune(115)、eval-brainstorm(399, 需 brainstorm/orchestrator)、eval-schema-authoring(137)。其中 export/prune/replay 共同依赖 eval_candidates 表——Rust migrations 里完全不存在，是先决条件。
【D 真 LLM】4 个：eval-cross-modal(849, 26 命中, G58 已列)、eval-longmemeval(887, 9 命中, G58 已列)、eval-takes-quality(259, 7 命中)、eval-suspected-contradictions(427, runContradictionProbe + judge)。

**决策：**
- Q: G74 标注的 blocked by G58（LLM seam）是否属实？ → 不属实。19 命令中真依赖 LLM 的只有 4 个（21%）：cross-modal、longmemeval、takes-quality、suspected-contradictions；其中前两个已由 G58 单列。其余 15 个不碰 LLM，G74 整体不应标 blocked。 (审计法：git show 3c09a69f^ 取回 20 个 eval*.ts（含 notability-eval.ts，不在 G74 的 19 个之列），regex 扫 isAvailable('chat')|ChatProvider|anthropic|openai|.chat(|messages.create|gateway。三个疑似命中经逐行核实为假阳性：eval-replay.ts:132 的 OpenAI 只在注释里且指 embedding 非 chat；eval-schema-authoring.ts:12 注释明说走 stubbed gateway test seam；eval-suspected-contradictions.ts:253 的 anthropic 是 resolveModel 的 fallback 模型 ID 字符串——但该文件确实调 runContradictionProbe + judge model + 成本预算门，属真 LLM。)

**当前子树：**
├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
├── [x][X+] 1-1-3. 判废真空壳 + 重分类 2 非空壳 eval 命令（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 cycle phase 已在 Rust）
├── [x] 1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 D 类）
└── [x][X+] 1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）
    ... 11 more child nodes; run tree 1-1-5 --depth 2 for full view
<!-- ROADMAP_SECTION_END -->
