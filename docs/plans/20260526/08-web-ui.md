# Web 界面设计

## 功能需求

### 核心功能

| 功能 | 说明 |
|------|------|
| 页面列表 | 显示所有知识库页面，支持分页和排序 |
| 页面搜索 | 混合 RAG 搜索，关键词 + 向量 |
| 页面查看 | Markdown 渲染，显示元数据 |
| 页面编辑 | 实时编辑，保存历史 |
| 页面创建 | 新建页面，支持模板 |
| 页面删除 | 软删除，可恢复 |
| 实体关系图 | 可视化显示实体关联 |
| 文件管理 | 上传、下载、删除附件 |
| 搜索统计 | 显示搜索质量指标 |

## 技术方案

### 方案 A: 纯 Rust 前端 (Leptos)

```
zbrain/
├── src/
│   ├── web/
│   │   ├── mod.rs
│   │   ├── routes.rs
│   │   ├── handlers.rs
│   │   └── templates/          # Askama 模板
│   │       ├── base.html
│   │       ├── pages.html
│   │       ├── edit.html
│   │       └── search.html
│   └── ...
└── Cargo.toml
```

**优点**:
- 单一技术栈
- 编译为单一二进制
- 部署简单

**缺点**:
- 前端开发体验不如 React
- 生态系统较小

---

### 方案 B: 传统 SPA (React + Axum)

```
zbrain/
├── src/
│   ├── web/
│   │   ├── mod.rs
│   │   ├── routes.rs        # API 路由
│   │   ├── handlers.rs      # API 处理器
│   │   └── static.rs        # 静态文件服务
│   └── ...
├── frontend/
│   ├── index.html
│   ├── package.json
│   ├── vite.config.ts
│   └── src/
│       ├── main.tsx
│       ├── App.tsx
│       ├── pages/
│       │   ├── List.tsx
│       │   ├── Edit.tsx
│       │   ├── View.tsx
│       │   └── Search.tsx
│       ├── components/
│       │   ├── Markdown.tsx
│       │   ├── Graph.tsx
│       │   └── ...
│       └── api/
│           └── client.ts
└── Cargo.toml
```

**优点**:
- 前端生态成熟
- 可以复用原 admin/ 代码
- 开发体验好

**缺点**:
- 需要构建两个部分
- 部署稍复杂

---

### 推荐方案: B (React + Axum)

可以最大限度复用原项目的 admin/ 前端代码。

## API 设计

### RESTful API

```
GET     /api/pages              # 页面列表
POST    /api/pages              # 创建页面
GET     /api/pages/:slug        # 获取页面
PUT     /api/pages/:slug        # 更新页面
DELETE  /api/pages/:slug        # 删除页面
POST    /api/search             # 搜索
GET     /api/graph              # 关系图数据
GET     /api/stats              # 统计信息
```

### 页面列表 API

```rust
// src/web/types.rs
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

#[derive(Serialize, Deserialize)]
pub struct PageListResponse {
    pub pages: Vec<PageInfo>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Serialize, Deserialize)]
pub struct PageInfo {
    pub slug: String,
    pub title: String,
    pub page_type: String,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}
```

### 搜索 API

```rust
#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub types: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub slug: String,
    pub title: String,
    pub snippet: String,
    pub score: f64,
    pub page_type: String,
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total: usize,
    pub took_ms: u64,
}
```

## 路由设计

```rust
// src/web/routes.rs
use axum::{routing::{get, post, put, delete}, Router};
use crate::web::handlers;

pub fn create_router() -> Router {
    Router::new()
        // API 路由
        .route("/api/pages", get(handlers::list_pages))
        .route("/api/pages", post(handlers::create_page))
        .route("/api/pages/:slug", get(handlers::get_page))
        .route("/api/pages/:slug", put(handlers::update_page))
        .route("/api/pages/:slug", delete(handlers::delete_page))
        .route("/api/search", post(handlers::search))
        .route("/api/graph", get(handlers::get_graph))
        .route("/api/stats", get(handlers::get_stats))
        // 静态文件
        .fallback_service(tower_http::services::ServeDir::new("frontend/dist"))
}
```

## 处理器示例

