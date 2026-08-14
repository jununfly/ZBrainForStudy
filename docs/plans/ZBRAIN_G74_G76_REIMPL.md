<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-12 20:41:30

[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [~][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
│   ├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
│   ├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
│   ├── [x][X+] 1-1-3. 判废真空壳 + 重分类 2 非空壳 eval 命令（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 cycle phase 已在 Rust）
│   ├── [x] 1-1-4. 非 LLM 但需新基建的 9 个 eval 命令（eval-brainstorm 复核为 LLM 已移 D 类）
│   └── [~][X+] 1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）
└── [~][X+] 1-2. G76 extract 族命令 Rust 重实现
    ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
    ├── [ ][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
    ├── [ ][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，blocked by G35）
    └── [ ][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb

### 当前施工：1-1-5. 真 LLM 的 5 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions / brainstorm）

5 个真 LLM eval 命令（D 类；eval-brainstorm 由 1-1-4 的 C 类复核中移入）。通用 LLM seam（ChatProvider + ai/tool_loop.rs）已 port 完，理论上不阻塞，但各自需 port 领域 runner。保真策略用户确认 q-0 全保真 port。进度：#59 cross-modal 已完成（1-1-5-1，core+CLI+e2e 全绿）；#60 longmemeval / #61 takes-quality / #62 suspected-contradictions / #63 brainstorm 待启动（各建独立子节点）。后续族开工前一律先「TS 源 vs Rust 现状」对账。

**决策：**
- Q: 保真度策略（5 族统一）？ → q-0 → 全保真 port（逐文件搬，最接近 TS 原貌） (用户 2026-08-09 在「开始 1-1-5」时确认；TS 源从 git 历史 3c09a69f^ 取回)
- Q: 下一步优先级？ → 继续 1-1-5，收口 D 类 5 个真 LLM eval 命令 (G58 已关；剩余 3 项有界(259/399/427L TS 可恢复)；1-2 有 G35 阻塞+1-2-4 未决，不宜并行)
- Q: 3 个命令先后顺序？ → #61 takes-quality → #63 brainstorm → #62 suspected-contradictions (#61 最小且有 takes_scorecard Rust 杠杆；#63 需先 port brainstorm orchestrator 前置；#62 逻辑面最大(153 judge) 放最后)
- Q: 无 API key 时构建/验收策略？ → 沿用 G58：provider 无关 + mock 单测，无 key 也 cargo test 绿、无 provider 诚实 Err (不为 API key 或 libsql Windows FFI flake 卡构建)
- Q: 下一步优先级重排（推翻 2026-08-09 的 #61→#63→#62）？ → 翻转为 #61 → #62 → #63（#62 优先） (2026-08-12 对账 git 历史 39e14cd5：#63 实为完整 generator 移植（4 引擎方法缺失 + orchestrator/domain-bank/judges 三模块 + search/embed 集成，多会话量级），原「#63 只需 port 3 引擎方法」前提不成立。#62 复用 cross_modal judge 基础设施、零新引擎方法、无 generator，单会话可收口，故优先。)
- Q: 是否修正 1-1-5-5 过时描述并给 1-1-5-4 补收敛说明？ → 是 (1-1-5-5：「3 引擎方法」→ 真实依赖面（4 引擎方法 + 三模块 + search/embed）；1-1-5-4：补「复用 cross_modal，收敛、无新引擎方法」。地图须反映真实范围，避免后续 agent 按低估前提误判。)
- Q: Q6 #62 之后优先级 → 先补 #62 延伸 trend(1-1-5-6)→review(1-1-5-7)，再 #63 brainstorm(1-1-5-5) (trend/review 是小延伸、直接完善 #62 价值；#63 是独立大块宜在 #62 完全收口后做)

**当前子树：**
├── [x] 1-1-5-1. eval-cross-modal（#59 全保真 port，core+CLI+e2e 全绿）
├── [x][X+] 1-1-5-2. eval-longmemeval（#60，替换 G58 占位）
├── [x][X+] 1-1-5-3. eval-takes-quality（#61，新建 EvalTakesQuality variant）
│   ... 2 more child nodes; run tree 1-1-5-3 --depth 2 for full view
├── [x][X+] 1-1-5-4. eval-suspected-contradictions（#62，最大一族 judge×153）
├── [ ][X+] 1-1-5-5. eval-brainstorm（#63，完整 generator 移植：4 引擎方法 + orchestrator/domain-bank/judges 三模块 + search/embed）
├── [x][X+] 1-1-5-6. eval-suspected-contradictions trend 子命令（#62 延伸：write/loadContradictionsRun 引擎方法 + ASCII 图表）
├── [x][X+] 1-1-5-7. eval-suspected-contradictions review 子命令（依赖 trend store report_json viewer）
└── [ ][X+] 1-1-5-8. eval-suspected-contradictions JudgeCache 持久化 judge 缓存（独立性能优化，正交）
<!-- ROADMAP_SECTION_END -->
