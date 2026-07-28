# zbrain 项目记忆

## 项目方向
- 当前处于 TS → Rust 迁移（"Rust 重写线 ZBrain"），整仓统一迁到 ZBrain，所有 GBrain 品牌改名 ZBrain（无线上用户，第一阶段可破坏性改名、不留兼容别名）。`brain`/`source` 作领域词不改。
- 下个 PRD：`docs/prd/complete-ts-to-rust.md`。原则：TS 先不动，Rust 迁成一块删一块；不适合删的到时讨论记录决策。

## 命名 / 品牌迁移
- 连文件名一起改：`gbrain.yml→zbrain.yml`、`docs/GBRAIN_*.md→docs/ZBRAIN_*.md`、package/bin/env/dotfile/path/docs/test 引用。分层执行：配置/包名/bin → env/dotfile/path → docs 文件名与引用 → 测试脚本引用 → 验证断链。

## Roadmap 铁律
- **位置**：所有 `.json`/`.md` 统一放 `docs/plans/`（JSON `zbrain-ts-to-rust-partN-*.json`，md `ZBRAIN_TS_TO_RUST_PARTN_*.md`，同名转大写下划线，无 `ZJ_ROADMAP.md` 特例）。
- **一 part 一 md**：每个 part JSON 必须 `link` 到各自独立 md；严禁多 JSON 共用一 md。`roadmap_cli.py render`（经 `write_markdown_section` roadmap.py:539）**只**重写带 ⚠️ 的 `<!-- ⚠️ ROADMAP_SECTION_START/END -->` 段落——即由 JSON 状态自动生成的"树形视图"活段；**不碰**顶部纯 `<!-- ROADMAP_SECTION_START/END -->` 段。故顶部纯段已成**孤儿**（工具不再维护，会 stale）；`update`+`render` 后若顶部纯段树与 `⚠️` 段不一致，需**手动同步顶部纯段**（render 不回写它；`⚠️` 段自身由 JSON 驱动始终正确）。CLI：`link/render/decide/add/tree` 第一 positional 是 **JSON 完整路径**；render 从项目根 cwd 跑；读的是 JSON 根级 `md_file` key（非 `metadata.md_file`）。
- **代码注释禁引 roadmap 编号**（roadmap JSON 是临时文件，清理后成死链）；必要信息自解释写进注释。docs/plans/ 下 canonical 文档可引用。
- **拆分约定**：审计/拆解发现与当前节点语义偏差且有跟进价值 → 拆 sub-node，不吸收进当前 plan。
- **进度 SSOT**：各 part 完成状态以 roadmap JSON 为准；历史 phase 完成明细在每日 `YYYY-MM-DD.md`，不在本文件重复。

## 已知缺口 SSOT
- 目标范围外、不值得单建 roadmap 节点的缺口，集中登记 `docs/plans/KNOWN-GAPS.md`（活文档，无日期前缀；双向指针：代码锚点 `// registered in docs/plans/KNOWN-GAPS.md (Gn)` ↔ 文档"现载体"列）。`FUTURE(tag)` 注释瘦身为一行指路牌；`UNMIGRATED_TS_*` 常量+锚点测试原样保留（CI 防漂移）。

## 工程铁律
- **TS 子进程调 Rust 命令**：`src/cli.ts` 不代理 Rust 子命令。TS 调 Rust CLI 用 `src/core/zbrain-bin.ts` 的 `resolveZbrainBin()`（`$ZBRAIN_BIN`→`target/debug/zbrain[.exe]`→`target/release`→PATH）。改了 Rust 子命令后必 `cargo build -p zbrain-cli` 重建主二进制，否则测试 exit 2。
- **提交完整性**：lib.rs 加 `pub mod X;` / Cargo.toml 加依赖 / 删 TS 文件时，必确认新增被引用文件本身也已 `git add`；commit 前 `git status` 确认无 untracked 遗漏，`git cat-file -e HEAD:<path>` 验证文件真在 HEAD。

## 迁移进度（摘要）
- 迁移全 12 Part。主线已完成：Sources/Capture、Facts/Takes/Timeline/Salience/Graph、Search/Retrieval 生产后端复活、minions、autopilot、Part7 Phase9、Part9 Phase11（残留 TS 终局）、Part10 Phase12 Schema-Pack 路线图。
- **当前最前沿**：Part11 calibration 簇已收口并 push（e7d7ffe/0e93162）。Part12 cycle 大迁移已启动：1-1 facts-extraction 簇 in_progress（6 sub-node + 7 grill 决策落盘），1-1-1 extract-facts 与 1-1-2 extract-atoms 均已落地并 push（eca3650/34a931b feat + 1e991af/e747b70 chore）；下一步 1-1-3 extract-takes。**迁移范式**：cycle phase = `execute_phase` 真实 match 臂 + `autopilot/phases/<name>.rs` 模块函数（Orphans/Purge 先例）；改一个 phase 为真实臂后 `run_cycle_empty_brain` 的 skipped 断言要 -1 并加该 phase 状态断言。libsql 加 trait 方法：inherent `_impl` + 既有 impl 块内委托（开第二个 `impl BrainEngine` 块必 E0119）。

## 其他
- Admin 路由差异：Rust admin API 在 `/*`（如 `/register-client`），TS 在 `/admin/api/*`；路线图 Q6 决策"保持 /admin/api/*"，待对齐。
- bun 已可用（`~/.bun/bin/bun` v1.3.14）：`bun test` 与 `bash scripts/typecheck-baseline.sh` 本机可跑。
- skillpack 测试仅 `--all-features` 下编译；`std::tempfile` 等预存 bug 已于 2026-07-27 修复（见当日日志）。
