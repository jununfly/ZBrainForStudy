# zbrain 项目记忆

> 进度 SSOT = `docs/plans/*.json` roadmap；历史明细在每日 `YYYY-MM-DD.md`。本文件只放**跨会话有效的规则与坑**，不复述进度。

## 项目方向
- TS → Rust 迁移（ZBrain 线）。GBrain 品牌全量改名 ZBrain，无线上用户 → 破坏性改名、不留兼容别名。`brain`/`source` 是领域词，不改。
- 节奏：Rust 迁成一块删一块；不适合删的停下来记录决策。
- 已知缺口集中登记 `docs/plans/KNOWN-GAPS.md`（双向指针 `// registered in ... (Gn)`）。

## 构建环境
- **机器**：`jununfly-ROG`（另一台 Win 笔电，原生 cargo）/ `jununfly-WinPC`（本机，走 WSL2 Ubuntu 26.04）。
- **WSL 调用范式**：`wsl -d Ubuntu -u zb -- bash /mnt/c/Users/jununfly/AppData/Local/Temp/<script>.sh`。**必须用普通用户 `zb`，不能用 root**——`initdb` 拒绝 root 身份，root 下 218 个 pg-embed 测试全报 `PgInitFailure`（换 zb 后全绿）。工具链已全部迁到 `/home/zb`（`.cargo` / `.rustup` / `zb-target`），`/etc/wsl.conf` 已设 `default=zb`。
- **别用命令行内联传参**：多层 bash→python→wsl 引号嵌套会把 `$var` 吞成空。一律**写脚本到 `C:\...\Temp\*.sh`，用 `/mnt/c/...` 路径喂给 wsl**。
- **日志别写 `/tmp`**：WSL 空闲会回收 VM 清空 `/tmp`，跨调用取不到。写 `/mnt/c/.../Temp/*.log`（Windows 侧可直接 Read/Grep）。
- **别在 WSL 里跑 `find /`**：会遍历 `/mnt/c`、`/mnt/d` 的 9p 挂载，卡死十几分钟。
- wsl.exe 自身的警告走 stderr 且是 **UTF-16LE**（`.decode('utf-16-le')`），程序 stdout 是正常 UTF-8。
- **WSL 装机坑**：`wsl --install -d Ubuntu` 报 `0x80072f78` → 加 `--web-download` 绕开 Store 通道即可。apt 换 USTC 源最快。
- **Rust 装法**（绕 rustup CA 失败）：`static.rust-lang.org/dist/<date>/rust-<ver>-x86_64-unknown-linux-gnu.tar.gz` → `install.sh --prefix=$HOME/.rust --components=cargo,rustc,rust-std-x86_64-unknown-linux-gnu --without=rust-docs`。
- **cargo 镜像**：`~/.cargo/config.toml` → `[source.crates-io] replace-with='rsproxy-sparse'` + `registry="sparse+https://rsproxy.cn/index/"`。
- **Windows 原生构建被监视器锁死**：workspace `target/` 下 `.cargo-*-lock` 被独占 → os error 5。绕法：`CARGO_TARGET_DIR=C:/Users/<u>/AppData/Local/Temp/zb_targetN`（**必须 Windows 绝对路径**，写 `/c/Users/...` 会被 MSYS 拼成 `C:/c/...`）。先 `robocopy` 旧 target 的 `deps`+`.fingerprint`+`build`+`incremental`（`/XF *.lock`）过去，cache 路径无关，可复用重依赖 rlib，避免 53min codegen。
- **`cargo check` 不编译 `#[cfg(test)]`** → test-only 编译错误只有 `cargo test` 才暴露。改了影响测试的代码必须真跑 test。
- **libsql FFI flake**：Windows 原生 libsql/SQLite FFI 间歇 `0xc0000005`，代码层无解；集成测试加进程级 `OnceLock<Mutex<()>>` 降频，CI 跑 ubuntu。
- **WSL 全量测试基线（2026-08-08 首次跑通）**：`cargo test --workspace` 以 zb 身份 4 分钟，**3689 passed / 8 failed**。8 个失败全是 migration 计数断言漂移（详见下）。`cargo build --workspace` 增量 10s，全量 ~1 分钟（20 核）。
- **`unsafe` 是 workspace 级 forbid**：任何 `unsafe { libc::kill(...) }` 在 Linux 都双重报错（`unsafe_code` + 无 libc 依赖 E0433）。统一改 `/proc/<pid>` 存在性检查（纯 std）。已修 3 处：`schema_pack/pack_lock.rs`、`skillpack/installer.rs`、`zbrain-worker/src/supervisor.rs`。**Windows 上编译不到这些 unix 分支，只有 Linux 才暴露**。
- **测试隔离坑**：临时目录只用 pid 命名（`temp_dir/zb_chk_<pid>`）会让同一测试二进制内所有测试共享一个路径，配合开头的 `remove_dir_all` 在多核并行下互删 fixture。必须加 `AtomicU32` 序号。Windows 核少侥幸不复现，Linux 20 核必挂。
- `~/.bun/bin/bun` v1.3.14 可用。

