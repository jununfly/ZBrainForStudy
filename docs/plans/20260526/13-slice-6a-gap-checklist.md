# 切片 6a Gap Checklist — 待定细节 & Plan 遗漏

> 盘点日期: 2026-05-28
> 范围: 6a (Page 完整 CRUD) 启动前的差距盘点；跨切片的前置假设也一并收录

---

## 0 盘点方法

| 基线 | 文件 |
|------|------|
| TS schema | `src/core/pglite-schema.ts` pages 区段 (L70-160) |
| TS 118 方法 | `src/core/pglite-engine.ts` |
| Rust Page 结构 | `crates/zbrain-core/src/engine.rs` |
| Rust trait | `crates/zbrain-core/src/engine.rs` BrainEngine |
| Rust LibsqlEngine | `crates/zbrain-core/src/libsql.rs` |
| SQLite migration 0001 | `migrations-sqlite/0001_init.sql` |
| SQLite migration 0002 | `migrations-sqlite/0002_pages_full_columns.sql` |
| 审计文档 | `docs/plans/20260526/12-slice-6-audit.md` |

---

## 1 Schema ↔ Page 结构字段对齐

TS `pages` 表有 27 列，0002 migration 对齐后也应有 27 列。
当前 `Page` 结构 24 字段。

| # | 列名 (schema) | `Page` 字段 | 状态 |
|---|--------------|------------|------|
| 1 | id | id | ✅ |
| 2 | source_id | source_id | ✅ |
| 3 | slug | slug | ✅ |
| 4 | type | page_type | ✅ |
| 5 | page_kind | page_kind | ✅ |
| 6 | title | title | ✅ |
| 7 | compiled_truth | compiled_truth | ✅ |
| 8 | timeline | timeline | ✅ |
| 9 | frontmatter | frontmatter | ✅ |
| 10 | content_hash | content_hash | ✅ |
| 11 | emotional_weight | emotional_weight | ✅ |
| 12 | created_at | created_at | ✅ |
| 13 | updated_at | updated_at | ✅ |
| 14 | deleted_at | deleted_at | ✅ |
| 15 | effective_date | effective_date | ✅ |
| 16 | effective_date_source | effective_date_source | ✅ |
| 17 | import_filename | import_filename | ✅ |
| 18 | salience_touched_at | salience_touched_at | ✅ |
| 19 | last_retrieved_at | — | ❌ **缺失** |
| 20 | contextual_retrieval_mode | contextual_retrieval_mode | ✅ |
| 21 | corpus_generation | corpus_generation | ✅ |
| 22 | generation | — | ❌ **缺失** |
| 23 | embedding (BLOB) | — | ❌ **缺失** (6e 读写, 但 6a 需占位) |
| 24 | chunker_version (0002 ADD) | — | ❌ **缺失** |
| 25 | source_path (0002 ADD) | — | ❌ **缺失** |

> **结论**: `Page` 需追加 5 个字段: `last_retrieved_at`, `generation`, `embedding`, `chunker_version`, `source_path`。
> `PageInput` 同步需追加: `last_retrieved_at`, `generation` (可能只读?), `embedding` (6e 写), `chunker_version`, `source_path`。

---

## 2 Generation Trigger 对齐

| 维度 | TS (PG) | Rust (SQLite 0002) | 状态 |
|------|---------|-------------------|------|
| 时机 | BEFORE INSERT OR UPDATE | AFTER INSERT / AFTER UPDATE OF … | ✅ 已适配 (SQLite 限制) |
| INSERT 逻辑 | `COALESCE(MAX(generation), 0) + 1` | 同 | ✅ |
| UPDATE 允许列 | compiled_truth, timeline, frontmatter, deleted_at, cr_mode, title, type, page_kind, corpus_generation, content_hash (10 列) | compiled_truth, title, frontmatter, deleted_at, contextual_retrieval_mode, corpus_generation, content_hash (7 列) | ❌ **缺 3 列**: `timeline`, `type`, `page_kind` |
| IS DISTINCT FROM | PG 原生 | SQLite `IS NOT` (NULL-unsafe) | ⚠️ **边界**: 当新旧值同为 NULL 时 `IS NOT` 返回 FALSE (正确)；当一端 NULL 另一端非 NULL 时返回 TRUE (正确)；当两端相等非 NULL 时返回 FALSE (正确)。**但 SQLite `IS NOT` 不等价于 `IS DISTINCT FROM` 处理两端都 NULL 的情况** — 实际场景中列均为 NOT NULL 或允许 NULL 但业务语义一致，暂无实际 bug。 |

