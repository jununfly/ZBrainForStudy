# zbrain 项目记忆

## 项目方向
- TS → Rust 迁移（"Rust 重写线 ZBrain"），整仓统一迁到 ZBrain，GBrain 品牌改名 ZBrain（无线上用户，可破坏性改名、不留兼容别名）。`brain`/`source` 作领域词不改。
- 下个 PRD：`docs/prd/complete-ts-to-rust.md`。原则：TS 先不动，Rust 迁成一块删一块；不适合删的到时讨论记录决策。

## 命名 / 品牌迁移
- 连文件名一起改：`gbrain.yml→zbrain.yml`、`docs/GBRAIN_*.md→docs/ZBRAIN_*.md`、package/bin/env/dotfile/path/docs/test 引用。分层：配置/包名/bin → env/dotfile/path → docs → 测试脚本 → 验证断链。

## Roadmap 铁律
- 所有 `.json`/`.md` 放 `docs/plans/`（JSON `zbrain-ts-to-rust-partN-*.json`，md `ZBRAIN_TS_TO_RUST_PARTN_*.md`）。每个 part JSON `link` 各自独立 md，禁共用。
- **marker**：`roadmap_cli.py render` 只读/写 `<!-- ROADMAP_SECTION_START/END -->`（无 ⚠️ 变体），即 JSON 驱动的活段。顶部手写段工具不维护、会 stale。
- **单一干净段**：在已有 md 上 render 若 marker 不符会**追加**成重复段 → 要单一段须**先删 md 再 render**。Part12 cycle md 顶部孤儿段已有意删除，只留 JSON 驱动段；勿手动重建/同步。CLI：`link/render/decide/add/tree` 第一参数是 JSON 完整路径；render 从项目根 cwd 跑；读根级 `md_file` key。
- **decisions 键格式坑**：`nodes[*].decisions[*]` 须 `{"q","answer","note?"}`；用 `a` 键会让 `_build_focus_section` 的 `d['answer']` 报 `KeyError`。只焦点节点(in_progress)的 decisions 被读。
- **lock 僵尸坑**：`roadmap_file_lock` acquire 超时时不清理 `.lock` 目录（`__enter__` 抛异常、`__exit__` 不执行）。修法：同命令内先 `python -c "import shutil,os; shutil.rmtree(r'<json>.lock', ignore_errors=True)"` 强删再 render。
- **python 路径**：bash 里传原生 exe 用 `C:/Users/...` 冒号形式；`/c/Users/...` 会被 MSYS 把参数转坏成 `C:\c\...`（命令名本身可用 `/c/`）。
- 代码注释禁引 roadmap 编号（JSON 是临时文件，清理后成死链）；必要信息自解释写注释。docs/plans/ 下 canonical 文档可引。
- 拆分约定：与当前节点语义偏差且有跟进价值 → 拆 sub-node，不吸收进当前 plan。
- 进度 SSOT = roadmap JSON；历史明细在每日 `YYYY-MM-DD.md`，本文件不重复。

## 已知缺口 SSOT
- 范围外缺口集中登记 `docs/plans/KNOWN-GAPS.md`（活文档；双向指针 `// registered in docs/plans/KNOWN-GAPS.md (Gn)` ↔ 文档"现载体"）。`FUTURE(tag)` 注释瘦身一行；`UNMIGRATED_TS_*` 常量+锚点测试原样保留（CI 防漂移）。

