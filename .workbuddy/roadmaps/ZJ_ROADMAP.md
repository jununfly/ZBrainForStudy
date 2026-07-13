# ZBrain TS→Rust Part5: Phase 7 — Facts, Takes, Timeline, Salience, Graph

- **1**: 🔄 Phase 7 — Facts, Takes, Timeline, Salience, Graph
  - 💡 Decision: undefined
  - **1-1**: ✅ Phase 7A: Takes 引擎层 (DB schema + trait + fence parser + salience wiring + scorecard)
    - 📝 Fence parser/renderer included per 方案B. All 7 children completed 2026-07-09.
    - **1-1-1**: ✅ DB migration: extend takes table to full TS schema
      - 📝 From 0007 3-col (id/page_id/active) → full ~21-col. New migration 0012. Done 2026-07-09.
    - **1-1-2**: ✅ Rust types: Take struct, TakeRow, TakeKind, fence types
      - 📝 In zbrain-core types.rs. Take (21 fields), TakeInput, TakeResolution, UpsertTakesResult, SEED_TAKE_KINDS. Done 2026-07-09.
    - **1-1-3**: ✅ Fence parser/renderer: parse_takes_fence / render_takes_fence
      - 📝 Port takes-fence.ts. 32 round-trip tests in takes_fence.rs. Done 2026-07-09.
    - **1-1-4**: ✅ BrainEngine trait: add get_takes_for_page, add_takes_batch, resolve_take
      - 📝 3 new trait methods with default Unsupported. Plus InMemory CalibrationQueries::get_scorecard real impl. Done 2026-07-09.
    - **1-1-5**: ✅ Backend impls: libsql + postgres + InMemory takes CRUD + scorecard
      - 📝 All 3 backends. Libsql ~110 lines, Postgres ~120 lines sqlx, InMemory full CRUD + real scorecard. Done 2026-07-09.
    - **1-1-6**: ✅ Wire salience: update get_salience_scores to use real takes_count
      - 📝 Already done in previous 6c-takes-salience slice. Libsql: LEFT JOIN takes + ln(1+N) in Rust. Postgres: ln(1+COUNT) in SQL. InMemory: ln(1+distinct_takes). Existing 6+7 salience tests pass. Done 2026-07-09.
    - **1-1-7**: ✅ Tests: fence round-trip, CRUD integration, scorecard, salience with takes
      - 📝 takes_crud.rs: 19 tests (13 InMemory + 6 Libsql). page_methods_salience_scores_with_takes.rs: 6 tests (3 libsql + 3 postgres). takes_fence.rs: 32 round-trip tests. Libsql/postgres resolve_take not-found error unified to not_found code. Done 2026-07-09.
  - **1-2**: ✅ Phase 7B: Backlinks + Facts 引擎层
    - 📝 links table exists (0006 migration), just need query methods. Facts: new table + fence parser + CRUD.
    - **1-2-1**: ✅ 1-2-1: Rust types (Link, LinkBatchInput, GraphNode, GraphPath)
    - **1-2-10**: ✅ Facts integration tests: fence round-trip, CRUD, supersede, health
      - 💡 Postgres 集成测试策略？: 和 Links 一致：InMemory + libsql 写集成测试，Postgres 后端实现但不写测试，延后到 Phase 7C 统一补。
      - 📝 35 tests: 17 InMemory + 18 Libsql. Covers insert roundtrip, duplicate, supersede (high/low conf), list filters (active_only/kinds/visibility/limit/offset), health, expire, persistence. libsql list_facts rebuilt with ? placeholders (was ?NNN).
    - **1-2-2**: ✅ 1-2-2: BrainEngine trait link methods (add_links_batch, remove_link, get_links, get_backlinks, get_backlink_counts, traverse_paths)
    - **1-2-3**: ✅ 1-2-3: Backend implementations (InMemory + libsql + postgres links CRUD)
      - 📝 All 3 backends done: InMemory BFS traverse, libsql JOIN-based, postgres unnest(). Verified by 26 integration tests in 1-2-4.
    - **1-2-4**: ✅ 1-2-4: Links integration tests
      - 💡 Decision: undefined
      - 💡 Decision: undefined
      - 💡 Decision: undefined
      - 📝 26 tests: 16 InMemory + 10 libsql. Covers add/remove/get_links/get_backlinks/get_backlink_counts/traverse_paths. Postgres deferred to Phase 7C when pg-embed parallelism is addressed.
    - **1-2-5**: ✅ Facts DB migration: new facts table (~20+ columns)
      - 💡 Facts 表 schema 范围？: 全量建表（~24列），包含 CRUD 核心列 + 向量列(embedding/embedded_at) + typed-claim 列(claim_metric/value/unit/period, event_type) + fence 对账列(row_num, source_markdown_slug, consolidated_at/into)。新表边际成本为零，避免后续 ALTER TABLE churn。向量列用 VECTOR 类型，各引擎可用空实现。
      - 📝 PostgreSQL + SQLite migrations created (0013_facts.sql). 27 columns: CRUD core, consolidation, REAL[] embedding, typed-claim, fence sync. CHECK constraints for kind/visibility/notability/confidence. Registered as v13 in both LIBQL_MIGRATIONS and POSTGRES_MIGRATIONS. All 717 tests pass.
    - **1-2-6**: ✅ Rust types: FactRow, NewFact, FactKind, FactVisibility, ParsedFact, FactsHealth
      - 📝 FactKind(5 variants), FactVisibility(2), FactInsertStatus(3), FactRow(19 fields), NewFact(17 fields), FactsHealth, EntityCount, FactListOpts. Serde camelCase wire format. All 691 tests pass.
    - **1-2-7**: ✅ Facts fence parser/renderer: parse_facts_fence / render_facts_fence
      - 💡 Facts fence 列数？: 全量 14 列：基础 9 列(claim/kind/confidence/visibility/notability/valid_from/valid_until/source/context) + typed-claim 5 列(claim_metric/value/unit/period, event_type)。parser 一次写完，和 DB 全量建表对称。
    - **1-2-8**: ✅ BrainEngine trait facts methods: insertFact(supersede), listFactsByEntity, getFactsHealth, expireFact
      - 💡 Facts 引擎层方法范围？: insertFact + 3 个查询：insertFact(含内部 supersede 事务)、listFactsByEntity(按 entity 查询)、getFactsHealth(运维指标)、expireFact(软删除)。不做 insertFacts(batch)、deleteFactsForPage、consolidateFact、findTrajectory。
    - **1-2-9**: ✅ Backend implementations: libsql + postgres facts CRUD
      - 💡 libsql insert_fact 事务策略: 方案A: libsql transaction API
      - 💡 Postgres insert_fact 事务策略: 方案B: 单条 CTE
      - 💡 Row mapping 策略: 方案C: helper 函数
      - 💡 valid_from NOT NULL 处理: 方案B: Rust 层默认 now()
      - 💡 实现顺序: 方案A: 先 libsql 后 postgres
      - 💡 1-2-9 内是否写 unit test: 方案A: 不写，编译通过即可
      - 📝 libsql: transaction API insert. Postgres: single CTE with NOT EXISTS guard. Both pass cargo check.
  - **1-3**: 🔄 Phase 7C: Graph + Salience 收尾 + CLI 接线
    - 💡 1-3 拆分为几个 sub-node？: 5 个：1-3-1 Graph 方法、1-3-2 Salience 方法、1-3-3 CLI 接线(facts/links/takes)、1-3-4 CLI 查询(salience/orphans/backlinks)、1-3-5 PG 测试补全
    - 📝 traverse_paths, adjacency boost, get_recent_salience, CLI commands for all migrated domains.
    - **1-3-1**: ✅ 1-3-1: Graph traverse_paths 三后端实现
      - 💡 1-3-1 Graph 方法范围？: traverse_paths 三后端实现 + adjacency_boosts trait 新增 + 三后端实现
      - 📝 InMemory BFS already existed. Libsql: fetch pages+links to memory, BFS. Postgres: same pattern via sqlx. 8 new libsql traverse tests + libsql_traverse_paths_basic_bfs in links_crud (replaced unsupported stub test). All pass. adjacency_boosts trait already existed from prior work.
    - **1-3-2**: ✅ 1-3-2: Salience 方法（get_recent_salience + touch_salience trait + 三后端）
      - 💡 salience_touched_at bump 怎么实现？: A1: 在 set_emotional_weight 中内嵌 SQL bump
      - 💡 Recency decay 子系统范围？: B1: 只做 flat 模式（halflife=1 day）
      - 💡 touch_salience 签名: 独立 trait 方法: async fn touch_salience(&self, slug: &str, source_id: &str) -> Result<bool>
      - 💡 get_recent_salience 返回类型: 新建 SalienceResult struct（9 字段: slug, source_id, title, page_type, updated_at, emotional_weight, take_count, take_avg_weight, score）
      - 💡 分数计算位置: Rust 侧统一计算。SQL 只取组件（emotional_weight, take_count, updated_at），Rust 算 ln(1+take_count) + recency_decay(1.0/(1.0+days_old)) + 排序截断
      - 💡 recency_bias 参数: 不暴露 recency_bias 参数，硬编码 flat 模式（1.0/(1.0+days_old)）。on 模式延后
      - 💡 时间窗口过滤: Rust 侧算 boundary（Utc::now() - Duration::days(days)），传参到 SQL。SQL 用 CASE WHEN salience_touched_at > updated_at THEN salience_touched_at ELSE updated_at END >= ? 替代 GREATEST()
    - **1-3-3**: ✅ 1-3-3: CLI 接线 — facts/links/takes 命令（增删改查 + fence 交互）
      - 💡 CLI facts/links/takes 命令范围？: Engine CRUD 模式：facts list/add、links list/add、takes list/add
      - 💡 实际交付范围 vs 原始决策？: 比决策多出：facts health/expire、links backlinks/rm。全部是纯 CRUD 映射，边际成本接近零，符合 Engine CRUD 模式精神。takes 仅 add/list（需要 slug→page_id 解析）
      - 📝 13 个命令函数：4 facts (add/list/health/expire) + 4 links (add/list/backlinks/rm) + 2 takes (add/list) + 3 dispatch + 2 parse helpers。所有命令用直接 LibsqlEngine 实例化模式（同 sources）。takes 通过 get_page 解析 slug→page_id。125 CLI tests pass + 735 core tests pass。2026-07-09。
    - **1-3-4**: ⏳ 1-3-4: CLI 查询命令 — salience/orphans/backlinks/graph-query
      - 💡 CLI 查询命令范围？: salience/orphans/backlinks/graph-query 四个命令全做
    - **1-3-5**: ⏳ 1-3-5: Postgres 集成测试补全（links/facts/takes PG mirror）
      - 💡 PG 测试文件组织 + 覆盖范围？: 方案 A: 同文件 + 每域 3-5 个测试（links: add/remove/backlinks/traverse, facts: insert/supersede/list, takes: add/get）

