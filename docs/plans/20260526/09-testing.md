# 测试迁移方案

## 测试策略

### 分层测试架构

```
┌─────────────────────────────────────────┐
│     E2E Integration Tests              │
│  (CLI + MCP + Web UI)                  │
├─────────────────────────────────────────┤
│     Module Tests                        │
│  (core + cli + web modules)            │
├─────────────────────────────────────────┤
│     Unit Tests                          │
│  (function-level, no external deps)    │
└─────────────────────────────────────────┘
```

### 测试覆盖率目标

- 单元测试覆盖率：≥ 80%
- 核心模块覆盖率：≥ 90%
- CLI 命令覆盖率：≥ 95%
- API 端点覆盖率：100%

## TypeScript 测试迁移指南

### Jest 测试 → Rust `cargo test` 迁移

#### TypeScript 测试示例

```typescript
// test/core/markdown.test.ts
import { parseMarkdown, serializeMarkdown } from '../../src/core/markdown';

describe('markdown', () => {
  test('parseMarkdown should extract frontmatter', () => {
    const input = `---
title: Test Page
tags: [tag1, tag2]
---
Content here`;
    const result = parseMarkdown(input);
    expect(result.title).toEqual('Test Page');
    expect(result.tags).toEqual(['tag1', 'tag2']);
  });
});
```

#### Rust 测试对应

```rust
// tests/markdown_test.rs
use zbrain::core::markdown::{parse_markdown, serialize_markdown};

#[cfg(test)]
mod markdown_tests {
    use super::*;

    #[test]
    fn test_parse_markdown_extracts_frontmatter() {
        let input = r#"---
title: Test Page
tags: [tag1, tag2]
---
Content here"#;
        
        let result = parse_markdown(input).unwrap();
        assert_eq!(result.title, Some("Test Page".to_string()));
        assert_eq!(result.tags, vec!["tag1".to_string(), "tag2".to_string()]);
    }
}
```

### 测试断言映射

| Jest 断言 | Rust `assert!` 宏 |
|----------|------------------|
| `expect(a).toEqual(b)` | `assert_eq!(a, b)` |
| `expect(a).not.toEqual(b)` | `assert_ne!(a, b)` |
| `expect(a).toBeTruthy()` | `assert!(a)` |
| `expect(a).toBeFalsy()` | `assert!(!a)` |
| `expect(a).toBeNull()` | `assert!(a.is_none())` |
| `expect(a).toBeDefined()` | `assert!(a.is_some())` |
| `expect(fn).toThrow()` | `assert!(fn.is_err())` |
| `expect(fn).not.toThrow()` | `assert!(fn.is_ok())` |

## 测试文件组织结构

### 项目测试目录结构

```
zbrain/
├── src/
│   ├── core/
│   │   ├── types.rs          # 内嵌单元测试
│   │   ├── error.rs
│   │   ├── engine.rs
│   │   └── ...
│   ├── cli/
│   ├── mcp/
│   └── web/
├── tests/                   # 集成测试目录
│   ├── common/
│   │   └── mod.rs          # 测试工具
│   ├── core/
│   │   ├── types_test.rs
│   │   ├── engine_test.rs
│   │   └── search_test.rs
│   ├── cli/
│   │   └── cli_test.rs     # CLI 命令测试
│   ├── mcp/
│   │   └── mcp_test.rs
│   └── web/
│       └── api_test.rs
└── Cargo.toml
```

### 内嵌单元测试

```rust
// src/core/types.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_type_from_str() {
        assert_eq!(PageType::from_str("markdown"), Ok(PageType::Markdown));
        assert_eq!(PageType::from_str("code"), Ok(PageType::Code));
        assert!(PageType::from_str("invalid").is_err());
    }
}
```

## 测试工具开发

### 测试数据库管理

