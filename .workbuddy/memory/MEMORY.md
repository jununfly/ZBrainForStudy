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
- **libsql FFI flake（CI 层已处理）**：该崩溃是 **Windows 原生 libsql/SQLite FFI 的间歇性崩溃**（exit 0xc0000005），代码层无法根除（实测 body-mutex / current_thread / `--test-threads=1` / 泄漏 temp 文件均仍崩）。各 libsql 集成测试已加进程级 `OnceLock<Mutex<()>>` 锁作降频缓解，但不可承诺稳定。`cargo test` 此前完全没进 CI；已新增 `.github/workflows/rust-tests.yml`：在 `ubuntu-latest`(Linux，不触发该崩溃) + 外层 max-3 重试守卫，动态收集引用 `LibsqlEngine` 的 52 个测试 target（+ `--lib`）运行，有意排除 pg-embed postgres 测试。改动未提交（等指令）。
- **WSL 装 Rust 避坑（2026-07-28 实踩）**：WSL(Ubuntu-24.04) 里 `rustup-init`/`rustup` 用**内置 rustls+webpki-roots 信任链**，缺 `GlobalSign Atlas R3 DV TLS CA 2025 Q3` 中间 CA → 下载 `static.rust-lang.org` 工具链组件时报 `invalid peer certificate: UnknownIssuer` 而 `rolling back` 退出（curl 走系统 CA 库、含 GlobalSign 根，所以 curl 能下、`openssl s_client` 也看到真 GlobalSign 证书，非 MITM）。**修法**：放弃 rustup，改用 `curl -fSL` 直接拉组件 tarball（`rust-std`/`rustc`/`cargo`-`1.97.1`-`x86_64-unknown-linux-gnu.tar.xz`，基址 `https://static.rust-lang.org/dist/2026-07-16/`）→ 各自 `./install.sh --prefix=/root/.rust --disable-ldconfig` → `PATH=/root/.rust/bin:$PATH`。直连 `static.rust-lang.org` 稳定 ~100KB/s（慢但稳）；国内 `rsproxy.cn` 镜像证书是 DigiCert RapidSSL（在 webpki-roots 里、rustup 本可信任），但其 dist 路径返回 307 且探测 0 B/s、走不通，**别依赖镜像**。仓库无 `rust-toolchain.toml`，手动装不影响。`set -e` 会让 install.sh 非零退出直接挂，脚本用 `set -uo pipefail`（不 `-e`）+ 每步显式检查。临时验证脚本 `.workbuddy/_tmp_wsl_run.sh`、日志 `.workbuddy/wsl_libsql.log`（Windows 挂载盘，WSL 重启不丢）。
- **WSL 跑 cargo 测试避坑（2026-07-29 实踩）**：Rust 工具链手动装好后，`cargo` 从 `crates.io` 直连下载依赖会撞**房东网络慢到超时**（`transfer too slow: 0 bytes in 30s` 反复 fail，`/root/.cargo/registry/cache` 0 crate），Phase 3/4 一直空转重试。但 rustup 证书坑里说"rsproxy dist 路径走不通"是**错的**——那是 rust 组件 dist 路径；**cargo 的 sparse registry 镜像 `https://rsproxy.cn/index/` 完全可用**（证书 DigiCert RapidSSL、cargo 信任，index HTTP 200，`cargo fetch` 115s 下 477 crate/428MB）。**修法**：写 `/root/.cargo/config.toml` 配 `source.crates-io.replace-with = "rsproxy-sparse"` + `[source.rsproxy-sparse] registry = "sparse+https://rsproxy.cn/index/"` + `[net] git-fetch-with-cli=true, retry=10`（国内 CDN 快，绕开 crates.io 慢网络）。另：手动装的 `cargo` 不会自动找同目录 `rustc`，执行须把 `/root/.rust/bin` 放进 **固定 WSL PATH**（别用 `$PATH`，Windows 侧 shell 会把 `$PATH` 展开成带括号的 Windows PATH 导致 WSL bash 语法错误）；验证脚本 `cargo test ... | tail -N` 会**截断编译错误**（warning 把真实 error 挤出 tail 窗口），务必**完整输出**或只 grep `^error` / `test result`。
- **zbrain-core 在 Linux 编译失败（2026-07-29 发现，已修未提交）**：`schema_pack/pack_lock.rs` 的 `default_is_pid_alive` unix 分支用 `unsafe { libc::kill(pid,0) }` 查进程存活，但 `libc` 只在 `zbrain-worker` 声明、**`zbrain-core` 没声明**，且 `zbrain-core/Cargo.toml` 有 `unsafe_code = "forbid"` → 报 `E0433` + `unsafe block forbidden`。Windows 上该分支被 `#[cfg(not(unix))]` 剔除从不编译，故**只在 Linux 暴露**（CI `ubuntu-latest` 同理会红）。**修法**（用户选 `/proc` 方案）：unix 分支改为 `std::path::Path::new(&format!("/proc/{}", pid)).exists()`（纯 std、无 libc、无 unsafe，契合 forbid 策略）。此 bug 说明 zbrain-core 之前**从没在 Linux 编译过**（只 Windows 开发），Linux 验证必须先过这一关。host 侧 `pack_lock.rs` 已改（working tree，未提交，等指令）。
- **新增 DB migration 必做三件事（2026-07-29 实踩）**：Rust migration 用 `include_str!` **编译期**嵌入 SQL + 运行时 `MigrationRegistry`（`libsql.rs` 的 `LIBQL_MIGRATIONS` / `postgres.rs` 的 `POSTGRES_MIGRATIONS`，双 dialect 分别读 `migrations-sqlite/` 与 `migrations/`）。**仅新增 `00NN_*.sql` 文件不会自动应用**——`init_schema` 停在旧版本（migration 测试 `left:旧版本 == right:新版本` 即此因）。完整步骤：① 在 `migrations/` 与 `migrations-sqlite/` 各放双 dialect `.sql`（同名同号）；② 在 `libsql.rs` 加 `const MIGRATION_00NN: &str = include_str!("../migrations-sqlite/00NN_*.sql");`，在 `postgres.rs` 加对应 `../migrations/00NN_*.sql`；③ 在两 registry 各加 `registry.add(Box::new(LibsqlMigration/PostgresMigration { version: NN, name: "...", sql: MIGRATION_00NN }))`；④ 若测试有 `EXPECTED_VERSION` 常量（libsql_engine_migrations.rs），同步 bump。改完须 `cargo test --test libsql_engine_migrations`（经 `OnceLock<Mutex>` 锁避免 Windows FFI 崩溃）确认版本号到位 + idempotent 二次跑 0 应用。

