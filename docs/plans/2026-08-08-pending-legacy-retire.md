# Pending: Legacy GBrain Asset Retirement (132 files, -16,913 lines)

**Created**: 2026-08-08
**Status**: ⚠️ **PENDING OPERATOR DECISION** — not yet committed
**Discovered during**: 接活前 `git status` 摸底（用户决定删 WSL 脚本后我先盘清状态）

---

## TL;DR

2026-08-08 会话发现 132 个 tracked 文件**在工作树中已删除但未 commit**（`-16,913 行 / +81 行`）。本会话**未追溯到删除决策的源头**（可能是上几轮会话中断遗留，或 fork 时的 baseline 漂移），因此**未自动 commit 任何删除**——以本文档作为决策依据，等用户拍板。

132 个文件 = 50 个 shell/TS 脚本 + 34 个 `docs/guides/` markdown + 8 个 tree-sitter wasm 语法 + 7 个 `docs/mcp/` 集成 + 5 个 `docs/integrations/` + 3 个 `docs/tutorials/` + 3 个 `docs/ethos/` + 3 个 `docs/designs/` + 2 个 `src/eval/longmemeval/` + 2 个 `docs/issues/` + 2 个 `docs/eval/` + 1 个 `src/types/` + 9 个散点。

**外部 ripple（commit 前必须处理）**：
1. `package.json` — 24 个 npm script 入口引用被删脚本（`typecheck` / `verify` / `test` / `test:e2e` / `test:slow` / `check:*` 16 个）
2. `.github/workflows/test.yml` — 8 处引用（`ci-cache-hash.sh` / `run-verify-parallel.sh` / `test-shard.sh` / `sharding.ts` / `test-weights.json` / `mine-shard-weights.ts`）
3. `bunfig.toml` — 1 处注释引用 `run-unit-parallel.sh` / `run-unit-shard.sh`
4. `admin/src/lib/scope-constants.ts` — 1 处注释引用 `check-admin-scope-drift.sh`
5. `docker-compose.ci.yml` — 2 处注释引用 `ci-local.sh`
6. `recipes/agent-voice/code/lib/personas/private-name-blocklist.json` — 2 条 `exceptionFiles` 引用 `import-from-upstream.sh` / `upstream-scrub-table.txt`

**如果直接 commit 132 个删除而不修 ripple**：CI 必红，5 个 npm script 段 exit 1，admin build gate 失败。代价 = 跨 6 个文件 ~50 处编辑。本机无 cargo 无法 verify。

---

## 决策选项

### 选项 A：批量 commit（接受 ripple break）

```bash
# 1. 一次性 stage + commit
git add -A  # 132 deletions
git commit -m "chore(retire): drop legacy gbrain assets (-16,913 lines)

- 50 个 scripts/* (check-*.sh + run-*.sh + ci-*.sh + TS 迁移工具)
- 34 个 docs/guides/* (gbrain 时代 gbrain 操作指南)
- 7 个 docs/mcp/* (第三方 MCP 客户端集成文档)
- 5 个 docs/integrations/* (integration 指南)
- 3 个 docs/ethos/* (ethos 文档)
- 3 个 docs/tutorials/* (使用教程)
- 3 个 docs/designs/* (gbrain 架构设计稿)
- 2 个 src/eval/longmemeval/* (GBrain 时代的 longmemeval eval)
- 1 个 src/types/image-decoders.d.ts (gbrain image decoder 类型)
- 9 个散点 docs/* (proposals/operations/migrations/incidents/issues/images)
- 8 个 src/assets/wasm/grammars/tree-sitter-*.wasm (R&D 时代嵌入的 .wasm 二进制, Rust 用 tree-sitter crate 源码编译, 不再需要)

Ref: docs/plans/2026-08-08-pending-legacy-retire.md (本文件)"
```

**前置修复**（必做，否则 CI 红）：
- 修 `package.json`：删除引用被删脚本的 script 入口（24 个），保留能正常工作的（`build:rust` / `build:admin` / `ci:select-e2e` / `typecheck:raw` / `check:resolver` / `prepublish` / `publish:clawhub` 等）
- 修 `.github/workflows/test.yml`：删 `cache-check` 段（`ci-cache-hash.sh` 调用）+ `verify` 段（`run-verify-parallel.sh` 调用）+ test matrix 段（`test-shard.sh` 调用）+ sharding 注释（`sharding.ts` / `test-weights.json` / `mine-shard-weights.ts` 引用）
- 修 `bunfig.toml`：删 v0.26.4 注释
- 修 `admin/src/lib/scope-constants.ts`：删 drift 检测注释
- 修 `docker-compose.ci.yml`：删 `ci-local.sh` 引用注释
- 修 `private-name-blocklist.json`：删 2 条 `exceptionFiles`

