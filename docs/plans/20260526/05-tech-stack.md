# 技术栈选择

## Rust 核心依赖

### Web 框架

| Crate | 用途 | 版本 |
|-------|------|------|
| `axum` | Web 应用框架 | latest |
| `tokio` | 异步运行时 | latest |
| `tower` | 中间件层 | latest |
| `tower-http` | HTTP 中间件 | latest |

### 数据库

| Crate | 用途 | 版本 |
|-------|------|------|
| `sqlx` | 数据库抽象层 | latest |
| `postgres` | PostgreSQL 驱动 | latest |
| `libsql` | 嵌入式 SQLite 替代 PGLite | latest |
| `sqlx-migrate` | 数据库迁移 | latest |

### CLI 框架

| Crate | 用途 | 版本 |
|-------|------|------|
| `clap` | 命令行参数解析 | latest |
| `clap_derive` | clap 派生宏 | latest |
| `console` | 终端输出 | latest |
| `indicatif` | 进度条 | latest |

### 序列化和验证

| Crate | 用途 | 版本 |
|-------|------|------|
| `serde` | 序列化框架 | latest |
| `serde_json` | JSON 支持 | latest |
| `serde_yaml` | YAML 支持 | latest |
| `validator` | 数据验证 | latest |
| `schemars` | JSON Schema 生成 | latest |

### 代码分析

| Crate | 用途 | 版本 |
|-------|------|------|
| `tree-sitter` | 代码分析基础 | latest |
| `tree-sitter-rust` | Rust 语法 | latest |
| `tree-sitter-typescript` | TypeScript 语法 | latest |
| `tree-sitter-javascript` | JavaScript 语法 | latest |
| `tree-sitter-python` | Python 语法 | latest |
| `tree-sitter-go` | Go 语法 | latest |
| `tree-sitter-sql` | SQL 语法 | latest |

### AI 和 API 客户端

| Crate | 用途 | 版本 |
|-------|------|------|
| `async-openai` | OpenAI 客户端 | latest |
| `reqwest` | HTTP 客户端 | latest |
| `reqwest-middleware` | HTTP 中间件 | latest |
| `anthropic-rs` | Anthropic 客户端 | latest |

### 工具和辅助

| Crate | 用途 | 版本 |
|-------|------|------|
| `anyhow` | 错误处理 | latest |
| `thiserror` | 错误派生 | latest |
| `tracing` | 日志和追踪 | latest |
| `tracing-subscriber` | 日志订阅者 | latest |
| `once_cell` | 延迟初始化 | latest |
| `parking_lot` | 同步原语 | latest |
| `chrono` | 日期时间 | latest |
| `uuid` | UUID 生成 | latest |
| `regex` | 正则表达式 | latest |

### Web 前端（可选方案）

#### 方案 A: Rust 前端
| Crate | 用途 |
|-------|------|
| `leptos` | 全栈 Rust Web 框架 |
| `yew` | React-like 框架 |
| `dioxus` | 跨平台 UI 框架 |

#### 方案 B: 传统前端
| Tech | 用途 |
|------|------|
| React + TypeScript | 保持原有前端 |
| Vite | 构建工具 |
| Axum 提供静态文件服务 | 后端托管 |

## Cargo.toml 示例

```toml
[package]
name = "zbrain"
version = "0.41.14.0"
edition = "2021"
authors = ["Your Name <your@email.com>"]
description = "Personal knowledge brain and AI agent platform"
license = "MIT"
repository = "https://github.com/jununfly/zbrain"

[[bin]]
name = "zbrain"
path = "src/main.rs"

[lib]
name = "zbrain"
path = "src/lib.rs"

[dependencies]
# Web
axum = { version = "0.7", features = ["macros"] }
tokio = { version = "1.0", features = ["full"] }
tower = "0.4"
tower-http = { version = "0.5", features = ["fs", "cors"] }

# Database
sqlx = { version = "0.7", features = ["postgres", "runtime-tokio", "chrono", "uuid", "json"] }
libsql = { version = "0.3", optional = true }

# CLI
clap = { version = "4.0", features = ["derive", "cargo"] }
console = "0.15"
indicatif = "0.17"

# Serialization
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
serde_yaml = "0.9"
validator = { version = "0.16", features = ["derive"] }

# Code analysis
tree-sitter = "0.22"
tree-sitter-rust = "0.22"
tree-sitter-typescript = "0.21"
tree-sitter-javascript = "0.21"
tree-sitter-python = "0.21"
tree-sitter-go = "0.20"
tree-sitter-sql = "0.21"

# AI / API
async-openai = "0.20"
reqwest = { version = "0.12", features = ["json", "stream"] }

# Utilities
anyhow = "1.0"
thiserror = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
once_cell = "1.19"
parking_lot = "0.12"
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1.0", features = ["serde", "v4"] }
regex = "1.10"

[dev-dependencies]
tokio-test = "0.4"
tempfile = "3.8"
assert2 = "0.3"

[features]
default = ["postgres"]
postgres = []
pglite = ["libsql"]
web = []
```
