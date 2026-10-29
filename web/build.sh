#!/bin/sh
# Builds the browser version into web/pkg. Same steps as build.ps1.
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.129 --locked   # must match Cargo.lock
set -e
cd "$(dirname "$0")/.."
cargo build -p viewer --release --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir web/pkg --no-typescript target/wasm32-unknown-unknown/release/viewer.wasm
if command -v wasm-opt >/dev/null 2>&1; then
    wasm-opt -O3 -o web/pkg/viewer_bg.wasm web/pkg/viewer_bg.wasm
fi
ls -l web/pkg/viewer_bg.wasm
