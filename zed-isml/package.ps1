# Packages a *built* Zed dev extension into a zip a teammate can drop in
# without any toolchain: no Rust, no cargo, no tree-sitter, no clang.
#
#   .\package.ps1                                        # this repo's extension/
#   .\package.ps1 -ExtensionDir C:\dev\zed-b2c-debug     # any other one
#
# It ships only what Zed loads at runtime — the same layout it keeps in
# %LOCALAPPDATA%\Zed\extensions\installed\<id>:
#
#   extension.toml  extension.wasm  grammars\*.wasm  languages\  (themes, schemas...)
#
# `extension.wasm` and `grammars\*.wasm` are produced by Zed itself when you
# run `zed: install dev extension`, so install the extension here once before
# packaging. Zed watches the installed directory, so the teammate does not even
# need to restart it.

[CmdletBinding()]
param(
    [string] $ExtensionDir = "$PSScriptRoot\extension",
    [string] $OutDir = "$PSScriptRoot\dist",
    # Extra executables to ship alongside, e.g. the language server.
    [string[]] $Binary = @()
)

$ErrorActionPreference = 'Stop'

$ExtensionDir = (Resolve-Path $ExtensionDir).Path
$manifestPath = Join-Path $ExtensionDir 'extension.toml'
if (-not (Test-Path $manifestPath)) { throw "no extension.toml in $ExtensionDir" }

$wasm = Join-Path $ExtensionDir 'extension.wasm'
if (-not (Test-Path $wasm)) {
    throw "no extension.wasm in $ExtensionDir — install it in Zed once (`zed: install dev extension`) so Zed builds it"
}

$manifest = Get-Content $manifestPath -Raw
$id = [regex]::Match($manifest, '(?m)^id\s*=\s*"([^"]+)"').Groups[1].Value
$version = [regex]::Match($manifest, '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
if (-not $id) { throw "extension.toml has no id" }

Write-Host "==> Packaging $id $version" -ForegroundColor Cyan

$stage = Join-Path ([System.IO.Path]::GetTempPath()) "zed-pack-$id-$(Get-Random)"
$payload = Join-Path $stage $id
New-Item -ItemType Directory -Force -Path $payload | Out-Null

Copy-Item $manifestPath $payload
Copy-Item $wasm $payload

# Only the compiled grammars; grammars\<name>\ holds the cloned source Zed
# built them from and is dead weight in a package.
$grammars = Get-ChildItem (Join-Path $ExtensionDir 'grammars') -Filter *.wasm -ErrorAction SilentlyContinue
if ($grammars) {
    New-Item -ItemType Directory -Force -Path "$payload\grammars" | Out-Null
    $grammars | Copy-Item -Destination "$payload\grammars"
}

# `tools` matters: an extension that ships a launcher script resolves it inside
# its own installed directory, so leaving it out yields a zip that installs and
# then quietly misbehaves.
foreach ($dir in @('languages', 'themes', 'icon_themes', 'icons', 'schemas', 'snippets', 'tools')) {
    $source = Join-Path $ExtensionDir $dir
    if (Test-Path $source) { Copy-Item $source $payload -Recurse }
}
foreach ($file in @('snippets.json', 'LICENSE', 'LICENSE.md', 'README.md')) {
    $source = Join-Path $ExtensionDir $file
    if (Test-Path $source) { Copy-Item $source $payload }
}

foreach ($exe in $Binary) {
    if (-not (Test-Path $exe)) { throw "binary not found: $exe" }
    New-Item -ItemType Directory -Force -Path "$stage\bin" | Out-Null
    Copy-Item $exe "$stage\bin"
}

# One installer, shared with the CI packaging job. The .cmd is what a teammate
# actually double-clicks: a downloaded .ps1 is blocked by the default execution
# policy.
Copy-Item "$PSScriptRoot\packaging\install.ps1", "$PSScriptRoot\packaging\install.cmd" $stage

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$zip = Join-Path $OutDir "$id-$version.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Remove-Item -Recurse -Force $stage

Write-Host ""
Write-Host "  $zip  ($([math]::Round((Get-Item $zip).Length / 1KB)) KB)" -ForegroundColor Green
Write-Host '  Teammate: unzip anywhere, run install.cmd.' -ForegroundColor Green
