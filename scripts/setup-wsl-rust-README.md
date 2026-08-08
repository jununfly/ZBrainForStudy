# WSL + Rust 一键安装指南

为 ZBrain 项目本地开发验证用的 WSL2 + Rust toolchain 自动化脚本。

## 为什么需要 WSL

项目核心代码（`crates/zbrain-core` 等）是 Rust，本机 Windows 无 cargo。
CI 跑 `ubuntu-latest`，但本地写完代码后**等 CI 太慢**。WSL Ubuntu + Rust
能在本机直接 `cargo test -p zbrain-core --lib` 跑 ~2337 测试，秒级反馈。

## 沙箱限制

WorkBuddy 沙箱禁用了 `wsl / dism / wmic / sc / reg / schtasks` 等系统级工具。
所以**安装必须在你手动开的 PowerShell 终端里跑**（Agent 无法代跑）。

## 步骤

### A. 一次性安装（仅首次）

> 需要**管理员权限**。在开始菜单搜 PowerShell → **以管理员身份运行**。

```powershell
cd D:\workspace\github\jununfly\ZBrain
powershell -ExecutionPolicy Bypass -File .\scripts\setup-wsl-rust.ps1
```

脚本会做：

1. 启用 WSL 功能（`Microsoft-Windows-Subsystem-Linux`）
2. 启用 WSL2 后端（`VirtualMachinePlatform`）
3. 设默认 WSL 版本为 2
4. 从 Microsoft Store 下载并安装 Ubuntu（~500MB）
5. **第一次跑会进入 Ubuntu** —— 创建 UNIX 用户（用户名密码**不是** Windows 密码）
6. 创建用户后回到 PowerShell，**再跑一次** 脚本（这次会自动跳过 1-5，直接进 step 6）

### B. 验证

```powershell
wsl -d Ubuntu -- cargo --version
# 期望: cargo 1.85.0 (或更新)

wsl -d Ubuntu -- rustc --version
# 期望: rustc 1.85.0 (...)
```

### C. 跑 ZBrain 测试

```powershell
wsl -d Ubuntu -- bash -c "cd /mnt/d/workspace/github/jununfly/ZBrain && cargo test -p zbrain-core --lib 2>&1 | tail -10"
```

**期望**：~2337 passed / 0 failed（handoff §1 基线）。

**注意**：
- 第一次跑会从 rsproxy.cn 拉所有 crate（~3GB），慢
- `libsql_ffi` 在 Windows native 偶发崩溃（`exit 0xc0000005`），但 WSL Ubuntu 用 libsql 不会有这问题
- 如果报 `error: linker not found`，回到 step 1 确认 `build-essential` 装上了

## 跳过部分步骤

脚本支持 3 个 flag：

```powershell
# 跳过 Ubuntu 安装（你已经有别的 distro）
.\scripts\setup-wsl-rust.ps1 -SkipUbuntuInstall

# 跳过 Rust 安装（你已经有 toolchain）
.\scripts\setup-wsl-rust.ps1 -SkipRustInstall

# 用别的 distro 名
.\scripts\setup-wsl-rust.ps1 -UbuntuDistro Ubuntu-22.04
```

## 故障排查

| 症状 | 原因 | 解决 |
|---|---|---|
| `wsl --install` 一直卡下载 | 国内网络 | 手动从 [WSL2 kernel update](https://aka.ms/wsl2kernel) 装 |
| `dism` 报 0x800f0922 | 缺管理员 | 重开管理员 PowerShell |
| `rustup` 下载报 CA 错 | 已知 bug | 脚本已绕过：用 static tarball 直装 |
| `cargo` 拉 crate 慢 | 走 crates.io | 脚本已配 rsproxy.cn sparse 镜像 |
| 测试跑出 `0xc0000005` | Windows libsql FFI flake | 在 WSL Ubuntu 跑，无此问题 |

## 已知的"另一台机器" vs 本机差异

- **另一台**（`jununfly-ROG`）：用 `C:/Users/.../AppData/Local/Temp/zb_target_*` 路径
  + `CARGO_TARGET_DIR=Windows 绝对路径` 做 cargo build（绕开外部监视器锁）
- **本机**（`jununfly-WinPC`）：用 WSL Ubuntu，cargo 直接跑，没这限制
- 通用规约仍适用：构建用未监控的 `target/` 路径
