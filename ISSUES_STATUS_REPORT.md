# ZBrain Issues 状态报告
生成时间: 2026-07-02 16:30

## 📊 总览

- **Open Issues**: 24 个
- **Closed Issues**: 91 个
- **总计**: 115 个 issues

---

## ✅ 最近关闭的 Issues (2026-07-02)

| # | 标题 | 关闭原因 |
|---|------|---------|
| #107 | chunking: CJK word counting + recursive text chunker | completed |
| #108 | chunking: tree-sitter code semantic chunker | completed |
| #109 | chunking: call-graph edge extraction + symbol resolution + qualified names | completed |
| #112 | chunking: Rust tree-sitter crate scaffold + language detection + feature flags | completed |
| #113 | chunking: language manifest registry + tree-sitter parser init | completed |
| #114 | chunking: AST walk + semantic chunk emission with metadata | completed |
| #115 | chunking: mergeSmallSiblings algorithm + chunkCodeText public API | completed |

---

## 🔓 Open Issues 按类别分组

### 1. Admin Auth & Magic Link (6个)
- **#86**: Port magic-link admin auth (POST /admin/api/issue-magic-link + GET /admin/auth/:token)
- **#89**: 1-6-4-5/Slice1: MagicLinkAuth core state machine + rate limiting
- **#90**: 1-6-4-5/Slice2: AdminAuth::create_session_with_ttl
- **#91**: 1-6-4-5/Slice3: POST /admin/api/issue-magic-link handler
- **#92**: Slice4: GET /admin/auth/:token handler
- **#93**: Slice5: TS cleanup - delete magic-link handlers from serve-http.ts

### 2. OAuth & MCP Server (4个)
- **#80**: Port OAuth client management endpoints (register, update-ttl, revoke)
- **#83**: Port OAuth scope hierarchy + access token verification to Rust
- **#84**: Port /token OAuth handler (client_credentials + confidential client flows)
- **#85**: Port /mcp HTTP JSON-RPC dispatch with bearer auth, scope enforcement, and request logging

### 3. Web Backend Cleanup (2个)
- **#82**: Clean up TS admin route handlers replaced by Rust
- **#88**: Final TS web-backend cleanup: delete serve-http.ts and remaining TS web modules

### 4. Sync Engine (8个)
- **#94**: sync: import_one_path — 文件读取 → capture → parse_markdown → putPage + addTag
- **#95**: sync: sync_manifest — git diff 解析 + is_syncable 文件过滤
- **#96**: sync: sync_walker — walkdir 遍历 + inode 循环检测 + 策略过滤
- **#97**: sync: sync_failures — JSONL 失败记录 + acknowledge 机制
- **#98**: sync: sync_anchor — last_commit + chunker_version 锚点管理
- **#99**: sync: sync_concurrency — 引擎类型检测 → Postgres多worker / PGLite串行
- **#100**: sync: sync_core — perform_sync + perform_full_sync 主循环管道
- **#101**: sync: sync_cli — CLI 命令 zbrain sync 入口 + 参数解析

### 5. Chunking & Embedding Pipeline (2个)
- **#110**: import: transaction write orchestration — putPage + upsertChunks + tags + links
- **#111**: embedding: AI gateway embedding path + batch API + context retrieval wrapper

### 6. Other (2个)
- **#81**: Port SSE live activity feed (/admin/events)
- **#87**: Port webhook ingestion endpoints (POST /ingest + POST /webhooks/github)

---

## 🎯 路线图当前状态

**当前施工**: 1-4-1-7. Chunking & Embedding Pipeline

**已完成** (3/5):
- ✅ #107: CJK word counting module + recursive text chunker
- ✅ #108: Tree-sitter code semantic chunker
- ✅ #109: Edge extraction + symbol resolution + qualified names

**待处理** (2/5):
- ⏳ #110: Transaction write orchestration
- ⏳ #111: Embedding gateway + batch API + context retrieval

---

## 🚀 建议下一步

根据路线图优先级，建议按以下顺序处理：

1. **#110**: Transaction write orchestration (导入事务编排)
2. **#111**: Embedding gateway + batch API + context retrieval (嵌入网关)
3. **Sync Engine** (#94-#101): 文件系统同步引擎
4. **Admin Auth** (#86, #89-#93): Magic Link 认证
5. **OAuth & MCP** (#80, #83-#85): OAuth 和 MCP 服务

---

## 📝 备注

- 所有 open issues 都带有 `ready-for-agent` 标签，说明它们已完全规格化，可以交给 AFK agent 执行
- Sync Engine 相关的 8 个 issues (#94-#101) 有详细的决策记录（见路线图文档）
- 路线图的下一个里程碑是完成 Chunking & Embedding Pipeline (#110, #111)