> **结论**: 0002 trigger 的 UPDATE OF 列表遗漏 `timeline`, `type`, `page_kind`，需补充。

---

## 3 Index 对齐

| # | TS PG Index | SQLite 已有 | 状态 |
|---|------------|-----------|------|
| 1 | idx_pages_type (type) | pages_type_idx (0001) | ✅ |
| 2 | idx_pages_frontmatter (GIN) | — | 🔜 6e FTS5 |
| 3 | idx_pages_trgm (GIN trgm on title) | — | 🔜 6e FTS5 |
| 4 | idx_pages_source_id | idx_pages_source_id (0002) | ✅ |
| 5 | pages_deleted_at_purge_idx (partial) | pages_deleted_at_purge_idx (0002) | ✅ |
| 6 | pages_coalesce_date_idx (expression) | pages_coalesce_date_idx (0002) | ✅ |
| 7 | pages_last_retrieved_at_idx | pages_last_retrieved_at_idx (0002) | ✅ |

> 全部对齐，GIN/FTS5 已正确推迟到 6e。

---

## 4 6a 目标方法 vs trait 签名

审计文档 §6 列出 12 个方法。当前 trait 只有 7 个方法（kind + 3 lifecycle + 4 CRUD + resolve_slugs）。

| # | 方法 | trait 签名 | InMemory 桩 | Libsql 实现 | 状态 |
|---|------|----------|------------|------------|------|
| 1 | findDuplicatePage | ❌ 未加 | ❌ | ❌ | 待加 |
| 2 | softDeletePage | ❌ 未加 | ❌ | ❌ | 待加 |
| 3 | restorePage | ❌ 未加 | ❌ | ❌ | 待加 |
| 4 | purgeDeletedPages | ❌ 未加 | ❌ | ❌ | 待加 |
| 5 | refreshPageBody | ❌ 未加 | ❌ | ❌ | 待加 |
| 6 | updatePageContextualRetrievalState | ❌ 未加 | ❌ | ❌ | 待加 |
| 7 | getAllSlugs | ❌ 未加 | ❌ | ❌ | 待加 |
| 8 | listAllPageRefs | ❌ 未加 | ❌ | ❌ | 待加 |
| 9 | updateSlug | ❌ 未加 | ❌ | ❌ | 待加 |
| 10 | getPageTimestamps | ❌ 未加 | ❌ | ❌ | 待加 |
| 11 | getEffectiveDates | ❌ 未加 | ❌ | ❌ | 待快 |
| 12 | getSalienceScores | ❌ 未加 | ❌ | ❌ | 待加 |

> **结论**: 12 个方法签名全部待加，这是 S3 (trait 签名) 的核心工作量。

---

## 5 辅助类型缺失

6a 的 12 个方法需要额外入参/出参类型，审计文档 §6 提到但未展开：

| 类型 | 用途 | 状态 |
|------|------|------|
| `FindDuplicatePageOpts` | findDuplicatePage 的查询参数 (content_hash? frontmatter.id? deleted 过滤?) | ❌ 未定义 |
| `PageRef` | listAllPageRefs 返回 (slug, source_id) 对 | ❌ 未定义 |
| `PurgeResult` | purgeDeletedPages 返回已清除数量 | ❌ 未定义 |
| `RefreshPageBodyArgs` | refreshPageBody 的入参 (新 body 等) | ❌ 未定义 |
| ~~`ContextualRetrievalState` / `cr_state`~~ | ~~updatePageContextualRetrievalState 的入参~~ | ✅ **撤销** — 经核查 TS schema 并无独立 cr_state 列，状态分散在 `contextual_retrieval_mode` + `corpus_generation` 两列，trait 直接接 `(slug, source_id, mode: CRMode, corpus_generation: Option<String>)` |

