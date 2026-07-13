<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part3-release-and-ts-retirement.json` | 最后更新: 2026-07-13 17:21:52

[x][X+] 1. ZBrain TS→Rust Part3: 发布链迁移 + 子系统补齐 + TS 入口退役
├── [x][X+] 1-1. 发布基础设施迁移: 交叉编译多平台 Rust 二进制 + openclaw bundle-plugin 清单 serve/serve-mcp 语义对齐 + 二进制命名对齐
│   ├── [x][X+] 1-1-1. openclaw manifest serve 语义对齐 + bin 路径修复: mcpServers command/args 指向存在的二进制且启动 stdio MCP(serve-mcp 而非 serve=HTTP)
│   ├── [x][X+] 1-1-2. 二进制命名对齐: 决定 ./bin/zbrain 是创建实体 vs manifest 改指 bin/zbrain-rs.js; 统一 npm bin / openclaw / Rust crate 三方命名
│   └── [x][X+] 1-1-3. Rust 交叉编译 + 发布管线: 解除 .cargo/config.toml host-target 硬编码, 多平台 cargo build, 退役 TS build:all/release.yml(gbrain 命名 + bun 编译 TS)
├── [x][X+] 1-2. 子系统补齐(1-8 审计移交): MCP timeout (--timeout) + progress reporter (--quiet/--progress-json/--progress-interval)
│   ├── [x][Y+] 1-2-1. MCP per-call timeout wiring (--timeout): 把 timeout 值接进 McpClient 的 http Client，消费已有 RemoteMcpError::Timeout 路由骨架
│   ├── [x][X+] 1-2-2. progress reporter (--quiet/--progress-json/--progress-interval): 迁 TS src/core/progress.ts 三态+interval 节流, 接 sync perform_full_sync per-path 循环真实 .tick()
│   ├── [x][Y+] 1-2-3. local read-only wallclock timeout (--timeout 第二消费者): 迁 TS cli.ts:1125-1170 v0.41.6.0 特性——search 30s/sources-list 10s 的 connect+dispatch wallclock 超时 + exit 124；依赖 withTimeout/connectEngine-timeout 基础设施(Rust 未迁)
│   └── [x][X+] 1-2-4. [possible-bug-parity] TS search 本地 wallclock 潜伏 bug: cli.ts:1136 'search->30s' 是死代码(search 是 shared op 永不进 handleCliOnly), 本地 search/query 无 connect/dispatch wallclock, hang-in-connect 无限挂起。决定 Rust query 是否主动补 wallclock(修 TS 从未生效的意图) vs 保持纯移植不发明行为
├── [x][X+] 1-3. TS 入口整体退役: src/cli.ts + postinstall TS 兜底 + check-cli-executable.sh + src/commands 未迁命令(依赖发布链切 Rust 完成)
└── [x][X+] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
    ├── [x][X+] 1-4-1. hybrid + vector 检索地基: Rust 侧从零建 embedding 写入/查询 + cosine 重打分 + RRF 融合(跨 InMemory/postgres/libsql 引擎), 为 rerank/attribution 提供真实融合分数 base_score
    ├── [x][X+] 1-4-2. rerank 服务层: Rust gateway rerank client(外部 cross-encoder, fail-open) + rerank-audit JSONL + doctor reranker_health 真实断言(替换 UNMIGRATED_TS 留痕)
    ├── [x][X+] 1-4-3. explain 分阶段归因输出(--explain flag): 展示层, 依赖 1-4-1/1-4-2 在 SearchResult 盖的 base_score/boost_*/reranker_delta 字段, 逐结果逐阶段乘子分解
    └── [x][X+] 1-4-4. boost metadata-axis 子系统迁移: Rust query 补齐 TS post-fusion boost 阶段(backlink/salience/recency/exact-match/graph-signals/source-boost + floor-threshold gate), 填 --explain 三档归因的中间 boost 档; 按数据就绪度分层, 数据未迁的 boost 不硬做
<!-- ROADMAP_SECTION_END -->

<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## ZBrain TS→Rust Part3: 发布链迁移 + 子系统补齐 + TS 入口退役

### 树形视图 (depth=2)

```
[~][X+] 1. ZBrain TS→Rust Part3: 发布链迁移 + 子系统补齐 + TS 入口退役
├── [x][X+] 1-1. 发布基础设施迁移: 交叉编译多平台 Rust 二进制 + openclaw bundle-plugin 清单 serve/serve-mcp 语义对齐 + 二进制命名对齐
│   ├── [x][X+] 1-1-1. openclaw manifest serve 语义对齐 + bin 路径修复: mcpServers command/args 指向存在的二进制且启动 stdio MCP(serve-mcp 而非 serve=HTTP)
│   ├── [x][X+] 1-1-2. 二进制命名对齐: 决定 ./bin/zbrain 是创建实体 vs manifest 改指 bin/zbrain-rs.js; 统一 npm bin / openclaw / Rust crate 三方命名
│   └── [x][X+] 1-1-3. Rust 交叉编译 + 发布管线: 解除 .cargo/config.toml host-target 硬编码, 多平台 cargo build, 退役 TS build:all/release.yml(gbrain 命名 + bun 编译 TS)
├── [ ][X+] 1-2. 子系统补齐(1-8 审计移交): MCP timeout (--timeout) + progress reporter (--quiet/--progress-json/--progress-interval)
│   ├── [x][Y+] 1-2-1. MCP per-call timeout wiring (--timeout): 把 timeout 值接进 McpClient 的 http Client，消费已有 RemoteMcpError::Timeout 路由骨架
│   ├── [!][X+] 1-2-2. progress reporter (--quiet/--progress-json/--progress-interval): 迁 TS src/core/progress.ts 三态+interval 节流，横切 op 进度上报【BLOCKED: 需先有首个带 per-item tick 循环的消费者命令】
│   ├── [x][Y+] 1-2-3. local read-only wallclock timeout (--timeout 第二消费者): 迁 TS cli.ts:1125-1170 v0.41.6.0 特性——search 30s/sources-list 10s 的 connect+dispatch wallclock 超时 + exit 124；依赖 withTimeout/connectEngine-timeout 基础设施(Rust 未迁)
│   └── [x][X+] 1-2-4. [possible-bug-parity] TS search 本地 wallclock 潜伏 bug: cli.ts:1136 'search->30s' 是死代码(search 是 shared op 永不进 handleCliOnly), 本地 search/query 无 connect/dispatch wallclock, hang-in-connect 无限挂起。决定 Rust query 是否主动补 wallclock(修 TS 从未生效的意图) vs 保持纯移植不发明行为
├── [ ][X+] 1-3. TS 入口整体退役: src/cli.ts + postinstall TS 兜底 + check-cli-executable.sh + src/commands 未迁命令(依赖发布链切 Rust 完成)
└── [x][X+] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
    ├── [x][X+] 1-4-1. hybrid + vector 检索地基: Rust 侧从零建 embedding 写入/查询 + cosine 重打分 + RRF 融合(跨 InMemory/postgres/libsql 引擎), 为 rerank/attribution 提供真实融合分数 base_score
    ├── [x][X+] 1-4-2. rerank 服务层: Rust gateway rerank client(外部 cross-encoder, fail-open) + rerank-audit JSONL + doctor reranker_health 真实断言(替换 UNMIGRATED_TS 留痕)
    ├── [x][X+] 1-4-3. explain 分阶段归因输出(--explain flag): 展示层, 依赖 1-4-1/1-4-2 在 SearchResult 盖的 base_score/boost_*/reranker_delta 字段, 逐结果逐阶段乘子分解
    └── [x][X+] 1-4-4. boost metadata-axis 子系统迁移: Rust query 补齐 TS post-fusion boost 阶段(backlink/salience/recency/exact-match/graph-signals/source-boost + floor-threshold gate), 填 --explain 三档归因的中间 boost 档; 按数据就绪度分层, 数据未迁的 boost 不硬做
