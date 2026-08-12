# Cross-OS Agent Development Guide (ZBrain)

This repository is developed across **Windows** (primary, WorkBuddy IDE), **macOS**, and **WSL2/Linux**. Several environment-specific pitfalls have caused real repository corruption and lost work. This document is the canonical **do / don't** list for any agent — Claude Code, Codex, Cursor, OpenClaw, a human via WorkBuddy, or an LLM fetching via URL — that touches the repo from a non-Linux shell.

**Keep this file updated.** When you hit a new OS-specific pitfall, add it here rather than only logging it in chat memory.

---

## 0. Which clone / repo integrity (read this first)

- **DO** verify the repo is healthy before any large git operation:
  - `git fsck --full` must exit `0` with no `missing` objects.
  - `git status` should show no unexpectedly large diffs.
- **History lesson:** the original `zbrain` working copy was corrupted by a missing-tree object (`0e0d0a2d`, a leftover from an earlier `git stash` disaster that orphaned HEAD). It had to be rebuilt **in place** from the healthy `zbrain-clean` clone. Trust `git fsck`, not the folder name. *(Note: the `zbrain-clean` sibling backup clone was **removed on 2026-08-12** — treat this lesson as historical; do not assume that clone still exists.)*
- **DON'T** assume the IDE's default workspace folder is the healthy / active clone. Always confirm HEAD and run `git fsck` first.
- **DO** treat the single active clone (`zbrain`) as the only source of truth. The `zbrain-clean` sibling backup clone was **removed on 2026-08-12** — there is no longer a second clone to recover from. Recovering a corrupted `zbrain` now means a **fresh clone from `origin/rust-rewrite`** (see §2), not copying `.git` from a local sibling.

---

## 1. Git on Windows is a NATIVE binary — path handling

- **DO** `cd` into the repo directory, then run git:
  ```bash
  cd /c/workspace/github/jununfly/ZBrain && git status
  ```
- **DON'T** use `git -C /c/workspace/.../zbrain ...` or `git clone --local /c/workspace/...`. The native Windows git binary does **not** understand MSYS/POSIX paths and fails with `No such file or directory` even when `ls`/`cp` see the path perfectly fine.
- **Exception / mental model:** shell builtins and utilities (`cd`, `cp`, `ls`, `find`, `mv`) **do** translate `/c/...` correctly via MSYS. So use MSYS `cp -a` to copy `.git`, but never `git -C`.

---

## 2. Never `rm -rf` / `mv` the repo root or large trees on Windows

- **DON'T** `rm -rf` or `mv` the whole repo root. The IDE (WorkBuddy) locks the worktree root → `Device or resource busy` (EBUSY).
- **DON'T** rely on `rm -rf` succeeding. The safe-delete layer (genie-trash) intercepts it; on abort it can relocate `.git` and root files into the trash, leaving a rootless, headless worktree.
- **DO** rebuild a corrupted repo **in place without renaming**. Because the `zbrain-clean` sibling clone was removed (2026-08-12), the healthy `.git` source is now a **fresh clone from `origin/rust-rewrite`** — not a local sibling:
  ```bash
  git clone --no-checkout <origin-url> /tmp/zbrain-fresh   # fetches a clean .git
  cp -a /tmp/zbrain-fresh/.git <target>/.git               # MSYS cp creates subdirs, bypasses root lock
  cd <target> && git reset --hard HEAD                     # restore all tracked files to HEAD
  ```
- **DO** back up unique untracked files (e.g. `.tmp_ts_src/`) before any destructive op, and verify with `sha1sum` on both sides.

---

## 3. MSYS / mingw path mangling

- **DON'T** let tooling build absolute `/c/...` paths that get re-parsed by mingw git — it can mangle `/c/workspace/...` into `C:\c\workspace\...`, spawning a phantom sibling directory that holds only a `target_alt` cargo cache.
- **DO** prefer relative paths inside the repo, or `cd` first.
- If you ever see a `C:\c\...` directory: it is a bug residue. Confirm it holds only build cache, then remove it. Never trust its contents as repo state.

---

## 4. Cargo / building on Windows

- **DO** set an explicit **Windows-style** target dir to reuse cached `.rlib` and skip the ~53-min codegen on monitor-locked machines:
  ```bash
  export CARGO_TARGET_DIR=C:/Users/<user>/AppData/Local/Temp/zb_clean_target
  ```
  Note: must be `C:/...` (Windows path). A `/c/...` MSYS path is mis-parsed by cargo.
