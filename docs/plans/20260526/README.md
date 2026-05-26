# ZBrain 手术式代码库重写方案

## 概述

本文档系列记录了将 GBrain 项目从 TypeScript 完整重写为 Rust 的详细手术方案。

## 项目基本信息

- **原项目**: GBrain v0.41.14.0
- **新项目**: ZBrain
- **语言转换**: TypeScript → Rust
- **预计工期**: 多阶段执行

## 文档结构

| 文档 | 内容 |
|------|------|
| [01-goals.md](./01-goals.md) | 改写目标确认 |
| [02-scope.md](./02-scope.md) | 范围和边界澄清 |
| [03-impact.md](./03-impact.md) | 影响盘点分析 |
| [04-plan.md](./04-plan.md) | 详细手术方案 |
| [05-tech-stack.md](./05-tech-stack.md) | 技术栈选择 |
| [06-project-structure.md](./06-project-structure.md) | 项目结构映射 |
| [07-singleton.md](./07-singleton.md) | 单例引擎设计 |
| [08-web-ui.md](./08-web-ui.md) | Web界面设计 |
| [09-testing.md](./09-testing.md) | 测试保持策略 |
| [10-migration.md](./10-migration.md) | 分阶段迁移计划 |

## 核心原则

1. **方案先行**: 本方案已完整制定，未开始实际改写
2. **最小侵入**: 如执行，将按切片逐步改写
3. **复用优先**: 充分利用 Rust 生态系统现有库
4. **上下游闭环**: 保持 API 兼容性
5. **可回滚**: 每个阶段都有明确的回滚策略

## 当前状态

✅ 方案制定完成  
⏸️ 改写执行暂停（按用户要求）