> **结论**: 5 个辅助类型需在 S3 一起定义。

---

## 6 libsql.rs 现存 FixMe & 技术债

| # | FixMe | 当前状态 | 6a 处理 |
|---|-------|---------|---------|
| 6.5a | get_page include_deleted 返回 Error::unsupported | 0002 已加 deleted_at 列，FixMe 文案与现状不一致 | 6a 修: get_page 要用 deleted_at 过滤 |
| 6.5c | resolve_slugs 只做精确匹配 | 仍精确匹配 | 6a 修: 改 LIKE %partial% |
| — | put_page INSERT 只写 4 列 | 0002 已加 19 列，INSERT 需扩展 | 6a 修: 全列 upsert |
| — | SELECT 只投影 7 列 + row_to_page 占位 | 需扩展到 27 列 | 6a 修: 全列投影 |
| — | list_pages 过滤条件简陋 | 仅 type + limit | 6a 扩: deleted / source_id / slug_prefix / updated_after / sort 等 PageFilters 字段 |

> **结论**: libsql.rs 5 处需改，且 **互相关联**——改 SELECT 投影影响 row_to_page 签名，改 INSERT 影响 put_page 绑参。

---

## 7 已对齐决策点复查

| # | 议题 | 决议 | 遗留疑问 |
|---|------|------|---------|
| D1 | 向量搜索 | Rust 等价实现 (BLOB + 线性 cosine) | ⚠️ embedding BLOB **编码格式**未拍板: f32 LE flat? f16? 量化? |
| D2 | minion/subagent/oauth 表 | 全量建空表 | 无遗留 |
| D3 | 全文搜索 | FTS5 + trigger | 无遗留 |
| D4 | 子切片粒度 | 9 子切片 6a-6i | 无遗留 |
| D5 | cr_state 存储 | ~~TEXT JSON~~ → 实际无独立列，分散在 `contextual_retrieval_mode` + `corpus_generation` 两 TEXT 列 | ✅ 已澄清：无需建模 cr_state 类型 |
| D6 | embedding 列归属 | 6a 加列, 6e 读写 | ⚠️ **Page.embedding 字段类型**: `Option<Vec<u8>>`? `Option<Box<[u8]>>`? 专用 `Embedding` newtype? |
| D7 | salience_score 列归属 | 6a 加列, 6h 计算逻辑 | ⚠️ 0002 schema 里 **没有** `salience_score` 列！审计文档 §6 说 "6a 加列" 但 0002 迁移未包含 |

> **结论**: D7 是硬缺口——0002 migration 漏掉 `salience_score REAL` 列。D1/D5/D6 三个类型建模问题待拍板。

---

## 8 PostgresEngine 同步

当前 `crates/zbrain-core/src/postgres.rs` 的 Page CRUD 同样是 7 列投影 + 占位。6a 扩列后，postgres.rs 需要 **同步修改**：

- `row_to_page` 扩展
- `put_page` 全列 INSERT
- `get_page` 全列 SELECT + deleted_at 过滤
- `list_pages` 扩展过滤条件
- 12 个新方法签名 + 实现

> **结论**: 每个子切片都要双引擎同步，但 postgres.rs 可延后（6a 可先只做 InMemory + Libsql，postgres.rs 在 6a 完成后补齐）。**需确认**: 6a PR 是否要求 postgres.rs 同步？

---

## 9 测试覆盖缺口

| 测试场景 | 现有 | 6a 需补 |
|----------|------|---------|
| 全列往返 (27 列 round-trip) | ❌ | ✅ (新测试) |
| soft-delete / restore / purge 生命周期 | ❌ | ✅ |
| findDuplicatePage 各分支 | ❌ | ✅ |
| updateSlug + 唯一约束冲突 | ❌ | ✅ |
| getAllSlugs / listAllPageRefs | ❌ | ✅ |
| getPageTimestamps / getEffectiveDates / getSalienceScores | ❌ | ✅ |
| refreshPageBody 不动其它字段 | ❌ | ✅ |
| updatePageContextualRetrievalState JSON 写入 | ❌ | ✅ |
| list_pages 新过滤条件 (deleted, source_id, slug_prefix, sort) | ❌ | ✅ |
| generation trigger 对齐 (timeline/type/page_kind 变更应 bump) | ❌ | ✅ |

