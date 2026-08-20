# 详细手术方案

## 8步手术流程

### Step 1: 现状保存 ✅ 已完成方案制定

- [x] Git 状态保存（方案中定义）
- [x] 记录当前测试基线
- [x] 备份现有配置

### Step 2: 目标确认 ✅ 已完成

- [x] 确认改写目标（5大目标）
- [x] 确认目标状态描述
- [x] 用户确认（本方案即确认）

### Step 3: 边界澄清 ✅ 已完成

- [x] 明确范围：核心 + CLI + MCP + Web + 技能 + 测试
- [x] 明确排除：Git 历史、Node 依赖、用户数据
- [x] 明确验收标准

### Step 4: 影响盘点 ✅ 已完成

- [x] 列出待改写文件清单
- [x] 追踪上下游依赖
- [x] 标记风险点

### Step 5: 手术方案 ✅ 正在保存

- [x] 编写 checklist
- [x] 按切片组织
- [x] 定义每个切片的验收标准

### Step 6: 编排路线 ⏸️ 待执行

- [ ] 确定执行顺序：低风险 → 高风险
- [ ] 识别可并行切片
- [ ] 标注前置依赖

### Step 7: 切片执行 ⏸️ 待执行

- [ ] 一次只动一个切片
- [ ] 改完立即验证
- [ ] 发现超出方案的新问题 → 暂停，记入 follow-up

### Step 8: 闭合验证 ⏸️ 待执行

- [ ] 跑全量相关测试
- [ ] 影响面回归

## 切片执行计划

### 切片 1: 项目脚手架

**目标**: 建立 Rust 项目基础结构

**文件**:
- `Cargo.toml` - 项目配置和依赖
- `src/main.rs` - 空入口
- `src/lib.rs` - 空库
- `.gitignore` - Rust 专用

**验收标准**:
- [ ] `cargo build` 成功
- [ ] `cargo check` 通过
- [ ] 项目名称为 `zbrain`

---

### 切片 2: 核心类型系统

**目标**: 转换 `src/core/types.ts` 到 Rust

**文件**:
- `src/core/types.rs`
- `src/core/error.rs` - 错误类型

**验收标准**:
- [ ] 所有类型定义完整
- [ ] 编译通过
- [ ] 类型安全检查通过

---

### 切片 3: 引擎 Trait

**目标**: 转换 `src/core/engine.ts` 到 Rust trait

**文件**:
- `src/core/engine.rs`

**验收标准**:
- [ ] BrainEngine trait 定义完整
- [ ] 所有方法签名对应
- [ ] 编译通过

---

### 切片 4: PostgreSQL 引擎

> ⚠️ 切片号已按 `11-execution-decisions.md` 决策 5 重排：Postgres 先做（比 libsql 简单），单例后做。
> 4780 行 TS 源码不一次性迁完，按"小切片原则"拆成 4a/4b/4c 三个子切片。

**目标**: 转换 `src/core/postgres-engine.ts` 中切片 3 已定义的 9 个 BrainEngine 方法

**子切片**:
- **4a**: sqlx 依赖 + Postgres 连接 + init_schema（pages 表 DDL）+ docker-compose
- **4b**: Page CRUD 5 方法 SQL 实现 + impl BrainEngine for PostgresEngine
- **4c**: edge cases + 错误映射到 zbrain-core::Error

**文件**:
- `crates/zbrain-core/src/postgres.rs`
- `crates/zbrain-core/migrations/0001_init.sql`
- `docker-compose.test.yml`

**验收标准**:
- [ ] 实现 BrainEngine trait（仅切片 3 已定义的 9 个方法）
- [ ] 使用 sqlx 进行数据库操作
- [ ] 集成测试与 InMemoryEngine 行为对齐（复用切片 3 的 8 个测试）
- [ ] 每个子切片独立三连绿 + tag

---

### 切片 5: libsql 引擎（嵌入式数据库）

**目标**: 实现 libsql 嵌入式引擎，替代原 PGLite（WASM-Postgres）

