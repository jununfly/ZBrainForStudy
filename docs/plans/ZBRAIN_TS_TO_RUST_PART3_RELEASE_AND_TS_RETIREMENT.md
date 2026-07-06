<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part3-release-and-ts-retirement.json` | 最后更新: 2026-07-06 19:15:23

[~][X+] 1. ZBrain TS→Rust Part3: 发布链迁移 + 子系统补齐 + TS 入口退役
├── [ ][X+] 1-1. 发布基础设施迁移: 交叉编译多平台 Rust 二进制 + openclaw bundle-plugin 清单 serve/serve-mcp 语义对齐 + 二进制命名对齐
├── [ ][X+] 1-2. 子系统补齐(1-8 审计移交): MCP timeout (--timeout) + progress reporter (--quiet/--progress-json/--progress-interval)
│   ├── [x][Y+] 1-2-1. MCP per-call timeout wiring (--timeout): 把 timeout 值接进 McpClient 的 http Client，消费已有 RemoteMcpError::Timeout 路由骨架
│   ├── [ ][X+] 1-2-2. progress reporter (--quiet/--progress-json/--progress-interval): 迁 TS src/core/progress.ts 三态+interval 节流，横切 op 进度上报
│   └── [ ][X+] 1-2-3. local read-only wallclock timeout (--timeout 第二消费者): 迁 TS cli.ts:1125-1170 v0.41.6.0 特性——search 30s/sources-list 10s 的 connect+dispatch wallclock 超时 + exit 124；依赖 withTimeout/connectEngine-timeout 基础设施(Rust 未迁)
├── [ ][X+] 1-3. TS 入口整体退役: src/cli.ts + postinstall TS 兜底 + check-cli-executable.sh + src/commands 未迁命令(依赖发布链切 Rust 完成)
└── [ ][X+] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)

### 当前施工：1. ZBrain TS→Rust Part3: 发布链迁移 + 子系统补齐 + TS 入口退役

**决策：**
- Q: part3 从何而来 + 三节点是否已确认实施顺序？ → part2 节点 1-6(migration cleanup) Q4/Q5 决策移交: 1-6 只清能无损删的死残留(死 build script/死 allowlist 项/失效 build 命令文档链接)，扛不动且有本机验证盲区的(mac/linux 交叉编译产物 + openclaw 清单语义)诚实移交到此。1-1/1-2/1-3 仅为移交锚点，实施顺序与切片待开 part3 时用 grill-me 逐题确认，不代表已确认。 (1-3(TS退役)硬依赖 1-1(发布链切 Rust)完成，否则 src/cli.ts 仍是活发布入口不能删)
- Q: Q1: part3 本轮 grill 策略 + 第一刀选哪块？ → A: 本轮只定 part3 全局排序 + 聚焦第一刀展开到可 TDD 切片；1-1/1-3 保持 explore 锚点各自开工前再单独 grill。第一刀锁 1-2 子系统补齐 (第一刀选 1-2 而非 1-1: (1) 1-3 依赖 1-1、1-1 有本机验证盲区(mac/linux 交叉编译无法在此验证)——两块都带外部阻塞; (2) 1-2 三 flag 落点 1-8 已 audit 清楚有 FUTURE 锚点、纯 Rust 内可测可验、唯一能立即 TDD 无盲区的块。符合先动能动的、诚实对待扛不动的、不做一次性大设计)

**当前子树：**
├── [ ][X+] 1-1. 发布基础设施迁移: 交叉编译多平台 Rust 二进制 + openclaw bundle-plugin 清单 serve/serve-mcp 语义对齐 + 二进制命名对齐
├── [ ][X+] 1-2. 子系统补齐(1-8 审计移交): MCP timeout (--timeout) + progress reporter (--quiet/--progress-json/--progress-interval)
│   ... 3 more child nodes; run tree 1-2 --depth 2 for full view
├── [ ][X+] 1-3. TS 入口整体退役: src/cli.ts + postinstall TS 兜底 + check-cli-executable.sh + src/commands 未迁命令(依赖发布链切 Rust 完成)
└── [ ][X+] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
<!-- ROADMAP_SECTION_END -->
