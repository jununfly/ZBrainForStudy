# PR Description — C1 BrainEngine contract hardening

> 复用说明：本文件即 PR 描述本体，直接复制 `## ` 以下内容粘贴到 GitHub PR 即可。
> 对应 plan：`docs/plans/2026-06-01-brainengine-contract-hardening.md`

---

## Summary

把 `BrainEngine` trait 中 15 个 Slice 6a S6 page methods 的 `Err(Error::unsupported("pending slice 6a"))` 默认 fallback 全部删除，升级为每个 backend 必须显式实现的 required trait contract。InMemoryEngine 同步补齐至 15/15 parity，漏实现即编译错。

## Commit 序列

| # | Commit | Plan task | 内容 |
|---|---|---|---|
| 1 | `67ebcce` | Task 1 + 2 | InMemoryEngine lifecycle + tag contract |
| 2 | `415d856` | Task 3 | InMemoryEngine advanced-write contract |
| 3 | `2654584` | Task 4 | InMemoryEngine advanced-read contract |
| 4 | `b456e6b` | hotfix | `in_memory_soft_delete_page_matches_libsql_contract` 用 `include_deleted=true` 复查 |
| 5 | `f7a647e` | Task 5 | 删除 BrainEngine trait 15 个 S6 fallback |
| 6 | `6d434c4` | Task 6 | 清 3 个 `page_methods_*.rs` 中 stale RED 注释 |
| 7 | `f48f6bd` | Task 7（style 收尾） | 见下方 |

## Plan 外说明：`f48f6bd` style commit

Task 7 终验首步 `cargo fmt --all` 重排了 `engine.rs` 与 `tests/in_memory_engine_contract.rs` 中既有的长 async fn 签名 / closure / macro 参数布局，**纯样式无语义改动**。

按 plan 第 7 节 `Use a new commit. Do not amend previous commits.` 的硬性要求，没有 amend 进 `f7a647e`，独立成 commit。因此 PR 实际 7 个 commit，plan 是 6 个 task —— 多出的 `f48f6bd` 是 plan fmt 约束的直接产物，不是计划外偏差。

如团队偏好 squash，可在 merge 时选 squash strategy。

## 终验（Task 7 四连绿，workspace 级）

```
cargo fmt --all --check                                  PASS
cargo build --workspace                                  PASS
cargo test --workspace                                   PASS
cargo clippy --workspace --all-targets -- -D warnings    PASS
```

PG 套件实跑非 skip（`ZBRAIN_TEST_PG_URL` 已设）。

## Acceptance

- [x] InMemoryEngine 覆盖 15/15 S6 method
- [x] BrainEngine trait 不含任何 `pending slice 6a` 默认 body
- [x] stale "RED until S3 GREEN ..." 描述性注释已清，保留 PG SQL 行为契约说明
- [x] fmt / build / test / clippy 全绿
- [x] commits 不 amend，新增 commit 形式收尾

## Reviewer 关注点

1. `f48f6bd` 是否 squash 进 `f7a647e`：plan 明文禁止 amend，默认保留独立 style commit。
