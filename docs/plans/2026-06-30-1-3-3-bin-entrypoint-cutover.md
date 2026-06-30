# 1-3-3: Cut package bin and install flow to Rust binary

## Scope

Replace TypeScript `zbrain` CLI entrypoint with compiled Rust binary.

### In scope:
1. **Package.json bin change**: Replace `src/cli.ts` with `target/release/zbrain`
2. **Build script integration**: Add `cargo build --release` to npm scripts
3. **Install hook**: Add `postinstall` script that builds Rust binary
4. **NAPI shim layer**: Verify compatibility with existing NAPI/FFI calls
5. **Fallback mechanism**: Graceful fallback to bun + TS if Rust build fails

### Out of scope:
- MCP server migration (deferred to 1-5)
- Web backend migration (deferred to 1-6)

---

## Decisions

| # | Question | Answer | Rationale |
|---|----------|--------|-----------|
| 1 | Release build mode? | `--release` | Production-ready binary; acceptable build time trade-off |
| 2 | Build fallback? | Yes | Graceful fallback to bun/TS if rust toolchain missing |
| 3 | Postinstall hook? | Yes | Build binary on npm install |
| 4 | Binary placement? | `target/release/zbrain` | Standard cargo output location |

---

## Implementation Plan (3 slices)

### Slice A: Package.json scripts update

- Update `main` / `bin` fields
- Add `build:rust` script: `cargo build --release`
- Add `postinstall` script that attempts Rust build
- Add graceful fallback wrapper script

### Slice B: Verify NAPI/FFI compatibility

- Confirm all FFI calls (`brain-registry.ts`, `operations.ts`) work with Rust binary
- Verify `init` / `doctor` / `config` / `schema` work end-to-end
- Check `think` / `get-page` operation flow

### Slice C: Integration testing

- Fresh install test: `rm -rf node_modules && npm install`
- Verify `zbrain init` with Rust backend
- Verify backward compatibility with existing configs

---

## Acceptance Criteria

1. ✅ `npm run build:rust` produces working binary
2. ✅ `npx zbrain --version` uses Rust binary (not bun + TS)
3. ✅ `zbrain init` / `zbrain doctor` / `zbrain config show` all work
4. ✅ Operation layer (`zbrain think`, `zbrain get-page`) works end-to-end
5. ✅ Fresh npm install works reliably
6. ✅ Fallback to bun + TS works if rust toolchain missing

---

## Next Node

**1-3-4**: Delete replaced TypeScript bootstrap command surface
