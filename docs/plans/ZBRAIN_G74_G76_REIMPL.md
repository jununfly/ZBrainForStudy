<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-11 15:43:57

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

### 当前施工：1-1-5-3. eval-takes-quality（#61，新建 EvalTakesQuality variant）

CORRECTED scope (2026-08-11): not 259L — full surface = command 259L + takes-quality-eval module (runner 274 + receipt-write 94 + replay 83 + regress 96 + trend 83 = 630L) = ~889L TS + 12 test specs. Judge panel (DEFAULT_MODEL_PANEL, 3-model Promise.allSettled) ALREADY ported into cross_modal.rs (#59) -> reuse cross_modal::run_eval, do NOT re-port. takes_scorecard math already in calibration_queries.rs. Genuinely new: takes-specific runner + receipt/replay/regress/trend harness + CLI verb. Recover module from bcafcafd^ (src/core deleted by bcafcafd).

**决策：**
- Q: 端口范围？ → 待定（round 2 烤问） (raw 体量 ~889L 为三者最大；judge 复用 cross_modal，scorecard 复用 calibration_queries)
- Q: 顺序重确认（#61 体量已校正为最大）？ → 维持 #61→#63→#62：#61 复用度最高(judge+scorecard 已就位)+最自包含 (raw 体量 #61~889L>#62 427L>#63 399L，但 #61 新增集中且 judge/scorecard 大块已免做；#62 153-judge 矩阵、#63 orchestrator 前置为更差开局点)
- Q: 端口范围？ → MVP 先行：takes runner + 复用 cross_modal::run_eval judge + takes_scorecard 数学 + CLI verb；诚实 Err 无 key (与增量交付/诚实降级哲学一致；receipt/replay/regress/trend(356L) 拆后续子节点)
- Q: judge 复用？ → 复用 cross_modal::run_eval（三模型 panel），不重 port DEFAULT_MODEL_PANEL (同一套并行 judge 范式，重 port 违反 DRY 且 verdict 汇总语义分叉)

**当前子树：**
├── [x][X+] 1-1-5-3-1. eval-takes-quality MVP: takes runner + CLI verb + honest Err(no key)
└── [ ][X+] 1-1-5-3-2. eval-takes-quality harness: receipt/replay/regress/trend playback (356L TS)
<!-- ROADMAP_SECTION_END -->