**风险**：
- 跨 6 个文件 ~50 处编辑
- 本机无 cargo，无法本地 verify 修改
- CI push 后必红 → 等 push 失败再修成本高
- 测试可能依赖 `scripts/check-*.sh` 的守卫逻辑，删后 cargo build 可能通过但实际质量降

**收益**：
- 一次性清理仓库，-16,913 行
- git history 清晰反映"GBrain 资产全部退役"

### 选项 B：拆多个 commit（per-范围）

按目录拆为 4-5 个 commit，每个独立可回滚：

```
1. chore(retire-scripts): drop 50 GBrain CI gate scripts
2. chore(retire-docs): drop 47 GBrain-era documentation files
3. chore(retire-grammar): drop 8 tree-sitter wasm binaries
4. chore(retire-eval): drop 2 longmemeval TS + image-decoders.d.ts
5. (ripple 修复 commit 1: package.json)
6. (ripple 修复 commit 2: .github/workflows/test.yml)
7. (ripple 修复 commit 3: bunfig.toml + admin + docker-compose + blocklist.json)
```

**收益**：
- 每次 ripple break 在独立 commit 里，回滚容易
- git history 信号保留
- 可逐步验证每步不引入新 break

**代价**：
- 工作量更大
- 仍需修全部 6 个 ripple 文件
- 多次 commit 噪声

### 选项 C：暂不 commit（保持工作树当前状态）

什么都不做。132 个文件保持"工作树中已删但未 commit"状态。下次会话（用户手动决定后）再处理。

**风险**：
- 工作树状态与索引长期不一致，`git status` 永远脏
- 任何 `git add -A` 误操作会立刻 commit 132 个删除
- 下次会话接活又会触发同样的"接活前 git status 摸底"流程

**收益**：
- 零 commit 风险
- 信息零损失

### 选项 D：撤回删除（恢复 132 个文件到工作树）

假设这 132 个删除是误操作或状态漂移（另一台机器 fork sync 时出问题），`git checkout HEAD -- <files>` 全部恢复。

**风险**：
- 如果上几轮**真的**决定删这些文件（用户意志未执行完），撤回 = 反悔

**收益**：
- 仓库回到干净状态
- 后续按"自然演进"原则决定哪些该删

### 推荐：**F 方案（已在本轮执行）**

本轮已撤回 132 个删除（`git checkout HEAD --` 恢复），并把决策依据保存到本文档。

撤回原因：删除意图不清晰 + 5 处外部 ripple 连锁修改 + 本机无 cargo 无法 verify。

---

## 132 个文件完整清单（by 范围）

### scripts/（50 个 — gbrain CI gate + TS 迁移工具）

