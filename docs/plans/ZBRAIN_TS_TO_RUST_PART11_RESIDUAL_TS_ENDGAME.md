<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part11 — 残留 TS 收尾 (综合容器)

### 树形视图 (depth=2)

```
[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [ ][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [ ][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [~][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
│   ├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
│   └── [!][X+] 1-3-3. calibration Phase 2 engine/LLM 支撑（queryAcrossBrains/aggregateDomainScorecards/undoWave/gateVoice/runAbTrial）
├── [~][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
│   ├── [x][Y+] 1-4-1. output page validators port (citation + triple-hr 纯字符串 + link + back-link engine-read)
│   └── [!][X+] 1-4-2. output infra port + TS 删除 [BLOCKED: BrainWriter 撞逃生舱禁令 + 消费者 integrity.ts/operations.ts 未迁]
├── [x][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ├── [x][X+] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
│   ├── [x][Y+] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
│   ├── [x][Y+] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
│   ├── [x][Y+] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
│   ├── [x][Y+] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
│   └── [x][Y+] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
├── [~][X+] 1-6. 孤儿命令迁移 (审计: 83 唯一活命令 = RUST_OWNED 17 / TRIVIAL_DELETE 27 / REAL_MIGRATE 33 / PARITY_REVIEW 6)
│   ├── [x][X+] 1-6-1. 孤儿命令审计 (TS 活 dispatch ~50 vs Rust 已注册, 分类 trivial-delete / real-migrate)
│   ├── [x][Y+] 1-6-2. RUST_OWNED 壳清理 (删TS副本, 过1-6-5对等闸门: config/query/search/get-page/list-pages/sync/takes/orphans/import/reconcile-links/skillpack/schema/init/doctor)
│   ├── [x][Y+] 1-6-3. TRIVIAL_DELETE 批 [已收口: 真零依赖仅3个 cache/claw-test/report 已整删; 原审计宣称27为过度分类, 20个带test_refs命令归1-6-4, discovery/network/parse非命令+call幽灵条目已从审计剔除]
│   ├── [x][X+] 1-6-4. REAL_MIGRATE 孤儿命令批 [去重后: 移出 skillify->1-1 / eval族->1-2 / calibration->1-3 / dream->1-12 / extract·export·integrity->1-4; 真孤儿=code-intel(code-*·reindex*·edges-backfill·backfill) + memory(recall·forget) + models·providers + whoknows·brainstorm·auth·features·storage·migrate·publish·extract-conversation-facts·resolvers·check-resolvable + 20个1-6-3归入带test命令]
│   └── [ ][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
```

### 🔨 当前施工: 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
**Status:** `in_progress` | **Mode:** `explore`

calibration 算法补齐：Rust 已有 calibration_queries.rs(DB 层) + web admin；待补 TS src/core/calibration 10 文件算法。2026-07-15 pivot 自 doctor 封顶后选此——领域自包含、边界清晰、不与 doctor 基建阻塞重叠。先探查 TS 算法边界与 Rust 缺口，再定整体 port 或按函数切片。

**决策记录:**
- Q: calibration 10 文件(1802 行)怎么切？
  A: 分阶段：Phase 1 先 port 纯函数(templates 5 builder / recall-footer / 纯解析器 parseJudgeOutput / 纯数学 computeForecast+resolveDomainPrefix / 纯规则 takeDomainHint+evaluateNudgeRule+buildLearningEntry / formatAbReport)，自包含可单测；Phase 2 再啃 engine/LLM 支撑(async 读引擎 + LLM 调用)，重 LLM 项(voice-gate gateVoice / think-ab runAbTrial)留 G-gap。
  > 与 doctor 切片同构：纯函数子集是便宜镜像，engine/LLM 子集是基建。不整体 port 避免大爆炸。
- Q: Phase 2 calibration 怎么切？全子集都卡在 engine trait 扩展或 LLM，非干净切片
  A: 不开大 Phase 2；开 1-3-2 = engine-read 子集（forecastForTake+batchForecast+get_scorecard domain_prefix），其余（mount 解析/execute_raw/LLM）留后续节点或登记 gap

