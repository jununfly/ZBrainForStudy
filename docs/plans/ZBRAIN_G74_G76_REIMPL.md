<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-g74-g76-reimpl.json` | 最后更新: 2026-08-09 09:25:00

[~][X+] 1. ZBrain G74/G76 eval+extract 命令 Rust 重实现
├── [ ][X+] 1-1. G74 eval 族命令 Rust 重实现（19 命令）
└── [~][X+] 1-2. G76 extract 族命令 Rust 重实现
    ├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
    ├── [ ][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
    ├── [ ][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，blocked by G35）
    └── [ ][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb

### 当前施工：1-2. G76 extract 族命令 Rust 重实现

G76 能力对账表（TS extract vs Rust）：links --source db → auto_fix::extract_links + CLI links rebuild-md-links 已覆盖；timeline --source db → 算法已实现缺 verb；all --source db → run_auto_fix 近似覆盖；--source fs --dir → 缺；--by-mention → 缺；extract-conversation-facts → 缺(真 LLM)。 【进度】1-2-1 已完成（extract verb 落地 + SSOT 修正），剩 1-2-2(fs-source) / 1-2-3(conversation-facts, blocked G35)。

**决策：**
- Q: G76 第一刀切哪个切片？ → extract 单命令端到端垂直切片：从 git 历史取回 extract.ts + extract-facts 算法，用 ChatProvider/tool_loop 重写抽取逻辑，接成 extract verb，TDD(red→green, MockChatProvider) 验证 seam 可行后，再批量扩 extract-conversation-facts + G74 的 19 命令 (【已作废】此决策的前提（extract 依赖 LLM）经源码核实为假，见下一条决策)
- Q: 【前提推翻后重选】extract.ts 实为纯解析且大半已实现，第一刀改选哪个？ → 选 A：先修 SSOT（KNOWN-GAPS G76 拆为 G76a 非阻塞 / G76b 真 blocked by G35）+ 把已实现的 auto_fix::extract_timeline 暴露为独立 CLI verb。成本最低、顺带关掉 G76 一半、且先让真相源恢复准确。fs-source 与 conversation-facts 顺延为 1-2-2 / 1-2-3。 (对账证据：extract.ts grep chat|llm|openai|anthropic|ChatProvider 零命中；用 extractPageLinks/parseTimelineEntries 纯解析 + engine.addLinksBatch/addTimelineEntriesBatch。Rust 侧 auto_fix.rs:179 extract_links 已由 CLI `links rebuild-md-links` 暴露(G77-1)，auto_fix.rs:319 extract_timeline 已实现但仅被 run_auto_fix(lib.rs:5153) 内部调用。真 LLM 的只有 extract-conversation-facts(isAvailable('chat')+分段+insertFacts+断点续跑审计行)。)

**当前子树：**
├── [x] 1-2-1. 修正 KNOWN-GAPS G76 描述 + 新增顶层 extract verb（links/timeline/all）
├── [ ][X+] 1-2-2. G76a 补齐：--source fs 文件系统抽取路径（含 --by-mention）
├── [ ][X+] 1-2-3. G76b：extract-conversation-facts（真 LLM，blocked by G35）
└── [ ][X+] 1-2-4. 决策：minion extract job type 是否接线到新 extract verb
<!-- ROADMAP_SECTION_END -->