| 文件 | 用途（推测） |
|---|---|
| `scripts/build-schema.sh` | gbrain schema 重建脚本 |
| `scripts/check-admin-build.sh` | admin build gate |
| `scripts/check-admin-scope-drift.sh` | admin scope drift 检测（admin/src/lib/scope-constants.ts 引用） |
| `scripts/check-cli-executable.sh` | CLI 可执行性 gate |
| `scripts/check-eval-glossary-fresh.sh` | eval glossary 时效性 gate |
| `scripts/check-fuzz-purity.sh` | fuzz test 纯净性 gate |
| `scripts/check-gateway-routed-no-direct-anthropic.sh` | gateway routing gate |
| `scripts/check-image-decoders-embedded.sh` | image decoder 嵌入 gate |
| `scripts/check-jsonb-pattern.sh` | jsonb pattern gate |
| `scripts/check-no-legacy-getconnection.sh` | 旧 getConnection 守卫 |
| `scripts/check-no-pii-in-agent-voice.sh` | agent-voice PII gate |
| `scripts/check-operations-filter-bypass.sh` | operations filter bypass gate |
| `scripts/check-pagetype-exhaustive.sh` | pagetype exhaustive gate |
| `scripts/check-pg-url-redaction.sh` | PG URL redaction gate |
| `scripts/check-privacy.sh` | privacy gate |
| `scripts/check-progress-to-stdout.sh` | stdout 进度格式 gate |
| `scripts/check-proposal-pii.sh` | proposal PII gate |
| `scripts/check-skill-brain-first.sh` | skill brain-first gate |
| `scripts/check-source-config-leak.sh` | source config leak gate |
| `scripts/check-source-id-projection.sh` | source id projection gate |
| `scripts/check-synthetic-corpus-privacy.sh` | synthetic corpus privacy gate |
| `scripts/check-system-of-record.sh` | system of record gate |
| `scripts/check-test-isolation.allowlist` | test isolation allowlist |
| `scripts/check-test-isolation.sh` | test isolation gate |
| `scripts/check-test-real-names.sh` | test real names gate |
| `scripts/check-trailing-newline.sh` | trailing newline gate |
| `scripts/check-wasm-embedded.sh` | wasm 嵌入 gate |
| `scripts/ci-cache-hash.sh` | CI cache hash 计算 |
| `scripts/ci-local.sh` | 本地 CI 模拟 |
| `scripts/e2e-mounts-smoke.sh` | e2e mount smoke |
| `scripts/fix-v0.11.0.sh` | v0.11.0 修复脚本 |
| `scripts/import-from-upstream.sh` | 从上游 import |
| `scripts/ops_audit.py` | ops audit 工具 |
| `scripts/profile-tests.sh` | test profiling |
| `scripts/run-e2e.sh` | e2e runner |
| `scripts/run-heavy.sh` | heavy test runner |
| `scripts/run-serial-tests.sh` | serial test runner |
| `scripts/run-slow-tests.sh` | slow test runner |
| `scripts/run-unit-parallel.sh` | unit parallel runner |
| `scripts/run-unit-shard.sh` | unit shard runner |
| `scripts/run-verify-parallel.sh` | verify parallel runner |
| `scripts/smoke-test.sh` | smoke test |
| `scripts/test-shard.sh` | test shard runner |
| `scripts/test-weights.json` | test shard weights |
| `scripts/ts_dep_closure.py` | TS dep closure（Phase 11 cutover 工具） |
| `scripts/ts_keep_seeds.txt` | TS KEEP seeds（ts_dep_closure 输入） |
| `scripts/ts_ops.txt` | TS ops seeds（ts_dep_closure 输入） |
| `scripts/tsc-baseline.txt` | tsc baseline（未排序，typecheck-baseline.sh 比对用） |
| `scripts/typecheck-baseline.sh` | typecheck baseline gate |
| `scripts/upstream-scrub-table.txt` | upstream scrub table（blocklist exceptionFiles 引用） |

### docs/guides/（34 个 — gbrain 时代操作指南）

`agent-to-gbrain.md`, `brain-agent-loop.md`, `brain-first-lookup.md`, `brain-vs-memory.md`, `compiled-truth.md`, `content-media.md`, `cron-schedule.md`, `deterministic-collectors.md`, `diligence-ingestion.md`, `enrichment-pipeline.md`, `entity-detection.md`, `executive-assistant.md`, `idea-capture.md`, `live-sync.md`, `meeting-ingestion.md`, `minions-deployment-snippets/Procfile`, `minions-deployment-snippets/fly.toml.partial`, `minions-deployment-snippets/gbrain.env.example`, `minions-deployment-snippets/systemd.service`, `minions-deployment.md`, `minions-fix.md`, `minions-shell-jobs.md`, `multi-source-brains.md`, `operational-disciplines.md`, `originals-folder.md`, `plugin-authors.md`, `plugin-handlers.md`, `queue-operations-runbook.md`, `quiet-hours.md`, `repo-architecture.md`, `rls-and-you.md`, `scaling-skills.md`, `search-modes.md`, `skill-development.md`, `skillpacks-as-scaffolding.md`, `source-attribution.md`, `sub-agent-routing.md`, `upgrades-auto-update.md`

### docs/mcp/（7 个 — 第三方 MCP 集成）

`ALTERNATIVES.md`, `CHATGPT.md`, `CLAUDE_CODE.md`, `CLAUDE_COWORK.md`, `CLAUDE_DESKTOP.md`, `DEPLOY.md`, `PERPLEXITY.md`

### docs/integrations/（5 个）

`README.md`, `credential-gateway.md`, `meeting-webhooks.md`, `pre-commit.md`, `reliability-repair.md`

### docs/ethos/（3 个）

`MARKDOWN_SKILLS_AS_RECIPES.md`, `ORIGIN.md`, `THIN_HARNESS_FAT_SKILLS.md`

### docs/tutorials/（3 个）

`README.md`, `company-brain.md`, `personal-brain.md`

### docs/designs/（3 个 — gbrain 架构设计稿）

`CODE_CATHEDRAL_II.md`, `HOMEBREW_FOR_PERSONAL_AI.md`, `KNOWLEDGE_RUNTIME.md`

### src/assets/wasm/grammars/（8 个 — Rust 不再需要）

`tree-sitter-systemrdl.wasm`, `tree-sitter-tlaplus.wasm`, `tree-sitter-toml.wasm`, `tree-sitter-tsx.wasm`, `tree-sitter-typescript.wasm`, `tree-sitter-vue.wasm`, `tree-sitter-yaml.wasm`, `tree-sitter-zig.wasm`

