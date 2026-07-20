#!/usr/bin/env bash
# Build + install monitor on Gadi.
#
# The Rust toolchain and build artifacts live on /scratch (home has a 10GB
# quota; the toolchain alone is >1GB), and the finished binaries are *copied*
# into ~/.local/bin — not symlinked — because /scratch files unaccessed for
# 100 days are purged. If the toolchain gets purged, this script simply
# reinstalls it on the next run.
set -euo pipefail

SCRATCH_BASE="/scratch/${PROJECT:-xr78}/$USER"
export RUSTUP_HOME="$SCRATCH_BASE/rust/rustup"
export CARGO_HOME="$SCRATCH_BASE/rust/cargo"
export CARGO_TARGET_DIR="$SCRATCH_BASE/build/monitor-target"

if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
    echo "== Rust toolchain not found under $SCRATCH_BASE/rust — installing via rustup =="
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable
fi
export PATH="$CARGO_HOME/bin:$PATH"

cd "$(dirname "$0")"
if [ "${1:-}" = "--test" ]; then
    cargo test --release
fi
cargo build --release

BIN="$HOME/.local/bin"
mkdir -p "$BIN"
for f in monitor qusage qarray; do
    # Back up a pre-existing (non-backup) command once; never clobber a backup.
    if [ -e "$BIN/$f" ] && [ ! -e "$BIN/$f.bak" ]; then
        cp -a "$BIN/$f" "$BIN/$f.bak"
    fi
    install -m 755 "$CARGO_TARGET_DIR/release/$f" "$BIN/$f"
done
echo "Installed: monitor, qusage, qarray -> $BIN"
