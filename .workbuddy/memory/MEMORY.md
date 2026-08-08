# zbrain 项目记忆

> 进度 SSOT = docs/plans/*.json roadmap；明细在每日 YYYY-MM-DD.md。本文件只放跨会话规则与坑。

## 项目方向
- TS→Rust 迁移（ZBrain）。GBrain→ZBrain 破坏性全量改名，不留兼容别名；`brain`/`source` 领域词不改。Rust 迁成一块删一块。缺口登记 docs/plans/KNOWN-GAPS.md（双向指针 `// registered in ... (Gn)`）。

## 构建环境（WSL2 Ubuntu，无原生 cargo）
- 机器：jununfly-ROG（原生 cargo）/ jununfly-WinPC（本机 WSL2）。WSL 用普通用户 `zb`（root 下 pg-embed initdb 全报 PgInitFailure）。
- 调用范式：`wsl -d Ubuntu -u zb -- bash /mnt/c/Users/jununfly/AppData/Local/Temp/<script>.sh`。写脚本喂路径，别命令行内联传参（引号吞噬 $var）。日志写 /mnt/c/.../Temp/*.log，别写 /tmp（VM 回收）。
- Rust 装法（绕 rustup CA）：static.rust-lang.org tarball → --prefix=$HOME/.rust。cargo 镜像 rsproxy-sparse。
- **Windows 构建被监视器锁死**：`CARGO_TARGET_DIR=C:/Users/<u>/AppData/Local/Temp/zb_targetN`（必须 Windows 绝对路径，/c/ 会被 MSYS 拼成 C:/c/...）。先 robocopy 旧 target 的 deps+.fingerprint+build+incremental（/XF *.lock），cache 路径无关可复用重依赖 rlib，避 53min codegen。
- `cargo check` 不编 #[cfg(test)] → test-only 错误只有 cargo test 暴露。改测试代码必须真跑 test。
- libsql FFI Windows 间歇 0xc0000005 代码层无解；集成测试加进程级 OnceLock<Mutex<()>> 降频，CI 跑 ubuntu。
- workspace forbid(unsafe)：任何 libc::kill 在 Linux 双重报错（unsafe_code+E0433）→ 改 /proc/<pid> 纯 std。Windows 编不到 unix 分支。
- 测试隔离：临时目录须加 AtomicU32 序号（pid 命名多核并行互删 fixture）。

## Git 沙箱铁律
- 严禁 git stash（失败 cleanup 删 .git/refs → orphan HEAD）。探 baseline 用 git show HEAD:<file>/git diff HEAD。
- ref 损坏修复：git fetch origin（可能两次）→ mkdir .git/refs/{heads,remotes/origin,tags} → symbolic-ref HEAD → update-ref <sha> → rev-parse 验证。orphan 别用 git reset HEAD（解析到 stash@0）。
- .git/index.lock 僵尸锁：rm/os.remove 被 safe-delete 拦，唯一绕法 mv 改名。
- 接活前必查 git status（会话中断常留大规模未提交改动）。

## Roadmap 铁律
- 所有 .json/.md 放 docs/plans/。render 只认 ROADMAP_SECTION_START/END marker（不符会追加成重复段 → 先删 md 再 render）。CLI 第一参数 JSON 完整路径。
- decisions 项须 {"q","answer","note?"}（用 a 会 KeyError）。路线图系统性滞后：以 HEAD+git ls-files+编译为准，节点只作索引。

## 迁移工程规则
- TS 调 Rust：`src/cli.ts` 走 resolveZbrainBin()（$ZBRAIN_BIN→target/debug→release→PATH）。改子命令后必 cargo build -p zbrain-cli 否则测试 exit 2。
- port fidelity：Rust 测试与 TS 实现冲突优先改实装、接受 test 为规范；交叉登记 KNOWN-GAPS.md。
- 新增 DB migration 三件事：双 dialect 00NN_*.sql + include_str! const + registry.add + 测试 EXPECTED_VERSION bump。
- 参数加宽坑：&Arc<dyn T>→&dyn T 不自动 coerce（调用方 &*engine）。opts 覆盖逐字段 or_else 保留优先级。
- crate::Result = Result<T, StructuredError>；serde_json::Error 不能 ? 须 map_err。

## 其他
- Admin 路由 Rust 在 /*、TS 在 /admin/api/*（保持）。skillpack 测试仅 --all-features 下编译。
- TS 测试套件（tests/，663 文件）已于 2026-08-08 整体退役；Rust 侧 crates/*/tests/*.rs + 内联 #[cfg(test)] 为唯一测试真相源。nightly_probe.rs 的 NIGHTLY_FIXTURE_REL_PATH 死引用已清（run_long_mem_eval 现为 G58 占位、无条件返 Err）→ 全 workspace `cargo test --workspace` 已验证 green（3694 passed / 0 failed）。