<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## ZBrain TS→Rust Part6: Phase 8 — AI Gateway / Providers / Models / Routing

### 树形视图 (depth=2)

```
[~][X+] 1. ZBrain TS→Rust Part6: Phase 8 — AI Gateway / Providers / Models / Routing
├── [x][Y+] 1-1. Model registry + capabilities + pricing 数据层 (仿 embedding_pricing.rs static 表, 迁 recipes/capabilities/dims/types)
├── [~][Y+] 1-2. model-resolver + routing (parseModelId/resolveRecipe/tier-alias/assertTouchpoint, 依赖数据层)
│   └── [~][X+] 1-2-1. tier-routing 层 (model-config.ts resolveModel/TIER_DEFAULTS/enforceSubagentCapable/Anthropic, async+BrainEngine config 读取+capability 门控)
├── [x][Y+] 1-3. ChatProvider trait + 类型全保真 + OpenAI HTTP 实现 (独立 trait 非扩 LlmClient; 吸收原 1-4 chat 单调用 HTTP)
├── [x][Y+] 1-4. chat 剩余 provider (anthropic/google native) + BudgetTracker 接线 (chat trait/OpenAI HTTP 已在刀3)
│   ├── [x][Y+] 1-4-1. Anthropic native chat provider (build_body/serialize/parse 三段式, 照 subagent.ts:478 逐字对照 + cache token + tool_use stop_reason)
│   ├── [x][Y+] 1-4-2. Google/Gemini native chat provider (contents/parts/functionCall 格式, 照 Gemini 官方 REST, 无 TS 逐字样本)
│   └── [x][X+] 1-4-3. BudgetTracker 接线 (reserve/record + pricing 查表 + ISO 周审计 JSONL 复用 rerank_audit 先例, ambient-store 注入方式单独 grill)
├── [x][Y+] 1-5. toolLoop 工具循环 (provider-agnostic tool loop, 最重, 依赖 chat)
└── [x][Y+] 1-6. embed/rerank/expand 收口 (rerank 近完成去 mock, embed 去 MockProvider, expand 迁移; 可与数据层并行)
```

