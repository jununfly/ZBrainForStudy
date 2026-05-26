# zbrain — Rust rewrite (rust-rewrite branch)

> ⚠️ This is the **`rust-rewrite`** branch checked out via `git worktree` at
> `/Users/bilibili/Documents/workspace/jununfly/zbrain-rust/`.
> Master keeps the TypeScript v0.41.x release line untouched.

## 当前状态

切片进度：见 `docs/plans/20260526/04-plan.md` 与 `docs/plans/20260526/11-execution-decisions.md`。

| Slice | Scope                       | Tag             | Status |
|-------|-----------------------------|-----------------|--------|
| 1     | Workspace 脚手架（4 crates） | `rust-slice-1`  | 🟡 in progress |
| 2     | core 类型 + error            | `rust-slice-2`  | ⏳ pending |
| 3     | Engine trait                 | `rust-slice-3`  | ⏳ pending |
| 4     | Postgres 引擎                | `rust-slice-4`  | ⏳ pending |
| 5     | libsql 引擎                  | `rust-slice-5`  | ⏳ pending |
| 6     | 单例引擎                     | `rust-slice-6`  | ⏳ pending |
| 7     | operations 契约              | `rust-slice-7`  | ⏳ pending |
| 8     | CLI 框架                     | `rust-slice-8`  | ⏳ pending |

## 工作区结构

```
zbrain-rust/
├── Cargo.toml                  # workspace root (resolver = 2)
├── crates/
│   ├── zbrain-core/            # 引擎、类型、操作、单例
│   ├── zbrain-cli/             # clap 命令行（bin: zbrain）
│   ├── zbrain-web/             # axum Web API（占位）
│   └── zbrain-mcp/             # MCP 服务（占位）
├── src/                        # ⚠️ 旧 TS 源码，对照参考用，slice 12 清理
├── admin/                      # 旧 React 前端，slice 9-10 复用
└── docs/plans/20260526/        # 改写方案文档
```

## 本地命令

```bash
# 全量构建 / 测试 / lint（每个切片闭合时必须三连绿）
cargo build --workspace --all-targets
cargo test  --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings

# 运行 CLI 占位
cargo run -p zbrain-cli
```

## TS 主干（master）

`src/`、`admin/`、`bun.lock`、`package.json` 等仍是 TypeScript 项目的活产物，
本分支保留它们仅作对照参考。**不要在本分支上回写到 master**。
