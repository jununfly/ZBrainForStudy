<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part11-residual-ts-endgame.json` | 最后更新: 2026-07-15 19:26:58

[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [ ][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [ ][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [ ][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
├── [ ][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
├── [~][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ├── [x][X+] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
│   ├── [ ][Y+] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
│   ├── [ ][Y+] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
│   ├── [~][Y+] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
│   ├── [ ][Y+] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
│   └── [ ][Y+] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
├── [ ][X+] 1-6. 孤儿命令迁移 (~20 命令：whoknows/brainstorm/dream/publish/models/providers/...)
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
└── [ ][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)

### 当前施工：1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)

eval_drift 已迁（首端口，端到端模式验证完成）：Rust zbrain_core::eval_drift 模块(matches_watch_pattern/files_drifted_since/watched_files_drifted/eval_drift_status + 3 单测) → 接线 lib.rs doctor runner 替换 NotImplemented → 从 UNMIGRATED_TS_DOCTOR_CHECKS 锚点移除 + 加 eval_drift_is_no_longer_unmigrated 守卫测试 → 删 TS drift-watch.ts + tests/unit/drift-watch.test.ts(死代码,无活 importer) → typecheck 绿(64 inherited,无新增)。**教训**：删 TS 模块须同时 grep tests/unit/ 的锚点测试(不止 src/)，否则留 TS2307 新错误。待 skill_conformance / frontmatter_integrity。
<!-- ROADMAP_SECTION_END -->

<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## Part11 — 残留 TS 收尾 (综合容器)

### 树形视图 (depth=2)

```
[~][X+] 1. Part11 — 残留 TS 收尾 (综合容器)
├── [ ][X+] 1-1. skillpack / skillify 迁移 (27+ 文件 Schema/Subagent 包)
├── [ ][X+] 1-2. eval 一族迁移 (~20 eval-* 命令 + src/eval + core/eval)
├── [ ][X+] 1-3. calibration 算法迁移 (10 文件，当前仅 DB 层)
├── [ ][X+] 1-4. output 模块迁移 (src/core/output 9 文件)
├── [~][X+] 1-5. doctor 11 项健康检查迁移 (G5)
│   ├── [x][X+] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
│   ├── [ ][Y+] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
│   ├── [ ][Y+] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
│   ├── [x][Y+] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
│   ├── [x][Y+] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
│   └── [ ][Y+] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
├── [ ][X+] 1-6. 孤儿命令迁移 (~20 命令：whoknows/brainstorm/dream/publish/models/providers/...)
├── [ ][X+] 1-7. search core 模块补齐 (C 类，src/core/search 23 文件)
├── [ ][X+] 1-8. facts core 模块补齐 (C 类，src/core/facts 13 文件)
├── [ ][X+] 1-9. think core 模块补齐 (C 类，src/core/think 7 文件)
├── [ ][X+] 1-10. G38 schema-pack TS 删除尾 (gate=operations.ts 移植)
└── [ ][X+] 1-11. A 类已迁 TS 删除 (minions/ai/ingestion/cycle + 命令)
```

### 🔨 当前施工: 1-5. doctor 11 项健康检查迁移 (G5)
**Status:** `in_progress` | **Mode:** `explore`

**决策记录:**
- Q: 1-5 doctor 内部拆法?
  A: 聚类拆 6 子节点：1-5-1 探查+tracer bullet(定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口) → 1-5-2 基础健康类(embedding/sync_freshness/federation) → 1-5-3 配置模式类(search_mode/resolver/schema_packs) → 1-5-4 内容一致性类(skill_conformance/frontmatter/eval_drift) → 1-5-5 评分类(brain_score/takes_weight_grid) → 1-5-6 收尾(删 TS doctor+缩 typecheck 基线+锚点清空)。每聚类内逐 check 迁完立即从 UNMIGRATED_TS_DOCTOR_CHECKS 锚点移除。
  > 用户 2026-07-15 决策
- Q: doctor 检查逻辑落点?
  A: 镜像 reranker_health 模式：检查逻辑放 zbrain_core 对应领域模块（embedding→embedding 模块、config→config、DB/engine→engine），lib.rs doctor runner 只组装 DoctorCheck+计分。逻辑可单测可复用、与既有分层一致。
  > 用户 2026-07-15 决策
- Q: doctor 端口顺序 refinement?
  A: 先迁最低风险：eval_drift(纯死代码 FS/git,无活命令依赖)+takes_weight_grid(Rust takes_fence.rs 已存在)打头验证端到端模式；再迁活命令镜像类(resolver/skill_conformance/frontmatter/sync_freshness/federation/embedding/search_mode)；schema_packs 最后(被 G38 纠缠,须与 schema_pack 信任门同步)。跨聚类挑最低风险项先打,不严格按 1-5-2→1-5-5 顺序。
  > 用户未明确,agent 提议(基于 tracer bullet 结论)

**子节点:**
- [x] 1-5-1. doctor 探查 + tracer bullet (定位 11 检查 TS 实现与 Rust 依赖、确认 runner 入口)
- [ ] 1-5-2. 基础健康类检查迁移 (embedding_health / sync_freshness / federation_health)
- [ ] 1-5-3. 配置模式类检查迁移 (search_mode / resolver_health / schema_packs)
- [x] 1-5-4. 内容一致性类检查迁移 (skill_conformance / frontmatter_integrity / eval_drift)
- [x] 1-5-5. 评分类检查迁移 (brain_score / takes_weight_grid)
- [ ] 1-5-6. doctor 收尾 (删 TS doctor + 缩 typecheck 基线 + 锚点常量清空)
<!-- ⚠️ ROADMAP_SECTION_END -->