> **结论**: 至少 10 组新测试，S3 红测试阶段全部先写。

---

## 10 集中讨论清单 (按优先级排序)

### 🔴 P0 — 阻塞 6a 动手

| # | 议题 | 选项 | 推荐 |
|---|------|------|------|
| C1 | **0002 migration 漏 `salience_score REAL` 列** | A) 新建 0003 补列; B) 改 0002 直接加 (破坏已迁移 DB) | **A) 0003 补列** — 不破坏已有迁移 |
| C2 | **Page 结构缺 5 字段** (last_retrieved_at, generation, embedding, chunker_version, source_path) | 全部加 + 同步 PageInput | **全部加** |
| C3 | **Generation trigger 缺 3 列** (timeline, type, page_kind) | A) 改 0002 trigger; B) 0003 补 trigger | **B) 0003 补** — 同 C1 |
| C4 | **embedding BLOB 编码格式** | A) f32 LE flat array; B) f16 LE; C) 延迟到 6e 再定, 6a 只加列不碰 | **C) 延迟** — 6a 不读写 embedding，编码格式对 6a 无影响 |
| C5 | **cr_state Rust 侧类型** | A) `serde_json::Value` (灵活但无类型安全); B) `CRState` struct (类型安全但需维护); C) `String` (最简) | ✅ **撤销** — 经核查 TS schema 并无独立 cr_state 列，状态分散在 `contextual_retrieval_mode` + `corpus_generation` 两列，trait 直接接 `(slug, source_id, mode: CRMode, corpus_generation: Option<String>)` |
| C6 | **Page.embedding 字段 Rust 类型** | A) `Option<Vec<u8>>`; B) `Option<Box<[u8]>>`; C) 6a 不加字段, 6e 再加 | ✅ **A) `Option<Vec<u8>>`** (已拍板) — 6a 占位 None，6e 接管时 PageInput → INSERT 绑参处加写入即可；与 TS `Buffer \| null` (PG BYTEA) 直译对齐 |

### 🟡 P1 — 不阻塞但需确认

| # | 议题 | 选项 | 推荐 |
|---|------|------|------|
| C7 | **6a PR 是否要求 PostgresEngine 同步** | A) 要求; B) 允许延后一个切片 | ✅ **B) 允许延后** (已拍板) — 6a 仅做 libsql + InMemory 完整实现；postgres.rs 12 方法用 `Err(Error::Unsupported("pending slice 6a-pg"))` 占位，trait 契约不破。**必须落地为独立切片 6a-pg**（见审计文档 12-slice-6-audit.md §3 / §5），避免遗漏。 |
| C8 | **resolve_slugs 改 LIKE 的范围** | A) 仅 libsql; B) libsql + postgres + InMemory 同步 | ✅ **B) 三引擎同步** (已拍板) — trait 行为跨 backend 必须一致；改 WHERE 子句不属于 C7 所指"12 个新方法"，已有方法的行为对齐型修改不受"延迟到镜像切片"约束（见 12-slice-6-audit.md §5 第 5 条豁免脚注） |
| C9 | **list_pages 新过滤条件范围** | A) 仅 source_id + include_deleted; B) 全部 PageFilters 字段; C) 排除依赖 tags 表的 tag 过滤，其余全部 | ✅ **C) 6a 全部 9 项 (type/updated_after/slugPrefix/sourceId/sourceIds/includeDeleted/sort/limit/offset)，tag 过滤推迟到 6c** (已拍板) — 理由：tag 过滤需 `JOIN tags ON page_id` ，而 tags 表归属 6c (`12-slice-6-audit.md §3` 子切片表)，6a 提前建 tags 表会越界。**遗留 todo**: 6c (Tags 切片) 必须在 tag 表落地后回头补齐 `list_pages` 的 tag 分支，**禁止** 在 6c 完成后仍留 stub。6a 阶段 tag 字段非空时返回空结果集 (或 `Error::Unsupported("tag filter lands in slice 6c")`)，且写一条红测试 `list_pages_tag_filter_returns_unsupported_until_6c` 锁定这个回归点。 |
| C10 | **findDuplicatePage 语义** | TS 原版: OR(content_hash 匹配, frontmatter.id 匹配), 排除 deleted。Rust 是否完全镜像？ | ✅ **A) 完全镜像 TS** (已拍板) — 行为对齐型方法，零创新空间；deleted 过滤默认开启 (`WHERE deleted_at IS NULL`)；返回 `Option<Page>` (TS 端返回第一条匹配)。三引擎同步实现（属"行为对齐"豁免，不延迟到 6a-pg；见 12-slice-6-audit.md §5 第 5 条豁免脚注）。 |
| C11 | **refreshPageBody 入参** | TS 版: (slug, newBody, contentHash?)。Rust 版: 需确认是否同步 content_hash 更新 | ✅ **A) `(slug: &str, source_id: i64, new_body: String, content_hash: Option<String>)`** (已拍板) — slug 在多 source 场景下不唯一，必须带 source_id 消歧；content_hash 保持 `Option<String>` 与 TS 同语义 (调用方可预算或委托引擎重算)；同时刷新 `updated_at`，由 0002 trigger 自动 bump `generation` (前提：refreshPageBody 改 `compiled_truth`，已被现有 trigger UPDATE OF 列表覆盖)。 |