- **DO** use the managed cargo binary: `C:/Users/<user>/.cargo/bin/cargo.exe`.
- **DO** when the Clash proxy (`127.0.0.1:7890`) returns `BAD_DECRYPT` on cargo network calls:
  ```bash
  unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY
  cargo test --offline -p zbrain-cli --lib    # only if deps are cached
  ```
  A proxy TLS-intercept error can **mask the real compile error** — always read the actual cargo output, not just the proxy message.
- **DON'T** run full `cargo test --workspace` on Windows for heavy libsql crates for fast iteration. Use `cargo check` (compiles `.rmeta` only) instead.

---

## 5. CRLF line endings (critical — easy to silently corrupt)

The repo has **MIXED** line endings: some files are CRLF (`lib.rs`, `engine.rs`, `runner.rs`), some are LF. There is no single rule.

- **DO** match the existing line ending of the file you edit. For new `.rs` files, use CRLF to match the crate.
- **DON'T** run bulk `sed` / `awk` / `echo >` line-ending conversions. Edit tools can rewrite the **entire file** on save, turning a 3-line change into an `N/N`-line diff.
- **DO** before committing, run `git diff --numstat` on **every** changed Rust file. If a small edit shows `N/N` (every line changed), you've hit a whole-file rewrite — fix it:
  ```python
  # restore LF
  open(f, 'wb').write(open(f, 'rb').read().replace(b'\r\n', b'\n'))
  ```
  or align to the original `git show HEAD:<f>` line endings.
- **DO** validate content integrity after a suspected line-ending leak with `git hash-object`:
  ```bash
  A=$(git show HEAD:<f> | tr -d '\r' | git hash-object --stdin)
  B=$(git hash-object <f>)
  # A == B  =>  content intact, only line endings differ
  ```

---

## 6. libsql FFI on Windows

- **Known flake:** libsql FFI intermittently crashes with `0xc0000005` (access violation) on Windows. This is environment-level; there is **no** code-level fix.
- **DO** run libsql integration tests under WSL2/Linux or CI (ubuntu).
- **DO** use `cargo test --lib` (in-memory backend) for unit tests on Windows to avoid the FFI path.
- **DO** guard libsql integration tests with a process-level `OnceLock<Mutex<()>>` to reduce parallel flakiness.

---

## 7. Git safety discipline

- **DON'T** use `git stash` mechanically — a past `stash` failure orphaned HEAD and corrupted refs (the root cause of the `0e0d0a2d` corruption). Inspect baselines with `git show HEAD:<file>` / `git diff HEAD` instead.
- **DON'T** `git reset --hard` or force-push without explicit user confirmation.
- **DO** verify remote HEAD before pushing: `git log --oneline origin/rust-rewrite -3`.
- **DON'T** auto-commit or auto-push. `commit`+`push` is a user-authorized checkpoint — wait for the explicit trigger.

---

## 8. macOS / WSL2 (Linux) notes

- **DO** prefer WSL2 for Rust builds — `cargo check -p zbrain-cli` ~49s vs a magnitude slower on Windows monitor-locked machines.
- **DO** run cargo as a **normal user** in WSL, not root — `pg-embed` `initdb` fails under root with `PgInitFailure`.
- WSL `git status` can report the entire tree as modified due to CRLF differences between filesystems — judge change scope by the **Windows-side** git, not WSL.

---

## 9. Trust & external operations

- **DON'T** push, send messages, open PRs, or perform other external actions without explicit user confirmation (unless pre-authorized).
- **DO** be bold with internal ops (read/write code, organize files) once the repo is verified healthy.

---

## 10. Memory & roadmap discipline

- **DO** maintain `docs/plans/` as the roadmap SSOT and append to `.workbuddy/memory/YYYY-MM-DD.md` after substantive work.
- **DON'T** trust a session summary or chat recap as ground truth for repo state — verify with real `git` / `fs` commands. Roadmaps systematically **lag** the code; treat roadmap nodes as an index, and trust HEAD + a successful compile.

---

## Pre-commit / pre-push checklist

1. `git fsck --full` clean (exit 0, no `missing`)?
2. `git status` — only expected files changed? (exclude `.tmp_ts_src/`, `target/`, `node_modules/`)
3. `git diff --numstat` each Rust file — no `N/N` whole-file rewrites?
4. Ran git via `cd` into repo (never `git -C /c/...`)?
5. Verified remote HEAD: `git log --oneline origin/rust-rewrite -3`?
6. Build/test green (`cargo test --lib` / `cargo check`)?

---

*Maintained alongside `AGENTS.md`. If you discover a new cross-OS pitfall, add it above and commit it with the fix.*
