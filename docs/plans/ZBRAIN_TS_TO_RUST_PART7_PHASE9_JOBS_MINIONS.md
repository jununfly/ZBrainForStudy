<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part7-phase9-jobs-minions.json` | 最后更新: 2026-07-13 20:18:34

[!][X+] 1. ZBrain TS→Rust Part7: Phase 9 — Jobs / Agents / Minions / Autopilot / Remote
├── [x][X+] 1-1. MinionQueue + job 持久化 (queue.ts, job 生命周期/优先级/状态; jobs CLI 是其 thin wrapper)
│   ├── [x] 1-1-1. A+B: schema migration + Job 类型/status 枚举 + add/getJob/getJobs + claim/completeJob/failJob/renewLock/retryJob (最小可用队列, SKIP LOCKED 双后端岔口在此)
│   ├── [x][Y+] 1-1-2. C: 后台 sweep (promoteDelayed/handleStalled/handleTimeouts/handleWallClockTimeouts, 延迟提升/停滞恢复/超时→dead)
│   └── [x][X+] 1-1-3. D: 高级 (父子依赖 resolveParent/cancelJob 递归CTE + inbox sendMessage/readInbox + 附件CRUD + pause/resume/prune/getStats)
├── [x][X+] 1-2. MinionWorker + supervisor (worker.ts/supervisor.ts/child-worker-supervisor.ts, 调 gateway.toolLoop 干活)
│   ├── [x] 1-2-1. 契约层: MinionHandler trait + MinionJobContext struct(双 CancellationToken+5 能力方法委托 engine) + MinionWorkerOpts (仅 zbrain-core, 引 tokio-util{sync}+async-trait)
│   ├── [x] 1-2-2. 队列消费核: worker 主循环 promoteDelayed->claim(<=concurrency)->launchJob->executeJob->complete/fail, 串行 concurrency=1 先跑通闭环 (zbrain-worker crate, 依赖 1-2-1)
│   ├── [x] 1-2-3. 并发池+生命周期: JoinSet 并发上限+per-job 续锁 interval+per-job 超时+SIGTERM/SIGINT->drain 30s+global vs per-job CancellationToken 分离 (依赖 1-2-2)
│   ├── [x] 1-2-4. 自监控外围(全可 flag 关): RSS parse_rss_from_proc_status/get_accurate_rss/check_memory_limit + self-health-check + quiet-hours defer + unhealthy 事件 (依赖 1-2-3)
│   └── [x] 1-2-5. supervisor+child-spawn: PID 锁+spawn 'zbrain jobs work' 子进程+退避 respawn+DB 健康检查(PG-only)+ZBRAIN_SUPERVISED 联动 (依赖 1-2-3, 可与 1-2-4 并行)
├── [~][X+] 1-3. Budget + rate leases (budget-tracker/budget-*/rate-leases.ts, 成本上限与限流)
│   ├── [x][Y+] 1-3-1. 新建 zbrain-budget crate: BudgetTracker + pricing + token-budget + audit trait（零DB，纯内存）
│   ├── [ ][Y+] 1-3-2. Engine 预算扩展: BrainEngine trait reserve_budget/refund/set_owner/halt/inherit + PG CAS 实现 + 0017 migration (ALTER minion_jobs + minion_budget_log)
│   └── [ ][Y+] 1-3-3. Engine 租约扩展: BrainEngine trait acquire_rate_lease/renew/release + fnv1a_64 hash + PG pg_advisory_xact_lock 实现 + 0017 migration (subagent_rate_leases 表)
├── [ ][X+] 1-4. Minion handlers + tools (handlers/ + tools/, 具体任务类型: subagent/embed-backfill 等)
├── [ ][X+] 1-5. Autopilot + fanout (autopilot.ts/autopilot-fanout.ts 命令 + core)
├── [ ][X+] 1-6. Remote execution (remote.ts 命令 + 远程 fanout, 保 PII/trust 边界)
├── [ ][X+] 1-7. jobs/agent CLI 命令层 (jobs/jobs-watch/agent/agent-logs, thin wrapper over queue/worker)
└── [ ][X+] 1-8. G7 收口: webhook 接入 MinionQueue (替换 zbrain-web 直写 put_page + placeholder job_id)

### 当前施工：1-3. Budget + rate leases (budget-tracker/budget-*/rate-leases.ts, 成本上限与限流)

grill 收敛 8 决策全 A。范围: A(内存 BudgetTracker)+B(job 级 DB CAS)+C(rate leases)+G(token-budget)。外围 D(minions/budget-meter client_id 每日)+E(enrichment/budget)+F(cycle/budget-meter legacy) 登记 KNOWN-GAPS 或后续子节点。

Crate 布局:
- zbrain-budget(新): BudgetTracker + ModelPricing/EmbeddingPricing 静态表 + token-budget pure fn + BudgetAuditor trait(默认 no-op) + BudgetExhausted error
- zbrain-core: engine trait +reserve_budget/refund/set_owner/halt/inherit(1-3-2) + acquire_rate_lease/renew/release(1-3-3) + fnv1a_64 hash + PG impl(postgres.rs)
- 0017 迁移: ALTER minion_jobs(D3 columns) + CREATE minion_budget_log + CREATE subagent_rate_leases

注入: gateway fn 显式 Option<&BudgetTracker> 参数，不用 task-local storage。审计: trait 抽象，首版 no-op，后续接 JSONL 文件写入

**决策：**
- Q: 1-3 范围边界？ → A: 首版核心 A+B+C（内存 BudgetTracker + job 级 DB CAS reserve + rate leases）+ 定价基础设施 + 独立 G(token-budget 零成本顺手带）。外围 D+E+旧版 F 后续子节点或 KNOWN-GAPS (A: 内存 BudgetTracker（gateway 注入路径）= zbrain-budget crate。B: minions/budget-tracker（minion_jobs CAS）= zbrain-core engine trait 扩展。C: rate-leases（subagent_rate_leases + pg_advisory_xact_lock）= zbrain-core。G: token-budget 纯函数无依赖。定价表：静态 const 嵌入 zbrain-budget)
- Q: BudgetTracker crate 归属？ → A: 新建 zbrain-budget crate。纯内存 tracker + 定价表(const) + token-budget + 审计 trait(默认 no-op)。零 DB 依赖 (被 gateway 引用，不依赖 zbrain-worker。zbrain-core 的 minion budget 扩展也依赖 zbrain-budget 的 BudgetExhausted 基础类型)
- Q: BudgetTracker 如何注入 gateway？ → A: 显式参数传递。gateway fn 签加 Option<&BudgetTracker>。调用方显式传，零魔法，编译器全追踪 (TS AsyncLocalStorage 在 Rust 无等价物。tokio::task::LocalKey 不跨 spawn。显式参数最 Rust 惯用)
- Q: Job 级预算如何接入 BrainEngine？ → A: trait 加方法 reserve_budget/refund_budget/set_owner_budget/halt_budget_subtree/inherit_budget_owner。PG 实现，默认 Unsupported (与 1-2-5 health_check() 模式一致。保持后端中立，不泄 SQL)
- Q: Rate leases 实现策略？ → A: BrainEngine trait 加 acquire_rate_lease/renew_rate_lease/release_rate_lease + fnv1a_64 纯函数。PG 实现，默认 Unsupported (pg_advisory_xact_lock PG 专有，InMemory 永不支持，trait 默认 Unsupported 干净)
- Q: Token-budget 放哪？ → A: zbrain-budget crate。与 BudgetTracker 同 crate，无额外依赖 (纯函数查 char/4 + 贪心截断，不污染 zbrain-core)
- Q: Schema 迁移策略？ → A: 新 0017_budget_rate_leases.sql，单文件含 ALTER minion_jobs + CREATE minion_budget_log + CREATE subagent_rate_leases (遵循现有顺序编号惯例，不修改已有迁移。同功能域放一个迁移文件)
- Q: 实现分片策略？ → A: 三子节点 1-3-1(zbrain-budget crate) + 1-3-2(Engine 预算扩展) + 1-3-3(Engine 租约扩展)。1-3-2+1-3-3 共享 0017 迁移 (1-3-1 零 DB 独立可测先做。1-3-2 依赖 1-3-1 的错误类型。1-3-3 独立于预算可并行)

**当前子树：**
├── [x][Y+] 1-3-1. 新建 zbrain-budget crate: BudgetTracker + pricing + token-budget + audit trait（零DB，纯内存）
├── [ ][Y+] 1-3-2. Engine 预算扩展: BrainEngine trait reserve_budget/refund/set_owner/halt/inherit + PG CAS 实现 + 0017 migration (ALTER minion_jobs + minion_budget_log)
└── [ ][Y+] 1-3-3. Engine 租约扩展: BrainEngine trait acquire_rate_lease/renew/release + fnv1a_64 hash + PG pg_advisory_xact_lock 实现 + 0017 migration (subagent_rate_leases 表)
<!-- ROADMAP_SECTION_END -->
