<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part3-release-and-ts-retirement.json` | 最后更新: 2026-07-08 14:17:18

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
└── [ ][X+] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
    ├── [x][X+] 1-4-1. hybrid + vector 检索地基: Rust 侧从零建 embedding 写入/查询 + cosine 重打分 + RRF 融合(跨 InMemory/postgres/libsql 引擎), 为 rerank/attribution 提供真实融合分数 base_score
    ├── [~][X+] 1-4-2. rerank 服务层: Rust gateway rerank client(外部 cross-encoder, fail-open) + rerank-audit JSONL + doctor reranker_health 真实断言(替换 UNMIGRATED_TS 留痕)
    └── [!][X+] 1-4-3. explain 分阶段归因输出(--explain flag): 展示层, 依赖 1-4-1/1-4-2 在 SearchResult 盖的 base_score/boost_*/reranker_delta 字段, 逐结果逐阶段乘子分解

### 当前施工：1-4-2-2. rerank HTTP client + call-site 接线: Rust gateway rerank client(reqwest POST 外部 cross-encoder, RerankError 分类 + fail-open) 接进 query 搜索管道后处理阶段, 失败写 1-4-2-1 的 audit

**决策：**
- Q: 这一刀完成形态: 真 client + 接线, 还是骨架/fail-fast? → a: 完整迁 client(reqwest 真 POST + RerankError 分类 + payload 预检 + 超时) + operation 层 fail-open 接线, 由 reranker_enabled 门控(默认 false); TDD 用 RerankClient trait + mock 断言分类/fail-open/audit 写入, 真实 HTTP 路径不进单测 (rerank 不依赖 embedding(cross-encoder 直吃文本), 无理由捆绑等真实 embedding provider; audit writer 上一刀即为此 producer 而建, 不接线则一直无生产调用点; fail-fast(C)违背 TS fail-open 契约(rerank 失败必须降级 RRF 不能挂搜索); mock 单测对齐本机无 ZE key 验证边界 + embedding.rs MockProvider 先例; base_score 已是真实融合分数, 非 horizontal slicing)
- Q: rerank 接入点位置与范围: 只接 query 还是连 Think/evidence? → A+新建node: 本刀只接 QueryOperation::execute(1788 search_pages 之后/分页 map 之前); Think/evidence 内部检索路径(operation.rs:1562)本刀不接, 拆新 sub-node 承载; Think 检索点留自解释注释(不引 roadmap 编号) (rerank 是给用户看的最终排序后处理, 接用户显式 query 路径语义最清晰最易测; Think 内部检索是否 rerank 是独立产品判断(多一次外部 API 往返+增 Think 延迟), 不该顺手决定; engine 层排除(纯 trait 双实现 InMemory/Postgres 塞 HTTP 污染且拿不到 audit_dir); CLI 层排除(拿不到 base_score); operation 层可拿 Vec<SearchResult> 做后处理+fail-open 回退 RRF 序最干净)
- Q: rerank provider config 与 API key 来源: 全量多-provider 配置 vs 最小单-provider? → A: 最小 config 字段 + 从 env 读 key, 本刀只支持 ZeroEntropy 单 provider (对齐 embedding provider 先例(单 provider 起步, MockProvider 测试); 多-provider 抽象是投机(YAGNI), 现在没有第二个 reranker; key 走 env 不入 config 文件(secret 不落盘, 对齐 embedding provider key 处理); config 仅需 reranker_enabled(已在 1-4-2-1 加) + 可能 reranker_model/endpoint, 具体字段在 TDD 时按 TS gateway rerank 契约最小化)
- Q: rerank 产物字段: 本刀给 SearchResult 加哪些字段? → 同时加 rerank_score + reranker_delta 两个字段 (reranker_delta(原index-新pos)是 head 重排那一刻天然算出的副产品, 此刻不存则 1-4-3 需重新推导, 就近落盘更省事; 两字段均 Option, 未 rerank(tail/关闭)时为 None; 不覆盖 base_score(RRF 融合分, 对齐 TS 不覆盖 score); 1-4-3 --explain 直接读这两字段做乘子分解无需重算)
- Q: rerank head topN 默认值与归属? → 参照 TS: 硬编码常量 DEFAULT_RERANK_TOP_N=30, 不入 config (TS applyReranker topNIn 默认 30(只 rerank 前 30, tail 保留原 RRF 序追加); 硬编码常量镜像 TS 现状, 不发明 config 可调面(YAGNI, 无人要求调 topN); 全量 rerank 会偏离 TS 且大结果集撞 5MB payload 上限; 常量放 rerank client 模块与 RRF_K/max_payload_bytes 并列, 自解释注释标 TS 来源)
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
└── [ ][X+] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
    ├── [x][X+] 1-4-1. hybrid + vector 检索地基: Rust 侧从零建 embedding 写入/查询 + cosine 重打分 + RRF 融合(跨 InMemory/postgres/libsql 引擎), 为 rerank/attribution 提供真实融合分数 base_score
    ├── [x][X+] 1-4-2. rerank 服务层: Rust gateway rerank client(外部 cross-encoder, fail-open) + rerank-audit JSONL + doctor reranker_health 真实断言(替换 UNMIGRATED_TS 留痕)
    └── [!][X+] 1-4-3. explain 分阶段归因输出(--explain flag): 展示层, 依赖 1-4-1/1-4-2 在 SearchResult 盖的 base_score/boost_*/reranker_delta 字段, 逐结果逐阶段乘子分解
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
- [ ] 1-4. search rerank + 分阶段归因子系统迁移(--explain): Rust query 现为硬编码关键字加权，需迁 rerank/boost/attribution stages (doctor reranker_health=UNMIGRATED_TS)
<!-- ⚠️ ROADMAP_SECTION_END -->
