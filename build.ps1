# Builds the plugin on Windows: target\wasm32-wasip2\release\bedrock_skins_bridge.wasm
#
# The plugin needs Pumpkin's plugin API from a checkout with the patches in patches\ applied.
# Point PUMPKIN_DIR at it (default: ..\pumpkin-patched), for example:
#   $env:PUMPKIN_DIR = "C:\dev\pumpkin-patched"; .\build.ps1
$ErrorActionPreference = "Stop"

$Here = $PSScriptRoot
if ($env:PUMPKIN_DIR) { $PumpkinDir = $env:PUMPKIN_DIR } else { $PumpkinDir = Join-Path (Split-Path $Here -Parent) "pumpkin-patched" }

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found: install Rust from https://rustup.rs"
}

$wit = Join-Path $PumpkinDir "crates\pumpkin-plugin-wit\v0.1\bedrock-packets.wit"
if (-not ((Test-Path $wit) -and (Select-String -Path $wit -Pattern "c-emote" -Quiet))) {
    throw "PUMPKIN_DIR ($PumpkinDir) is not a Pumpkin checkout with the patches applied. See README.md, 'Install'."
}

if ((rustup target list --installed) -notcontains "wasm32-wasip2") {
    rustup target add wasm32-wasip2
    if ($LASTEXITCODE -ne 0) { throw "could not add the wasm32-wasip2 target" }
}

Push-Location (Join-Path $Here "core")
try {
    cargo test --quiet
    if ($LASTEXITCODE -ne 0) { throw "core tests failed" }
} finally { Pop-Location }

# Cargo reads the patch from a small config file, which avoids quoting problems in PowerShell.
$api = (Resolve-Path (Join-Path $PumpkinDir "crates\pumpkin-plugin-api")).Path -replace '\\', '/'
$cfg = Join-Path $env:TEMP "bedrock-skins-bridge-patch.toml"
$content = @"
[patch."https://github.com/Pumpkin-MC/Pumpkin.git"]
pumpkin-plugin-api = { path = "$api" }
"@
Set-Content -Path $cfg -Value $content -Encoding ascii

Push-Location $Here
try {
    cargo build --release --target wasm32-wasip2 --config $cfg
    if ($LASTEXITCODE -ne 0) { throw "plugin build failed" }
} finally { Pop-Location }

Get-Item (Join-Path $Here "target\wasm32-wasip2\release\bedrock_skins_bridge.wasm") | Format-List FullName, Length
