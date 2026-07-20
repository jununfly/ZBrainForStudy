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
│   ├── [~][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
│   └── [~] 1-6-6. skill/resolver 校验子系统全量迁 Rust (check-resolvable 全轨道): 覆盖 resolver-filenames / skill-frontmatter / skill-manifest / trigger-index(+parseResolverEntries) / check-resolvable core(checks 1-4) / repo-root / CLI / routing-eval(Check5) / filing-audit(Check6) / dry-fix(--fix) / 重接 doctor+skillify-check。非孤儿命令——是整条 skill 树校验栈，耦合 doctor/skillify-check 共享核心。
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场
```

### 🔨 当前施工: 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
**Status:** `in_progress` | **Mode:** `exploit`

**子节点:**
- [x] 1-6-5-1. skill_resolver 基础模块 Rust 化 (resolver-filenames + skill-frontmatter + skill-manifest + trigger-index 含 parseResolverEntries 表+紧凑列表双格式) + 单测
- [x] 1-6-5-2. check_resolvable 核心 checks 1-4 (reachability + missing_file + MECE overlap/gap + DRY + skillify_stub 扫描) Rust 化 + 单测
- [x] 1-6-5-3. repo-root skills-dir 自动检测 (autoDetectSkillsDir / ReadOnly + install_path 只读兜底 gated by is_gbrain_repo_root) Rust 化
- [x] 1-6-5-4. check_resolvable CLI 命令外壳 (clap 子命令 + flags/envelope/render/退出码, checks 1-4; --fix 暂拒绝提示) + 接线 lib.rs
- [x] 1-6-5-5. 收尾: 删除 TS check-resolvable CLI dispatch + 保留 TS core(被 doctor/skillify-check 消费, 记录决策) + PARITY_GATE + E2E + 提交
- [x] 1-6-5-6. routing-eval (Check 5): loadRoutingFixtures + indexResolverTriggers + lintRoutingFixtures + runRoutingEval Rust 化 + 接 check_resolvable 警告
- [x] 1-6-5-7. filing-audit (Check 6): loadFilingRules + runFilingAudit (writes_pages/writes_to 声明审计) Rust 化 + 接 check_resolvable 警告
- [ ] 1-6-5-8. dry-fix (--fix): autoFixDryViolations + CROSS_CUTTING REPLACE + MISSING_RULE INSERT + skill-fix-gates(git/code-fence) + skill-brain-first 合规 Rust 化
- [ ] 1-6-5-9. 重接 doctor / skillify-check: 待这两个 TS 命令各自迁移切片时改用 Rust skill_resolver 核心, 删除 TS src/core/check-resolvable.ts 及其依赖栈
<!-- ⚠️ ROADMAP_SECTION_END -->

<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-20 17:03:27

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
│   ├── [x][Y+] 1-6-5. PARITY_GATE (删除任何TS命令前: 确认零src引用+零test引用+真Rust覆盖非stub; 1-6-2/1-6-3共用)
│   ├── [x] 1-6-6. skill/resolver 校验子系统全量迁 Rust (check-resolvable 全轨道): 覆盖 resolver-filenames / skill-frontmatter / skill-manifest / trigger-index(+parseResolverEntries) / check-resolvable core(checks 1-4) / repo-root / CLI / routing-eval(Check5) / filing-audit(Check6) / dry-fix(--fix) / 重接 doctor+skillify-check。非孤儿命令——是整条 skill 树校验栈，耦合 doctor/skillify-check 共享核心。
│   └── [~][Y+] 1-6-7. operations.ts 替换式迁移 (Rust OperationRegistry 为继任者): 107 op 逐一对齐, 随迁随删 TS; 覆盖审计见 docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
├── [~][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
│   ├── [x][X+] 1-11-1. ingestion A类闭合簇删除 (src/core/ingestion 10文件 + ingest-capture.ts + 测试；Rust ingestion.rs/sync/import.rs/ingest_capture.rs 已覆盖)
│   └── [!][X+] 1-11-2. minions 纯删除探查 [BLOCKED: minions 100% 测试耦合, 无零引用叶子; A类纯删除已耗尽]
└── [!][X+] 1-12. cycle 大迁移 (runCycle 2057行主循环 + 20 phase 全未迁, Rust autopilot/cycle.rs 仅骨架 stub) — B类真迁移主战场

### 当前施工：1-6-7. operations.ts 替换式迁移 (Rust OperationRegistry 为继任者): 107 op 逐一对齐, 随迁随删 TS; 覆盖审计见 docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md

**决策：**
- Q: 缺口 ops 处理策略（审计 per-op 不可信，探查发现 3 处硬伤） → 就地补方法：每个 3…8 切片遇到无 engine 方法的 op，当场补 engine 方法 + 包成 Operation，不延后到独立切片 (探查修正：①schema-pack(9)实为 COMMAND_ELSEWHERE(Part10 Phase12 已100%完成)→从 operations.ts 摘除非 wrap；②recall 实为 TS-only(无 Rust CLI)→必须迁非摘除；③jobs/minions 仅6/12有方法，缺 get_job_progress/replay_job/send_job_message/submit_job/submit_agent/subagent；④NET_NEW=file_upload/search_by_image/get_brain_identity 零 Rust 实现。故不设 1-6-7-10，缺口在各自切片内联消化。)
- Q: 执行节奏 → 一次推完 1-6-7-3…8 六切片：每切片三道门(core test/mcp test/cli build)全绿后独立 commit，切片间不打断；终局 1-6-7-9 下次再开 (用户选『一次推完』。)

**当前子树：**
├── [x][Y+] 1-6-7-1. 统一 live registry 汇总 + tracer-bullet: 汇总 operation.rs 碎片注册为全量 register_all, page 剩余 WRAP 首批迁 (update_slug/rewrite_links/soft_delete/page timestamps)
├── [x][Y+] 1-6-7-2. 标注/图域迁移: tags(3)+links(5)+timeline(2)=10 op 迁入 register_all (8 纯 WRAP + traverse_graph 形状适配 + get_timeline wrap get_page + add_timeline_entry 日期校验)
├── [x][Y+] 1-6-7-3. sources(4)+facts(3)+anomalies(1)+health-stats(3)=11 op 迁入 register_all (health-stats 实为3: health/salience/stats 仅是 cliHints 别名; facts 2 个简化 stand-in)
├── [ ][Y+] 1-6-7-4. jobs-minions 域 wrap (12 op, engine job 方法)
├── [ ][Y+] 1-6-7-5. ingestion(4)+files-attachments(5) 域: 含 file_upload NET_NEW 校验器 validateUploadPath 信任边界
├── [ ][Y+] 1-6-7-6. schema-pack 域 (9 op: 多已迁命令→从 operations.ts 摘除, 少数 wrap)
├── [ ][Y+] 1-6-7-7. code-intel(7)+search-query(3) 收尾: 含 search_by_image NET_NEW
├── [ ][Y+] 1-6-7-8. commands-misc(13)+takes(4) 收尾: 含 get_brain_identity NET_NEW; takes_calibration/scorecard 待 1-3-3 解锁
└── [ ][Y+] 1-6-7-9. 终局: 删 operations.ts + cli.ts/mcp 切换 + typecheck baseline 0 new + 提交
<!-- ROADMAP_SECTION_END -->
