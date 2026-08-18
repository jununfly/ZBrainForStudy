# ZBrain 迁移总览（TS → Rust）

> **单一长期文档**（2026-08-16 整合自 `KNOWN-GAPS.md` + 13 份 Part 路线图 + 各类审计/交接文档）。
> 配套实时收尾地图：`roadmap-remaining.json`（未完成事项节点，zj-roadmap-driven 格式）。
> 本文件取代历史上散落的 `KNOWN-GAPS.md`、各 `ZBRAIN_TS_TO_RUST_PART*` 文档、`*audit*.md`、`*handoff*.md`、`COMMANDS_TEAR_DOWN.md` 等——它们已合并后删除（见 §6）。

## 1. 迁移状态裁定（2026-08-16 Grilling）

- **命令级完成即终点**（Q1b）：Rust 已服务全部产品命令，CLI verb 全覆盖（含 Tier-C 五命令 `export`/`frontmatter`/`auth`/`providers`/`upgrade`）。`bin/zbrain-rs.js` 直接跑 Rust 二进制，TS 运行时已死。
- 残留事项（`KNOWN-GAPS` 的 open/blocked 条目）作为 **documented limitations** 保留，不阻塞「迁移完成」声明。
- **安全闸门 G55**（remote MCP 不透传 `takes_holders`）按 Q3b 维持已知缺口（本地/受信部署）；若 `serve --http` MCP 对不受信外部客户端暴露则升级为 must-fix 安全闸门（AuthInfo 增 `takes_holders` + transport 填充）。
- **TS 死代码**（`src/` 树）依 Q1b 保留为 documented limitation，**不删**（G38 frontier 裁定）。

## 2. 各阶段成果（Part 1–14 蒸馏）

| Part | 主题 | 关键成果 |
|------|------|----------|
| 1 | Core Storage Parity | Page CRUD / InMemory / PostgreSQL / libsql 合约闭合 |
| 2 | Config / Bootstrap / Package 入口 | init/config/doctor strict flag parity + bin 透传 |
| 3 | 发布链迁移 + TS 入口退役 | 交叉编译多平台 + `serve`/`serve-mcp` 对齐 + MCP timeout / progress reporter 接线 |
| 4 | Phase 6 摄入/源/搜索/检索 | 源管理 + 递归分块(CJK) + `fuse_and_boost` 融合 + libsql/postgres `search_pages` + query embedding |
| 5 | Phase 7 Facts/Takes/Timeline/Graph | takes 全 schema + fence 解析 + backlinks + facts CRUD + salience + graph traverse |
| 6 | Phase 8 AI Gateway | model registry + resolver/routing + ChatProvider(OpenAI/Anthropic/Google) + toolLoop + BudgetTracker |
| 7 | Phase 9 Jobs/Minions/Autopilot | MinionQueue + Worker/Supervisor + Budget/rate-leases + handlers/tools + fanout + Remote + webhook 接入队列 |
| 8 | Phase10 eval 处置 | longmemeval/cross-modal 留 Rust seam 决策；其余 eval 子命令后续切片 |
| 9 | Phase11 package cutover | package.json cutover + src/core 已迁 impl 分批删除 + cli.ts 退役 |
| 10 | Phase12 Schema-Pack (G4) | 32-verb schema-pack 全量迁移（inspection/activation/authoring/discovery/repair） |
| 11 | 残留 TS 收尾（综合容器） | skillify / eval / calibration / output / doctor / 孤儿命令 / search / facts / think / cycle / operations.ts 替换式迁移规划 |
| 12 | cycle 大迁移 | runCycle→`run_cycle` + 48 phase arms（extract-facts/atoms/takes/calibration/consolidate/phantom-redirect/drift 等） |
| 13 | minion/MCP handler 接线 | cycle-phase handlers delegate `run_cycle`；CLI verb delegate；图维护族 verb（backfill/reconcile/edges-backfill） |
| 14 | 收尾地图 | 本文件 + `roadmap-remaining.json`（未完成事项实时索引） |

## 3. 决策记录（Grilling 2026-08-16）

| 决策 | 裁定 | 备注 |
|------|------|------|
| Q1 迁移完成口径 | (b) 命令级完成即终点 | 450/450 路线图节点全 completed 但系统性滞后（节点只作索引）；真实剩余在 KNOWN-GAPS（54 条 open/blocked）。bin/zbrain-rs.js 直跑 Rust，TS 运行时已死 |
| Q3 安全闸门 G55 | (b) 本地/受信部署维持已知缺口 | 若 serve --http 对不受信外部暴露→升级 must-fix（AuthInfo 增 takes_holders + transport 填充） |
| Q4 路线图脱节 | (a) 新建 part14 路线图 | 现收敛为 `roadmap-remaining.json`（本文件 §4 的实时索引） |
| G38 删死 src/ 树 | 不删 | operations.ts 4 个 schema 操作经 grep 零活调用方，前置门槛解除但按 Q1b 保留为 documented limitation |

