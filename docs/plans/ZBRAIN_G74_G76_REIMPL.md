<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-09 10:49:00

[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [~][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
│   ├── [x] 1-1-1. 修正 KNOWN-GAPS/COMMANDS_TEAR_DOWN 的 G74 失准描述
│   ├── [x] 1-1-2. 第一刀：zbrain eval 核心 verb（暴露已 port 的 run_eval）
│   ├── [x] 1-1-3. 判废真空壳 + 重分类 2 非空壳（markdown-greenfield 判废；extract-atoms/synthesize-concepts 底层 phase 已在 Rust）
│   ├── [ ][X+] 1-1-4. 非 LLM 但需新基建的 10 个 eval 命令
│   └── [ ][X+] 1-1-5. 真 LLM 的 4 个 eval 命令（cross-modal / longmemeval / takes-quality / suspected-contradictions）
└── [~][X+] 1-2. G76 extract 族命令 Rust 重实现
    ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
    ├── [ ][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
    ├── [ ][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，blocked by G35）
    └── [ ][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb

### 当前施工：1-1-4. 非 LLM 但需新基建的 10 个 eval 命令

两处失准：① 「多数依赖 LLM/Anthropic SDK」→ 实为少数（4/19=21%）；② 状态标 open (blocked: LLM seam G58) → 整体 blocked 不成立，应为 partial：15/19 不阻塞、1 个真空壳可判废（eval-markdown-greenfield）；2 个（eval-extract-atoms/synthesize-concepts）复核非空壳、重分类为可 port eval 子命令、仅 4 个真 LLM（其中 2 个已由 G58 单列）。③ 「仅 eval_drift（语义不同）」也不全 —— 还有 search/eval.rs(run_eval + 4 IR 指标, G73)、cli/routing_eval.rs、skill_resolver/routing_eval.rs。保留单 G74 ID（照 G76 先例，拆分只做在描述里，避免打断代码指针）。
<!-- ROADMAP_SECTION_END -->
