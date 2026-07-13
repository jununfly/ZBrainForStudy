<!-- ROADMAP_SECTION_START -->
## ZJ Roadmap

> 数据文件: `zbrain-ts-to-rust-part4-phase6-ingestion-sources-search.json` | 最后更新: 2026-07-13 17:21:51

[x][X+] 1. ZBrain TS→Rust Part4: Phase 6 — Ingestion, Sources, Search, Retrieval
├── [x][Y+] 1-1. Source Management + Capture + Sync + CLI (1-7-1-1~1-7-1-5, 1-7-1-7)
├── [x][Y+] 1-2. Chunking & Embedding Pipeline
│   ├── [x][Y+] 1-2-1. Recursive text chunker with CJK support (#107)
│   └── [x][Y+] 1-2-2. Tree-sitter code chunker integration into import pipeline (#108)
└── [x][Y+] 1-3. Search/Retrieval 生产后端复活 (libsql search_pages + query embedding 接线)
    ├── [x][Y+] 1-3-1. 抽后端无关 fuse_and_boost 融合 helper (lexical打分+RRF+snippet+salience/recency boost, 吃 &[Page]+&dyn BrainEngine)
    ├── [x][Y+] 1-3-2. LibsqlEngine::search_pages 实现 (SQL 拉候选 page + 调 fuse_and_boost，替换 trait default 空实现)
    ├── [x][Y+] 1-3-3. query embedding 接线 (CLI 按 config+env-key 构造 EmbeddingClient 注入 OperationContext + QueryOperation embed_query 填 query_embedding)
    └── [x][Y+] 1-3-4. 验证 + KNOWN-GAPS 登记 (postgres search 未实现 / page.embedding 无写入口 / import index 路 chunk embedding 未接)
<!-- ROADMAP_SECTION_END -->
