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

### 当前施工：1-1-5-4. eval-suspected-contradictions（#62，最大一族 judge×153）

TS 源实为 18 文件子系统（orchestrator/judge/calibration/cost-tracker/severity-classify/trends/cache/cross-source/date-filter/auto-supersession/judge-errors），427 行命令文件严重低估体量。Rust 复用资产：facts/classify.rs（fact 级分类器 cosine+LLM）、calibration.rs:1103 部分镜像。

MVP 已交付（2026-08-12）：顶层动词 eval-suspected-contradictions（run/trend/review，仅 run 实装）+ 自有 query-conditioned one-call-one-pair judge（非 cross_modal panel）+ 现有 takes 语料配对发现 + 与 TS 一致的 6 类 verdict / severity 分类法 + judge-errors 一等公民 + 空语料诚实 Err。MVP 自身零新引擎方法。

retrieval 配对发现延伸已 port（2026-08-11）：新增 `PairingMode::Retrieval`（per-query `hybrid_search` 取 top-K 页面 → cross/intra 配对；cross = 页面 compiled_truth×页面，intra = 页面 compiled_truth×该页 takes，faithful 适配 TS chunk 级逻辑到 Rust 页面级）+ `TakesListOpts.page_ids`（三后端 list_takes 批量取 takes，faithful port of TS `listActiveTakesForPages`）+ CLI `--pairing retrieval --queries ... --top-k N`。无 embedding provider 时 hybrid_search fail-open 降级 keyword-only，离线可用；contradictions 模块 6 测试（含 2 retrieval 新测）+ cli 1 解析新测全绿。

DEFERRED：trend（run-row ASCII 图表 + DB）、review 子命令——依赖 TS 专属的 trends/cache 子系统，Rust 尚未 port。

诚实缺口：无 LLM key 时 judge 调用失败 → judge-errors 计入分母（已测）；不伪 PASS。
<!-- ROADMAP_SECTION_END -->
