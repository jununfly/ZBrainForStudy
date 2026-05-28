# 切片 6 完整性审计 & 迁移路线图

> 审计日期: 2026-05-28
> 基线: pglite-engine.ts (118 unique async methods) / engine.ts (89 trait 方法) / pglite-schema.ts (34 表)

---

## 1 差距量化

| 维度 | TS 基线 | Rust 已实现 | 缺口 | 覆盖率 |
|------|---------|------------|------|--------|
| pglite-engine.ts async 方法 | 118 | 7 (kind+connect+disconnect+init_schema+get_page+put_page+delete_page+list_pages+resolve_slugs, 其中 kind 不是 async) | 111 | 6% |
| engine.ts trait 方法 | 89 | 7 | 82 | 8% |
| schema CREATE TABLE | 34 | 2 (sources, pages) | 32 | 6% |
| schema CREATE INDEX/VIEW | 68 | 1 (pages_type_idx) | 67 | 1.5% |
| pglite-lock.ts 文件锁 | 3 symbols | 0 | 3 | 0% |

## 2 方法族分类（按业务域聚类）

### A. Page 完整 CRUD (已有骨架，需补齐)
- `softDeletePage` — 软删除 (FixMe 6.5a)
- `restorePage` — 恢复已删除
- `purgeDeletedPages` — 物理清除过期软删
- `findDuplicatePage` — 去重检测
- `refreshPageBody` — 刷新内容
- `updatePageContextualRetrievalState` — CR 状态更新
- `getAllSlugs` — 全量 slug 列表
- `listAllPageRefs` — 带 source_id 的轻量列表
- `updateSlug` — slug 改名
- `getPageTimestamps` — 批量取时间戳
- `getEffectiveDates` — 取生效日期
- `getSalienceScores` — 显著性分数

### B. Sources (0 → n)
- `listAllSources`
- `updateSourceConfig`

### C. Search / Chunks / Embeddings (0 → n)
- `searchKeyword` — 全文搜索
- `searchKeywordChunks` — 分片级搜索
- `searchVector` — 向量搜索
- `upsertChunks` — 写入分片
- `getChunks` / `getChunksWithEmbeddings`
- `countStaleChunks` / `listStaleChunks` / `deleteChunks`
- `getEmbeddingsByChunkIds` / `getTakeEmbeddings`

### D. Links / Graph (0 → n)
- `addLink` / `addLinksBatch` / `removeLink`
- `getLinks` / `getBacklinks` / `getBacklinkCounts`
- `findByTitleFuzzy`
- `traverseGraph` / `traversePaths`
- `getAdjacencyBoosts`
- `rewriteLinks`

### E. Tags (0 → n)
- `addTag` / `removeTag` / `getTags`

### F. Timeline (0 → n)
- `addTimelineEntry` / `addTimelineEntriesBatch`
- `getTimeline`

### G. Raw Data / Files / Dream (0 → n)
- `putRawData` / `getRawData`
- `upsertFile` / `getFile` / `listFilesForPage`
- `getDreamVerdict` / `putDreamVerdict`

### H. Facts / Trajectory (0 → n)
- `insertFact` / `insertFacts` / `deleteFactsForPage`
- `listFactsByEntity` / `listFactsSince` / `listFactsBySession` / `listSupersessions`
- `countUnconsolidatedFacts` / `findCandidateDuplicates`
- `expireFact` / `consolidateFact` / `getFactsHealth`
- `findTrajectory`
- `migrateFactsToCanonical`

### I. Takes / Eval / Calibration (0 → n)
- `addTakesBatch` / `listActiveTakesForPages`
- `listTakes` / `searchTakes` / `searchTakesVector`
- `countStaleTakes` / `listStaleTakes`
- `updateTake` / `supersedeTake` / `resolveTake`
- `getScorecard` / `getCalibrationCurve`
- `addSynthesisEvidence`
- `writeContradictionsRun` / `loadContradictionsTrend`
- `getContradictionCacheEntry` / `putContradictionCacheEntry` / `sweepContradictionCache`
- `logEvalCandidate` / `listEvalCandidates` / `deleteEvalCandidatesBefore`
- `logEvalCaptureFailure` / `listEvalCaptureFailures`

