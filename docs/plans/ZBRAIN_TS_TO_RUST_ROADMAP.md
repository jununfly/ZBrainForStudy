<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust.json` | 最后更新: 2026-07-02 18:59:15

[~][X+] 1. ZBrain TS -> Rust 迁移路线图
├── [x][Y+] 1-1. Phase 0: 路线图与清单 — 品牌迁移/目录规范/Plans清理
├── [x][Y+] 1-2. Phase 1: Core Storage Parity — Page CRUD/InMemory/PostgreSQL/libsql 合约闭合
├── [ ][X+] 1-3. Phase 2: Config/Bootstrap/Package Entrypoint — 配置发现/init/doctor/storage/schema 命令迁移
└── [~][Y+] 1-4. Phase 6: Sources/Ingestion/Search/Retrieval — 源管理/导入/捕获/提取/同步/搜索/嵌入
    └── [~][Y+] 1-4-1. Sources 管理: CRUD API + Import/Clone/Capture/Extraction

### 当前施工：1-4-1-8. 集成 Chunking & Embedding 到 Sync 管道：让 sync 产生的页面可被语义搜索

**决策：**
- Q: 下一刀做 TS 清理还是继续功能开发？ → 选择选项2：继续功能开发，优先集成 Chunking & Embedding 到 Sync 管道 (理由：当前 sync 可写 page/tag，但还不能产生 searchable chunks；#110/#111 的基础设施需要接入 sync 才能释放语义搜索价值。)
- Q: 下一刀最小闭环范围是什么？ → 先做 Markdown sync → chunk_text → upsert_chunks，不接真实 embedding (完成定义：import_one_path(markdown) 在 put_page 后对 compiled truth/body 做 chunking，转成 ChunkInput，并调用 engine.upsert_chunks(slug, chunks)。暂不接 HTTP embedding、CLI embedding 参数、code chunking、vector search 或 storage schema 深化。)
- Q: chunk 的来源文本应该用哪一份？ → 用 parse_markdown 后写入 page 的正文/body 作为 chunk 源 (chunk 与最终 put_page 的内容保持一致：搜索命中的 chunk 应能回到页面中真实保存的内容。本刀暂不做 title/tags/source path 上下文包装。)
- Q: 这一刀要不要新增可验证 chunk 写入的读接口？ → 新增 get_chunks_for_page(slug) trait 方法，并给 InMemoryEngine 实现 (用途：TDD 能断言 import_one_path() 后确实产生 chunks；后续 retrieval 也需要从 page 取 chunks。默认 trait 实现返回 Unsupported，先不设计复杂 query/vector API。)
- Q: Markdown 第一刀 chunk metadata 写到什么程度？ → 只填稳定必需字段：chunk_index、chunk_text、chunk_source=CompiledTruth、token_count；其余字段留空 (chunk_index 来自 TextChunk.index；chunk_text 来自 TextChunk.text；token_count 用 CJK-aware word count；embedding=None；code metadata 不在 Markdown 第一刀中虚构。)
- Q: 这一刀要不要新增可验证 chunk 写入的读接口？ → 新增 BrainEngine::get_chunks_for_page(slug)，并给 InMemoryEngine 实现 (用途：TDD 能断言 import_one_path 后确实写入 chunks；后续 retrieval 也需要该基础读接口。默认 trait 实现仍返回 Unsupported，避免提前承诺复杂 query/vector API。)
- Q: import_one_path 返回值要不要暴露 chunks 数量？ → 新增 chunks_upserted: usize 到 ImportOnePathResult (sync 层可汇总 chunk 写入量；TDD 可断言 result.chunks_upserted 与 get_chunks_for_page()；暂不引入更复杂 ImportStats。)
- Q: upsert_chunks 失败时 import_one_path 应该怎么处理？ → upsert_chunks() 失败则 import_one_path() 返回错误，不吞掉失败 (put_page 和 upsert_chunks 都是主链路；chunk 写入失败会导致页面不可搜索，应交给 sync failures 记录并重试。add_tag 保持 best-effort。暂不做 rollback，事务层后续单独设计。)
- Q: grill 是否停止并进入 TDD 实施？ → 停止 grill，开始 TDD 实施 (先写 import_markdown_upserts_chunks_from_compiled_truth 红灯测试，然后补 get_chunks_for_page、chunk_text→ChunkInput→upsert_chunks、chunks_upserted。)
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
└── [x][Y+] 1-4. Phase 6: Sources/Ingestion/Search/Retrieval — 源管理/导入/捕获/提取/同步/搜索/嵌入
    └── [x][Y+] 1-4-1. Sources 管理: CRUD API + Import/Clone/Capture/Extraction
```

### 🔨 当前施工: 1. ZBrain TS -> Rust 迁移路线图
**Status:** `in_progress` | **Mode:** `explore`

**子节点:**
- [x] 1-1. Phase 0: 路线图与清单 — 品牌迁移/目录规范/Plans清理
- [x] 1-2. Phase 1: Core Storage Parity — Page CRUD/InMemory/PostgreSQL/libsql 合约闭合
- [ ] 1-3. Phase 2: Config/Bootstrap/Package Entrypoint — 配置发现/init/doctor/storage/schema 命令迁移
- [x] 1-4. Phase 6: Sources/Ingestion/Search/Retrieval — 源管理/导入/捕获/提取/同步/搜索/嵌入
<!-- ⚠️ ROADMAP_SECTION_END -->