### 🟢 P2 — 可延后

| # | 议题 | 选项 | 结论 |
|---|------|------|------|
| C12 | 0001 pages 表 `source_path` 列未出现在 TS schema，是 0002 新增——确认来源 | A) TS 漏建; B) 通过 migration 动态加; C) Rust 多建 | ✅ **B) TS 通过 migration 动态加** (已核查) — `src/core/migrate.ts:2698` 含 `ALTER TABLE pages ADD COLUMN IF NOT EXISTS source_path TEXT`，且被 `brain-writer.ts` / `import-file.ts` / `pglite-engine.ts` 等 21 个文件实际使用。Rust 0002 一并建入是**正确补全**，无需回退。 |
| C13 | `chunker_version` 列 TS 无对应——确认来源 | 同 C12 | ✅ **B) TS 通过 migration 动态加** (已核查) — `src/core/migrate.ts:2697` 含 `ALTER TABLE pages ADD COLUMN IF NOT EXISTS chunker_version SMALLINT NOT NULL DEFAULT 1`，被 `chunkers/recursive.ts` / `chunkers/code.ts` / `reindex.ts` 等 17 个文件使用。Rust 0002 等价建为 `INTEGER NOT NULL DEFAULT 1` (SQLite 无 SMALLINT)，正确。 |
| C14 | InMemoryEngine 12 个新方法的桩实现策略 (todo! / unimplemented / 最小逻辑 / Err::Unsupported) | A) `todo!()`; B) `unimplemented!()`; C) `Err(Error::Unsupported)`; D) 最小正确逻辑 | ✅ **D) 写最小正确逻辑** (已拍板) — 6a 红测试需要在 InMemory 上跑通，桩 panic 会让测试无法启动；最小逻辑只是 `HashMap` insert/filter/find，开发成本低且让 trait 契约可在多 backend 上对照验证。复杂度集中在 `findDuplicatePage` (OR 两条件) 和 `list_pages` (9 过滤项) — 两者都用迭代器链表达，<50 行即可。 |

---

## 11 行动项

1. ~~对 C1-C6 逐条讨论并拍板~~ ✅ 已完成 (C1-C9 + C10-C14 全部拍板)
2. 创建 0003 migration: `salience_score REAL` 列 + 重建 trigger (UPDATE OF 列表补 timeline/type/page_kind)
3. 扩展 Page / PageInput 结构 (5 字段: last_retrieved_at / generation / embedding / chunker_version / source_path)
4. 进入 S3: trait 签名 (12 个新方法 + findDuplicatePage/refreshPageBody 拍板签名) + 辅助类型 (FindDuplicatePageOpts / PageRef / PurgeResult / RefreshPageBodyArgs) + InMemory 最小实现 + 红测试 (10 组 + tag stub 锁定测试)
5. 进入 S4: libsql.rs 全列投影 + 全列 upsert + 12 方法实现 + list_pages 8 项过滤 (tag 留 `Error::Unsupported("tag filter lands in slice 6c")`)
6. 进入 S5: 三连绿 (cargo build / test / clippy) + commit + tag `slice-6a-libsql`
7. 进入镜像切片 6a-pg: 同步 postgres.rs 12 方法 + 全列投影 + 三连绿 + tag `slice-6a-pg`