**子节点:**
- [x] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
- [x] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
- [!] 1-3-3. calibration Phase 2 engine/LLM 支撑（queryAcrossBrains/aggregateDomainScorecards/undoWave/gateVoice/runAbTrial）
<!-- ⚠️ ROADMAP_SECTION_END -->

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-17 20:52:14

[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [ ][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [ ][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [~][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
│   ├── [x][Y+] 1-3-1. calibration 纯函数 port (Phase 1: 零依赖纯函数)
│   ├── [x][Y+] 1-3-2. calibration engine-read 子集 (forecastForTake + batchForecast + get_scorecard domain_prefix)
│   └── [!][X+] 1-3-3. calibration Phase 2 engine/LLM 支撑（queryAcrossBrains/aggregateDomainScorecards/undoWave/gateVoice/runAbTrial）
├── [~][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
│   ├── [x][Y+] 1-4-1. output page validators port (citation + triple-hr 纯字符串 + link + back-link engine-read)
│   └── [!][X+] 1-4-2. output infra port + TS 删除 [BLOCKED: BrainWriter 撞逃生舱禁令 + 消费者 integrity.ts/operations.ts 未迁]
├── [x][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ├── [x][X+] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
│   ├── [x][Y+] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
│   ├── [x][Y+] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
│   ├── [x][Y+] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
│   ├── [x][Y+] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
│   └── [x][Y+] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
├── [~][X+] 1-6. 孤儿命令迁移 (审计: 83 唯一活命令 = RUST_OWNED 17 / TRIVIAL_DELETE 27 / REAL_MIGRATE 33 / PARITY_REVIEW 6)
│   ├── [x][X+] 1-6-1. 孤儿命令审计 (TS 活 dispatch ~50 vs Rust 已注册, 分类 trivial-delete / real-migrate)
│   ├── [x][Y+] 1-6-2. RUST_OWNED 壳清理 (删TS副本, 过1-6-5对等闸门: config/query/search/get-page/list-pages/sync/takes/orphans/import/reconcile-links/skillpack/schema/init/doctor)
│   ├── [x][Y+] 1-6-3. TRIVIAL_DELETE 批 [已收口: 真零依赖仅3个 cache/claw-test/report 已整删; 原审计宣称27为过度分类, 20个带test_refs命令归1-6-4, discovery/network/parse非命令+call幽灵条目已从审计剔除]
│   ├── [~][X+] 1-6-4. REAL_MIGRATE 孤儿命令批 [去重后: 移出 skillify->1-1 / eval族->1-2 / calibration->1-3 / dream->1-12 / extract·export·integrity->1-4; 真孤儿=code-intel(code-*·reindex*·edges-backfill·backfill) + memory(recall·forget) + models·providers + whoknows·brainstorm·auth·features·storage·migrate·publish·extract-conversation-facts·resolvers·check-resolvable + 20个1-6-3归入带test命令]
│   └── [ ][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场

### 当前施工：1-6-4-10. resolvers 命令 + Resolver SDK 迁移 (interface/registry/url-reachable/x-api + CLI)

**决策：**
- Q: 先启动哪个孤儿命令（resolvers vs check-resolvable）? → resolvers（用户 2026-07-17 选定；check-resolvable 留待后续，其底层静态分析 core ~2300 行且 --fix 改写用户文件风险高）
- Q: 本切片切多深（x-api 是否一起迁）? → 全量迁：SDK 核心 + url-reachable + x-api/handle-to-tweet + CLI 全做。x-api 的 X API 网络路径在沙箱无 token 不可真跑，靠可注入的 HttpClient trait + MockHttpClient 离线测试（与 embedding/rerank/chat 既有的 trait+mock 模式一致）。url-reachable 直接复用 zbrain-core 已有的 url_safety::is_internal_url，不重迁 SSRF。
- Q: x-api 的 X API HTTP 调用怎么落地? → trait 在 core + live 在 core feature：zbrain-core 定义 HttpClient trait(async get+超时)+ MockHttpClient 测试替身；live ReqwestHttpClient 放 zbrain-core 新 resolvers feature(复用已有 optional reqwest)。register_builtin_resolvers() 在 feature 开启时建 live client；测试用 with_client(mock) 注入。token 在 resolve() 内读 ctx.secret('X_API_BEARER_TOKEN')。完全离线可测，零 CLI 依赖。
- Q: Rust 模块布局? → zbrain-core/src/resolvers/{mod.rs, interface.rs, registry.rs, url_reachable.rs, x_api.rs, http.rs} 镜像 TS src/core/resolvers/*。http.rs 放 HttpClient trait + MockHttpClient + (feature) ReqwestHttpClient。lib.rs 加 pub mod resolvers。
- Q: CLI 接口 parity? → 1:1 镜像 TS：zbrain resolvers list [--json] [--cost free|rate-limited|paid] [--backend <b>] 与 resolvers describe <id>。--cost/--backend 走 registry 过滤；unknown subcommand -> exit 1。
- Q: 测试策略? → TDD 垂直切片，忠实 port TS 717 行 resolvers.test.ts 的离线部分：registry 契约(单例/重复/过滤/错误码) + url-reachable(SSRF localhost/RFC1918/metadata、重定向逐跳、HEAD→GET 回退、AbortError) + x-api(纯函数 computeBackoffMs、available、非法 handle、零/单/多候选置信度分桶、401/403/500/429 错误码、关键字注入防护) 全部用 MockHttpClient。CI 防漂移锚点(若有)同步清空。
- Q: resolver 注册与 secret 读取模型 + feature 门控? → register_builtin_resolvers() 总是注册 url-reachable + x-handle-to-tweet 两个 resolver(镜像 TS)。ResolverContext.secret(name)->Option<String> 由闭包/struct 提供：运行时读 env X_API_BEARER_TOKEN + config，测试注入返回 token 的闭包。x-api resolve 无 token -> ResolverError(Config)。模块常编译；仅 ReqwestHttpClient + live register 路径在 resolvers feature 后；MockHttpClient 无条件编译供测试。
- Q: x-api 缺 token 的错误码（更正前条笔误） → 更正：TS ResolverErrorCode 无 'config' 变体；x-api 缺 token 时实际映射为 ResolverError(Unavailable)（TS 测试 resolvers.test.ts:461 断言 'unavailable'），available() 同样返回 false。前条 'ResolverError(Config)' 为笔误，以本条为准。
- Q: abort 信号怎么落地（url_reachable 实测修正）? → UrlReachableResolver 在 resolve() 最开头用 biased tokio::select 先查 req.context.abort.notified()，预触发则立即返回 ResolverError(Aborted)（忠实 TS checkReachable 开头检查，且避免与 mock 同步 ready 的 transport future 竞态）。逐跳循环内仍用 select 兼听 abort 处理 in-flight 取消。ResolverContext.abort: Arc<Notify>，defaults 到独立未触发 Notify。
- Q: DNS rebinding 防御 → url_reachable 忠实 port TS checkDnsRebinding：新增 url_safety::is_private_addr(IP)（判定 RFC1918/metadata/CGNAT/link-local/loopback/ULA 等私有范围），对解析出的 A/AAAA 逐条检查；命中即阻断。IP literal 跳过 DNS（is_internal_url 已挡私有）。DnsResolver trait 默认 mock 空、live 用 tokio::net::lookup_host（无新依赖）。

**当前子树：**
├── [x][Y+] 1-6-4-10-1. Resolver SDK 核心 (Resolver trait + ResolverRegistry + 类型 + ResolverError)
├── [x][Y+] 1-6-4-10-2. url-reachable resolver port (HEAD 检查 + SSRF 防护, 复用 url_safety::is_internal_url)
├── [ ][Y+] 1-6-4-10-3. x-api handle-to-tweet resolver (HttpClient trait + ReqwestHttpClient[resolvers feature] + Bearer + 429 退避 + 打分纯函数)
└── [ ][Y+] 1-6-4-10-4. resolvers CLI 接线 (list [--json/--cost/--backend] + describe <id>) + E2E smoke
<!-- ROADMAP_SECTION_END -->