```

### 🔨 当前施工: 1. ZBrain TS→Rust Part3: 发布链迁移 + 子系统补齐 + TS 入口退役
**Status:** `in_progress` | **Mode:** `explore`

**决策记录:**
- Q: part3 从何而来 + 三节点是否已确认实施顺序？
  A: part2 节点 1-6(migration cleanup) Q4/Q5 决策移交: 1-6 只清能无损删的死残留(死 build script/死 allowlist 项/失效 build 命令文档链接)，扛不动且有本机验证盲区的(mac/linux 交叉编译产物 + openclaw 清单语义)诚实移交到此。1-1/1-2/1-3 仅为移交锚点，实施顺序与切片待开 part3 时用 grill-me 逐题确认，不代表已确认。
  > 1-3(TS退役)硬依赖 1-1(发布链切 Rust)完成，否则 src/cli.ts 仍是活发布入口不能删
- Q: Q1: part3 本轮 grill 策略 + 第一刀选哪块？
  A: A: 本轮只定 part3 全局排序 + 聚焦第一刀展开到可 TDD 切片；1-1/1-3 保持 explore 锚点各自开工前再单独 grill。第一刀锁 1-2 子系统补齐
  > 第一刀选 1-2 而非 1-1: (1) 1-3 依赖 1-1、1-1 有本机验证盲区(mac/linux 交叉编译无法在此验证)——两块都带外部阻塞; (2) 1-2 三 flag 落点 1-8 已 audit 清楚有 FUTURE 锚点、纯 Rust 内可测可验、唯一能立即 TDD 无盲区的块。符合先动能动的、诚实对待扛不动的、不做一次性大设计

**子节点:**
- [x] 1-1. 发布基础设施迁移: 交叉编译多平台 Rust 二进制 + openclaw bundle-plugin 清单 serve/serve-mcp 语义对齐 + 二进制命名对齐
- [ ] 1-2. 子系统补齐(1-8 审计移交): MCP timeout (--timeout) + progress reporter (--quiet/--progress-json/--progress-interval)
- [ ] 1-3. TS 入口整体退役: src/cli.ts + postinstall TS 兜底 + check-cli-executable.sh + src/commands 未迁命令(依赖发布链切 Rust 完成)
- [x] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
<!-- ⚠️ ROADMAP_SECTION_END -->