**文件**:
- `crates/zbrain-core/src/libsql.rs`
- `crates/zbrain-core/migrations-sqlite/0001_init.sql`

**验收标准**:
- [ ] 实现 BrainEngine trait（9 方法）
- [ ] SQL 方言适配 SQLite（从 Postgres 调整）
- [ ] 集成测试通过

---

### 切片 6: 单例引擎模式

**目标**: 实现全局唯一引擎实例

**文件**:
- `crates/zbrain-core/src/singleton.rs`

**验收标准**:
- [ ] 使用 OnceCell + Arc<Mutex<dyn BrainEngine>>
- [ ] init_engine / get_engine / is_initialized API
- [ ] 防止重复初始化（Error::AlreadyInitialized）
- [ ] 线程安全 + 并发访问测试通过

---

### 切片 6.5a: BrainEngine trait 扩展（批次 1）—— sourceId opt + soft-delete

> 📌 起源：切片 4b 范围切分时，决定先严格遵循切片 3 trait 签名实现 Page CRUD，把 trait 扩展统一推迟到两个引擎都就位之后再做。此切片为承接位置。

**目标**: 扩展 BrainEngine trait 加入多 source 隔离与软删除能力，Postgres + libsql 双引擎同步升级。

**Trait 变更（append-only）**:
- `GetPageOpts` 新增字段已在切片 3 就位（`source_id` / `include_deleted`），此切片让 `include_deleted` 在引擎层"真实生效"
- 新增 `async fn soft_delete_page(&self, slug: &str) -> Result<()>;`
- `PutPageInput` 增加 `source_id: Option<String>`（如尚未具备）

**Schema 变更**:
- `crates/zbrain-core/migrations/0002_soft_delete.sql` —— `ALTER TABLE pages ADD COLUMN deleted_at TIMESTAMPTZ`
- `crates/zbrain-core/migrations-sqlite/0002_soft_delete.sql` —— libsql 对应

**文件**:
- `crates/zbrain-core/src/engine.rs`（trait 扩展 + InMemoryEngine 实现）
- `crates/zbrain-core/src/postgres.rs`（put/get/list/softDelete 升级）
- `crates/zbrain-core/src/libsql.rs`（同步升级）
- 上述 2 个 migration 文件
- `crates/zbrain-core/tests/soft_delete.rs`（新集成测试）

**验收标准**:
- [ ] 旧测试不破（append-only）
- [ ] `getPage(slug, { include_deleted: false })` 默认隐藏 soft-deleted（两引擎一致）
- [ ] `getPage(slug, { include_deleted: true })` 能取回 soft-deleted
- [ ] `softDeletePage` 幂等（重复调用不报错）
- [ ] `sourceId` 过滤生效（put/get/list 三方法）
- [ ] 三连绿 + tag `rust-slice-6-5a`

---

### 切片 6.5b: BrainEngine trait 扩展（批次 2）—— provenance schema

**目标**: 扩展 pages 表 provenance 列，让 putPage UPSERT 完整 COALESCE 行为对齐 TS 版。

**Trait 变更（append-only）**:
- `PageInput` 增加 `frontmatter: Option<Value>` / `content_hash: Option<String>` / `effective_date: Option<DateTime>` / `effective_date_source: Option<EffectiveDateSource>`
- 新增 `async fn find_duplicate_page(&self, content_hash: &str) -> Result<Option<Page>>;`
- 新增 `async fn get_all_slugs(&self) -> Result<Vec<String>>;`
- 新增 `async fn list_all_page_refs(&self) -> Result<Vec<PageRef>>;`

**Schema 变更**:
- `crates/zbrain-core/migrations/0003_provenance.sql` —— `ALTER TABLE pages ADD COLUMN frontmatter JSONB, content_hash TEXT, effective_date TIMESTAMPTZ, effective_date_source TEXT`
- `crates/zbrain-core/migrations-sqlite/0003_provenance.sql` —— libsql 对应（JSONB → TEXT）