### 🔨 当前施工: 1-2-1. tier-routing 层 (model-config.ts resolveModel/TIER_DEFAULTS/enforceSubagentCapable/Anthropic, async+BrainEngine config 读取+capability 门控)
**Status:** `in_progress` | **Mode:** `explore`

**决策记录:**
- Q: Q0 config 读取注入方式
  A: A: resolve_model 收注入的 ConfigLookup (key->Option<String>),不碰 BrainEngine trait,零 DB 耦合,纯同步可测。DB/YAML 读取由调用方(CLI/gateway/Phase9消费方)在边界完成再注入。
  > TS 靠 engine.getConfig() 异步读 DB config 表;Rust BrainEngine trait 无 get_config,CLI get_config_value 读的是 zbrain.yml YAML 非 DB。方案B(加 trait async get_config + 建 config 表迁移 + 3后端 impl)范围爆炸且与 resolver.rs '纯静态零 engine 耦合' 定位冲突。resolver.rs 头部已明说 tier-routing async/DB-coupled 部分 lives on consumer side。
- Q: Q1 ConfigLookup 形态
  A: A: trait ConfigLookup { fn get(&self,key:&str)->Option<String>; },resolve_model 收 &dyn ConfigLookup。同步方法;调用方要读 DB 就在构造 lookup 时预取/持有快照,resolve_model 全程同步。
  > tier-routing 按精度链逐 key 惰性查(命中即返),天然匹配 get(key)->Option<String> 访问模式。方案C全量预取与惰性语义不合(key 动态 models.tier.{tier})。trait 给统一命名契约,Phase9 接 DB 时 impl ConfigLookup 比到处传闭包清爽。async 边界干净。
