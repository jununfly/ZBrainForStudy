<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part7-phase9-jobs-minions.json` | 最后更新: 2026-07-13 19:26:44

[!][X+] 1. ZBrain TS→Rust Part7: Phase 9 — Jobs / Agents / Minions / Autopilot / Remote
├── [x][X+] 1-1. MinionQueue + job 持久化 (queue.ts, job 生命周期/优先级/状态; jobs CLI 是其 thin wrapper)
│   ├── [x] 1-1-1. A+B: schema migration + Job 类型/status 枚举 + add/getJob/getJobs + claim/completeJob/failJob/renewLock/retryJob (最小可用队列, SKIP LOCKED 双后端岔口在此)
│   ├── [x][Y+] 1-1-2. C: 后台 sweep (promoteDelayed/handleStalled/handleTimeouts/handleWallClockTimeouts, 延迟提升/停滞恢复/超时→dead)
│   └── [x][X+] 1-1-3. D: 高级 (父子依赖 resolveParent/cancelJob 递归CTE + inbox sendMessage/readInbox + 附件CRUD + pause/resume/prune/getStats)
├── [~][X+] 1-2. MinionWorker + supervisor (worker.ts/supervisor.ts/child-worker-supervisor.ts, 调 gateway.toolLoop 干活)
│   ├── [x] 1-2-1. 契约层: MinionHandler trait + MinionJobContext struct(双 CancellationToken+5 能力方法委托 engine) + MinionWorkerOpts (仅 zbrain-core, 引 tokio-util{sync}+async-trait)
│   ├── [x] 1-2-2. 队列消费核: worker 主循环 promoteDelayed->claim(<=concurrency)->launchJob->executeJob->complete/fail, 串行 concurrency=1 先跑通闭环 (zbrain-worker crate, 依赖 1-2-1)
│   ├── [x] 1-2-3. 并发池+生命周期: JoinSet 并发上限+per-job 续锁 interval+per-job 超时+SIGTERM/SIGINT->drain 30s+global vs per-job CancellationToken 分离 (依赖 1-2-2)
│   ├── [x] 1-2-4. 自监控外围(全可 flag 关): RSS parse_rss_from_proc_status/get_accurate_rss/check_memory_limit + self-health-check + quiet-hours defer + unhealthy 事件 (依赖 1-2-3)
│   └── [ ] 1-2-5. supervisor+child-spawn: PID 锁+spawn 'zbrain jobs work' 子进程+退避 respawn+DB 健康检查(PG-only)+ZBRAIN_SUPERVISED 联动 (依赖 1-2-3, 可与 1-2-4 并行)
├── [ ][X+] 1-3. Budget + rate leases (budget-tracker/budget-*/rate-leases.ts, 成本上限与限流)
├── [ ][X+] 1-4. Minion handlers + tools (handlers/ + tools/, 具体任务类型: subagent/embed-backfill 等)
├── [ ][X+] 1-5. Autopilot + fanout (autopilot.ts/autopilot-fanout.ts 命令 + core)
├── [ ][X+] 1-6. Remote execution (remote.ts 命令 + 远程 fanout, 保 PII/trust 边界)
├── [ ][X+] 1-7. jobs/agent CLI 命令层 (jobs/jobs-watch/agent/agent-logs, thin wrapper over queue/worker)
└── [ ][X+] 1-8. G7 收口: webhook 接入 MinionQueue (替换 zbrain-web 直写 put_page + placeholder job_id)

### 当前施工：1-2. MinionWorker + supervisor (worker.ts/supervisor.ts/child-worker-supervisor.ts, 调 gateway.toolLoop 干活)

grill 收敛 5 决策(Q1 tokio/Q2 zbrain-worker crate/Q3 MinionHandler async_trait+context/Q4 5 片切分/Q5 外围 ①留桩②③④flag 做). 5 子节点 1-2-1~1-2-5 已建. TS 参照 worker.ts/supervisor.ts/child-worker-supervisor.ts. 待 /zj-tdd 从 1-2-1 起. 留桩: lease-full 依赖 1-3(实现到 1-2-3 时登记 KNOWN-GAPS).

**决策：**
- Q: 并发运行时选型 + worker 落点 crate 分层? → B: worker 是 runtime-aware 消费者层, 坦然用 tokio(tokio::spawn/JoinSet/CancellationToken/time::interval), 不做无谓 runtime 抽象; zbrain-core 保持 runtime 中立(仅 tokio::sync, 不启 rt-*), 只提供 BrainEngine+MinionQueue 数据层 + MinionHandler 纯 trait 定义 (确认: tokio 已是全 workspace 运行时(rt-multi-thread+macros+sync+process), sqlx/pg-embed 绑 tokio; core Cargo.toml 显式注释保持 runtime-agnostic。CancellationToken≈AbortController(可层级派生 global->per-job), JoinSet≈有上限 Promise pool, time::interval≈setInterval 续锁, 迁移认知负担最小。拒 std 线程池(与 async 基座割裂, 信号/timer/取消全手搓); 拒 async-std(sqlx/libsql 兼容不如 tokio))
- Q: worker 引擎物理落点 crate? → A: 新开 zbrain-worker crate, 依赖 zbrain-core(BrainEngine/MinionQueue/MinionHandler trait) + 启 tokio rt-multi-thread; worker+supervisor 同住(内聚: supervisor spawn 的子进程跑 worker.start()); zbrain-cli 的 jobs work(1-7) 做 thin wrapper 依赖它 (worker+supervisor 是可独立编译/测试的引擎单元, 埋进 CLI 会让集成测试背 CLI 包袱且复用受限(web/mcp 未来 in-process worker 可复用)。符合 core 已确立的 deep module 清晰边界品味。拒塞 cli(引擎与命令解析混); 拒塞 core(破坏 Q1 runtime 中立))
- Q: MinionHandler trait 形状 + MinionJobContext 建模与落点? → A: MinionHandler = #[async_trait] trait{ async fn handle(&self, ctx:&MinionJobContext)->Result<Value> }, 放 zbrain-core(纯 trait 契约, 与 Q1 一致 core 不 spawn). MinionJobContext 做 concrete struct 忠实映射 TS 对象, 持 Arc<dyn BrainEngine>+job_id+数据快照+双 CancellationToken(signal/shutdown), 5 个异步能力方法(update_progress/update_tokens/log/is_active/read_inbox)impl 在其上委托 engine 回写 DB. register(name, Arc<dyn MinionHandler>) 存 HashMap. core 新引 tokio-util{sync} — CancellationToken 是 runtime-agnostic(Arc<AtomicBool>+Notify), 不破坏 core runtime 中立
- Q: 1-2 切分粒度与子节点边界? → 5 片, 依赖单向: 1-2-1 契约层(MinionHandler trait+MinionJobContext+MinionWorkerOpts, 仅 core); 1-2-2 队列消费核(主循环 promoteDelayed->claim->launch->execute->complete/fail, 串行 concurrency=1 先通, 依赖 1-2-1); 1-2-3 并发池+锁续期+abort+graceful shutdown(JoinSet+per-job 续锁 interval+超时+SIGTERM drain 30s+global/per-job token 分离, 依赖 1-2-2); 1-2-4 自监控外围全可 flag 关(RSS parse/get/check+self-health+quiet-hours+unhealthy 事件, 依赖 1-2-3); 1-2-5 supervisor+child-spawn(PID 锁+spawn zbrain jobs work+退避 respawn+DB 健康检查 PG-only+ZBRAIN_SUPERVISED, 依赖 1-2-3 可与 1-2-4 并行). 原则每片独立 TDD/编译/验证
- Q: 外围功能取舍/留桩? → A: (1)lease-full 路径留桩(依赖 1-3 rate-leases 未迁)——1-2-3 fail 分支首版只做 普通失败/UnrecoverableError/退避重试, RateLeaseUnavailable 分支写 no-op 桩+KNOWN-GAPS 登记, 1-3 落地再接; (2)RSS 检测 parse_rss_from_proc_status 纯函数照迁可单测, 读取路径 Linux 走 /proc 非 Linux 返 None 降级, 整体 flag 关(maxRssMb 默认 disabled); (3)quiet-hours defer 首版做(不依赖未迁模块, quiet_hours 列已在, flag 可关); (4)self-health/unhealthy 用 mpsc::Sender<UnhealthyReason> 或 callback 替代 EventEmitter, 1-2-4 定义信号形状 1-2-5 消费. 总纲: ①留桩 ②③④首版做但全 flag 可关

**当前子树：**
├── [x] 1-2-1. 契约层: MinionHandler trait + MinionJobContext struct(双 CancellationToken+5 能力方法委托 engine) + MinionWorkerOpts (仅 zbrain-core, 引 tokio-util{sync}+async-trait)
├── [x] 1-2-2. 队列消费核: worker 主循环 promoteDelayed->claim(<=concurrency)->launchJob->executeJob->complete/fail, 串行 concurrency=1 先跑通闭环 (zbrain-worker crate, 依赖 1-2-1)
├── [x] 1-2-3. 并发池+生命周期: JoinSet 并发上限+per-job 续锁 interval+per-job 超时+SIGTERM/SIGINT->drain 30s+global vs per-job CancellationToken 分离 (依赖 1-2-2)
├── [x] 1-2-4. 自监控外围(全可 flag 关): RSS parse_rss_from_proc_status/get_accurate_rss/check_memory_limit + self-health-check + quiet-hours defer + unhealthy 事件 (依赖 1-2-3)
└── [ ] 1-2-5. supervisor+child-spawn: PID 锁+spawn 'zbrain jobs work' 子进程+退避 respawn+DB 健康检查(PG-only)+ZBRAIN_SUPERVISED 联动 (依赖 1-2-3, 可与 1-2-4 并行)
<!-- ROADMAP_SECTION_END -->
