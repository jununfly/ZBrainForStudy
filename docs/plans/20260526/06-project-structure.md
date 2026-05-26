# 项目结构映射

## TypeScript → Rust 结构对应

### 原项目结构

```
gbrain/
├── src/
│   ├── core/              # 核心引擎
│   │   ├── engine.ts
│   │   ├── postgres-engine.ts
│   │   ├── pglite-engine.ts
│   │   ├── types.ts
│   │   ├── operations.ts
│   │   ├── index.ts
│   │   ├── markdown.ts
│   │   ├── search/         # 搜索模块
│   │   ├── cycle/          # 周期任务
│   │   ├── facts/          # 事实提取
│   │   ├── brainstorm/     # 头脑风暴
│   │   └── ...
│   ├── commands/           # CLI 命令
│   │   ├── search.ts
│   │   ├── sync.ts
│   │   └── ...
│   ├── mcp/                # MCP 服务器
│   │   ├── server.ts
│   │   └── ...
│   ├── cli.ts              # CLI 入口
│   └── version.ts
├── admin/                  # Web 管理界面
├── skills/                 # 技能系统
├── docs/                   # 文档
├── package.json
└── ...
```

### 新项目结构

```
zbrain/
├── src/
│   ├── main.rs             # CLI 入口
│   ├── lib.rs              # 库导出
│   ├── version.rs          # 版本信息
│   │
│   ├── core/               # 核心引擎
│   │   ├── mod.rs
│   │   ├── types.rs        # 核心类型
│   │   ├── error.rs        # 错误类型
│   │   ├── engine.rs       # 引擎 trait
│   │   ├── postgres_engine.rs
│   │   ├── libsql_engine.rs # PGLite 替代
│   │   ├── operations.rs   # 操作契约
│   │   ├── markdown.rs     # Markdown 处理
│   │   ├── singleton.rs    # 单例引擎
│   │   │
│   │   ├── search/         # 搜索模块
│   │   │   ├── mod.rs
│   │   │   ├── hybrid.rs   # 混合搜索
│   │   │   ├── intent.rs   # 意图识别
│   │   │   └── ...
│   │   ├── cycle/          # 周期任务
│   │   │   ├── mod.rs
│   │   │   └── ...
│   │   ├── facts/          # 事实提取
│   │   │   ├── mod.rs
│   │   │   └── ...
│   │   └── brainstorm/     # 头脑风暴
│   │       ├── mod.rs
│   │       └── ...
│   │
│   ├── cli/                # CLI 命令
│   │   ├── mod.rs
│   │   ├── args.rs         # 参数定义
│   │   ├── search.rs
│   │   ├── sync.rs
│   │   └── ...
│   │
│   ├── mcp/                # MCP 服务器
│   │   ├── mod.rs
│   │   ├── server.rs
│   │   └── ...
│   │
│   └── web/                # Web 界面
│       ├── mod.rs
│       ├── routes.rs       # 路由定义
│       ├── handlers.rs     # 请求处理
│       └── templates/      # 模板（如使用 askama）
│
├── frontend/               # Web 前端（如使用分离架构）
│   ├── src/
│   ├── index.html
│   └── package.json
│
├── skills/                 # 技能系统（保持原样）
│   ├── RESOLVER.md
│   └── ...
│
├── docs/                   # 文档
│   └── plan/               # 本方案文档
│
├── migrations/             # 数据库迁移
│   └── ...
│
├── tests/                  # 集成测试
│   ├── cli_tests.rs
│   ├── engine_tests.rs
│   └── ...
│
├── Cargo.toml
├── Cargo.lock
├── build.rs                # 构建脚本
└── ...
```

## 模块导出映射

### src/lib.rs

```rust
// 对应 src/core/index.ts
pub mod core {
    pub use crate::core::engine::{BrainEngine, SearchOpts, PageFilters};
    pub use crate::core::postgres_engine::PostgresEngine;
    pub use crate::core::types::*;
    pub use crate::core::markdown::{parse_markdown, serialize_markdown, split_body};
}

pub mod cli;
pub mod mcp;

pub use version::VERSION;
mod version;
```

## 文件命名约定

- TypeScript 驼峰命名 → Rust 蛇形命名
- `postgres-engine.ts` → `postgres_engine.rs`
- `searchKeyword` → `search_keyword`
- 保持语义一致
