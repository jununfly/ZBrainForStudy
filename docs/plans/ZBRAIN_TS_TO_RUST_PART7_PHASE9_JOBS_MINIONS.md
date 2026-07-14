<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part7-phase9-jobs-minions.json` | 最后更新: 2026-07-14 13:57:59

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
├── [x][X+] 1-3. Budget + rate leases (budget-tracker/budget-*/rate-leases.ts, 成本上限与限流)
│   ├── [x][Y+] 1-3-1. 新建 zbrain-budget crate: BudgetTracker + pricing + token-budget + audit trait（零DB，纯内存）
│   ├── [x][Y+] 1-3-2. Engine 预算扩展: BrainEngine trait reserve_budget/refund/set_owner/halt/inherit + PG CAS 实现 + 0017 migration (ALTER minion_jobs + minion_budget_log)
│   └── [x][Y+] 1-3-3. Engine 租约扩展: BrainEngine trait acquire_rate_lease/renew/release + fnv1a_64 hash + PG pg_advisory_xact_lock 实现 + 0018 migration (subagent_rate_leases 表)
├── [x][X+] 1-4. Minion handlers + tools (handlers/ + tools/, 具体任务类型: subagent/embed-backfill 等)
│   ├── [x][X+] 1-4-1. Brain tools: ToolDef trait + buildBrainTools + brain-allowlist + to_gateway_tool 适配层
│   ├── [x][X+] 1-4-2. 低复杂度 handlers batch: sync/embed/lint/import/extract/backlinks/...
│   ├── [x][X+] 1-4-3. Autopilot + phase handlers: synthesize/patterns/consolidate/extract_facts/...
│   ├── [x][X+] 1-4-4. Handler trait + MinionHandlerRegistry + Subagent handler v1
│   └── [x][X+] 1-4-5. 中复杂度 handlers: embed-backfill + contextual-reindex + shell + 辅助
├── [ ][X+] 1-5. Autopilot + fanout (autopilot.ts/autopilot-fanout.ts 命令 + core)
│   ├── [ ][Y+] 1-5-1. Autopilot fanout 纯函数 + dispatchPerSource (resolveFanoutMax/readLastFullCycleAt/isSourceStale/selectSourcesForDispatch + dispatchPerSource 含 legacy fallback)
│   ├── [ ][Y+] 1-5-2. brain-score-recommendations 模块 (computeRecommendations/classifyChecks/maxReachableScore/estimateAnthropicCost + Check/RecommendationContext 类型)
│   ├── [ ][Y+] 1-5-3. Cycle 模块: types + phases + runCycle 编排器 (CyclePhase/ALL_PHASES/PhaseScope/PhaseResult/CycleReport/CycleOpts + runPhaseLint/Backlinks/Sync/Extract/Embed/Orphans 等 + runCycle 主函数)
│   ├── [ ][Y+] 1-5-4. Autopilot 主循环 + 模式判定 + reconnect 分类 (classifyReconnectError/shouldSpawnAutopilotWorker/runAutopilot 主循环 DB健康→reconnect→dispatch→sleep + report/showStatus)
│   ├── [ ][Y+] 1-5-5. Daemon install/uninstall (detectInstallTarget/generateLaunchdPlist/installLaunchd/installSystemd/installCrontab/installEphemeralContainer/uninstallDaemon/writeWrapperScript)
│   ├── [ ][Y+] 1-5-6. CLI 接线: autopilot 命令 args (--install/--uninstall/--status/--inline/--interval/--json) + dispatch 到 1-5-1~5
│   └── [ ][X+] 1-5-7. Nightly quality probe 子系统 (runNightlyQualityProbe + runLongMemEvalForProbe + runCrossModalBatchForProbe + AI gateway 集成) — LLM eval 子系统，deferred port
├── [ ][X+] 1-6. Remote execution (remote.ts 命令 + 远程 fanout, 保 PII/trust 边界)
├── [ ][X+] 1-7. jobs/agent CLI 命令层 (jobs/jobs-watch/agent/agent-logs, thin wrapper over queue/worker)
└── [ ][X+] 1-8. G7 收口: webhook 接入 MinionQueue (替换 zbrain-web 直写 put_page + placeholder job_id)
<!-- ROADMAP_SECTION_END -->
