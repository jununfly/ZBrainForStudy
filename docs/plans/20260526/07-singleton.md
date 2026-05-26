# 单例引擎设计

## 设计目标

确保全局唯一的 `BrainEngine` 实例，防止多实例操作写坏数据。

## 核心实现

### src/core/singleton.rs

```rust
use once_cell::sync::OnceCell;
use parking_lot::Mutex;
use std::sync::Arc;
use crate::core::engine::BrainEngine;
use crate::core::error::{Error, Result};
use crate::core::types::Config;

/// 全局单例引擎
pub static ENGINE: OnceCell<Arc<Mutex<dyn BrainEngine + Send + Sync>>> = OnceCell::new();

/// 初始化引擎
///
/// # Errors
///
/// 如果引擎已经初始化，返回 `Error::AlreadyInitialized`
pub fn init_engine(config: Config) -> Result<()> {
    let engine = create_engine(config)?;
    ENGINE.set(Arc::new(Mutex::new(engine)))
        .map_err(|_| Error::AlreadyInitialized)?;
    Ok(())
}

/// 获取引擎实例
///
/// # Errors
///
/// 如果引擎未初始化，返回 `Error::NotInitialized`
pub fn get_engine() -> Result<Arc<Mutex<dyn BrainEngine + Send + Sync>>> {
    ENGINE.get()
        .cloned()
        .ok_or(Error::NotInitialized)
}

/// 检查引擎是否已初始化
pub fn is_initialized() -> bool {
    ENGINE.get().is_some()
}

/// 创建具体引擎实例（工厂函数）
fn create_engine(config: Config) -> Result<Box<dyn BrainEngine + Send + Sync>> {
    match config.engine_type {
        EngineType::Postgres => {
            Ok(Box::new(PostgresEngine::new(config)?))
        }
        EngineType::LibSql => {
            Ok(Box::new(LibSqlEngine::new(config)?))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineType {
    Postgres,
    LibSql,
}
```

### src/core/error.rs（相关部分）

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Engine already initialized")]
    AlreadyInitialized,

    #[error("Engine not initialized")]
    NotInitialized,

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Config error: {0}")]
    Config(String),

    // ... 其他错误类型
}

pub type Result<T> = std::result::Result<T, Error>;
```

## 使用示例

### CLI 入口 (src/main.rs)

```rust
use zbrain::core::singleton::{init_engine, get_engine, is_initialized};
use zbrain::core::types::Config;
use zbrain::cli::args::Args;
use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // 加载配置
    let config = Config::load(&args.config)?;

    // 初始化引擎（只一次）
    if !is_initialized() {
        init_engine(config)?;
    }

    // 获取引擎并使用
    let engine = get_engine()?;
    let mut engine = engine.lock();

    // 执行命令
    match args.command {
        Command::Search { query } => {
            let results = engine.search(&query).await?;
            // 输出结果
        }
        // ... 其他命令
    }

    Ok(())
}
```

### Web 处理器 (src/web/handlers.rs)

```rust
use axum::{extract::State, http::StatusCode, Json};
use zbrain::core::singleton::get_engine;
use zbrain::core::types::Page;

#[derive(Clone)]
struct AppState;

pub async fn list_pages() -> Result<Json<Vec<Page>>, StatusCode> {
    let engine = get_engine()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let engine = engine.lock();
    let pages = engine.list_pages(None).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(pages))
}

pub async fn create_page(Json(page): Json<Page>) -> Result<StatusCode, StatusCode> {
    let engine = get_engine()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut engine = engine.lock();
    engine.put_page(&page).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::CREATED)
}
```

## 线程安全保证

### Mutex 的使用

- 所有引擎访问通过 `Arc<Mutex<Engine>>`
- 读写都需要获取锁
- 防止并发写冲突
- 允许并发读（如果使用 `RwLock`，但这里简化为 `Mutex`）

### 可选优化：RwLock

```rust
use parking_lot::RwLock;

pub static ENGINE: OnceCell<Arc<RwLock<dyn BrainEngine + Send + Sync>>> = OnceCell::new();

pub fn get_engine() -> Result<Arc<RwLock<dyn BrainEngine + Send + Sync>>> {
    // ...
}

// 使用:
let engine = get_engine()?;
let read_guard = engine.read();  // 读操作
let mut write_guard = engine.write();  // 写操作
```

## 测试策略

### tests/singleton_tests.rs

```rust
use zbrain::core::singleton::*;
use zbrain::core::types::Config;

#[tokio::test]
async fn test_singleton_initialization() {
    // 注意：单例测试需要隔离运行
    let config = Config::test_config();
    assert!(!is_initialized());
    init_engine(config).unwrap();
    assert!(is_initialized());
}

#[tokio::test]
async fn test_double_initialization_fails() {
    let config = Config::test_config();
    let _ = init_engine(config.clone());
    let result = init_engine(config);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_concurrent_access() {
    // 使用 tokio::spawn 测试并发访问
    let config = Config::test_config();
    let _ = init_engine(config);

    let handles: Vec<_> = (0..10)
        .map(|_| {
            tokio::spawn(async {
                let engine = get_engine().unwrap();
                let _guard = engine.lock();
                // 执行一些操作
            })
        })
        .collect();

    for handle in handles {
        handle.await.unwrap();
    }
}
```

## 防止多实例的额外措施

### 进程级锁文件

```rust
// src/core/lock.rs
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

pub struct ProcessLock {
    lock_file: PathBuf,
}

impl ProcessLock {
    pub fn acquire(lock_path: PathBuf) -> Result<Self, std::io::Error> {
        // 检查锁文件是否存在且进程仍在运行
        if lock_path.exists() {
            // 检查 PID 是否存活
            // ...
        }

        // 创建锁文件，写入当前 PID
        let mut file = File::create(&lock_path)?;
        writeln!(file, "{}", std::process::id())?;

        Ok(Self { lock_file: lock_path })
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.lock_file);
    }
}
```

## 与 Web 服务器集成

```rust
// src/web/mod.rs
use axum::Router;
use std::sync::Arc;

pub fn create_router() -> Router {
    Router::new()
        // 路由定义...
}

// 在 main.rs 中确保引擎初始化在服务器启动前完成
```
