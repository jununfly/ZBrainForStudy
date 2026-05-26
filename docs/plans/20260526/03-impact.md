# 影响盘点分析

## 核心文件清单

### TypeScript → Rust 映射

| TS 文件 | Rust 对应 | 说明 |
|---------|-----------|------|
| `src/core/types.ts` | `src/core/types.rs` | 核心类型定义 |
| `src/core/engine.ts` | `src/core/engine.rs` | 引擎 trait 定义 |
| `src/core/postgres-engine.ts` | `src/core/postgres_engine.rs` | PostgreSQL 引擎实现 |
| `src/core/pglite-engine.ts` | `src/core/pglite_engine.rs` | PGLite 引擎实现 |
| `src/core/operations.ts` | `src/core/operations.rs` | 操作契约定义 |
| `src/core/index.ts` | `src/lib.rs` | 公共导出 |
| `src/version.ts` | `src/version.rs` | 版本信息 |
| `src/cli.ts` | `src/main.rs` | CLI 入口 |

## 依赖影响分析

### Node.js 依赖 → Rust 替代

| Node 依赖 | Rust 替代 | 说明 |
|-----------|-----------|------|
| `@anthropic-ai/sdk` | `anthropic-rs` 或 `reqwest` | Anthropic API 客户端 |
| `@electric-sql/pglite` | `embeddingdb` 或 `libsql` | 嵌入式数据库 |
| `postgres` | `sqlx` 或 `postgres` | PostgreSQL 客户端 |
| `web-tree-sitter` | `tree-sitter` + `tree-sitter-*` crates | 代码分析 |
| `ai` | `async-openai` 或自定义 | AI 网关 |
| `zod` | `validator` 或 `schemars` | 数据验证 |
| `express` | `axum` | Web 框架 |

## 类型映射表

### 基础类型映射

| TypeScript | Rust | 说明 |
|------------|------|------|
| `string` | `String` / `&str` | 字符串 |
| `number` | `i64` / `f64` / `usize` | 数字（根据上下文） |
| `boolean` | `bool` | 布尔值 |
| `null` / `undefined` | `Option<T>` | 可选值 |
| `Array<T>` | `Vec<T>` | 数组 |
| `object` | `struct` / `HashMap<String, Value>` | 对象 |
| `Promise<T>` | `impl Future<Output = Result<T, E>>` | 异步 |
| `async function` | `async fn` | 异步函数 |

### 高级类型映射

| TypeScript | Rust | 说明 |
|------------|------|------|
| `interface` | `trait` / `struct` | 接口/结构 |
| `type` | `type` / `enum` | 类型别名 |
| `union` | `enum` | 联合类型 |
| `generics` | `generics` | 泛型 |
| `unknown` | `dyn Any` | 未知类型 |
| `any` | `serde_json::Value` | 任意类型 |

## 风险点识别

### 高风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| PGLite 嵌入式 WASM | 核心功能依赖 | 使用 libsql 或 embeddingdb 替代 |
| Tree-sitter WASM | 代码分析功能 | 使用 tree-sitter crates 重写 |
| 异步运行时差异 | Bun → Tokio | 仔细设计异步接口 |

### 中风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| 数据库迁移兼容性 | 现有数据 | 保持 schema 完全兼容 |
| 配置文件格式 | 用户配置 | 保持格式不变 |
| 测试覆盖保持 | 质量保证 | 逐功能转换测试 |

### 低风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| CLI 命令参数 | 用户体验 | 使用 clap 保持相同参数 |
| 输出格式 | 脚本兼容性 | 保持输出格式一致 |
