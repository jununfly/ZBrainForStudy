# 执行决策记录（Brainstorming 对齐结论）

> 本文档记录 2026-05-26 brainstorming 会话中对齐的 5 个关键决策，作为后续切片执行的指导依据。

## 决策 1：隔离策略 — git worktree 隔离分支

- 在 zbrain 仓库创建 `rust-rewrite` 分支
- `git worktree add ../zbrain-rust rust-rewrite` 物理隔离工作区
- master 分支保持 TS v0.41.x 可发布状态
- 每个切片完成后在 rust-rewrite 上打 tag（`rust-slice-1`, `rust-slice-2` ...）便于回滚
- 完成后用 squash merge 替换主干

**路径**：
- TS 主干：`/Users/bilibili/Documents/workspace/jununfly/zbrain/`
- Rust worktree：`/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/`

## 决策 2：代码组织 — Workspace 多 crate

```
zbrain/  (rust-rewrite worktree 根)
├── Cargo.toml                  # workspace 定义
├── crates/
│   ├── zbrain-core/            # 引擎、类型、操作、单例
│   ├── zbrain-cli/             # clap 命令行
│   ├── zbrain-web/             # axum Web API（MVP 占位）
│   └── zbrain-mcp/             # MCP 服务（MVP 占位）
```

**理由**：编译并行化、CI 按 crate 缓存、模块边界编译期强制约束。

## 决策 3：嵌入式数据库 — libsql

- 替代原 PGLite（WASM-Postgres）
- crate：`libsql 0.3+`
- 数据格式与 SQLite 兼容，迁移路径清晰
- SQL 方言需从 Postgres 调整为 SQLite（在切片 5 处理）

## 决策 4：Web 前端 — 复用现有 React + TS

- 保留 `admin/` 目录的 ~150 个 TSX 文件
- Vite 构建，axum 托管静态文件
- 不引入 Rust WASM 前端
- 前端改造与后端 API 对接放在切片 9-10

## 决策 5：MVP 范围 — 切片 1-8（core + CLI 闭环）

首版交付包含：
1. Workspace 脚手架
2. core 类型与 error
3. Engine trait
4. Postgres 引擎（先做，比 libsql 简单）
5. libsql 引擎（高风险点）
6. 单例引擎
7. operations 契约
8. CLI 框架

**不在 MVP 范围**：Web UI、MCP 服务、完整测试转换、品牌全量重命名。

## 执行原则（执行所有切片均适用）

- **TDD**：所有新功能先写失败测试，看到红 → 写最小代码 → 看到绿 → 重构
- **每片自闭合**：完成后必须 `cargo build && cargo test && cargo clippy -- -D warnings` 全绿
- **单切片提交**：每个切片一个或多个 commit，结束打 tag
- **暂停规则**：发现超出方案的新问题立即暂停，记入 follow-up，不就地扩展
- **TS 源码保留**：rust-rewrite 分支上保留 src/、admin/ 作为对照参考，最终切片 12 统一清理
