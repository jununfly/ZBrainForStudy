
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

### 🔨 当前施工: 1-6-7. operations.ts 替换式迁移 (Rust OperationRegistry 为继任者): 107 op 逐一对齐, 随迁随删 TS; 覆盖审计见 docs/plans/OPERATIONS_TS_TO_RUST_AUDIT.md
**Status:** `in_progress` | **Mode:** `exploit`

**决策记录:**
- Q: 缺口 ops 处理策略（审计 per-op 不可信，探查发现 3 处硬伤）
  A: 就地补方法：每个 3…8 切片遇到无 engine 方法的 op，当场补 engine 方法 + 包成 Operation，不延后到独立切片
  > 探查修正：①schema-pack(9)实为 COMMAND_ELSEWHERE(Part10 Phase12 已100%完成)→从 operations.ts 摘除非 wrap；②recall 实为 TS-only(无 Rust CLI)→必须迁非摘除；③jobs/minions 仅6/12有方法，缺 get_job_progress/replay_job/send_job_message/submit_job/submit_agent/subagent；④NET_NEW=file_upload/search_by_image/get_brain_identity 零 Rust 实现。故不设 1-6-7-10，缺口在各自切片内联消化。
- Q: 执行节奏
  A: 一次推完 1-6-7-3…8 六切片：每切片三道门(core test/mcp test/cli build)全绿后独立 commit，切片间不打断；终局 1-6-7-9 下次再开
  > 用户选『一次推完』。
- Q: 终局前方向?
  A: 留在 1-6-7 补齐所有真实 op 缺口后再走 1-6-7-9 删 operations.ts；不转其它分支(1-1/1-2/1-7~1-9/1-11/1-12)
- Q: 缺口是否全迁?
  A: 全迁才终局(B2)：code-intel×7 + find_orphans + takes_list/search + search_by_image + run_doctor + sync_brain + takes_calibration/scorecard 全部 Rust 化后才走 1-6-7-9；sync_brain/run_doctor 虽大也迁，不砍不延迟；takes_calibration/scorecard 受 1-3-3 阻塞，排最后
- Q: 首刀与节点补齐?
  A: A3：先把 6 个未入账缺口建为 1-6-7 子节点（search_by_image/run_doctor/sync_brain/find_orphans/takes_list+search/takes_calibration+scorecard），takes_calibration+scorecard 标 blocked(1-3-3)；首刀开 code-intel×7(1-6-7-10)，其已建节点、bounded、无外部阻塞

**子节点:**
- [x] 1-6-7-1. 统一 live registry 汇总 + tracer-bullet: 汇总 operation.rs 碎片注册为全量 register_all, page 剩余 WRAP 首批迁 (update_slug/rewrite_links/soft_delete/page timestamps)
- [x] 1-6-7-2. 标注/图域迁移: tags(3)+links(5)+timeline(2)=10 op 迁入 register_all (8 纯 WRAP + traverse_graph 形状适配 + get_timeline wrap get_page + add_timeline_entry 日期校验)
- [x] 1-6-7-3. sources(4)+facts(3)+anomalies(1)+health-stats(3)=11 op 迁入 register_all (health-stats 实为3: health/salience/stats 仅是 cliHints 别名; facts 2 个简化 stand-in)
- [x] 1-6-7-4. jobs-minions 域 wrap (11 op: submit/submit_agent/list/get/get_progress/replay/send_message/cancel/retry/pause/resume; engine job 方法 + MinionQueue)
- [x] 1-6-7-5. ingestion+files+calibration+transcripts 域 (8 op 迁入 register_all; audit '9' 又偏, 实际 8 个 distinct op)
- [x] 1-6-7-6. schema-pack 域 9 op 从 operations.ts 摘除 (COMMAND_ELSEWHERE: Rust schema_pack 模块 Part10 Phase12 已 100% 覆盖为 CLI 命令)
- [x] 1-6-7-7. search-query 收尾: Rust search op (lexical) + QueryParams boost/filter axes (salience/recency/min_score/types); search_by_image 仍 NET_NEW
- [x] 1-6-7-8. commands-misc(13)+takes(4) 收尾: 含 get_brain_identity NET_NEW; takes_calibration/scorecard 待 1-3-3 解锁
- [x] 1-6-7-9. 终局: 删 operations.ts + cli.ts/mcp 切换 + typecheck baseline 0 new + 提交
- [x] 1-6-7-10. code-intel(7) ops — NET_NEW 代码图子系统 (存储+图查询+符号查询+消歧+递归遍历+缓存), 非薄 wrapper; 已拆 6 sub-node
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