```rust
// tests/common/db.rs
use sqlx::PgPool;
use tempfile::tempdir;
use std::path::PathBuf;

pub struct TestDatabase {
    pool: Option<PgPool>,
    temp_dir: Option<tempfile::TempDir>,
}

impl TestDatabase {
    pub async fn new_pg() -> Self {
        let pool = PgPool::connect("postgres://...")
            .await
            .expect("Failed to connect to test database");
        // 初始化 schema
        Self { pool: Some(pool), temp_dir: None }
    }
    
    pub async fn new_libsql() -> Self {
        let temp_dir = tempdir().unwrap();
        let db_path = temp_dir.path().join("test.db");
        // 初始化 libsql
        Self { pool: None, temp_dir: Some(temp_dir) }
    }
    
    pub fn pool(&self) -> &PgPool {
        self.pool.as_ref().unwrap()
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        // 清理
    }
}
```

### 测试引擎工厂

```rust
// tests/common/engine.rs
use super::db::TestDatabase;
use crate::core::engine::BrainEngine;
use crate::core::postgres_engine::PostgresEngine;
use crate::core::libsql_engine::LibsqlEngine;

pub enum TestEngineKind {
    Postgres,
    Libsql,
}

pub async fn create_test_engine(kind: TestEngineKind) -> Box<dyn BrainEngine> {
    match kind {
        TestEngineKind::Postgres => {
            let db = TestDatabase::new_pg().await;
            Box::new(PostgresEngine::new(db.pool().clone()))
        }
        TestEngineKind::Libsql => {
            let db = TestDatabase::new_libsql().await;
            Box::new(LibsqlEngine::new(db.path()).await)
        }
    }
}
```

### CLI 测试工具

```rust
// tests/common/cli.rs
use assert_cmd::Command;
use tempfile::tempdir;
use std::path::PathBuf;

pub struct TestCli {
    config_dir: tempfile::TempDir,
}

impl TestCli {
    pub fn new() -> Self {
        Self { config_dir: tempdir().unwrap() }
    }
    
    pub fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("zbrain").unwrap();
        cmd.env("GBRAIN_HOME", self.config_dir.path());
        cmd
    }
    
    pub fn init(&self) -> &Self {
        self.cmd()
            .arg("init")
            .assert()
            .success();
        self
    }
}
```

## 核心模块测试示例

### 引擎 trait 测试

```rust
// tests/core/engine_test.rs
use crate::common::engine::{create_test_engine, TestEngineKind};
use crate::core::engine::BrainEngine;
use crate::core::types::Page;

#[tokio::test]
async fn test_page_crud() {
    let mut engine = create_test_engine(TestEngineKind::Libsql).await;
    
    // 创建页面
    let page = Page {
        slug: "test/page".to_string(),
        title: "Test Page".to_string(),
        content: "Content".to_string(),
        page_type: PageType::Markdown,
        ..Default::default()
    };
    
    engine.put_page(&page).await.expect("Failed to put page");
    
    // 读取页面
    let got = engine.get_page("test/page").await.expect("Failed to get page");
    assert_eq!(got.title, "Test Page");
    
    // 列出页面
    let pages = engine.list_pages(None).await.expect("Failed to list pages");
    assert!(!pages.is_empty());
    
    // 删除页面
    engine.delete_page("test/page").await.expect("Failed to delete page");
}
```

### 搜索模块测试

```rust
// tests/core/search_test.rs
use crate::common::engine::create_test_engine;
use crate::core::search::{SearchOpts, hybrid_search};

#[tokio::test]
async fn test_hybrid_search() {
    let engine = create_test_engine(TestEngineKind::Libsql).await;
    
    // 插入测试数据
    // ...
    
    let opts = SearchOpts {
        limit: 10,
        detail: Default::default(),
        exclude_slug_prefixes: None,
        include_slug_prefixes: None,
    };
    
    let results = hybrid_search(&engine, "test query", opts)
        .await
        .expect("Search failed");
    
    assert!(!results.is_empty());
}
```

