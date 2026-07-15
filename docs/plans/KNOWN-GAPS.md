# KNOWN-GAPS — ZBrain 已知缺口登记表 (SSOT)

> **用途**：这是 ZBrain 仓库**已知缺口的唯一权威索引 (Single Source of Truth)**。
> 凡是"当前 Rust 实现相对 TS 权威运行时存在的、已经知道但尚未补齐的空白"——
> 无论它当前以何种形式暂存（散落 `FUTURE(tag)` 注释、`UNMIGRATED_TS_*` 常量+锚点测试、
> 就近 `TODO`、还是完全无载体）——都必须在此登记。
>
> 设计原则：**毒草要长在阳光之下**。已知问题集中显式保存，便于后续统一规划、
> 排优先级、避免"清理 roadmap 后死链遗忘"。散落注释易被遗忘、新建 roadmap 节点会
> 污染树/卡死父子状态，都不是合适的缺口载体；本文档是持久、可引用的折中载体。

## 维护约定

1. **无日期前缀 = 活文档**。区别于 `docs/plans/YYYY-MM-DD-*.md`（一次性审计快照）。
   本表持续增删改，不冻结。
2. **发现新缺口 → 随手登记一行**。目标范围外、又不值得单独建 roadmap 节点的缺口，
   在完成当前工作的同一刀里补一行即可。
3. **双向指针**：代码里的 `FUTURE(tag)` 注释和 `UNMIGRATED_TS_*` 常量各保留一行
   `// registered in docs/plans/KNOWN-GAPS.md`；本表"现载体"列反向指回代码锚点。
   从任一端都能发现另一端。
4. **不搬运硬清单**：`UNMIGRATED_TS_*` 常量+锚点测试是"防误删漂移"的硬防线，
   原样留在代码里（删了失去 CI 防护），本表只登记并指向，不复制清单内容。
5. **本表是给人读的治理活文档，非运行时契约**。不加解析 markdown 存在性的脆测试；
   漂移的正确防线是 code review + 约定（沿用 1-8 已确立的"不测纯文档存在性"）。
6. **状态列取值**：`open`（已登记待处理）/ `blocked`（被前置依赖挡住）/
   `wontfix`（评估后决定不补，附理由）。补齐后从本表移除并在代码删除对应锚点。

## 缺口主表

