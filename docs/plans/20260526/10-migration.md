# 迁移策略

## 概述

本文档描述了从 TypeScript `gbrain` 到 Rust `zbrain` 的完整迁移路径，包括数据迁移、配置迁移和用户升级指南。

## 数据迁移策略

### 数据库迁移路径

#### 方案 A：Postgres 引擎直接迁移

如果用户使用 Postgres 引擎：

| 阶段 | 操作 | 说明 |
|------|------|------|
| 1 | 数据快照 | `gbrain export --all --output pre-migration-snapshot.json` |
| 2 | 验证快照 | `gbrain stats --json` 对比前后一致性 |
| 3 | 停止写入 | 确保没有新的写入操作 |
| 4 | Schema 保持 | Postgres schema 兼容（同一作者） |
| 5 | 切换二进制 | 用 `zbrain` 替换 `gbrain` |
| 6 | 验证读取 | `zbrain search` 验证数据可访问 |
| 7 | 恢复写入 | 恢复正常操作 |

**风险等级**：低 - schema 兼容

#### 方案 B：PGLite → LibSQL 迁移

如果用户使用 PGLite 引擎：

| 阶段 | 操作 | 说明 |
|------|------|------|
| 1 | 完整导出 | `gbrain export --all --output pglite-export.json` |
| 2 | 备份 PGLite | 复制整个 `~/.gbrain` 目录 |
| 3 | 初始化 LibSQL | `zbrain init --libsql` |
| 4 | 导入数据 | `zbrain import --file pglite-export.json` |
| 5 | 验证完整性 | `zbrain stats` + 抽样查询 |

**注意**：PGLite 是 WASM 包装的 SQLite，LibSQL 是 SQLite 分支，数据格式兼容。

### 配置迁移

#### 配置文件映射

| TypeScript 位置 | Rust 位置 | 说明 |
|-----------------|-----------|------|
| `~/.gbrain/config.json` | `~/.zbrain/config.json` | 主要配置 |
| `~/.gbrain/config.yml` | `~/.zbrain/config.yml` | YAML 配置（如果存在） |
| `~/.gbrain/skills/` | `~/.zbrain/skills/` | 技能目录（保持原样） |

#### 配置键重命名

```bash
# 自动迁移工具会处理这些重命名
gbrain.cli_path → zbrain.cli_path
gbrain.sync.enabled → zbrain.sync.enabled
```

## 用户升级路径

### 场景 1：Postgres 用户（推荐）

```bash
# 1. 准备
gbrain export --all --output pre-upgrade-$(date +%Y%m%d).json
gbrain doctor  # 确保健康

# 2. 安装新二进制
cargo install zbrain --locked  # 或从 GitHub Releases 下载

# 3. 迁移配置（可选，工具自动处理）
zbrain migrate-config --from ~/.gbrain --to ~/.zbrain

# 4. 验证
zbrain doctor
zbrain search "test query"  # 验证功能

# 5. （可选）清理旧二进制
which gbrain && rm "$(which gbrain)"
```

### 场景 2：PGLite → LibSQL 用户

```bash
# 1. 完整备份
cp -r ~/.gbrain ~/.gbrain.backup.$(date +%Y%m%d)
gbrain export --all --output pglite-full-export.json

# 2. 安装新二进制
cargo install zbrain --locked

# 3. 初始化新引擎
zbrain init --libsql

# 4. 导入数据
zbrain import --file pglite-full-export.json

# 5. 验证
zbrain doctor
zbrain stats  # 页面/区块计数应匹配
```

### 场景 3：从源构建的开发者

```bash
# 1. 保持旧仓库
cd ~/repos
mv gbrain gbrain-legacy  # 保留作为参考

# 2. 获取新仓库（假设已重命名）
git clone git@github.com:jununfly/zbrain.git
cd zbrain

# 3. 构建
cargo build --release

# 4. 链接新二进制
ln -sf $(pwd)/target/release/zbrain ~/.local/bin/zbrain

# 5. 迁移配置（如果需要）
zbrain migrate-config --from ~/.gbrain --to ~/.zbrain
```

## 迁移工具

### `zbrain migrate-config` 子命令

```rust
// src/commands/migrate_config.rs
#[derive(Debug, Parser)]
pub struct MigrateConfigArgs {
    /// 源配置目录（默认：~/.gbrain）
    #[arg(long, value_name = "DIR")]
    from: Option<PathBuf>,

    /// 目标配置目录（默认：~/.zbrain）
    #[arg(long, value_name = "DIR")]
    to: Option<PathBuf>,

    /// 仅预览，不实际复制
    #[arg(long)]
    dry_run: bool,

    /// 强制覆盖已存在的目标目录
    #[arg(long)]
    force: bool,
}

pub async fn run_migrate_config(args: MigrateConfigArgs) -> Result<()> {
    // 实现：
    // 1. 确认源存在
    // 2. 检查目标是否已存在（除非 --force）
    // 3. 复制配置文件，重命名键
    // 4. 复制技能目录（保持原样）
    // 5. 打印迁移摘要
}
```

### `zbrain doctor` 迁移检查

```rust
// src/commands/doctor.rs - 迁移相关检查
enum MigrationCheck {
    /// 检查是否有旧的 ~/.gbrain 目录
    LegacyConfigDirExists,

    /// 检查配置是否已迁移
    ConfigMigrated,

    /// 检查技能是否可访问
    SkillsAccessible,

    /// 检查数据库可访问
    DatabaseAccessible,
}
```