---

## 12 实施 todo — S5 / S6 细分方案 (方案 A 已拍板)

> **粒度决策**: 原 §11 item 3 ("S5 扩 Page/PageInput 5 字段") 在动手前重新评估后，确认其与 item 4 ("S3: 12 方法 + 4 helper + 10 测试") 体量差距过大 (80 行 vs 600 行)，**拆为两个独立切片**：
> - **S5 (本切片)**: 仅做 Page/PageInput 字段扩展 + 三引擎适配 default 值，体量 ~80 行，1 个 commit。
> - **S6 (下个切片)**: 12 方法 trait + 4 helper 类型 + InMemory 实现 + libsql 实现 + 10 组红测试，体量 ~600 行，按红/绿/重构 3 个 commit 拆分。
>
> 理由：保持单切片 commit ≤ 200 行的项目铁律；S5 即使失败也不会阻塞 S6 设计；分两切片便于 review 时定位回归。

### S5 实施 todo (本轮即将动手)

> **重要发现** (本轮 Read engine.rs 1-344 后修正)：`PageInput` 实际**已含** `chunker_version: Option<i32>` (行 ~?) + `source_path: Option<String>` (行 ~?)，与 §1 checklist 预期 "5 项全缺" 不符。S5 实际只需**新增 2 个字段到 PageInput**。Page 结构仍需补全 5 个字段。

**S5-T1 红阶段**: 写一个 `page_struct_shape_full.rs` 测试，断言 `Page` 含 `last_retrieved_at: Option<String>` / `generation: i64` / `embedding: Option<Vec<u8>>` / `chunker_version: i32` / `source_path: Option<String>` 5 字段；`PageInput` 含 `last_retrieved_at: Option<String>` + `embedding: Option<Vec<u8>>` 2 字段 (其余 chunker_version + source_path 已存在)。测试通过 `Page::default()` + 字段访问编译断言实现。预期 cargo build 红 (字段不存在)。

**S5-T2 绿阶段**: 修改 `crates/zbrain-core/src/engine.rs`:
- `Page` struct 追加 5 字段 (按 TS schema 顺序: last_retrieved_at 紧跟 updated_at；generation/embedding 紧跟 deleted_at；chunker_version/source_path 紧跟 import_filename)
- `PageInput` struct 追加 2 字段 (last_retrieved_at / embedding)
- 更新 `Page::default()` impl (如有手写) 或确认 `#[derive(Default)]` 仍生效
- `InMemoryEngine::put_page` 中构造 Page 时补 5 个 default 值 (generation = 1; 其余 None / Default)
- `LibsqlEngine::row_to_page` placeholder 模式延续 (新字段先用 None / 0 / Default 占位，真实读写留给 S6)

**S5-T3 三连绿**: cargo build / cargo test --workspace / cargo clippy -- -D warnings 全通过。

**S5-T4 commit + tag**: 
- commit message: `slice 6a S5: expand Page/PageInput with last_retrieved_at + generation + embedding + chunker_version + source_path`
- tag: `slice-6a-s5-page-fields`
- 仅 stage `crates/zbrain-core/src/engine.rs` + 新增的 `tests/page_struct_shape_full.rs`，不动 docs (留单独 commit)

**S5-T5 memory**: 追加 `.workbuddy/memory/2026-05-28.md`：记录"字段扩展切片完成 + PageInput 已含 2 字段的发现修正了原 checklist"。

### S6 实施 todo (下个切片，本轮仅列待办)

**S6-T0 设计冻结**: 在 13-slice-6a-gap-checklist.md §13 新增"S6 设计契约"小节，列 12 方法签名 + 4 helper 类型签名 (供 review 后再动 trait)。