| ID | 缺口 | 现载体 (代码锚点) | TS 权威来源 | 推荐路径 | 状态 |
|----|------|------------------|-------------|----------|------|
| G1 | **Think/evidence 检索丢失 rerank** | `crates/zbrain-core/src/operation.rs` `ThinkOperation::execute` 内 `engine.search_pages` 调用处 `FUTURE(think-rerank)` 注释 | `src/core/think/gather.ts:110` → `hybridSearch` → `src/core/search/hybrid.ts` 内建 `applyReranker`（受 `mode.reranker_enabled` 门控） | 见下方 **G1 详情** | open |
| G3 | ~~进度上报三态 flag 无消费者~~ | — | — | ✅ 已落地（roadmap 1-2-2）：`crates/zbrain-core/src/progress.rs` 端到端接线 `sync perform_full_sync/perform_sync` per-path 循环 + `--quiet/--progress-json/--progress-interval` clap flags | resolved |
| G14 | progress reporter 缺信号协调器（SIGINT/SIGTERM） | `crates/zbrain-core/src/progress.rs:11` | TS `progress.ts` `setupSignalHandlers` / `runAbortHandlers` | 实现 signal handler 的 graceful shutdown + abort handlers。当前 Rust 依赖 `anyhow` error 传播，无显式信号处理 | open |
| G15 | progress reporter 缺 EPIPE 防御 | `crates/zbrain-core/src/progress.rs:12` | TS `progress.ts` `safeWrite` + `brokenStreams` Set | 实现 safe write（write → EPIPE → 标记 broken → 后续 write 吞掉）+ broken reset 恢复 | open |
| G16 | progress reporter 缺 `child()` 工厂 | `crates/zbrain-core/src/progress.rs:13` | TS `progress.ts` `child(localPhase)` — 继承 interval + 构造 `parent.phase :: localPhase` 路径 | 实现 child 方法，用于嵌套操作（如 `sync.import :: embed`）的进度分阶段上报 | open |
| G17 | progress reporter 缺 heartbeat timer | `crates/zbrain-core/src/progress.rs:14` | TS `progress.ts` `heartbeat(note?)` / `startHeartbeat(ms)` 定时器 | 实现 heartbeat，用于长时间无 .tick() 时定期输出"还活着"信号（避免管道另一端误判挂死） | open |
| G18 | progress reporter 缺 TTY `\r` 重写模式 | `crates/zbrain-core/src/progress.rs:15` | TS `progress.ts` `human-tty` 模式：`\r\x1b[2K` 重写当前行而非逐行追加 | 实现 TTY 检测 + `human-tty` 模式：一行内刷新 `[phase] 42/100 (42%)` 而非持续 append。注：当前 `Human` 模式对应 TS `human-plain`（逐行追加） | open |
| G19 | progress reporter 缺 source-prefix 注入 | `crates/zbrain-core/src/progress.rs:16` | TS `progress.ts` `getSourcePrefix()` + `child()` 自动注入 source label | 实现 source 前缀注入，多 source 并发同步时区分 `[sourceA :: import]` vs `[sourceB :: import]` | open |
| G20 | progress reporter 缺 abort 事件 | `crates/zbrain-core/src/progress.rs:17` | TS `progress.ts` `abort` JSON 事件 + `finish({aborted:true})` | 实现 NDJSON `abort` 事件，供消费者基于进度 JSON 流判断"异常终止 vs 正常结束" | open |
| G4 | ~~`schema` 命令 32-verb schema-pack 管理器未迁~~ **RESOLVED** | `crates/zbrain-cli/src/schema_cmd.rs` 全 32 verb（inspection 9 + activation 3 + authoring 15 + discovery/repair 5）；`UNMIGRATED_TS_SCHEMA_PACK_VERBS` 常量已置空 + 锚点测试改为断言 `len()==0` | TS `src/commands/schema.ts` @ `5d5b404~1`（1166 行 Schema Cathedral v3）；详见 `docs/plans/2026-07-06-schema-rename-audit.md` | Roadmap Part10 Phase12 1-1..1-5 全量迁移（1-1 数据模型/1-2 registry/1-3 inspection/1-4 activation+authoring/1-5 discovery+repair）。discovery 动词走引擎无关内存式聚类+回填（不引入 `execute_raw`）。`zbrain-core` schema_pack 模块 188 tests 全绿 | resolved |
| G5 | doctor 11 项健康检查未迁 | `crates/zbrain-cli/src/lib.rs:84` `UNMIGRATED_TS_DOCTOR_CHECKS` 常量 + 锚点测试 | TS doctor 子系统（embedding_health / sync_freshness / search_mode / federation_health / schema_packs / resolver_health / skill_conformance / frontmatter_integrity / eval_drift / brain_score / takes_weight_grid） | 迁移某检查 = 把其条目移出常量、落地为真实 check（如 `reranker_health` 已迁出） | open |
| G6 | webhook 无 rate limiter | `crates/zbrain-web/src/webhook.rs:88`（`ingest_handler` doc `TODO: implement rate limiter`） | TS：ingest 端点 100 req/10s per IP | 接入 rate limiter 中间件（可复用 zbrain-mcp `SlidingWindowRateLimiter` 思路） | open |
| G7 | ~~webhook 直写绕过 MinionQueue~~ **RESOLVED** | `crates/zbrain-web/src/webhook.rs` — ingest 提交 `ingest_capture` job、github webhook 提交 `sync` job (priority -10) | TS：ingest → MinionQueue → ingest_capture → importFromContent；sync 提交优先级 -10 的 MinionQueue job | 已接入 MinionQueue (Part7 Phase9 1-8)。ingest → `MinionQueue::add('ingest_capture', {slug, content, source})`；sync → `MinionQueue::add('sync', {sourceId, ...}, priority=-10, idempotency_key)` | resolved |
| G8 | InMemoryEngine 不存 code edges | `crates/zbrain-core/src/engine.rs:2146` `add_code_edges` / `:2154` `delete_code_edges_for_chunks`（InMemory 空实现 `TODO`） | TS `addCodeEdges` / `deleteCodeEdgesForChunks` | 为 InMemoryEngine 实现 code-edge 存储（libsql 后端已实现，InMemory 待补，主要影响测试保真） | open |
| G9 | InMemoryEngine api_key 增删未实现 | `crates/zbrain-core/src/engine.rs:2591` `create_api_key` / `:2595` `revoke_api_key`（not-implemented） | TS admin api-key 生命周期 | InMemory 后端补上（libsql 已实现；InMemory 主要用于测试） | open |
| G10 | `import_code_file` 空壳 | `crates/zbrain-core/src/import.rs:124`（`TODO: 实现代码导入逻辑`，占位测试 `import_code_file_placeholder`） | TS 代码文件导入路径 | 实现 chunk 切分 + `add_code_edges` 接线（依赖 tree-sitter chunker，见 part2 roadmap #108） | open |
| G11 | `count_pages` 未进 BrainEngine trait | `crates/zbrain-core/src/sources_ops.rs:232`（自由函数）+ `:237` `TODO: add count_pages to BrainEngine trait` | — （Rust 内部结构缺口，非 TS 行为差异） | 把 `count_pages` 提升为 trait 方法，各后端各自高效实现（当前自由函数走通用路径） | open |
| G12 | libsql 非单线程序列化访问 | `crates/zbrain-core/src/libsql.rs:239`（`TODO`：借线程消息循环序列化所有读写避免竞态） | — （Rust 后端并发正确性加固） | 单例内单线程 + 消息循环序列化 DB 读写。注：schema init 已有进程级 `SCHEMA_INIT_LOCK`（`:233`）覆盖初始化竞态 | open |
| G21 | Rust 无 `apply-migrations` 命令（版本追踪 + 迁移脚本） | `scripts/postinstall.ts` — TS fallback 已删（roadmap 1-3），仅剩 DDL 级 `init --migrate-only` | TS `src/cli.ts` `apply-migrations`（版本追踪、迁移 runner、脚本执行） | 实现 `apply-migrations` Rust 命令：迁移表版本追踪 + 迁移脚本 runner + `--yes`/`--non-interactive` flags。当前开发者需完整迁移时用 `bun src/cli.ts apply-migrations` | open |
| G13 | boost metadata-axis 部分未迁 + salience/recency strength 硬编码 | `crates/zbrain-core/src/engine.rs` `SearchResult.salience_boost` 字段 `FUTURE(boost-metadata-axes)` 注释 + `search_pages` post-fusion 阶段 `FUTURE(salience-strength-by-mode)` 注释 | TS `runPostFusionStages`（`src/core/search/hybrid.ts:282`）编排 `applyBacklinkBoost`/`applySalienceBoost`/`applyRecencyBoost`/`applyGraphSignals` + `applyExactMatchBoost`（`intent-weights.ts`）；salience/recency strength 由 search mode（ModeBundle）解析 'on'/'strong'/'off' | (a) strength-by-mode：迁 search-mode 系统后从 ModeBundle 解析替换硬编码 'on'（salience k=0.15；recency 已实现但硬编码 `RecencyStrength::On`）。(b) sibling boosts 剩余未迁：backlink（缺 `get_backlink_counts` trait + count 数据）/graph-signals（InMemory 未实现 edges，见 G8）/source-boost（缺 source 权重）/exact-match（intent-weights 未迁）各自阻塞数据层，数据就绪一个迁一个，各加 `*_boost` stamp 字段。已迁：salience（1-4-4-2）、recency（1-4-4-3） | open |
| G22 | takes `row_num` 后端约束分歧（libsql 缺 CHECK + 默认值语义错） | libsql: `crates/zbrain-core/src/libsql.rs:2441` `input.row_num.unwrap_or(0)`（无 CHECK，默认存 0）；PG: `crates/zbrain-core/migrations/0012_takes_full_columns.sql:53-54` `CHECK (row_num > 0)` + `DEFAULT 1` | TS `src/commands/takes.ts:145` 用 `#${row_num}` 展示，take 编号 **1-based**（从 #1 起） | 对齐 libsql 到 1-based：给 libsql takes 加 `CHECK(row_num > 0)`（或应用层校验）+ 把 `unwrap_or(0)` 改 `unwrap_or(1)`。当前 libsql 允许 row_num=0 违反 TS 1-based 语义；PG mirror 测试（1-3-5）已暴露此分歧（PG 测试必须用 row_num≥1） | open |
| G23 | ~~**Postgres 后端 `search_pages` 未实现**~~ **RESOLVED** | `crates/zbrain-core/src/postgres.rs` `search_pages` override（拉候选 page → `fuse_and_boost`）+ 集成测试 `tests/postgres_engine_search_pages.rs` | TS `hybridSearch`（`src/core/search/hybrid.ts`）对所有后端一致 | 已照 libsql 先例实现：SQL 拉 live+source-scoped 候选（`FULL_PAGE_PROJECTION` 现含 embedding）→ 调 `fuse_and_boost(self, &candidates, opts)`。三后端（InMemory/libsql/PG）现共享同一评分真相 | resolved |
| G24 | ~~**libsql `put_page` 不持久化 `page.embedding`**~~ **RESOLVED** | libsql `put_page` INSERT/UPDATE 绑定 `embedding`（?20 + COALESCE 保留）；postgres `put_page` 绑定 `$19` + `FULL_PAGE_PROJECTION` 加 embedding 列 + `row_to_page` 解码；InMemory 本就写 `input.embedding` | TS 导入路写 page 级 embedding | 已给 libsql/postgres `put_page` 加 embedding 写入 + 读回。COALESCE 语义：`embedding=None` 保留旧值。page 级向量路（`fuse_and_boost` cosine 半边）现有真实数据来源。测试：`libsql_engine_full_columns.rs`（persist + None-preserve）、`libsql_engine_search_pages.rs`（vector-path-active）、`postgres_engine_full_columns.rs`（roundtrip） | resolved |
| G25 | ~~**`import_from_content` 不生成 chunk embedding**~~ **RESOLVED（函数层）** | `crates/zbrain-core/src/import.rs` `import_from_content` 新增 `embedding_client: Option<&EmbeddingClient>` 参数，批量 embed chunk_text 填 `ChunkInput.embedding`（fail-open）；测试 3 个 | TS 导入路对每个 chunk 调 embedding provider | 已给函数加可选 embedding client + 批量 embed + fail-open（provider 抖动降级 None，导入不失败）。**遗留**：`ingest_capture` handler 传 `None`（MinionJobContext 尚未携带 embedding client），minion 路径 chunk 仍无 embedding——将 embedding client 接入 minion context 是独立 follow-up（见 G30） | resolved |
| G30 | minion 路径 chunk embedding 未生成（MinionJobContext 无 embedding client） | `crates/zbrain-core/src/minions/handlers/ingest_capture.rs` 调 `import_from_content(..., None)` + 就近注释 | — （Rust 内部接线缺口，G25 的遗留半） | 给 `MinionJobContext` 加可选 `embedding_client` 字段（从 autopilot/worker 构造侧 `from_env` 软关闭注入，同 CLI query 先例），ingest_capture 传 `Some(client)`。当前 minion 导入 chunk 无向量、search 向量半降级 lexical-only | open |
| G26 | **query expansion 无真 structured-output HTTP provider（仅 trait seam + 纯层）** | `crates/zbrain-core/src/ai/expand.rs` — `ExpansionProvider` trait 定义 + `expand_query` 编排 + sanitize 纯函数已就位；生产实现缺失（无 `RealExpansionProvider`，无调用点接线，search 未接 `expand_query`） | TS `gateway.expand`（`src/core/ai/gateway.ts:2018`）用 `generateObject` + `ExpansionSchema{queries:z.array(string).min(1).max(5)}` 做 **structured JSON** 输出；`search/expansion.ts:expandQuery` 消费 | 实现真 `ExpansionProvider`：需 structured-output（JSON schema 约束）HTTP 调用。**阻塞点**：slice-3 的 `ChatProvider` 是 free-text chat seam，无 `generateObject` 等价物（structured-object seam）。待补 structured-output seam 后，实现 provider 并在 search 检索路接 `expand_query(query, Some(&provider))`。当前 query 检索不做多查询扩展（等价 `[query]` 降级，功能安全） | open |
| G27 | **minion 附件无外部存储路径（`storage_uri` 恒 NULL）** | `crates/zbrain-core/src/minions/types.rs:478` `Attachment.storage_uri` doc；三后端 insert 锚点 `engine.rs:4988`（InMemory）/ `libsql.rs:4542`/ `postgres.rs:3889`（均注释 inline content only, storage_uri 恒 NULL） | TS `src/core/minions/{attachments.ts,queue.ts}` — `addAttachment` 只写 inline `content`，`storage_uri` 从不写；schema 预留该列但运行时无写入路径（与 Rust 现状等价） | 附件当前仅走 inline `content` (BYTEA/BLOB) 通道，无大文件外部存储（S3/本地路径）卸载。TS 权威本身也未实现（schema 预留），故为**忠实 port 的既有降级**而非 Rust 独有缺口。真需要时：给 `NormalizedAttachment`/`insert_attachment` 加 storage backend seam，>阈值走外部存储写 `storage_uri`、小文件继续 inline，`get_attachment` 按 `storage_uri` 是否 NULL 分流读取 | open |
| G28 | **`pause_job` 不能暂停 `waiting-children` 父 job（共有设计边界）** | `crates/zbrain-core/src/engine.rs` InMemory `pause_job` 注释 `// waiting-children is intentionally out ... (G28)`；三后端 WHERE 均 `IN ('waiting','active','delayed')`（libsql.rs `pause_job` / postgres.rs `pause_job`） | TS `src/core/minions/queue.ts:1119` `pauseJob` WHERE `status IN ('waiting','active','delayed')` — **同样不含 `waiting-children`** | 阻塞在子 job 上的父 job 无法被 pause（要暂停整棵子树需先暂停/取消子 job）。**非 Rust 独有缺口**：TS 权威行为完全一致，Rust 忠实对齐（roadmap 1-1-3-3 拷问 Q 已确认）。真需要时：pause 语义扩展到 waiting-children 需同时定义"恢复后回到 waiting-children 还是 waiting"及与子 job 状态的一致性，属跨 C/D 层设计，不在当前切片范围 | open |
| G29 | **AI recipe 注册表未迁移（provider 元数据硬编码子集）** | 1-5-2 `embeddingProviderConfigured` 硬编码 openai/zeroentropy/ollama/llama-server 四 provider 的 auth_env 逻辑，无 `Recipe` struct/const table | TS `src/core/ai/recipes/` — 17 个 provider recipe 文件，含 id/name/tier/auth_env/touchpoints/pricing/aliases + `resolveAuth`/`resolveDefaultHeaders` 行为方法（95% 纯数据，2-3 个 override） | 实现正式 recipe 模块（const struct + 方法，17 provider 全字段 + match override），gateway 迁移时复用。当前硬编码覆盖 4 个生产 provider，新 provider 需手动加 match arm | open |
| G31 | **MCP 日志参数脱敏 `summarizeMcpParams` 未迁（隐私）** | Rust 侧零实现零测试（全 crate 无 `summarize_mcp_params`/`declared_keys`/`approx_bytes` 匹配） | TS `src/mcp/dispatch.ts` `summarizeMcpParams` — declared-keys allow-list（仅记已声明参数名）、unknown-key 只计数不命名、值大小按 1KB 分桶（防侧信道泄露）；测试 `tests/unit/mcp-dispatch-summarize.test.ts` | 在 Rust MCP dispatch 日志路实现等价脱敏：按 op 的 declared params 白名单过滤 key、未知 key 计数不落名、value 大小分桶。当前 Rust MCP 调用日志若含原始 params 有隐私泄露风险 | open |
| G32 | **MCP `_meta.brain_hot_memory` 注入未迁** | `crates/zbrain-web/src/mcp.rs` `dispatch_tool_call` 硬编码 `meta: None`（`operation.rs` 有 `ToolResult.meta` 字段 + `dispatch_tool_call_no_meta_by_default` 测试，但无注入逻辑） | TS `src/mcp/dispatch.ts` metaHook — op.handler 成功后计算 `_meta.brain_hot_memory`（可见性过滤 + per-allowlist 缓存 + best-effort try/catch 降级）；测试 `tests/unit/facts-context-injection.serial.test.ts` | 实现 metaHook seam：dispatch 成功后可选注入 hot-memory `_meta`，DB 错误吞掉不翻转 tool 成功。依赖 facts 层（G34）先落地 | blocked |
| G33 | **per-token takes-holder allow-list 过滤未接线（死字段）** | `takes_holders_allow_list` 字段仅定义+初始化 None，从未被读取用于过滤；引擎无 `list_takes`/`search_takes` 方法 | TS `src/mcp/dispatch.ts` + `queue`/takes ops — per-token `permissions.takes_holders` 过滤 `takes_list`/`takes_search`/`query`（返回 takes 时）`WHERE holder = ANY($allowlist)`；测试 `tests/unit/takes-mcp-allowlist.serial.test.ts` | 引擎补 `list_takes`/`search_takes`（带 holder 过滤参数），dispatch/token 层把 allow-list 下推。当前多租户 takes 隔离不生效——任意 token 可读全部 holder 的 takes（安全缺口） | open |
| G34 | **takes-fence 读操作遮蔽未接入读路径** | `crates/zbrain-core/src/takes_fence.rs` 是独立 markdown 解析模块，仅被 `lib.rs` 导出，未集成进 `GetPageOperation`/`GetVersions` | TS `get_page` takes-fence redaction — 读页面时按 allow-list 存在性遮蔽 takes 围栏内容；测试 `tests/unit/takes-fence-read-ops.serial.test.ts` (#728) | 把 `takes_fence` 解析接入 GetPage/GetVersions 读路径，按 caller 的 takes-holder allow-list 遮蔽围栏内容。当前读 page 不做 takes 遮蔽——围栏内容对无权限 caller 泄露（安全缺口） | open |
| G35 | **facts MCP op 层未移植（extract_facts/recall/forget_fact/anti-loop/backstop）** | `crates/zbrain-core/src/minions/handlers/extract_facts.rs` 是 `not_implemented` 桩；registry 无 facts TypedOperation；引擎无 `forget_fact`/`delete_fact` | TS facts 子系统：`extract_facts`/`recall`/`forget_fact` ops + anti-loop `dream_generated` marker（防 facts 自循环）+ put_page facts backstop 门控 + facts MCP ops 注册/scope；测试 `facts-forget`/`facts-anti-loop`/`facts-backstop-gating`/`facts-mcp-allowlist` | 移植 facts MCP op 层（引擎 CRUD 部分已有：`list_facts_by_entity` 等）。这是较大 slice——facts 的 dispatch 集成、anti-loop 标记、backstop 门控、ops 注册全缺。engine 层 facts CRUD 已在，缺 op/dispatch 封装 | open |
| G36 | **`ZBRAIN_PLUGIN_PATH` subagent 插件发现未迁移** | **TS 保留态**：`src/core/minions/plugin-loader.ts`（`loadPluginsFromEnv`/`loadSinglePlugin`，含 `zbrain-plugin-v1` manifest 校验 + SubagentDefinition 解析）+ `tests/unit/plugin-loader.test.ts`。当前**无任何生产代码 importer**（仅测试消费），是未接线的孤立能力 | TS `plugin-loader.ts` — 从 `ZBRAIN_PLUGIN_PATH`（冒号分隔绝对路径，仿 `$PATH`）发现 host-repo 自带的 subagent 定义，worker 启动时加载 | Rust 侧补等价加载器（env 解析 + manifest 校验 + subagent def 注册进 registry），迁移完成后删 TS 文件+test。Phase11 第六轮 D 尾巴清理时按 AGENTS.md 铁律"未迁移能力不盲删"暂缓，登记于此。注：`skillpack-load.ts`（保留）是其 sibling，共享 `ZBRAIN_PLUGIN_PATH` 语义但独立实现 | open |
| G37 | **`run_operation` 本地模式不解析 `database_url`→`database_path`，libsql 直连失败** | `crates/zbrain-cli/src/lib.rs:2073` `EngineConfig { database_path: None, database_url: Some(config.database_url) }` — 但 `LibsqlEngine::connect` 要求 `database_path`（libsql.rs:459 `requires EngineConfig.database_path`）。影响 put-page/get-page/query/think/list-pages/delete-page/restore-page/purge（全经 `run_operation`）| — （Rust 内部接线缺口，本地模式 free-engine 路径） | 镜像 `run_sync_command`：用 `resolve_database_path(&config.database_url)` 得到绝对路径再填 `database_path: Some(path)`。`run_operation` 的直连分支从未给 `database_path`，本地 libsql 模式报错；thin-client 模式因提前返回不受影响。**发现于 1-5 冒烟**：seed via `put-page` 报 `Engine: LibsqlEngine requires EngineConfig.database_path`。未改（scope creep，影响 8 命令行为），待独立 fix | open |


## G1 详情 — Think/evidence 检索丢失 rerank

**病根：接入点分层错位，不是"要不要 rerank"。**

- TS 里"是否 rerank"**不是 Think 层的产品判断**，而是 `hybridSearch` 引擎级检索原语的
  内建行为，由 search mode（`conservative` 关 / `balanced`·`tokenmax` 开，默认 `balanced`）
  的 `reranker_enabled` 统一决定。`gather.ts:110` 的 Think 检索调 `hybridSearch`，
  因此 TS 里 **Think 检索本就 rerank**（继承 `hybridSearch` 的内建行为），并非 Think
  显式选择了 rerank。
- Rust 侧上一刀（roadmap 1-4-2-2）把 rerank 接在 **operation 层**（`QueryOperation::execute`
  内联的 rerank 段：门控 `apply_reranker`，`document_of` 取 snippet→compiled_truth→title 回退，
  stamp `rerank_score`/`reranker_delta`）。而 `ThinkOperation::execute` 走
  `engine.search_pages`（`operation.rs` ~1580，`limit:5`、`min_score:0.1`、
  `query_embedding:None` 向量路关闭），**绕过了 operation 层的后处理** → Think 意外丢失了
  TS 本有的 rerank。
- **推荐路径（钉方向，本刀不实现）**：抽 operation 层共享检索后处理 helper
  `retrieve_and_rerank(ctx, opts, ...)`，`QueryOperation` 与 `ThinkOperation`（及未来
  evidence/brainstorm 入口）都调它。理由：
  - **否决"engine 层下沉"**：违背 1-4-2-2 Q2 明确排除的方案——engine trait 是纯存储抽象，
    拿不到 `config`/`audit_dir`，且会用 HTTP 污染 InMemory/Postgres 双实现。一刀前才确认，
    不该一刀后自推翻。
  - **否决"operation 层复制"**：Query/Think/未来入口每处复制一遍
    `apply_reranker` + `document_of`/stamp 闭包，改策略要同步 N 处，埋 DRY 债。
  - 共享 helper 是 TS 精神（rerank 是检索原语的一部分、调用方自动继承）在 Rust 分层约束下的
    正确落点：TS 放 `hybridSearch`，Rust 因 engine trait 拿不到 config/audit_dir 而放紧挨其上的
    operation 层共享 helper——同一意图的合理映射。顺带把上一刀内联的 rerank 段重构成可复用单元。
- **风险权衡**：若 Think 迟迟不需要 rerank（LLM 合成对证据顺序不敏感 + 多一次 rerank API
  往返的延迟/成本），共享 helper 的抽取属投机。但"钉方向不实现"零投机成本，未来真接时按此路径走即可。
  另注 Think 检索还有 `limit:5 < TOP_N` 与 `rerank_score` 当前无消费者两个语义问题，
  接线时需一并处理。