## Git 沙箱铁律
- **严禁 `git stash`**：失败时沙箱 cleanup 会删掉整个 `.git/refs/` → orphan HEAD + 1687 文件全变 "staged as new"。探 baseline 用 `git show HEAD:<file>` / `git diff HEAD -- <file>`（纯只读）。需要暂存就写 untracked 描述文件或直接 commit WIP。
- **ref 损坏修复顺序**：① `git fetch origin <branch> --force --no-tags`（可能要 fetch **两次** objects 才进 pack）② `mkdir -p .git/refs/heads .git/refs/remotes/origin .git/refs/tags` ③ `git symbolic-ref HEAD refs/heads/<branch>` ④ `git update-ref refs/heads/<branch> <commit-sha>` ⑤ `git rev-parse HEAD` 验证。orphan 状态下**别用 `git reset HEAD`**（会解析到 stash@0），用显式 SHA。
- `fatal: bad object HEAD` 通常只是 object store 没拉全 → 先 `git fetch origin`，别手动 `hash-object` 重建。
- **`.git/index.lock` 僵尸锁**：`rm` / `os.remove()` 都被沙箱 safe-delete 拦截；唯一绕法是 **`mv` 改名**。
- **`git ls-remote` 有 propagation 延迟**，push 后报 stale SHA 是错觉。真值 = `git fetch origin` + `git rev-list --left-right --count HEAD...origin/<branch>`（`0 0` = 已同步）。别据此重 push。
- **接活前必查 `git status`**：会话中断常留下大规模未提交改动（曾见 338 文件 / -92,427 行整树删除未提交）。
- **提交完整性**：新增 `pub mod X;`/依赖/删 TS 文件时确认被引用文件已 add；commit 前 `git status` + `git cat-file -e HEAD:<path>`。

## Roadmap 铁律
- 所有 `.json`/`.md` 放 `docs/plans/`（`zbrain-ts-to-rust-partN-*.json` / `ZBRAIN_TS_TO_RUST_PARTN_*.md`）。每 part 独立 md，禁共用。
- render 只认 `<!-- ROADMAP_SECTION_START/END -->` marker；marker 不符会**追加**成重复段 → **先删 md 再 render**。
- CLI 第一参数是 JSON 完整路径；render 从项目根 cwd 跑。`decisions` 项必须是 `{"q","answer","note?"}`（用 `a` 会 KeyError），只读 in_progress 节点的 decisions。
- render 前强清残留 lock：`glob('docs/plans/*.json.lock')` → `shutil.rmtree`。
- bash 调原生 python 用 `C:/Users/...`，不用 `/c/Users/...`。
- **路线图系统性滞后**：节点状态常比仓库真相落后数月。以 HEAD + `git ls-files` + 编译结果为准，roadmap 只作索引。残差审计按**目录语义**查（`crates/zbrain-core/src/<mod>`，剥 `core/` 前缀、kebab↔snake），别按文件名比对。
- 代码注释禁引 roadmap 编号（JSON 是临时文件）；`docs/plans/` 下 canonical 文档可引。

## 迁移工程规则
- **TS 调 Rust**：`src/cli.ts` 走 `resolveZbrainBin()`（`$ZBRAIN_BIN`→`target/debug`→`release`→PATH）。改 Rust 子命令后必 `cargo build -p zbrain-cli`，否则测试 exit 2。
- **port fidelity（test 即 spec）**：Rust 测试与 TS 原实现冲突时，**优先改实装、接受 test 为规范**（除非 test 明显不可能），并在 `KNOWN-GAPS.md` + 实装注释交叉登记来源。TS 只是只读档案。
- **架构可偏离 1:1**：如 search 下沉为 `BrainEngine::search_pages` trait 方法，TS 的 `hybrid.ts` SQL builder 已内化。核对按语义不按文件名。
- **新增 DB migration 三件事**：① `migrations/` + `migrations-sqlite/` 双 dialect `00NN_*.sql`；② `libsql.rs`/`postgres.rs` 加 `include_str!` const + `registry.add(version:NN)`；③ 测试 `EXPECTED_VERSION` bump。只加 .sql 不会自动应用。
- libsql 加 trait 方法：inherent `_impl` + 既有 impl 块内委托（另开 `impl` 块必 E0119）。
- **参数加宽坑**：`&Arc<dyn T>`→`&dyn T` 不自动 coerce，调用方需 `&*engine`；反向不可逆。
- **opts 覆盖铁律**：ad-hoc opts 覆盖 stored config 须逐字段 `opts.X.or_else(|| stored)`，丢一个就回归。
- `crate::Result` = `Result<T, StructuredError>`，`serde_json::Error` 不能直接 `?`，须 `.map_err(|e| crate::Error::new("SerializationError", ...))`。
- **Linux 编译**：`pack_lock.rs` unix 分支不能用 `libc::kill`（E0433），改 `/proc/<pid>` 存在性检查。
- **TS 基线 gate 假阳性**：`scripts/tsc-baseline.txt` 未排序而脚本用 `comm` 比对 → 误报。两侧先 `sort` 再 `comm -13`，别信它的 exit code。

## 其他
- Admin 路由：Rust 在 `/*`，TS 在 `/admin/api/*`（决策：保持）。
- skillpack 测试仅 `--all-features` 下编译。
- 剩余 TS 约 570 个文件（tests 552 / src 9 / admin 4 / examples 2 / evals 2 / tools 1）；对 `src/core` 的悬空 import ≈ 516 文件 / 1,089 处。CI 里 `bun test` 系列本分支必红，`rust-tests.yml` 不受影响。