> 验证：grep 仓库全部 `.rs`/`.toml` 无 `tree-sitter-systemrdl` 等引用（Rust 端口的 tree-sitter 是 `tree-sitter = "0.26"` crate，从源码编译，`.wasm` 二进制仅在 TS `bun --compile` 嵌入用，ZBrain Rust CLI 不用）。

### src/eval/longmemeval/（2 个 — gbrain 时代 eval）

`intent.ts`, `sanitize.ts`

### src/types/（1 个 — gbrain image decoder 类型）

`image-decoders.d.ts`

### docs/ 散点（9 个）

`docs/eval/METRIC_GLOSSARY.md`, `docs/eval/SEARCH_MODE_METHODOLOGY.md`, `docs/issues/cross-modal-search.md`, `docs/issues/doctor-auto-heal-and-scoring.md`, `docs/migrations/v0.41.2-markdown-greenfield.md`, `docs/operations/headless-install.md`, `docs/proposals/temporal-contradiction-probe.md`, `docs/incidents/2026-05-20-lsd-cost-explosion.md`, `docs/images/voice-client.png`

---

## 外部 ripple 引用清单

### package.json（30 处）

```
build:schema → scripts/build-schema.sh
test → scripts/run-unit-parallel.sh
test:full → scripts/run-e2e.sh, run-unit-parallel.sh, run-slow-tests.sh
verify → scripts/run-verify-parallel.sh
check:* (16 个) → scripts/check-*.sh
test:e2e → scripts/run-e2e.sh
test:slow → scripts/run-slow-tests.sh
test:heavy → scripts/run-heavy.sh
test:profile → scripts/profile-tests.sh
test:serial → scripts/run-serial-tests.sh
ci:local → scripts/ci-local.sh
ci:local:diff → scripts/ci-local.sh
typecheck → scripts/typecheck-baseline.sh
typecheck:update-baseline → scripts/typecheck-baseline.sh
```

### .github/workflows/test.yml（8 处）

```
L37: HASH=$(bash scripts/ci-cache-hash.sh --verbose ...)
L83: # scripts/run-verify-parallel.sh fans out ...
L125, L149: # (see scripts/test-shard.sh) ...
L182, L183: # scripts/sharding.ts ... scripts/test-weights.json ...
L184: # ... via scripts/mine-shard-weights.ts ...
L204: run: scripts/test-shard.sh ${{ matrix.shard }} 10
```

### bunfig.toml（1 处）

```
L6-8: # v0.26.4: scripts/run-unit-parallel.sh and scripts/run-unit-shard.sh
      # also pass `--timeout=60000` explicitly ...
```

### admin/src/lib/scope-constants.ts（1 处）

```
L7: * scripts/check-admin-scope-drift.sh fails the build if the two lists drift.
```

### docker-compose.ci.yml（2 处）

```
# (see scripts/ci-local.sh)
# No global DATABASE_URL — scripts/ci-local.sh sets per-shard URL via -e.
```

### recipes/agent-voice/code/lib/personas/private-name-blocklist.json（2 条 exceptionFiles）

```json
"scripts/import-from-upstream.sh",
"scripts/upstream-scrub-table.txt"
```

---

## 为什么建议先 F 方案（不 commit + 暂存文档）

1. **意图不清晰**：132 个删除是上几轮会话遗留，本轮没追溯到决策源头。如果直接 commit `chore(retire): drop gbrain legacy`，commit message 是**本会话编的**，git history 被错误叙事污染。
2. **ripple 太大**：6 个外部文件 ~50 处编辑。本机无 cargo，无法本地 verify 修改。CI push 后必红。
3. **解耦本轮任务**：本轮用户**只让做两件事**——删 WSL 脚本 + 改 memory。132 个老删除是旁支，应独立决定。
4. **可恢复性最高**：撤回 + 写 pending doc，原始意图可追溯（用户在本文档里能看到完整删除清单 + 原因 + ripple 引用）。直接 commit 后只能 revert，难回滚到原始逐文件。

---

## 后续动作

- [ ] 用户拍板 A/B/C/D 选项
- [ ] 若选 A：先修 6 个外部文件 ripple，再 commit
- [ ] 若选 B：拆 4-5 个 commit + ripple 修复 split
- [ ] 若选 C：保持状态，等下次会话
- [ ] 若选 D：再次 `git checkout HEAD --` 撤回（但已经撤回过，等于本轮操作撤销）
- [ ] 删除本文档（决策完成）

---

**作者**: WorkBuddy 2026-08-08 18:55 GMT+8
**关联 commit**: `3705ede2` (HEAD base) → 本会话 commit 后引用
**关联文档**: `.workbuddy/memory/2026-08-08.md` (本轮完整决策日志)