## 并行运行支持

### 为什么要同时运行两个版本？

- 平稳过渡：先验证新版本，再完全切换
- A/B 测试：对比功能和性能
- 回滚安全：如果出现问题，立即切换回旧版本

### 配置隔离

两个版本默认使用不同的配置目录：

| 版本 | 默认目录 | 环境变量覆盖 |
|------|---------|-------------|
| TypeScript `gbrain` | `~/.gbrain` | `GBRAIN_HOME` |
| Rust `zbrain` | `~/.zbrain` | `ZBRAIN_HOME` |

**Postgres 用户共享数据库注意事项**：

```bash
# 如果要同时运行两个版本访问同一数据库，
# 需要显式配置其中一个使用不同的源前缀或源 ID，
# 以防止覆盖写入冲突。

# 建议：先切换到 Rust，再完全停止使用 TypeScript 版本
```

## 回滚策略

### 如果 Rust 版本出现问题

```bash
# 场景：已切换到 zbrain 但需要回滚到 gbrain

# 1. 停止所有 zbrain 进程
pkill zbrain
# 或 MCP 会话中停止

# 2. 恢复配置（如果已迁移）
cp -r ~/.gbrain.backup ~/.gbrain  # 如有备份

# 3. 使用旧二进制
which gbrain  # 确保还在 PATH 中

# 4. 验证
gbrain doctor
gbrain stats
```

### 如果数据损坏

```bash
# 前提：已在迁移前做过导出

# 1. 清理损坏状态
mv ~/.zbrain ~/.zbrain.corrupted  # 保留用于调试

# 2. 重新初始化
zbrain init --[libsql|postgres]

# 3. 从导出恢复
zbrain import --file pre-upgrade-export.json

# 4. 验证
zbrain doctor
```

## 发布沟通计划

### CHANGELOG.md 条目（第一版 Rust）

```markdown
## [1.0.0] - 2026-XX-XX (Rust rewrite)

### 重大变更
- **完全重写为 Rust**：从 TypeScript 完整迁移到 Rust，
  提供相同功能但更好的性能和可靠性
- **项目重命名**：`gbrain` → `zbrain`（所有命令、配置、目录）
- **新增 Web UI**：内置本地 Web 界面，支持页面/知识库的 CRUD 操作
- **单例引擎**：全局唯一引擎实例，防止多实例并发写入导致的数据损坏

### 升级指南
1. **先备份**：`gbrain export --all --output pre-upgrade-$(date +%Y%m%d).json`
2. **下载/构建**：从 GitHub Releases 获取 `zbrain`，或 `cargo install zbrain`
3. **迁移配置**：`zbrain migrate-config`（自动处理）
4. **验证**：`zbrain doctor` + 抽样查询
5. **清理旧二进制**（可选）：`rm $(which gbrain)`

### 详细升级路径
- Postgres 用户：Schema 兼容 → 直接替换二进制即可
- PGLite 用户：导出 → 初始化 LibSQL → 导入
- 完整文档：[docs/plan/10-migration.md](docs/plan/10-migration.md)

### 新功能
- Web UI：`zbrain web --port 3000` 启动本地界面
- 更好的性能：启动时间 ~300ms vs ~2-3s
- 更小的二进制：~15MB（压缩）vs ~50MB（Bun 打包）

### 已知限制
- 暂未实现自动技能触发（计划后续版本）
- 某些边缘配置可能需要手动调整（运行 `zbrain doctor` 获取建议）
```

## 测试迁移路径

### 集成测试场景

```rust
// tests/migration_tests.rs
#[tokio::test]
async fn test_config_migration() {
    // 测试配置迁移逻辑
}

#[tokio::test]
async fn test_data_export_import_roundtrip() {
    // 导出 → 导入 → 验证完整性
}
```

### 端到端迁移测试流程

```bash
# 在干净的容器/VM 中运行
# 1. 从源码安装旧版本
git checkout v0.41.14.0  # 最新 TS 版本
bun install
bun run build

# 2. 初始化并填充一些测试数据
gbrain init --pglite
# ... 创建一些测试页面/区块/嵌入 ...

# 3. 导出
gbrain export --all --output test-export.json

# 4. 切换到新代码并构建
git checkout master  # 假设 master 是 Rust 版本
cargo build --release

# 5. 初始化并导入
./target/release/zbrain init --libsql
./target/release/zbrain import --file test-export.json

# 6. 验证
./target/release/zbrain doctor
./target/release/zbrain stats
# 运行一些查询验证结果质量
```

## 发布时间表建议

| 阶段 | 时间 | 动作 | 目标受众 |
|------|------|------|---------|
| 1. Alpha | Week 1 | 仅核心贡献者测试、bug bash | 维护者 |
| 2. Beta | Week 2-3 | 开放给社区早期采用者、收集反馈 | 贡献者/高级用户 |
| 3. RC1 | Week 4 | 功能冻结、仅 bug 修复 | 社区 |
| 4. 正式发布 | Week 5+ | 文档完成、CHANGELOG、博客文章 | 所有用户 |

## 相关文档

- 项目目标：[01-goals.md](01-goals.md)
- 范围边界：[02-scope.md](02-scope.md)
- 技术栈：[05-tech-stack.md](05-tech-stack.md)
- 测试策略：[09-testing.md](09-testing.md)
- 手术主计划：[README.md](README.md)
