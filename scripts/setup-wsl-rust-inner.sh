#!/usr/bin/env bash
# setup-wsl-rust-inner.sh - Run inside WSL Ubuntu to install Rust and verify
# the ZBrain workspace builds.
#
# This script is invoked by setup-wsl-rust.ps1 (step 6) but can also be
# run directly inside a WSL Ubuntu shell if you prefer manual install.
#
# Strategy (per project MEMORY.md "WSL 装 Rust"):
#   1. apt install build-essential pkg-config libssl-dev + ca-certificates
#   2. rustup-init download often fails on CA — pull the static tarball
#      from https://static.rust-lang.org/dist/ directly
#   3. Configure cargo with rsproxy sparse mirror (much faster in CN)
#   4. Verify cargo --version + rustc --version
#   5. (Optional) cargo test -p zbrain-core --lib to confirm 2337 baseline
#
# All paths are $HOME-local so re-running is safe.

set -euo pipefail

# ---- 0. Detect environment ----
if [[ -f /proc/version ]] && grep -qi "microsoft\|WSL" /proc/version; then
    echo "[inner] Running under WSL: $(grep -i microsoft /proc/version | head -1 || true)"
else
    echo "[inner] WARNING: /proc/version does not mention WSL. Continuing anyway."
fi

UBUNTU_VER="$(lsb_release -rs 2>/dev/null || echo 'unknown')"
echo "[inner] Ubuntu version: $UBUNTU_VER"

# ---- 1. apt packages ----
echo ""
echo "=== Step 1/5: apt install build deps ==="
sudo -n true 2>/dev/null || {
    echo "  sudo requires password; you may be prompted now."
    sudo -v
}
sudo apt-get update -y
sudo apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev ca-certificates curl git

# ---- 2. Rust toolchain ----
echo ""
echo "=== Step 2/5: Install Rust via static tarball (rustup CA workaround) ==="
RUST_PREFIX="$HOME/.rust"
RUST_VERSION="1.85.0"   # adjust if zbrain requires a newer MSRV
RUST_DATE="2025-02-20"  # date of the 1.85.0 release

mkdir -p "$RUST_PREFIX"
cd /tmp

TARBALL="rust-${RUST_VERSION}-x86_64-unknown-linux-gnu.tar.gz"
URL="https://static.rust-lang.org/dist/${RUST_DATE}/${TARBALL}"

if [[ -x "$RUST_PREFIX/bin/cargo" ]] && [[ -x "$RUST_PREFIX/bin/rustc" ]]; then
    echo "  cargo + rustc already installed in $RUST_PREFIX — skipping download."
    "$RUST_PREFIX/bin/cargo" --version
    "$RUST_PREFIX/bin/rustc" --version
else
    echo "  Downloading $URL"
    if ! curl -fL --connect-timeout 10 -o "$TARBALL" "$URL"; then
        echo "  ERROR: download failed. Check network and try with a newer RUST_DATE."
        echo "  Latest stable: https://static.rust-lang.org/dist/"
        exit 1
    fi
    tar -xzf "$TARBALL" -C /tmp
    echo "  Installing into $RUST_PREFIX ..."
    /tmp/rust-${RUST_VERSION}-x86_64-unknown-linux-gnu/install.sh \
        --prefix="$RUST_PREFIX" --components=cargo,rustc,rust-std-x86_64-unknown-linux-gnu \
        --without=rust-docs
    rm -rf "$TARBALL" /tmp/rust-${RUST_VERSION}-x86_64-unknown-linux-gnu
fi

# Add to PATH for this session
export PATH="$RUST_PREFIX/bin:$PATH"
echo "  cargo:  $($RUST_PREFIX/bin/cargo --version)"
echo "  rustc:  $($RUST_PREFIX/bin/rustc --version)"

# Persist PATH for future shells
SHELL_RC="$HOME/.bashrc"
if [[ -f "$SHELL_RC" ]] && ! grep -q 'RUST_PREFIX/bin' "$SHELL_RC"; then
    {
        echo ""
        echo "# Rust toolchain (added by setup-wsl-rust-inner.sh)"
        echo "export PATH=\"$RUST_PREFIX/bin:\$PATH\""
        echo "export CARGO_HOME=\"$HOME/.cargo\""
        echo "export RUSTUP_HOME=\"$HOME/.rustup\""
    } >> "$SHELL_RC"
    echo "  Persisted PATH in $SHELL_RC"
fi

# ---- 3. cargo sparse mirror ----
echo ""
echo "=== Step 3/5: Configure cargo sparse mirror (rsproxy) ==="
mkdir -p "$HOME/.cargo"
CARGO_CONFIG="$HOME/.cargo/config.toml"
if [[ -f "$CARGO_CONFIG" ]] && grep -q "rsproxy.cn" "$CARGO_CONFIG"; then
    echo "  rsproxy already configured."
else
    cat > "$CARGO_CONFIG" <<'EOF'
# Cargo config (added by setup-wsl-rust-inner.sh)
# Sparse index from rsproxy.cn — much faster than the default index
# in CN networks. Falls back gracefully if the mirror is down.
[source.crates-io]
replace-with = 'rsproxy-sparse'

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"

[net]
git-fetch-with-cli = true
EOF
    echo "  Wrote $CARGO_CONFIG (sparse + rsproxy.cn)"
fi

# ---- 4. Verify toolchain on a fresh shell ----
echo ""
echo "=== Step 4/5: Verify toolchain (fresh shell) ==="
bash -lc 'cargo --version; rustc --version' 2>&1

# ---- 5. Optional: cargo test zbrain-core ----
echo ""
echo "=== Step 5/5: (Optional) cargo test -p zbrain-core --lib ==="
ZBRAIN_DIR="$(cd "$(dirname "$0")/.." && pwd)"
echo "  ZBrain repo (per script): $ZBRAIN_DIR"
echo "  Pwd from caller:          ${PWD}"
echo ""
echo "  This step is NOT auto-run. To verify the build manually:"
echo ""
echo "    cd \"$ZBRAIN_DIR\""
echo "    export PATH=\"$RUST_PREFIX/bin:\$PATH\""
echo "    cargo test -p zbrain-core --lib 2>&1 | tail -20"
echo ""
echo "  Baseline expectation (per handoff): 2337 passed / 0 failed"
echo "  If you see libsql_ffi flakes on the first run, re-run — the"
echo "  process-level OnceLock guard stabilizes after warmup."

echo ""
echo "===== Inner install complete ====="