- Q: Q2 capability 层落点
  A: A: 新建 ai/capabilities.rs 全量,移植 get_provider_capabilities + classify_capabilities(5档裁决 ok/degraded:no_caching/degraded:no_parallel/unusable:no_tools/unknown)。与 resolver.rs/model_config.rs 并列,mod.rs re-export。
  > capability 分类是自洽 deep module(输入 model string 出裁决),值得独立文件+独立测试;tier-routing 消费它不拥有它。忠实复刻:supports_thinking 硬编码 false(照 TS,不读 supports_subagent_loop);supports_parallel_tools 复用 supports_tools(照 TS 注释,不用 Rust 独有的 supports_subagent_loop)。底料齐(ChatTouchpoint 字段全在)。ai/ 目录与 TS ai/ 一一对应利于 diff。
- Q: Q3 warn-once 副作用
  A: A: 照搬 budget.rs warn-once 先例(static Mutex<Option<HashSet<String>>> + reset_..._for_test seam + warn_once(key)->bool)。enforce_subagent_capable 内部保留 warn-once + eprintln,返回已修正 model id,与 TS 逐字对应。key=source:resolved。
  > warn-once 去重本质是进程级跨调用状态,天然需 memo;budget.rs 已为完全相同模式立先例,照搬使 Phase8 内两处 warn-once 同构。enforce 返回最终 model id 语义干净,调用方无脑用。方案B(返回 verdict 让调用方 warn)偏离 TS 且多 callsite(gateway.chat/subagent/auto-think)重复 warn 文案易漂移。保留 Fix: zbrain config set 提示。
- Q: Q4 命名+legacy 砍除
  A: A: 用 ModelTier(Utility/Reasoning/Deep/Subagent, as_str 小写)避开已存在 Tier(native/openai-compat)。砍掉 legacy:不移植 deprecatedConfigKey 三级键链+emitDeprecationWarning、isAnthropicProvider、enforceSubagentAnthropic 薄包装。8级精度链移植其中7步(砍第3步deprecated键)。不登记 KNOWN-GAPS(非缺口,是有意不搬的死代码),模块 doc 一句话自解释锚点即可。
  > AGENTS.md 宪法:无线上用户可破坏性清理不留兼容别名。deprecated键服务TS老版本升级路径(Rust全新实现无此概念);isAnthropicProvider 已被 v0.38 classifyCapabilities 取代(死门控);enforceSubagentAnthropic 是给外部TS插件shim(Rust无消费方)。迁移是清理legacy最佳时机,全量搬=把坟头搬进新房。KNOWN-GAPS只登'该有未有'的缺口,不登有意不搬的死代码以免污染清单。
- Q: Q5 文件落点
  A: A: 新建 crates/zbrain-core/src/ai/model_config.rs(ModelTier/TIER_DEFAULTS/DEFAULT_ALIASES/ConfigLookup trait/resolve_model/resolve_alias/enforce_subagent_capable),与 capabilities.rs/resolver.rs 并列,mod.rs re-export。下划线命名对齐 budget.rs/tool_loop.rs。
  > 与 TS model-config.ts+capabilities.ts 一一对应;resolver.rs 头部已把此层指向 sliced-out 到 1-2-1,新建兑现预告。塞回 resolver.rs 违背其'纯静态零engine耦合'边界声明。Phase8 收尾 ai/ 目录对称:resolver(纯静态校验)/capabilities(能力分类)/model_config(tier路由)/chat(provider+工厂)/tool_loop/expand。
