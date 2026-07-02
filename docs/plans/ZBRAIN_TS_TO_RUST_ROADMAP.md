<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust.json` | 最后更新: 2026-07-01 21:05:34

[~][X+] 1. ZBrain TS -> Rust 迁移路线图
├── [x][Y+] 1-1. Phase 0: 路线图与清单 — 品牌迁移/目录规范/Plans清理
├── [x][Y+] 1-2. Phase 1: Core Storage Parity — Page CRUD/InMemory/PostgreSQL/libsql 合约闭合
├── [ ][X+] 1-3. Phase 2: Config/Bootstrap/Package Entrypoint — 配置发现/init/doctor/storage/schema 命令迁移
└── [~][Y+] 1-4. Phase 6: Sources/Ingestion/Search/Retrieval — 源管理/导入/捕获/提取/同步/搜索/嵌入
    └── [~][Y+] 1-4-1. Sources 管理: CRUD API + Import/Clone/Capture/Extraction

### 当前施工：1-4-1-5. Sync Engine — 文件系统同步/变更检测/reindex/reclone

**决策：**
- Q: Q1: Sync Engine 范围边界 → 聚焦 sync 管道本身（git diff → 变更清单 → 文件遍历 → capture/markdown → putPage）。不包含 chunking/embedding（留到 1-4-1-6）。不包含 links/timeline extraction。content_hash 去重也暂跳过。目标是页面入库（body/frontmatter/type/tags）但无搜索。 (8项子任务：buildSyncManifest, isSyncable, performSyncInner, performFullSync, syncFailures, syncAnchor, concurrency, importOnePath)
- Q: Q2: 模块位置 → zbrain-core/src/sync.rs（或 sync/ 模块目录）。拆分为 sync.rs（主入口）、sync_manifest.rs（diff解析+过滤）、sync_failures.rs（失败记录）。 (被 CLI 和将来 admin API 调用)
- Q: Q3: 并发模型 → 方案B：运行时检测引擎类型。Postgres → 多 worker（tokio::spawn + Arc<AtomicUsize> 队列分派）。PGLite → 串行。 (和 TS 行为一致)
- Q: Q4: Git diff 解析 → std::process::Command（非 git2 crate）。和已有 git_remote.rs 风格一致，复用 GIT_SSRF_FLAGS。 (git diff --name-status -M 输出是稳定的 machine-readable 格式)
- Q: Q5: 文件遍历器 → walkdir crate + 自定义 filter_entry（跳过 .git/node_modules/.raw/ops）+ 手动 inode 循环检测。 (walkdir 默认不跟随符号链接)
- Q: Q6: Sync Anchor 管理 → last_commit + chunker_version 通过 engine.update_source() 写入 sources 表。sync-failures.jsonl 放 <zbrain_home>/sync-failures.jsonl。 (和 TS 一致)
- Q: Q7: import_one_path 实现深度 → 走到 putPage + addTag。不做 chunking/embedding（标记 TODO）、不做 content_hash 去重、不做 links/timeline extraction。 (可工作的中间态：页面入库但无搜索)

**当前子树：**
├── [ ][Y+] 1-4-1-5-1. sync_manifest: build_sync_manifest + is_syncable + unsyncable_reason
├── [ ][Y+] 1-4-1-5-2. sync_walker: collect_syncable_files — walkdir + inode循环检测 + 策略过滤
├── [ ][Y+] 1-4-1-5-3. sync_core: perform_sync + perform_full_sync — 主循环管道
├── [ ][Y+] 1-4-1-5-4. import_one_path: 文件读取 → capture → parse_markdown → putPage + addTag
├── [ ][Y+] 1-4-1-5-5. sync_failures: JSONL 失败记录 + acknowledge
├── [ ][Y+] 1-4-1-5-6. sync_concurrency: 运行时检测引擎类型 → Postgres多worker / PGLite串行
├── [ ][Y+] 1-4-1-5-7. sync_anchor: last_commit + chunker_version 写入 sources 表
└── [ ][Y+] 1-4-1-5-8. sync_cli: CLI 命令  入口 + 参数解析
<!-- ROADMAP_SECTION_END -->

<!-- ⚠️ ROADMAP_SECTION_START -->
<!-- ⚠️ 此 section 由 roadmap_cli.py render 自动生成，请勿手动编辑 -->
## ZBrain TS -> Rust 迁移路线图

### 树形视图 (depth=2)

```
[~][X+] 1. ZBrain TS -> Rust 迁移路线图
├── [x][Y+] 1-1. Phase 0: 路线图与清单 — 品牌迁移/目录规范/Plans清理
├── [x][Y+] 1-2. Phase 1: Core Storage Parity — Page CRUD/InMemory/PostgreSQL/libsql 合约闭合
├── [ ][X+] 1-3. Phase 2: Config/Bootstrap/Package Entrypoint — 配置发现/init/doctor/storage/schema 命令迁移
└── [~][Y+] 1-4. Phase 6: Sources/Ingestion/Search/Retrieval — 源管理/导入/捕获/提取/同步/搜索/嵌入
    └── [~][Y+] 1-4-1. Sources 管理: CRUD API + Import/Clone/Capture/Extraction
```

### 🔨 当前施工: 1-4-1-7. Chunking & Embedding Pipeline: CJK counting, recursive chunker, tree-sitter, edge extraction, transaction orchestration, embedding gateway — #107-#111
**Status:** `in_progress` | **Mode:** `exploit`

**子节点:**
- [x] 1-4-1-7-1. #107: CJK word counting module + recursive text chunker
- [x] 1-4-1-7-2. #108: Tree-sitter code semantic chunker
- [x] 1-4-1-7-3. #109: Edge extraction + symbol resolution + qualified names
- [ ] 1-4-1-7-4. #110: Transaction write orchestration
- [ ] 1-4-1-7-5. #111: Embedding gateway + batch API + context retrieval
<!-- ⚠️ ROADMAP_SECTION_END -->