早期铁律（沿用）：Rust 迁成一块删一块；诚实降级（无 key/缺依赖须 EXIT1+清晰报错，不伪造 PASS）；feat/chore 分离、不自动 commit；移植前必做「TS 源 vs Rust 现状」逐能力对账（KNOWN-GAPS 描述曾两次证伪：G76/G74 算法已在 Rust，缺的只是 CLI 出口）。

## 4. 未完成事项（documented limitations）

> 共 **54 条：48 open + 6 blocked**。完整节点树见 `roadmap-remaining.json`（8 簇 / 54 叶，每叶双向指回本文件 G-id）。完成一项即从 roadmap 标 completed 并从下表移除。

### 4.1 Progress reporter 打磨（输出保真，7 open）
- **G14** 缺 SIGINT/SIGTERM 信号协调器
- **G15** 缺 EPIPE 防御
- **G16** 缺 `child()` 嵌套工厂
- **G17** 缺 heartbeat timer
- **G18** 缺 TTY `\r` 重写模式
- **G19** 缺 source-prefix 注入
- **G20** 缺 abort 事件

### 4.2 Doctor 收尾（剩余检查，7 open）
- **G5** 切片封顶：剩余 5 项锚点（search_mode/federation/schema_packs/resolver/frontmatter_integrity）
- **G39** frontmatter_integrity 检查（依赖 scanBrainSources）
- **G40** search_mode 检查（依赖 search-mode 系统 + trait 配置读取）
- **G43** federation_health 检查（依赖源指标基建）
- **G44** schema_packs 检查（依赖 operations.ts 消费者）
- **G45** resolver_health 检查（依赖 check-resolvable.ts）
- **G47** 缺 features teaser 末行提示

### 4.3 Facts 子系统（MCP op + 实体解析 + 注入，4 open）
- **G35** facts MCP op 层未移植（extract_facts/recall/forget_fact/anti-loop/backstop）
- **G53** recall/trajectory 的 entity 参数不做实体解析
- **G54** facts 自动抽取 + hot-memory `_meta` 注入子系统缺失
- **G60** extract_facts phase 不生成 fact embeddings（fail-open）

### 4.4 Search 增强（1 open + 5 blocked）
- **G26** query expansion 无真 structured-output provider（仅 trait seam）— open
- **G67** cosineReScore 阶段未移植（需 chunk 级 embedding）— blocked
- **G68** LLM 模态意图分类未移植（需 chat seam）— blocked
- **G69** hybridSearchCached 语义查询缓存未移植（需 pgvector）— blocked
- **G70** 多列 embedding 选择 — done (2026-08-18): page-level `embedding_multimodal` (migration 0038, sqlite+postgres); `Page`/`ChunkInput` gain the field; `search_pages_by_embedding(column)` + `search_pages` column-swap route text vs multimodal on all 3 backends; `put_page_multimodal_embedding` on all 3 backends; reindex multimodal backfills page vector via mean-pool; `validate_embedding_column` accepts `embedding_multimodal` (`QueryParams::validate` hard-rejects others); CLI `--embedding-column` + `search.embedding_column` config; e2e `multimodal_embedding_column_e2e` passes.
- **G71** think vector-takes 流未移植（takes 表无 embedding 列）— blocked

### 4.5 Engine / 保真小缺口（16 open）
- **G8** InMemoryEngine 不存 code edges
- **G9** InMemoryEngine api_key 增删未实现
- **G10** `import_code_file` 空壳（需 tree-sitter chunker）
- **G11** `count_pages` 未进 BrainEngine trait
- **G12** libsql 非单线程序列化访问（需消息循环）
- **G13** boost metadata-axis 部分未迁 + strength 硬编码
- **G22** takes `row_num` 后端约束分歧（libsql 缺 CHECK/1-based）
- **G30** minion 路径 chunk embedding 未生成
- **G46** `get_brain_stats` chunk/embedded 计数为 page-level 代理
- **G49** capture `--type` 与 page_type 混淆
- **G50** whoknows 两处 parity 差异（刻意不修）
- **G64** drift phase raw 查询 sqlite/postgres 占位符不兼容
- **G29** AI recipe 注册表（REGISTRY 已落地，待对账 gateway auth_env 硬编码）
- **G86** libsql migration `0035_content_chunks_edges_backfilled_at` 漏注册（registry 与磁盘 sql 文件 drift；PR0 commit 1 修，仍记 open 待下轮验证）
- **G87** `libsql_engine_migrations` `EXPECTED_VERSION` 硬编码漂移（32 不再匹配 0036；PR0 commit 1 改动态）
- **G88** libsql migration 测试需 e2e 幂护（新增 `00NN_*.sql` 自动跟踪 EXPECTED_VERSION + registry，PR0 commit 2 加测试）

