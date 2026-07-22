
<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part11 — 残留 TS 收尾 (综合容器)

### 树形视图 (depth=2)

```
[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [x][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [x][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
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
│   ├── [x][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
│   ├── [x] 1-6-6. skill/resolver 校验子系统全量迁 Rust (check-resolvable 全轨道): 覆盖 resolver-filenames / skill-frontmatter / skill-manifest / trigger-index(+parseResolverEntries) / check-resolvable core(checks 1-4) / repo-root / CLI / routing-eval(Check5) / filing-audit(Check6) / dry-fix(--fix) / 重接 doctor+skillify-check。非孤儿命令——是整条 skill 树校验栈，耦合 doctor/skillify-check 共享核心。
│   └── [~][Y+] 1-6-7. operations.ts 替换式迁移 (Rust OperationRegistry 为继任者): 107 op 逐一对齐, 随迁随删 TS; 覆盖审计见 docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md
├── [x][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [x][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [x][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
```

### 🔨 当前施工: 1-6-7-13. sync_brain op (大子系统): 跨源同步引擎 Rust 化，需拆 sub-node（sync/pull/push/conflict）
**Status:** `in_progress` | **Mode:** `explore`

Grill 决策树(2026-07-22, zj-grill-me + zj-roadmap-driven): Q1 真·增量 Rust 端口(sync 做成 Rust op,ingest 半复用既有 engine 方法); Q2 git shell-out(tokio::process::Command 调 git,零新依赖,对齐 TS execFileSync); Q3 git 封装放 zbrain-core/src/git.rs(薄 Git 客户端 struct); Q4 文件锁 advisory lock(跨进程,OS 退出自动释放); Q5 抽可复用 ingest_file(path,source_id,engine) 助手 + happy-path sync(git pull → 循环 ingest → 更新 last_commit/last_sync_at); Q6 13-3 = pull 边界 fallback(detached/no-origin/diverged 非致命回退)+ 文件锁生命周期,不做 push(push 归 federation 独立 surface); Q7 真实 git 集成测试(tempfile 临时 repo + InMemory engine 断言)。首刀 13-1 = git pull happy-path + 循环 ingest_file + 更新元数据。延后: 并行>1、push/federation、rename 检测等。

【实现校正 2026-07-22】动手写 13-1 时探索发现 crates/zbrain-core/src/sync/ 已是完整但未接线的 Rust sync 端口(core.rs perform_sync/perform_full_sync + import.rs import_one_path + anchor.rs + walker.rs + manifest.rs + concurrency.rs + failures.rs)。因此 grill Q5 '抽可复用 ingest_file' 被实践校正为：直接复用既有 sync::import::import_one_path 与 sync::anchor，不新抽助手。SyncBrainOperation = git pull(git.rs) → rev-parse HEAD → get_sync_anchor 读 previous → perform_sync(委派 import_one_path + 写回 last_commit)。13-1/13-2 已完成；13-3 非致命 pull 已预覆盖，剩 advisory 文件锁(Q4)与 diverged 告警。

**决策记录:**
- Q: sync_brain 编排/git-pull/git-push(13-1..3)交付策略?
  A: 真·增量 Rust 端口:sync 做成 Rust op;ingest 半(chunk/embed/write)复用既有类型化 engine 方法(put_page/upsert_chunks/list_sources);只新写 git shell-out(std::process::Command 调 git,对齐 TS 现有 execFileSync)+ 编排循环 + 元数据更新。按 tracer bullet 先打通 happy path 再补 diverged/conflict/federation 边界。
  > TS sync.ts 2616 行含大量 CLI/progress/readline 样板,不需全迁;Rust CLI 入口已在 zbrain-cli。
- Q: Rust 端 git 操作怎么做?
  A: shell-out 调 git CLI:用 tokio::process::Command 调系统 git,对齐 TS 现有 execFileSync('git')。零新依赖,完整覆盖 detached HEAD/diverged/worktree 边界,集成测试直接用真实 git 行为。
  > workspace 已有 tokio process feature;不引入 git2/gix。
- Q: git shell-out 封装放哪个模块?
  A: zbrain-core/src/git.rs:薄 Git 客户端 struct(pull/push/status/checkout 等方法),SyncBrainOperation(同 crate operation.rs)直接调用;测试在 zbrain-core 内用临时 repo。
  > 不新建 crate,不放 zbrain-cli(op 在 core)。
- Q: 同一 source 的并发 sync 怎么防重入?
  A: 文件锁 advisory lock:在 source 的 local_path 下加排他锁文件(如 .zbrain-sync.lock),syncOneSource 开始取锁、结束(Drop)释放。跨进程生效,OS 进程退出自动释放防崩溃残留。可用轻量 fd-lock crate。
  > 不用 in-memory Mutex(锁不住跨进程)、不用 DB 标志位(需清理 stale lock)。