### J. Versions (0 → n)
- `createVersion` / `getVersions` / `revertToVersion`

### K. Ingest / Config / Migrations (0 → n)
- `logIngest` / `getIngestLog`
- `getConfig` / `setConfig` / `unsetConfig` / `listConfigKeys`
- `runMigration`

### L. Code Edges (0 → n)
- `addCodeEdges` / `deleteCodeEdgesForChunks`
- `getCallersOf` / `getCalleesOf` / `getEdgesByChunk`

### M. Emotional / Salience / Anomalies (0 → n)
- `batchLoadEmotionalInputs` / `setEmotionalWeightBatch`
- `getRecentSalience`
- `findAnomalies`

### N. Stats / Health / Orphans (0 → n)
- `getStats` / `getHealth`
- `findOrphanPages`
- `listPrefixSampledPages` / `listCorpusSample`

### O. Infrastructure / Locking (0 → n)
- `acquireLock` / `releaseLock` / `LockHandle` (pglite-lock.ts)

---

## 3 子切片拆分方案

每个子切片遵循：schema migration → trait 签名 → InMemory 桩 → Libsql 实现 → 测试 → 三连绿

| 子切片 | 方法族 | 方法数 | 新增表 | 依赖 |
|--------|--------|--------|--------|------|
| **6a** | Page 完整 CRUD (soft-delete 4 变体 + 补充查询) | ~12 | pages 增列(deleted_at, body 等) | 无 |
| **6b** | Sources + Config + Ingest + Migrations | ~8 | config, ingest_log | 6a |
| **6c** | Tags + Timeline + Raw Data + Files + Dream | ~10 | tags, timeline_entries, raw_data, files | 6a |
| **6d** | Links / Graph (含 traverse, backlinks) | ~11 | links | 6a |
| **6e** | Chunks / Embeddings / Search | ~10 | content_chunks | 6a, 6d |
| **6f** | Facts / Trajectory | ~12 | facts 相关(见 schema) | 6a, 6e |
| **6g** | Takes / Eval / Calibration / Contradictions | ~20 | takes/eval 多表 | 6a, 6e, 6f |
| **6h** | Versions + Code Edges + Emotional/Salience/Anomalies + Stats/Health | ~15 | page_versions, code_edges 等 | 6a |
| **6i** | Infrastructure: locking + OAuth + MCP log + minion + subagent + op_checkpoints | ~10 | 剩余所有表 | 6a |

> 总计: ~108 方法 (engine.ts 89 核心 + pglite 独有 29 = 118, 减去已实现 7 ≈ 111 新增方法, 上表按族聚合后 ~108)
> 总计: 32 张新表

## 4 方言适配备忘 (PG → SQLite)

| PG 特性 | SQLite 替代 |
|---------|------------|
| `BIGSERIAL` | `INTEGER PRIMARY KEY AUTOINCREMENT` |
| `TIMESTAMPTZ DEFAULT now()` | `TEXT DEFAULT (datetime('now'))` |
| `ON CONFLICT ON CONSTRAINT <name>` | `ON CONFLICT(<col1>, <col2>)` |
| `USING GIN(...)` | 丢弃或用 FTS5 VIRTUAL TABLE |
| `USING hnsw (...)` | 向量搜索暂 stub / 丢索引 |
| `gin_trgm_ops` | FTS5 替代 |
| `RETURNING` | SQLite 3.35+ 原生支持 |
| `?N` 占位符 | libsql 使用 `?1`, `?2`... |

## 5 决策点（已对齐 2026-05-28）

