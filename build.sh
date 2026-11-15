#!/usr/bin/env bash
# Builds the plugin: target/wasm32-wasip2/release/bedrock_skins_bridge.wasm
#
# The plugin needs Pumpkin's plugin API from a checkout with the patches in patches/ applied.
# Point PUMPKIN_DIR at it (default: ../pumpkin-patched).
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
PUMPKIN_DIR="${PUMPKIN_DIR:-$HERE/../pumpkin-patched}"
command -v cargo >/dev/null || { echo "cargo not found: install Rust from https://rustup.rs" >&2; exit 1; }
if ! grep -q "c-emote" "$PUMPKIN_DIR/crates/pumpkin-plugin-wit/v0.1/bedrock-packets.wit" 2>/dev/null; then
  echo "PUMPKIN_DIR ($PUMPKIN_DIR) is not a Pumpkin checkout with the patches applied." >&2
  echo "See README.md, \"Install\"." >&2
  exit 1
fi
rustup target list --installed 2>/dev/null | grep -q '^wasm32-wasip2$' || rustup target add wasm32-wasip2
(cd "$HERE/core" && cargo test --quiet)
(cd "$HERE" && cargo build --release --target wasm32-wasip2 \
  --config "patch.\"https://github.com/Pumpkin-MC/Pumpkin.git\".pumpkin-plugin-api.path=\"$PUMPKIN_DIR/crates/pumpkin-plugin-api\"")
ls -la "$HERE/target/wasm32-wasip2/release/bedrock_skins_bridge.wasm"