**S6-T1 红阶段** (~10 个测试，预计 1 个 commit `slice 6a S6 (red): 10 failing tests for 12 new BrainEngine methods`):
| # | 测试文件 | 覆盖方法 | 关键断言 |
|---|---------|---------|---------|
| 1 | `tests/page_methods_find_duplicate.rs` | findDuplicatePage | content_hash + frontmatter.id OR 匹配；deleted 过滤 |
| 2 | `tests/page_methods_soft_delete.rs` | softDeletePage + restorePage | deleted_at set/null 来回切换；list_pages 默认排除 |
| 3 | `tests/page_methods_purge.rs` | purgeDeletedPages | 真删除 deleted_at < cutoff 行；返回 PurgeResult{deleted_count} |
| 4 | `tests/page_methods_refresh_body.rs` | refreshPageBody | compiled_truth 更新；content_hash 可选更新；generation +1 (trigger 触发) |
| 5 | `tests/page_methods_cr_state.rs` | updatePageContextualRetrievalState | contextual_retrieval_mode + corpus_generation 写入 |
| 6 | `tests/page_methods_slugs.rs` | getAllSlugs + listAllPageRefs + updateSlug | slug 列表 / PageRef 列表 / slug 改名 + 唯一约束冲突 |
| 7 | `tests/page_methods_timestamps.rs` | getPageTimestamps + getEffectiveDates + getSalienceScores | 三个批量查询返回正确 map |
| 8 | `tests/page_methods_list_filters.rs` | list_pages 扩展 | 9 项过滤 (type/updated_after/slugPrefix/sourceId/sourceIds/includeDeleted/sort/limit/offset) |
| 9 | `tests/page_methods_tag_filter_unsupported.rs` | list_pages with tag | 返回 `Err(Error::Unsupported("tag filter lands in slice 6c"))` |
| 10 | `tests/page_methods_in_memory_parity.rs` | InMemory vs libsql 行为对齐 | 同样 PageInput 序列在两 backend 上 list_pages 结果一致 |

**S6-T2 绿阶段** (1 个 commit `slice 6a S6 (green): implement 12 BrainEngine methods on libsql + InMemory`):
- `engine.rs`: trait 加 12 方法签名 + 4 helper 类型 (`FindDuplicatePageOpts` / `PageRef` / `PurgeResult` / `RefreshPageBodyArgs`)
- `engine.rs` InMemoryEngine: 12 方法最小实现 (HashMap 迭代器链)
- `libsql.rs`: 12 方法 SQL 实现 + `row_to_page` 升级到全列投影 (取代 S2 placeholder)
- `postgres.rs`: 12 方法用 `Err(Error::Unsupported("pending slice 6a-pg"))` 占位 (按 C7 决策)
- 跨切片 todo 三重保险: postgres.rs 留 stub + tag 过滤红测试锁定 + checklist §11 item 7 留 6a-pg 镜像切片
- 触发 0003 generation trigger 行为：`refreshPageBody` 改 `compiled_truth` 应自动 bump generation (S4 已覆盖)

**S6-T3 三连绿 + commit + tag**: `slice-6a-s6-12-methods-libsql`

**S6-T4 重构机会扫描** (可选 commit): 12 方法实现完后扫一遍 libsql.rs 是否有 SQL 模板重复，可抽 helper；若 < 30 行重复不抽。

**S6-T5 镜像切片 6a-pg 入口**: 在 docs/plans/20260526/14-slice-6a-pg-plan.md 新建文件 (S6 完成后)，列 12 方法的 PG SQL 等价实现 + 9 项过滤的 PG 方言 (LIKE → ILIKE / FTS5 → tsvector 等)。

### 防遗漏三重保险机制 (再次重申)

1. **源切片留 stub**: postgres.rs 12 方法全部 `Err(Error::Unsupported("pending slice 6a-pg"))`
2. **红测试锁定**: `page_methods_tag_filter_unsupported.rs` 锁定 6c tag 过滤回归点；postgres 端可在 6a-pg 开始时写 `postgres_engine_methods_unsupported_until_6a_pg.rs` 锁定
3. **checklist 显式列附属切片**: §11 item 7 + 本节 S6-T5 双重提醒 6a-pg 必须落地

---

