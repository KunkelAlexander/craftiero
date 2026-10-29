# Builds the browser version into web/pkg and (with -Serve) serves web/ on :8080.
#
#   powershell -File web/build.ps1 [-Serve]
#
# One-time setup:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.129 --locked   # must match Cargo.lock
param([switch]$Serve)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    cargo build -p viewer --release --target wasm32-unknown-unknown
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }
    wasm-bindgen --target web --out-dir web/pkg --no-typescript target/wasm32-unknown-unknown/release/viewer.wasm
    if ($LASTEXITCODE -ne 0) { throw "wasm-bindgen failed" }
    # Optional size/speed pass (binaryen). Skipped when it is not installed.
    if (Get-Command wasm-opt -ErrorAction SilentlyContinue) {
        wasm-opt -O3 -o web/pkg/viewer_bg.wasm web/pkg/viewer_bg.wasm
    }
    Write-Host ("web/pkg/viewer_bg.wasm: {0:N1} MB" -f ((Get-Item web/pkg/viewer_bg.wasm).Length / 1MB))
    if ($Serve) { python -m http.server 8080 --directory web }
} finally {
    Pop-Location
}