- Q: 13-1 首刀 tracer-bullet 怎么做?
  A: 抽可复用 import_from_path + happy-path sync:在 zbrain-core 提取 ingest_file(path, source_id, engine)(read→chunk→embed→put_page/upsert_chunks),syncOneSource 循环调用;配 git pull happy path(有 origin/clean/ff)+ 更新 last_commit/last_sync_at。ingest 助手同时服务未来 capture-from-file/import。延后 diverged/detached/conflict/并行>1/federation push。
  > Rust 现状无按路径摄取操作(仅 capture_content 吃字节),故需新抽 ingest 助手——深度模块,避免未来 import/add 重写。
- Q: 13-3 实际范围怎么定?
  A: pull 边界 + lock 收尾:13-3 实现 git pull 的非致命 fallback(detached HEAD / no origin / remote diverged → 回退本地状态并告警),并把 Q4 文件锁接入 syncOneSource 生命周期。不做 push——push 属 zbrain sources federate(联邦命令,独立 surface)。原标签'git push+冲突'语义有偏差,需校正为 pull 边界。
  > TS 已确认 sync_brain 只 pull 不 push;push 触发的是 webhook 队列回调,非主动推。
- Q: git-sync 怎么测?
  A: 真实 git 集成测试:用 tempfile 起临时 git repo(git init + 提交),source.local_path 指向它,跑 Rust sync op 对 InMemory engine,断言 engine 状态(pages/chunks 数、last_commit 更新)。最贴近真实 git 行为,CI 有 git 可用。
  > 不为测试抽 GitClient trait(Q2 已拒 trait 抽象);Git 客户端保持具体 struct,集成测试用真实 git。

**子节点:**
- [x] 1-6-7-13-1. SyncBrainOperation: git pull happy-path → 委派既有 sync::core::perform_sync(reuse import_one_path + anchor, 不新抽 ingest_file)
- [x] 1-6-7-13-2. git pull happy path 封装到 zbrain-core/src/git.rs(有 origin / clean / fast-forward)
- [x] 1-6-7-13-3. git pull 边界 fallback(detached HEAD / no origin / remote diverged → 回退本地状态并告警) + 文件锁接入 syncOneSource 生命周期(不做 push,push 归 federation)
- [x] 1-6-7-13-4. sync 状态与报告: buildSyncStatusReport/printSyncStatusReport/resolveParallelism/syncOneSource/runSyncTrigger/manageGitignore (Rust 端口, 多为只读)
<!-- ⚠️ ROADMAP_SECTION_END -->

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-21 20:34:59

[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [x][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [x][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
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
│   ├── [x][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
│   ├── [x] 1-6-6. skill/resolver 校验子系统全量迁 Rust (check-resolvable 全轨道): 覆盖 resolver-filenames / skill-frontmatter / skill-manifest / trigger-index(+parseResolverEntries) / check-resolvable core(checks 1-4) / repo-root / CLI / routing-eval(Check5) / filing-audit(Check6) / dry-fix(--fix) / 重接 doctor+skillify-check。非孤儿命令——是整条 skill 树校验栈，耦合 doctor/skillify-check 共享核心。
│   └── [~][Y+] 1-6-7. operations.ts 替换式迁移 (Rust OperationRegistry 为继任者): 107 op 逐一对齐, 随迁随删 TS; 覆盖审计见 docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md
├── [x][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [x][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [x][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [x][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场

### 当前施工：1-6-7-11. search_by_image op (NET_NEW 图像嵌入): embedMultimodal + searchVector(embedding_image)，Rust 新增图像检索后端

**决策：**
- Q: 图片加载实现策略 → 完整实现 SSRF 防护 + 三种输入格式支持（image_path/image_url/image_data） (远程 MCP 调用 image_path 需要安全防护，一次性对齐 TS 安全模型)
- Q: 消费记录 schema 迁移范围 → 只添加 spend_log 表到 Rust migrations，满足 search_by_image 读写需求 (oauth_clients/mcp_spend_reservations 延后到 sync_brain 整体迁移 OAuth 时处理)
- Q: 嵌入调用扩展方式 → 新增单独 embed_image 方法，保持现有文本嵌入结构不变 (增量修改影响小，满足单张图片嵌入需求即可，长远多模态扩展后续再考虑)
- Q: 向量检索 trait 设计 → BrainEngine trait 新增 search_pages_by_embedding 默认方法，三后端各自实现 (通用能力放在 trait 里，Postgres 默认返回空（G23 缺口），InMemory/libsql 实现真实检索)
- Q: RRF 融合位置 → 在 search_by_image op handler 内部做 RRF 融合，不改动现有 fuse_and_boost (增量修改不影响现有 search_pages 调用，只有 search_by_image 需要混合两个检索结果)
<!-- ROADMAP_SECTION_END -->