### 4.6 命令 / 作业 / Misc（13 open）
- **G6** webhook 无 rate limiter
- **G27** minion 附件无外部存储路径（storage_uri 恒 NULL，忠实降级）
- **G28** `pause_job` 不能暂停 waiting-children 父 job（共有设计边界）
- **G36** `ZBRAIN_PLUGIN_PATH` subagent 插件发现未迁移
- **G51** integrity auto/review/reset-progress 未迁（check 已迁）
- **G52** mounts cache 聚合 publish 未迁（诚实 park）
- **G57** `jobs work` 仍是占位打印
- **G59** `undo_wave` 不执行 gstack-learnings scrub（best-effort 跳过）
- **G62** extract-atoms transcript 路径未迁
- **G63** `subagent_tool_executions` 写入侧未迁
- **G65** Lint phase 未移植（cycle 臂诚实 Skipped）
- **G74** eval 评测族命令仅缺 CLI 出口的部分子命令（B 类非 LLM）
- **G76** extract-conversation-facts（G76b 真 LLM，依赖 G35）

### 4.7 安全 / MCP 脱敏（2 open + 1 blocked）
- **G55** remote MCP 不透传 takes_holders（Q3b：本地/受信部署维持已知缺口）— open
- **G31** MCP 日志参数脱敏 `summarizeMcpParams` 未迁（隐私）— open
- **G32** MCP `_meta.brain_hot_memory` 注入未迁（依赖 facts G35）— blocked

### 4.8 TS 死代码清理（1 open）
- **G38** TS schema-pack core 26 文件删除 pending operations.ts（Q1b：保留为 documented limitation，不删 src/ 树）

### 已 resolved 概览
已解决缺口（历史在 git，不再单列）：G1, G3, G4, G7, G21, G23, G24, G25, G33, G34, G37, G41, G42, G48, G56, G58, G61, G66, G72, G73, G74b, G75, G77, G78, G79, G84, G85（及 G2/G4 等早期项）。其中 G78/G79 在 2026-08-16 收口：摄入/迁移族 7 wontfix + upgrade ported；杂项族 recall/brainstorm/lsd/export/frontmatter/auth/providers done + 8 wontfix。

### 已裁定 wontfix
- **G80** `lint` minion 作业 / **G81** `lint-fix` / **G82** `integrity-auto` / **G83** `sync-retry-failed` —— 对应 TS 命令已删、无 Rust 替代，handler 显式 `Unsupported`。
- G78/G79 内若干小众/dev/列表工具（bench-publish/files/founder-scorecard/friction/init-mode-picker/integrations/notability-eval/ze-switch 等）判废。

## 5. 命令覆盖映射（原 COMMANDS_TEAR_DOWN 精华）

Rust 已 1:1 接管的 TS 命令（删除铁律：Rust 迁成一块删一块）：search/pages/whoknows/integrity/storage/resolvers/orphans/features/publish/transcripts/code-*/skillpack + 后续补迁的 recall/reindex*/extract{links,timeline,all}/backfill*/reconcile-links/edges-backfill/auth/providers/frontmatter/export/upgrade。未覆盖命令的缺口登记于 §4（G74–G79）。

## 6. 整合来源索引（已合并后删除，便于追溯）

以下文件在 2026-08-16 整合进本文件 + `roadmap-remaining.json` 后从 `docs/plans/` 删除（内容无损沉淀，非信息丢失）：

- `zbrain-ts-to-rust-part1..part13-*.json` + `ZBRAIN_TS_TO_RUST_PART1..13*.md` —— 各阶段路线图与 SSOT
- `zbrain-ts-to-rust-part14-remaining-known-gaps.json` + `KNOWN-GAPS.md` —— 缺口登记（→ 本文件 §4 + roadmap）
- `2026-05-26-rust-rewrite-plan.md` / `2026-06-25-*/` / `2026-06-30-*/` / `2026-07-02-*/` / `2026-07-06-*/` / `20260629-*` / `2026-08-10-handoff.md` —— 各期审计与交接快照
- `OPERATIONS_TS_TO_RUST_AUDIT.md` / `RESIDUAL_TS_AUDIT.md` / `RESIDUAL-TS-INVENTORY.md` / `COMMANDS_TEAR_DOWN.md` / `CLI_CUTOVER_MANIFEST.md` —— 专项审计
- `ZBRAIN_G74_G76_REIMPL.md` + `zbrain-g74-g76-reimpl.json` / `ZBRAIN_LEGACY_RETIRE_REASSESSMENT.md` + `zbrain-legacy-retire-reassessment.json` —— 近期 reimpl 记录
- `1-6-orphan-audit.md` —— 孤儿命令审计
- `scripts/audit-orphan-commands.py` / `audit-trivial-deps.py` —— 审计脚本（其结论已沉淀，脚本随目录删除）

## 7. 维护约定

- 新缺口：在 `roadmap-remaining.json` 加叶节点（zj-roadmap-driven 格式）+ 在 §4 对应簇加一行；代码锚点注释写 `// registered in docs/plans/MIGRATION.md (Gxx)`。
- 完成一项：roadmap 标 `completed` + 从 §4 移除该行。
- 本文件是给人读的治理活文档，非运行时契约；不测纯文档存在性。