| # | 议题 | 决议 | 落地切片 |
|---|------|------|----------|
| 1 | 向量搜索 (searchVector / getEmbeddingsByChunkIds) | **Rust 等价实现**：embedding 以 BLOB 存 pages.embedding / content_chunks.embedding；查询时内存线性 cosine（O(n)，与 PG 原表行为一致，无索引加速但功能完整）。**否决** stub 路线。 | 6e |
| 2 | Minion / Subagent / OAuth 等运行时基础设施表 | **全量建空表**：保证 schema 一致性，幂等迁移一次建完。 | 6i |
| 3 | 全文搜索 (searchKeyword / searchKeywordChunks) | **FTS5 VIRTUAL TABLE + trigger 同步**：建 `pages_fts` / `chunks_fts` 虚拟表，CREATE TRIGGER AFTER INSERT/UPDATE/DELETE 镜像主表内容。**否决** LIKE %query% 临时路线。 | 6e |
| 4 | 子切片粒度 | **9 子切片（细粒度 6a–6i）**：保留 §3 表中拆分，便于每个 PR 单审。 | — |

---

## 6 子切片 6a 启动清单（Page 完整 CRUD）

**目标方法（12 个）**：

| # | 方法 | TS 行号 | schema 依赖 | 备注 |
|---|------|---------|------------|------|
| 1 | `findDuplicatePage` | pglite 817 | pages.content_hash / pages.frontmatter / pages.deleted_at | 新增列 |
| 2 | `softDeletePage` | pglite 900 | pages.deleted_at | 新增列 |
| 3 | `restorePage` | pglite 918 | pages.deleted_at | — |
| 4 | `purgeDeletedPages` | pglite 933 | pages.deleted_at | — |
| 5 | `refreshPageBody` | pglite 948 | — | 已有列即可 |
| 6 | `updatePageContextualRetrievalState` | pglite 971 | pages.cr_state（新增） | — |
| 7 | `getAllSlugs` | pglite 1071 | — | 返回 Set |
| 8 | `listAllPageRefs` | pglite 1088 | — | (slug, source_id) 对 |
| 9 | `updateSlug` | pglite 4189 | — | UPDATE pages SET slug |
| 10 | `getPageTimestamps` | pglite 2567 | pages.updated_at | — |
| 11 | `getEffectiveDates` | pglite 2577 | pages.effective_date | 新增列 |
| 12 | `getSalienceScores` | pglite 2596 | pages.salience_score | 新增列 |

**schema 迁移**：建 `0002_pages_full_columns.sql`（新增 `deleted_at`, `content_hash`, `frontmatter`, `effective_date`, `effective_date_source`, `import_filename`, `chunker_version`, `source_path`, `source_kind`, `source_uri`, `ingested_via`, `ingested_at`, `cr_state`, `salience_score`, `embedding` 等列）+ 升级 `SCHEMA_VERSION` 到 2。

**列类型与归属决策（2026-05-28 二次拍板）**：
- `cr_state` → SQLite `TEXT` 存 JSON 字符串（应用层 serde_json），不另立 `page_cr_state` 表。
- `embedding` → SQLite `BLOB`（可空），**6a 仅加列**，读写逻辑推到 6e。
- `salience_score` → SQLite `REAL`（可空），**6a 仅加列**，计算逻辑推到 6h。

**新增/扩展类型**：`Page` 结构追加可选字段；新增 `RefreshPageBodyArgs`、`FindDuplicatePageOpts`、`PageRef`、`PurgeResult` 等。

**测试矩阵**（红 → 绿）：
- duplicate: 同 content_hash 命中、同 frontmatter.id 命中、deleted 不命中
- soft/restore/purge: 三段式生命周期 + 过期阈值
- updateSlug: 普通改名 + 唯一冲突
- getAllSlugs: 跨 source 过滤
- getPageTimestamps / getEffectiveDates / getSalienceScores: 批量返回 Map 语义
- refreshPageBody: 不动其它字段
- updatePageContextualRetrievalState: cr_state JSON 写入
