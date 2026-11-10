#!/usr/bin/env bash
set -euo pipefail
export CARGO_HOME=/workspace/.cargo
export RUSTUP_HOME=/workspace/.rustup
export PATH="$CARGO_HOME/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1 || ! command -v rustup >/dev/null 2>&1; then
  installer=$(mktemp /tmp/payment-rustup.XXXXXX)
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$installer"
  sh "$installer" -y --profile minimal --default-toolchain 1.99.1 --no-modify-path
  rm -f "$installer"
fi
cd /workspace/web3testproj1
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
cargo build --locked -j 4