<!-- ⚠️ ROADMAP_SECTION_END -->

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part7-phase9-jobs-minions.json` | 最后更新: 2026-07-13 14:49:05

[!][X+] 1. ZBrain TS→Rust Part7: Phase 9 — Jobs / Agents / Minions / Autopilot / Remote
├── [ ][X+] 1-1. MinionQueue + job 持久化 (queue.ts, job 生命周期/优先级/状态; jobs CLI 是其 thin wrapper)
│   ├── [x] 1-1-1. A+B: schema migration + Job 类型/status 枚举 + add/getJob/getJobs + claim/completeJob/failJob/renewLock/retryJob (最小可用队列, SKIP LOCKED 双后端岔口在此)
│   ├── [x][Y+] 1-1-2. C: 后台 sweep (promoteDelayed/handleStalled/handleTimeouts/handleWallClockTimeouts, 延迟提升/停滞恢复/超时→dead)
│   └── [~][X+] 1-1-3. D: 高级 (父子依赖 resolveParent/cancelJob 递归CTE + inbox sendMessage/readInbox + 附件CRUD + pause/resume/prune/getStats)
├── [ ][X+] 1-2. MinionWorker + supervisor (worker.ts/supervisor.ts/child-worker-supervisor.ts, 调 gateway.toolLoop 干活)
├── [ ][X+] 1-3. Budget + rate leases (budget-tracker/budget-*/rate-leases.ts, 成本上限与限流)
├── [ ][X+] 1-4. Minion handlers + tools (handlers/ + tools/, 具体任务类型: subagent/embed-backfill 等)
├── [ ][X+] 1-5. Autopilot + fanout (autopilot.ts/autopilot-fanout.ts 命令 + core)
├── [ ][X+] 1-6. Remote execution (remote.ts 命令 + 远程 fanout, 保 PII/trust 边界)
├── [ ][X+] 1-7. jobs/agent CLI 命令层 (jobs/jobs-watch/agent/agent-logs, thin wrapper over queue/worker)
└── [ ][X+] 1-8. G7 收口: webhook 接入 MinionQueue (替换 zbrain-web 直写 put_page + placeholder job_id)

### 当前施工：1-1-3. D: 高级 (父子依赖 resolveParent/cancelJob 递归CTE + inbox sendMessage/readInbox + 附件CRUD + pause/resume/prune/getStats)

**决策：**
- Q: D 层范围大, 切片粒度? → 拆 3 子节点: 1-1-3-1 父子依赖链+inbox(建 minion_inbox 表, 回改 add/complete_job/fail_job 父 hook, cancel_job 递归 CTE, resolve_parent, sweep 通知, send_message/read_inbox/read_child_completions) | 1-1-3-2 附件 CRUD(独立表 minion_attachments) | 1-1-3-3 运维(pause/resume/prune/get_stats) (取证结论: 父子依赖是横跨 5 处的内聚原子链(add-parent/complete hook/fail hook/cancel/sweep 通知), 全共用 child_done+resolve_parent 语义且都依赖 minion_inbox 表, 拆开会让队列处于'父子协调半成品'不一致态, 不可再拆. 附件是物理独立表与 jobs/父子零耦合, 运维大多不依赖新表, 天然可切分. 拒整体一刀(diff 巨大难 review), 拒 inbox 再拆细(send/read 与父子 hook 写同表同批 child_done, 捆一起更内聚). 关键发现: 1-1-1 迁 complete_job/fail_job 时砍掉了整个父 hook(token rollup+child_done inbox+resolve_parent+on_child_fail 三策略), 1-1-2 砍掉 sweep 父通知 → 1-1-3-1 不是纯新增, 要回改已落地的 add/complete_job/fail_job 三后端且套事务(现有是单条 UPDATE RETURNING 无事务, 加 hook 后多语句必须原子))

**当前子树：**
├── [x] 1-1-3-1. 父子依赖链 + inbox: 建 minion_inbox 表 + 回改 add/complete_job/fail_job 父 hook(套事务) + cancel_job 递归 CTE + resolve_parent + sweep dead 通知 + send_message/read_inbox/read_child_completions
├── [x] 1-1-3-2. 附件 CRUD: 建 minion_attachments 表(独立表, BYTEA/storage_uri 二选一 CHECK + UNIQUE(job_id,filename)) + add/list/get/delete attachment
└── [ ] 1-1-3-3. 运维: pause_job/resume_job/prune(DELETE RETURNING count)/get_stats(by_status/by_type/queue_health 三段纯读)
<!-- ROADMAP_SECTION_END -->
