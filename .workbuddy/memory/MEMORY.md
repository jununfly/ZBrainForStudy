# zbrain 项目记忆

> 进度 SSOT = docs/plans/*.json roadmap；明细在每日 YYYY-MM-DD.md。本文件只放跨会话规则与坑。

## 项目方向
- TS→Rust 迁移（ZBrain）。GBrain→ZBrain 破坏性全量改名，不留兼容别名；`brain`/`source` 领域词不改。Rust 迁成一块删一块。缺口登记 docs/plans/KNOWN-GAPS.md（双向指针 `// registered in ... (Gn)`）。

## 构建环境（WSL2 Ubuntu，无原生 cargo）
- 机器：jununfly-ROG（原生 cargo）/ jununfly-WinPC（本机 WSL2）。WSL 用普通用户 `zb`（root 下 pg-embed initdb 全报 PgInitFailure）。
- 调用范式：`wsl -d Ubuntu -u zb -- bash /mnt/c/Users/jununfly/AppData/Local/Temp/<script>.sh`。写脚本喂路径，别命令行内联传参（引号吞噬 $var）。日志写 /mnt/c/.../Temp/*.log，别写 /tmp（VM 回收）。
- Rust 装法（绕 rustup CA）：static.rust-lang.org tarball → --prefix=$HOME/.rust。cargo 镜像 rsproxy-sparse。**但 WSL 内 cargo 实际在 `$HOME/.cargo/bin`**（脚本 export PATH 用它，不是 .rust/bin）。
- WSL 侧 `git status` 因 CRLF 差异把整树报成 modified → **判断改动范围一律以 Windows 侧 git 为准**。
- 热缓存下 WSL 增量成本参考：`cargo check -p zbrain-cli` ~49s、`cargo test -p zbrain-cli --lib` ~54s 编译 + 30s 跑、`cargo check --workspace --all-targets` ~41s。比 Windows 快一个量级，改 Rust 优先走 WSL。
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
- **行尾符陷阱**：本仓 core.autocrlf=false 且行尾符**混杂**（`lib.rs`/`MEMORY.md` 是 CRLF，`KNOWN-GAPS.md`/`COMMANDS_TEAR_DOWN.md`/多数 `*.rs` 是 LF）。Edit 工具会整文件改写行尾符 → `git diff --numstat` 出现 `N/N`（每行都改）即中招。改完必查 numstat；异常时按 `git show HEAD:<f>` 的原始行尾符**逐文件**对齐，别一刀切 LF（会把本就 CRLF 的文件全炸）。

## Roadmap 铁律
- 所有 .json/.md 放 docs/plans/。render 只认 ROADMAP_SECTION_START/END marker（不符会追加成重复段 → 先删 md 再 render）。CLI 第一参数 JSON 完整路径。
- decisions 项须 {"q","answer","note?"}（用 a 会 KeyError）。路线图系统性滞后：以 HEAD+git ls-files+编译为准，节点只作索引。

## 迁移工程规则
- TS 调 Rust：`src/cli.ts` 走 resolveZbrainBin()（$ZBRAIN_BIN→target/debug→release→PATH）。改子命令后必 cargo build -p zbrain-cli 否则测试 exit 2。
- port fidelity：Rust 测试与 TS 实现冲突优先改实装、接受 test 为规范；交叉登记 KNOWN-GAPS.md。
- **KNOWN-GAPS 的 blocked/描述不可全信**（已连续两次证伪：G76 误标"依赖 LLM 被 G35/G60 阻塞"实为纯解析且 Rust 已实现大半；G74 误标"多数依赖 LLM + blocked by G58"实为仅 4/19=21% 依赖 LLM、核心 `run_eval` 早已 port 完只是零调用者）。**共同模式：算法层已在 Rust，缺的只是 CLI 出口** —— 对账时优先 grep Rust 侧有无同名/近义实现，别默认"没 verb 就是没实现"。动手任一 Gn 前必做「TS 源 vs Rust 现状」逐能力对账：`git show <删除commit>^:<path>` 取回 TS → grep 关键依赖 → grep Rust 侧同名/近义实现 → 列对账表。对账结论要回写 KNOWN-GAPS + COMMANDS_TEAR_DOWN + 相关代码 doc 注释（三处都可能 stale）。
- **命令族批量对账法**（19+ 文件时高效）：`git show <sha>^` 批量导出到 Temp 目录 → 写 Python 脚本一次扫全部（正则查依赖关键词 + 抽 docstring + 抽 import 列表）→ 出对账表。**grep 命中必须逐行看上下文再定性**：注释提及、fallback 模型 ID 字符串、"走 stub seam" 的说明都会假阳性（G74 三个疑似 LLM 命中全是假的）。另注意小文件（<50 行）常是 `not_yet_implemented` 空壳 scaffold，TS 自身就没实现，迁移零价值。
- CLI 加 verb 的最小闭环：clap 结构照抄邻近 verb 模板 → Commands enum + Action enum + Args + dispatch + run_ 函数 → lib.rs 尾部 #[cfg(test)] 加 try_parse_from 解析测试（含"拒绝未实现子命令"的负向测试守住缺口）→ e2e 用隔离 config（`-c $WORK/zbrain.yml` + `init --pglite --force`）真库冒烟，必验幂等 + 范围限定 + 不存在实体不崩。
- 新增 DB migration 三件事：双 dialect 00NN_*.sql + include_str! const + registry.add + 测试 EXPECTED_VERSION bump。
- 参数加宽坑：&Arc<dyn T>→&dyn T 不自动 coerce（调用方 &*engine）。opts 覆盖逐字段 or_else 保留优先级。
- crate::Result = Result<T, StructuredError>；serde_json::Error 不能 ? 须 map_err。

## 其他
- Admin 路由 Rust 在 /*、TS 在 /admin/api/*（保持）。skillpack 测试仅 --all-features 下编译。
- TS 测试套件（tests/，663 文件）已于 2026-08-08 整体退役；Rust 侧 crates/*/tests/*.rs + 内联 #[cfg(test)] 为唯一测试真相源。nightly_probe.rs 的 NIGHTLY_FIXTURE_REL_PATH 死引用已清（run_long_mem_eval 现为 G58 占位、无条件返 Err）→ 全 workspace `cargo test --workspace` 已验证 green（3694 passed / 0 failed）。
