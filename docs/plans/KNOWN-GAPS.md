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
| G4 | `schema` 命令 32-verb schema-pack 管理器未迁 | `crates/zbrain-cli/src/lib.rs:2289` `UNMIGRATED_TS_SCHEMA_PACK_VERBS` 常量 + `:2277` `FUTURE(schema-pack)` + 锚点测试 | TS `src/commands/schema.ts` @ `5d5b404~1`（1166 行 Schema Cathedral v3）；详见 `docs/plans/2026-07-06-schema-rename-audit.md` | 迁移时把 32 个 verb 挂到新 `schema` 子命令树，从常量移除。Rust DDL dumper 已让名 `schema`→`schema-sql` 腾位 | open |
| G5 | doctor 11 项健康检查未迁 | `crates/zbrain-cli/src/lib.rs:84` `UNMIGRATED_TS_DOCTOR_CHECKS` 常量 + 锚点测试 | TS doctor 子系统（embedding_health / sync_freshness / search_mode / federation_health / schema_packs / resolver_health / skill_conformance / frontmatter_integrity / eval_drift / brain_score / takes_weight_grid） | 迁移某检查 = 把其条目移出常量、落地为真实 check（如 `reranker_health` 已迁出） | open |
| G6 | webhook 无 rate limiter | `crates/zbrain-web/src/webhook.rs:88`（`ingest_handler` doc `TODO: implement rate limiter`） | TS：ingest 端点 100 req/10s per IP | 接入 rate limiter 中间件（可复用 zbrain-mcp `SlidingWindowRateLimiter` 思路） | open |
| G7 | webhook 直写绕过 MinionQueue | `crates/zbrain-web/src/webhook.rs:240`（ingest 直 `put_page`）、`:511`（sync 造 placeholder job_id 返 202） | TS：ingest → MinionQueue → ingest_capture → importFromContent；sync 提交优先级 -10 的 MinionQueue job | 待 MinionQueue 移植后接入；当前直写/占位 job_id 是功能等价降级 | blocked |
| G8 | InMemoryEngine 不存 code edges | `crates/zbrain-core/src/engine.rs:2146` `add_code_edges` / `:2154` `delete_code_edges_for_chunks`（InMemory 空实现 `TODO`） | TS `addCodeEdges` / `deleteCodeEdgesForChunks` | 为 InMemoryEngine 实现 code-edge 存储（libsql 后端已实现，InMemory 待补，主要影响测试保真） | open |
| G9 | InMemoryEngine api_key 增删未实现 | `crates/zbrain-core/src/engine.rs:2591` `create_api_key` / `:2595` `revoke_api_key`（not-implemented） | TS admin api-key 生命周期 | InMemory 后端补上（libsql 已实现；InMemory 主要用于测试） | open |
| G10 | `import_code_file` 空壳 | `crates/zbrain-core/src/import.rs:124`（`TODO: 实现代码导入逻辑`，占位测试 `import_code_file_placeholder`） | TS 代码文件导入路径 | 实现 chunk 切分 + `add_code_edges` 接线（依赖 tree-sitter chunker，见 part2 roadmap #108） | open |
| G11 | `count_pages` 未进 BrainEngine trait | `crates/zbrain-core/src/sources_ops.rs:232`（自由函数）+ `:237` `TODO: add count_pages to BrainEngine trait` | — （Rust 内部结构缺口，非 TS 行为差异） | 把 `count_pages` 提升为 trait 方法，各后端各自高效实现（当前自由函数走通用路径） | open |
| G12 | libsql 非单线程序列化访问 | `crates/zbrain-core/src/libsql.rs:239`（`TODO`：借线程消息循环序列化所有读写避免竞态） | — （Rust 后端并发正确性加固） | 单例内单线程 + 消息循环序列化 DB 读写。注：schema init 已有进程级 `SCHEMA_INIT_LOCK`（`:233`）覆盖初始化竞态 | open |
| G21 | Rust 无 `apply-migrations` 命令（版本追踪 + 迁移脚本） | `scripts/postinstall.ts` — TS fallback 已删（roadmap 1-3），仅剩 DDL 级 `init --migrate-only` | TS `src/cli.ts` `apply-migrations`（版本追踪、迁移 runner、脚本执行） | 实现 `apply-migrations` Rust 命令：迁移表版本追踪 + 迁移脚本 runner + `--yes`/`--non-interactive` flags。当前开发者需完整迁移时用 `bun src/cli.ts apply-migrations` | open |
| G13 | boost metadata-axis 部分未迁 + salience/recency strength 硬编码 | `crates/zbrain-core/src/engine.rs` `SearchResult.salience_boost` 字段 `FUTURE(boost-metadata-axes)` 注释 + `search_pages` post-fusion 阶段 `FUTURE(salience-strength-by-mode)` 注释 | TS `runPostFusionStages`（`src/core/search/hybrid.ts:282`）编排 `applyBacklinkBoost`/`applySalienceBoost`/`applyRecencyBoost`/`applyGraphSignals` + `applyExactMatchBoost`（`intent-weights.ts`）；salience/recency strength 由 search mode（ModeBundle）解析 'on'/'strong'/'off' | (a) strength-by-mode：迁 search-mode 系统后从 ModeBundle 解析替换硬编码 'on'（salience k=0.15；recency 已实现但硬编码 `RecencyStrength::On`）。(b) sibling boosts 剩余未迁：backlink（缺 `get_backlink_counts` trait + count 数据）/graph-signals（InMemory 未实现 edges，见 G8）/source-boost（缺 source 权重）/exact-match（intent-weights 未迁）各自阻塞数据层，数据就绪一个迁一个，各加 `*_boost` stamp 字段。已迁：salience（1-4-4-2）、recency（1-4-4-3） | open |
| G22 | takes `row_num` 后端约束分歧（libsql 缺 CHECK + 默认值语义错） | libsql: `crates/zbrain-core/src/libsql.rs:2441` `input.row_num.unwrap_or(0)`（无 CHECK，默认存 0）；PG: `crates/zbrain-core/migrations/0012_takes_full_columns.sql:53-54` `CHECK (row_num > 0)` + `DEFAULT 1` | TS `src/commands/takes.ts:145` 用 `#${row_num}` 展示，take 编号 **1-based**（从 #1 起） | 对齐 libsql 到 1-based：给 libsql takes 加 `CHECK(row_num > 0)`（或应用层校验）+ 把 `unwrap_or(0)` 改 `unwrap_or(1)`。当前 libsql 允许 row_num=0 违反 TS 1-based 语义；PG mirror 测试（1-3-5）已暴露此分歧（PG 测试必须用 row_num≥1） | open |
| G23 | **Postgres 后端 `search_pages` 未实现（落 trait default 空实现）** | `crates/zbrain-core/src/engine.rs:1067` `BrainEngine::search_pages` trait default 返回空 `Vec`；`crates/zbrain-core/src/postgres.rs` 无 override（无 `fn search_pages`） | TS `hybridSearch`（`src/core/search/hybrid.ts`）对所有后端一致；PG 部署应可检索 | 照 libsql 先例（roadmap 1-3-2）给 postgres 实现真实 `search_pages`：SQL 拉候选 page → 调 `engine::fuse_and_boost(self, &candidates, opts)`（后端无关融合 helper 已就位，零重复）。libsql 已迁、InMemory 已迁，仅 PG 缺。生产若用 Postgres brain，`zbrain query` 当前返回空 | open |
| G24 | **libsql `put_page` 不持久化 `page.embedding`（写入口缺失）** | `crates/zbrain-core/src/libsql.rs:863` `put_page` INSERT 列清单无 `embedding`（读路 `:967`/`:1201` 投影含 `embedding`，写路不含）→ `pages.embedding` 恒 NULL | TS 导入路写 page 级 embedding | 给 `put_page` INSERT/UPDATE 加 `embedding` 列写入（需 `PutPageInput` 增字段或独立 `set_page_embedding`）。当前后果：libsql 后端 page 向量路（`fuse_and_boost` 的 cosine 半边）恒优雅降级为 lexical-only，query embedding 接线（1-3-3）只对已有 embedding 的 page 生效——而没有任何写入口能产生它 | open |
| G25 | **`import_from_content` 不生成 chunk embedding（doc 承诺未兑现）** | `crates/zbrain-core/src/import.rs:61` doc 写"2. 生成 embeddings (如果提供 embedding_client)"，但 `:83` chunk `embedding: None` 硬编码，函数签名无 `embedding_client` 参数 | TS 导入路对每个 chunk 调 embedding provider | 给 `import_from_content` 加可选 `embedding_client: Option<&EmbeddingClient>` 参数，对 chunk_text 批量 embed 填 `ChunkInput.embedding`。依赖 chunker 成熟度（part2 #107 CJK/recursive 已完成、#108 tree-sitter 进行中）。与 G24 互补：G24 是 page 级写入口，G25 是 chunk 级生成 | open |
| G26 | **query expansion 无真 structured-output HTTP provider（仅 trait seam + 纯层）** | `crates/zbrain-core/src/ai/expand.rs` — `ExpansionProvider` trait 定义 + `expand_query` 编排 + sanitize 纯函数已就位；生产实现缺失（无 `RealExpansionProvider`，无调用点接线，search 未接 `expand_query`） | TS `gateway.expand`（`src/core/ai/gateway.ts:2018`）用 `generateObject` + `ExpansionSchema{queries:z.array(string).min(1).max(5)}` 做 **structured JSON** 输出；`search/expansion.ts:expandQuery` 消费 | 实现真 `ExpansionProvider`：需 structured-output（JSON schema 约束）HTTP 调用。**阻塞点**：slice-3 的 `ChatProvider` 是 free-text chat seam，无 `generateObject` 等价物（structured-object seam）。待补 structured-output seam 后，实现 provider 并在 search 检索路接 `expand_query(query, Some(&provider))`。当前 query 检索不做多查询扩展（等价 `[query]` 降级，功能安全） | open |

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