### CLI 命令测试

```rust
// tests/cli/cli_test.rs
use crate::common::cli::TestCli;

#[test]
fn test_zbrain_help() {
    let cli = TestCli::new();
    cli.cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicates::str::contains("zbrain"));
}

#[test]
fn test_zbrain_init() {
    let cli = TestCli::new();
    cli.cmd()
        .arg("init")
        .assert()
        .success()
        .stdout(predicates::str::contains("Initialized"));
}

#[test]
fn test_zbrain_search() {
    let cli = TestCli::new();
    cli.init();
    
    cli.cmd()
        .arg("search")
        .arg("test")
        .assert()
        .success();
}
```

### Web API 测试

```rust
// tests/web/api_test.rs
use axum::{
    http::{Request, StatusCode},
    body::Body,
};
use tower::ServiceExt;
use crate::web::create_router;
use crate::common::engine::create_test_engine;

#[tokio::test]
async fn test_list_pages_api() {
    let engine = create_test_engine(TestEngineKind::Libsql).await;
    let router = create_router(engine);
    
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/pages")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_search_api() {
    let engine = create_test_engine(TestEngineKind::Libsql).await;
    let router = create_router(engine);
    
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/search")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"query": "test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    
    assert_eq!(response.status(), StatusCode::OK);
}
```

## E2E 测试策略

### E2E 测试场景

```rust
// tests/e2e/full_workflow_test.rs
#[tokio::test]
async fn test_full_workflow() {
    // 1. 初始化知识库
    let cli = TestCli::new();
    cli.init();
    
    // 2. 导入页面
    cli.cmd()
        .arg("import")
        .arg("test-page.md")
        .assert()
        .success();
    
    // 3. 搜索内容
    let output = cli.cmd()
        .arg("search")
        .arg("test")
        .output()
        .unwrap();
    assert!(output.status.success());
    
    // 4. 通过 Web UI 编辑
    // ...
}
```

## 性能测试

### 基准测试框架

```rust
// benches/search_bench.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use zbrain::core::search::hybrid_search;
use zbrain::common::engine::create_test_engine;

fn bench_search(c: &mut Criterion) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    
    c.bench_function("hybrid_search 100 pages", |b| {
        b.to_async(&runtime).iter(|| async {
            let engine = create_test_engine(TestEngineKind::Libsql).await;
            let _ = hybrid_search(black_box(&engine), "test query", Default::default()).await;
        });
    });
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
```

## 测试执行流程

### 开发时测试

```bash
# 快速单元测试
cargo test --lib

# 特定模块测试
cargo test types --lib

# 特定测试函数
cargo test --lib types::tests::test_page_type_from_str

# 显示测试输出
cargo test -- --nocapture
```

### 完整测试套件

```bash
# 运行所有测试（包括集成测试）
cargo test

# 运行特定测试目录
cargo test --test cli_test

# 运行基准测试
cargo bench

# 测试覆盖率
cargo tarpaulin --out Html
```

### CI 测试流程

```yaml
# .github/workflows/test.yml
name: Test

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    strategy:
      matrix:
        engine: [postgres, libsql]
        rust-version: ["1.75", stable]
    
    steps:
      - uses: actions/checkout@v4
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: ${{ matrix.rust-version }}
      
      - name: Build
        run: cargo build --all-features
      
      - name: Test
        run: cargo test --all-features
        env:
          DATABASE_URL: postgres://...
          
      - name: Coverage
        uses: actions-rs/tarpaulin@v0.1
```

## 测试验收标准

### 迁移完成检查清单

- [ ] 所有核心功能有 Rust 测试覆盖
- [ ] 测试覆盖率报告生成
- [ ] `cargo test` 全部通过
- [ ] 无遗留 TypeScript 测试未迁移
- [ ] 测试速度与原 Jest 测试相当或更快
- [ ] CI 集成测试通过
- [ ] 性能测试不低于原 TypeScript 实现