## 迁移进度（摘要）
- 迁移全 12 Part。主线已完成：Sources/Capture、Facts/Takes/Timeline/Salience/Graph、Search/Retrieval 生产后端复活、minions、autopilot、Part7 Phase9、Part9 Phase11（残留 TS 终局）、Part10 Phase12 Schema-Pack 路线图。
- **当前最前沿**：Part12 cycle 大迁移。**1-1 facts-extraction 簇已全 6 叶子 push 完成**（1-1-6 conversation-facts-backfill 已 push 至 dff29e4..4256e65）。**1-2 emotional-calibration 簇本会话迁完（working tree，待 commit+push）**：1-2-1 compute_emotional_weight 纯函数 + 1-2-2 引擎方法 `batch_load_emotional_inputs`/`set_emotional_weight_batch`(trait+InMemory+libsql+postgres) + 1-2-3 recompute_emotional_weight phase+cycle 真实臂 + 1-2-4 `run_calibration_profile` 接 cycle 真实臂；`get_config` 走 opts override（对齐 1-1-6）。autopilot lib 全量回归 239 测试全绿，skipped 计数保持 16（calibration 原已归 catch-all Skipped，接真实臂后仍 Skipped，故计数不变）。下一簇 **1-3 synthesis**（synthesize/synthesize-concepts/patterns/schema-suggest）。**迁移范式**：cycle phase = `execute_phase` 真实 match 臂 + `autopilot/phases/<name>.rs` 模块函数（Orphans/Purge 先例）；改一个 phase 为真实臂后 `run_cycle_empty_brain` 的 skipped 断言要 -1 并加该 phase 状态断言（但若该 phase 原已归 catch-all Skipped、接真实臂后仍 Skipped，则 skipped 计数不变）。libsql 加 trait 方法：inherent `_impl` + 既有 impl 块内委托（开第二个 `impl BrainEngine` 块必 E0119）。

## 其他
- Admin 路由差异：Rust admin API 在 `/*`（如 `/register-client`），TS 在 `/admin/api/*`；路线图 Q6 决策"保持 /admin/api/*"，待对齐。
- bun 已可用（`~/.bun/bin/bun` v1.3.14）：`bun test` 与 `bash scripts/typecheck-baseline.sh` 本机可跑。
- skillpack 测试仅 `--all-features` 下编译；`std::tempfile` 等预存 bug 已于 2026-07-27 修复（见当日日志）。