## 工程铁律
- **TS 调 Rust**：`src/cli.ts` 不代理 Rust 子命令；用 `resolveZbrainBin()`（`$ZBRAIN_BIN`→`target/debug/zbrain[.exe]`→`target/release`→PATH）。改 Rust 子命令后必 `cargo build -p zbrain-cli` 重建，否则测试 exit 2。
- **提交完整性**：新增 `pub mod X;`/依赖/删 TS 文件时，确认被引用文件已 `git add`；commit 前 `git status` + `git cat-file -e HEAD:<path>` 验证真在 HEAD。
- **libsql FFI flake**：Windows 原生 libsql/SQLite FFI 间歇崩溃（exit 0xc0000005），代码层无法根除。libsql 集成测试加进程级 `OnceLock<Mutex<()>>` 降频。CI 跑 `ubuntu-latest` 避此崩溃（见 `.github/workflows/rust-tests.yml`）。
- **WSL 装 Rust**：rustup 内置信任链缺中间 CA 报 `UnknownIssuer` → 放弃 rustup，用 `curl -fSL` 直拉 `static.rust-lang.org/dist/<date>/` 的 `rust-std`/`rustc`/`cargo` tarball → `--prefix=/root/.rust`；PATH 固定 `/root/.rust/bin`。别依赖 rsproxy 的 dist 镜像。
- **WSL cargo**：`crates.io` 直连超时 → 配 `/root/.cargo/config.toml` 用 rsproxy sparse 镜像 `sparse+https://rsproxy.cn/index/`（dist 镜像走不通但 registry 镜像可用）。验证脚本勿 `| tail` 截断错误，grep `^error`/`test result`。
- **zbrain-core Linux 编译**：`pack_lock.rs` unix 分支原用 `libc::kill`（zbrain-core 无 libc + `unsafe_code=forbid`）→ Linux 报 E0433。改 `/proc/<pid>` 存在性检查（纯 std）。Linux 验证须先过此关。
- **新增 DB migration 三件事**：① `migrations/`+`migrations-sqlite/` 各放双 dialect `00NN_*.sql`；② `libsql.rs`/`postgres.rs` 加 `include_str!` const + `registry.add(version:NN)`；③ 测试 `EXPECTED_VERSION` 同步 bump。仅加 .sql 文件不会自动应用。
- **Windows 验证 zbrain-core 测试**：复用默认 `target/` 的 libsql-ffi/aws-lc-sys 编译缓存；**勿用全新 `CARGO_TARGET_DIR`（如 target_alt2）**——全新目录会触发 libsql-ffi（Permission denied）/aws-lc-sys（缺 NASM、`lib.exe` 1114）完整重编而 EXIT=101。孤儿 cargo 锁进程退出后默认 `target/` 锁即释放，不必另开 target dir。测试用 `InMemoryEngine` 时不碰 libsql 运行时（避开 Windows FFI 崩溃），纯函数测试 Windows 直跑即可。

## 迁移进度（摘要）
- Part12 cycle 大迁移是当前最前沿。**1-3 synthesis 簇整体收口**（2026-07-30：1-3-4-6 完整复刻引擎 config）；**1-4 anomaly-transcript 簇完成**（2026-07-30：验证 Rust 实现等价 + 合并重复 dream-guard + 补 brain_find_anomalies minion tool）。下一 pending：1-5 auto-think、1-6 orchestration 主循环（含 TS 引擎 pglite/postgres 迁移，anomaly/transcript-discovery 的 TS 删除受此阻塞）。详细进度见 roadmap JSON + 每日 `YYYY-MM-DD.md`。
- **opts 覆盖铁律**：phase 接真实引擎 config 时，ad-hoc opts 覆盖必须逐字段保留优先级（`opts.X.or_else(|| stored)`），丢一个就回归既有测试（1-3-4-6 chunked 测试教训）。
- 迁移范式：cycle phase = `execute_phase` 真实 match 臂 + `autopilot/phases/<name>.rs` 模块。接真实臂后 `run_cycle_empty_brain` skipped 断言 -1（若该 phase 原已 catch-all Skipped 且接臂后仍 Skipped，则计数不变）。libsql 加 trait 方法：inherent `_impl` + 既有 impl 块内委托（开第二个 `impl` 块必 E0119）。

## 其他
- Admin 路由：Rust 在 `/*`（如 `/register-client`），TS 在 `/admin/api/*`；路线图 Q6 决策"保持 /admin/api/*"，待对齐。
- bun 可用（`~/.bun/bin/bun` v1.3.14）：`bun test` 与 `bash scripts/typecheck-baseline.sh` 本机可跑。
- skillpack 测试仅 `--all-features` 下编译；`std::tempfile` 等预存 bug 已修（2026-07-27）。