**文件**:
- `crates/zbrain-core/src/engine.rs`
- `crates/zbrain-core/src/postgres.rs`（putPage 完整 COALESCE UPSERT）
- `crates/zbrain-core/src/libsql.rs`
- 上述 2 个 migration 文件
- `crates/zbrain-core/tests/provenance.rs`

**验收标准**:
- [ ] `putPage` UPSERT 用 `COALESCE(EXCLUDED.col, pages.col)` 保留旧值
- [ ] `findDuplicatePage` 按 content_hash 找到已存在页面
- [ ] `getAllSlugs` / `listAllPageRefs` 两引擎结果一致
- [ ] 三连绿 + tag `rust-slice-6-5b`

---

### 切片 6.5c: resolveSlugs 模糊匹配

**目标**: 让 `resolveSlugs` 支持 exact match → 模糊匹配的二段策略，对齐 TS 版（postgres-engine.ts:1301-1330）。

**Trait 变更**: 无（沿用切片 3 签名 `async fn resolve_slugs(&self, partial: &str) -> Result<Vec<String>>;`）

**Schema 变更**:
- `crates/zbrain-core/migrations/0004_pg_trgm.sql` —— `CREATE EXTENSION IF NOT EXISTS pg_trgm;` + `CREATE INDEX pages_slug_trgm_idx ON pages USING gin (slug gin_trgm_ops);`
- libsql 侧：FTS5 或退化为 `LIKE %partial%`（决策见下）

**文件**:
- `crates/zbrain-core/src/postgres.rs`（resolve_slugs 升级 pg_trgm）
- `crates/zbrain-core/src/libsql.rs`（FTS5 或 LIKE）
- `crates/zbrain-core/migrations/0004_pg_trgm.sql`
- `crates/zbrain-core/tests/resolve_slugs_fuzzy.rs`

**验收标准**:
- [ ] Postgres: exact → trigram 二段策略，相似度阈值与 TS 版对齐
- [ ] libsql: 至少 LIKE 模糊（FTS5 视复杂度）
- [ ] 两引擎对相同输入返回顺序一致的候选集
- [ ] 三连绿 + tag `rust-slice-6-5c`

---

### 切片 7: 操作契约

**目标**: 转换 `src/core/operations.ts`

**文件**:
- `src/core/operations.rs`

**验收标准**:
- [ ] 所有操作定义完整
- [ ] 参数验证逻辑完整
- [ ] 编译通过

---

### 切片 8: CLI 框架

**目标**: 使用 clap 建立 CLI 框架

**文件**:
- `src/cli.rs`
- `src/main.rs`

**验收标准**:
- [ ] `zbrain --help` 工作
- [ ] 命令结构对应原项目
- [ ] 所有命令占位实现

---

### 切片 9: Web 界面后端

**目标**: 使用 axum 建立 Web 后端

**文件**:
- `src/web/mod.rs`
- `src/web/routes.rs`
- `src/web/handlers.rs`

**验收标准**:
- [ ] Web 服务器启动
- [ ] API 路由定义完整
- [ ] 基本增删改查工作

---

### 切片 10: Web 界面前端

**目标**: 建立前端界面

**文件**:
- `frontend/` (React/Vue 或 Rust 前端框架)
- 或使用 `askama` 模板

**验收标准**:
- [ ] 页面列表显示
- [ ] 页面编辑功能
- [ ] 搜索功能

---

### 切片 11: 测试转换

**目标**: 转换所有测试到 Rust

**文件**:
- `tests/` 目录
- 单元测试嵌入各模块

**验收标准**:
- [ ] `cargo test` 全部通过
- [ ] 测试覆盖率不降低

---

### 切片 12: 品牌重命名

**目标**: 所有 `gbrain` → `zbrain`

**文件**:
- 所有源代码
- 配置文件
- 文档

**验收标准**:
- [ ] 全文搜索无 `gbrain` 残留
- [ ] 所有输出显示 `zbrain`
- [ ] 命令名称正确

---

### 切片 13: 集成测试

**目标**: 端到端测试

**验收标准**:
- [ ] 完整工作流测试
- [ ] 性能基准测试
- [ ] 所有功能验证
