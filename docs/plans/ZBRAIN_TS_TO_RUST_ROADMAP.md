<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-roadmap.json` | 最后更新: 2026-06-30 17:47:04

[~][X+] 1. ZBrain TS to Rust Migration
├── [x][Y+] 1-1. Roadmap and TypeScript runtime inventory
│   ├── [x][Y+] 1-1-1. Restore canonical roadmap files
│   ├── [x][Y+] 1-1-2. Expand complete TS to Rust PRD from codebase facts
│   ├── [x][Y+] 1-1-3. Classify TypeScript runtime and frontend retention surfaces
│   └── [x][Y+] 1-1-4. Define per-slice deletion checklist and verification gates
├── [x][Y+] 1-2. Core storage parity closure
│   ├── [x][Y+] 1-2-1. Finish Page contract parity across storage backends
│   ├── [x][Y+] 1-2-2. Port missing advanced Page writes to Rust
│   ├── [x][Y+] 1-2-3. Move schema migrations ownership to Rust
│   ├── [x][X+] 1-2-4. Decide internal DB legacy identifier migration
│   └── [x][Y+] 1-2-5. Implement DB legacy identifier rename migration
├── [x][Y+] 1-3. Config bootstrap and package entrypoint cutover
│   ├── [x][Y+] 1-3-1. Port config discovery loading and writing to Rust
│   ├── [x][Y+] 1-3-2. Port init doctor config storage and schema commands
│   ├── [x][Y+] 1-3-3. Cut package bin and install flow to Rust binary
│   └── [x][Y+] 1-3-4. Delete replaced TypeScript bootstrap command surface
├── [x][Y+] 1-4. Operations layer and trust boundary migration
│   ├── [x][Y+] 1-4-1. Port operation definitions schemas and context
│   ├── [x][Y+] 1-4-2. Port local and remote trust boundary enforcement
│   └── [x][Y+] 1-4-3. Move shared CLI MCP dispatch to Rust operations
├── [x][Y+] 1-5. MCP server migration
│   ├── [x][Y+] 1-5-1. Implement Rust MCP tool definitions and parameter validation
│   ├── [x][Y+] 1-5-2. Implement Rust MCP server completion: engine wiring, rate limiting, and audit hooks
│   └── [x][Y+] 1-5-3. Delete TypeScript MCP implementation after parity
├── [~][Y+] 1-6. Web backend and admin API migration
│   ├── [x][Y+] 1-6-1. Implement Axum skeleton with admin auth, health, and SPA serving
│   ├── [~][Y+] 1-6-2. Port admin API business routes
│   ├── [ ][X+] 1-6-3. Retain React TypeScript admin frontend by explicit decision
│   └── [ ][Y+] 1-6-4. MCP HTTP dispatch with OAuth 2.1 and webhooks
├── [ ][Y+] 1-7. Ingestion sources search and retrieval migration
│   ├── [ ][Y+] 1-7-1. Port source management import capture extract and sync flows
│   ├── [ ][Y+] 1-7-2. Port embeddings chunking hybrid search and reindex flows
│   └── [ ][Y+] 1-7-3. Delete replaced TypeScript ingestion search and source modules
├── [ ][Y+] 1-8. Facts takes timeline salience and graph migration
│   ├── [ ][Y+] 1-8-1. Port facts takes timeline salience backlinks orphans and graph behavior
│   └── [ ][Y+] 1-8-2. Delete replaced TypeScript knowledge graph modules
├── [ ][Y+] 1-9. AI gateway providers models and routing migration
│   ├── [ ][Y+] 1-9-1. Port provider config model capabilities pricing and routed gateway
│   └── [ ][Y+] 1-9-2. Preserve routed gateway and no direct provider guardrails
├── [ ][Y+] 1-10. Jobs agents minions autopilot and remote execution migration
│   ├── [ ][Y+] 1-10-1. Port jobs lifecycle agent logs minions autopilot fanout and remote execution
│   └── [ ][Y+] 1-10-2. Preserve privacy PII and remote execution trust guardrails
├── [ ][Y+] 1-11. Evals benchmarks and developer tooling migration
│   ├── [ ][X+] 1-11-1. Decide product critical evals and benchmarks
│   └── [ ][Y+] 1-11-2. Port or archive TypeScript eval and developer tooling
└── [ ][Y+] 1-12. Final cutover and TypeScript runtime cleanup
    ├── [ ][Y+] 1-12-1. Remove TypeScript runtime package exports and entrypoints
    ├── [ ][Y+] 1-12-2. Add TypeScript runtime residue guard with frontend allowlist
    ├── [ ][Y+] 1-12-3. Verify final Rust workspace and retained TypeScript surfaces
    └── [ ][Y+] 1-12-4. Update docs examples and release baseline for Rust first ZBrain

### 当前施工：1-6-2. Port admin API business routes

Layer A (12 endpoints) → 5 issues: #70 AdminQueries trait + session, #74 stats(3), #73 agents(1), #72 api_keys(3), #71 requests(1). 全部 enhancement + ready-for-agent。AdminQueries trait 8 方法注入 AppState.admin: Arc<dyn AdminQueries>。路由: /admin/api/*。模块: src/admin/{mod,stats,api_keys,agents,requests,session}.rs。

**决策：**
- Q: Q1: 1-6-2 范围边界如何划分？ → 四层拆分：A层(1-6-2本体) stats/health-indicators/full-stats/sign-out-everywhere/api-keys/agents/requests — 无阻塞直接port; B层(归入1-6-4) register-client/update-client-ttl/revoke-client/issue-magic-link/auth/:token/events — OAuth/MCP/SSE依赖; C层(新建1-6-2-1) calibration/* — 重型子系统; D层(新建1-6-2-2) jobs/watch+agents/spend — minion_jobs依赖 (18个TS端点→12个归入本节点+6个归入1-6-4+3个calibration+2个jobs/spend。layer A不需要新依赖，现有engine SQL查询能力已覆盖。)
- Q: Q2: 12 个端点如何组织到 Rust 模块？ → 方案A: 按领域分模块 — admin/stats.rs(3) + admin/api_keys.rs(3) + admin/agents.rs(1) + admin/requests.rs(1) + admin/session.rs(1) (每个模块返回 Router，在 build_router() 中 .merge()。require_admin 中间件统一挂 /admin/api/* 层级。API keys 的 GET/POST/revoke 内聚在一个模块中。)
- Q: Q3: admin 端点数据访问层方案？ → 方案B: 在 zbrain-core 中新建 AdminQueries trait（6-8个查询方法），zbrain-web 依赖该 trait (Admin 查询读的是 oauth_clients/access_tokens/mcp_request_log/api_keys 这些认证审计表，与 BrainEngine 的脑内容(pages/tags/files)是不同关注面。单独 trait 不膨胀 BrainEngine。方法签名如 list_agents() -> Vec<AgentInfo>, list_api_keys() -> Vec<ApiKey>, list_requests(opts) -> Paginated<RequestLog>, get_stats() -> Stats, get_full_stats() -> FullStats, check_health() -> HealthIndicators。)
- Q: Q4: 1-6-2 拆成几个 tracer-bullet？ → 方案A: 5个 tracer-bullet issue, 每个独立可测互不阻塞 (#1 AdminQueries trait + session.rs / #2 stats.rs(3) / #3 agents.rs(1) / #4 api_keys.rs(3) / #5 requests.rs(1))
- Q: Q5: AdminQueries trait 如何注入 AppState？ → 方案A: AppState 加独立的 admin: Arc<dyn AdminQueries> 字段 (同一个 Arc<EngineImpl> cast 成 Arc<dyn BrainEngine> 和 Arc<dyn AdminQueries>，分别注入 engine 和 admin 字段。handler 按需取 app_state.admin.list_agents()。避免 nightly 的 dyn BrainEngine+AdminQueries 联合 trait object。)
- Q: Q6: Admin API 路径保持 /admin/api/* 还是简化？ → 方案A: 保持 /admin/api/* (与 TS 一致，SPA 前端无需改 API base URL。避免给 1-6-3 制造额外工作量。路由前缀 /admin/api/* 与 /admin/login (auth)、/admin/{*path} (SPA) 泾渭分明。)
- Q: Q7: AdminQueries trait 需要几个方法？ → 8个方法: get_stats()/get_full_stats()/check_health_indicators()/list_agents()/list_api_keys()/create_api_key()/revoke_api_key()/list_requests() (get_full_stats 由 AdminQueries 实现但内部分析 page_count/chunk_count 可委托 engine 已有逻辑。其余方法针对 oauth_clients/access_tokens/mcp_request_log 表独立实现。)

**当前子树：**
├── [ ][X+] 1-6-2-1. Port calibration subsystem (profiles, takes, SVG rendering)
└── [ ][X+] 1-6-2-2. Port jobs/watch and agents/spend endpoints
<!-- ROADMAP_SECTION_END -->