```rust
// src/web/handlers.rs
use axum::{
    extract::{Path, State, Json},
    http::StatusCode,
    response::IntoResponse,
};
use crate::core::singleton::get_engine;
use crate::web::types::*;

pub async fn list_pages() -> impl IntoResponse {
    let engine = match get_engine() {
        Ok(e) => e,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let engine = engine.lock();
    let pages = match engine.list_pages(None).await {
        Ok(p) => p,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let response = PageListResponse {
        pages: pages.into_iter().map(|p| PageInfo {
            slug: p.slug,
            title: p.title,
            page_type: p.page_type.to_string(),
            updated_at: p.updated_at,
            created_at: p.created_at,
        }).collect(),
        total: pages.len(),
        page: 1,
        page_size: 100,
    };

    Json(response).into_response()
}

pub async fn search(Json(req): Json<SearchRequest>) -> impl IntoResponse {
    let engine = match get_engine() {
        Ok(e) => e,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let engine = engine.lock();
    let results = match engine.search(&req.query).await {
        Ok(r) => r,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    let response = SearchResponse {
        results: results.into_iter().map(|r| SearchResult {
            slug: r.slug,
            title: r.title,
            snippet: r.snippet,
            score: r.score,
            page_type: r.page_type,
        }).collect(),
        total: results.len(),
        took_ms: 0,
    };

    Json(response).into_response()
}
```

## 前端页面设计

### 页面列表 (List.tsx)

```tsx
import { useState, useEffect } from 'react';
import { Link } from 'react-router-dom';
import api from '../api/client';

export default function PageList() {
    const [pages, setPages] = useState<PageInfo[]>([]);

    useEffect(() => {
        api.listPages().then(setPages);
    }, []);

    return (
        <div className="container mx-auto p-4">
            <h1 className="text-2xl font-bold mb-4">Pages</h1>
            <div className="grid gap-2">
                {pages.map(page => (
                    <Link
                        key={page.slug}
                        to={`/page/${page.slug}`}
                        className="p-4 border rounded hover:bg-gray-50"
                    >
                        <h2 className="font-semibold">{page.title}</h2>
                        <p className="text-sm text-gray-500">
                            {new Date(page.updated_at).toLocaleDateString()}
                        </p>
                    </Link>
                ))}
            </div>
        </div>
    );
}
```

### 编辑页面 (Edit.tsx)

```tsx
import { useState, useEffect } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import ReactMarkdown from 'react-markdown';
import api from '../api/client';

export default function PageEdit() {
    const { slug } = useParams();
    const navigate = useNavigate();
    const [content, setContent] = useState('');
    const [title, setTitle] = useState('');

    useEffect(() => {
        if (slug) {
            api.getPage(slug).then(page => {
                setTitle(page.title);
                setContent(page.content);
            });
        }
    }, [slug]);

    const handleSave = async () => {
        if (slug) {
            await api.updatePage(slug, { title, content });
        } else {
            await api.createPage({ title, content });
        }
        navigate('/');
    };

    return (
        <div className="container mx-auto p-4">
            <input
                type="text"
                value={title}
                onChange={e => setTitle(e.target.value)}
                placeholder="Title"
                className="w-full text-2xl font-bold mb-4 p-2 border"
            />
            <div className="grid grid-cols-2 gap-4">
                <textarea
                    value={content}
                    onChange={e => setContent(e.target.value)}
                    placeholder="Write your content..."
                    className="w-full h-96 p-2 border font-mono"
                />
                <div className="p-2 border overflow-auto">
                    <ReactMarkdown>{content}</ReactMarkdown>
                </div>
            </div>
            <button
                onClick={handleSave}
                className="mt-4 px-4 py-2 bg-blue-500 text-white rounded"
            >
                Save
            </button>
        </div>
    );
}
```

## 启动 Web 服务器

```rust
// src/main.rs (web 相关部分)
use clap::Parser;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Start the web server
    Web {
        #[arg(long, default_value = "3000")]
        port: u16,
    },
    // ... 其他命令
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Web { port } => {
            // 初始化引擎
            let config = Config::load()?;
            if !is_initialized() {
                init_engine(config)?;
            }

            // 启动服务器
            let app = create_router();
            let addr = ([127, 0, 0, 1], port).into();
            println!("Web server running on http://{}", addr);
            axum::Server::bind(&addr)
                .serve(app.into_make_services())
                .await?;
        }
        // ... 其他命令
    }

    Ok(())
}
```

## 使用方式

```bash
# 开发模式（前端）
cd frontend
npm install
npm run dev

# 开发模式（后端）
cargo run -- web --port 3000

# 生产构建（前端）
cd frontend
npm run build

# 生产构建（后端）
cargo build --release
./target/release/zbrain web --port 3000
```
