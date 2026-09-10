# Sets this machine up to run the extension from source.
#
#   .\install.ps1                 build isml-lsp into %CARGO_HOME%\bin (on PATH,
#                                 which is where the extension looks first)
#   .\install.ps1 -LocalGrammar   also point extension.toml at a local copy of
#                                 grammar\, so grammar edits take effect without
#                                 pushing. Leaves the manifest dirty on purpose.
#   .\install.ps1 -PinGrammar     point extension.toml back at this repo on
#                                 GitHub, at the current HEAD. Run before a
#                                 release whenever the grammar changed.
#
# Then, from Zed: `zed: install dev extension` and pick the `extension` folder
# (or `zed: reload extensions` if it is already installed).

[CmdletBinding()]
param(
    [switch] $LocalGrammar,
    [switch] $PinGrammar,
    # Only touch the manifest; useful while Zed is running the server.
    [switch] $SkipServer
)

$ErrorActionPreference = 'Stop'
$root = $PSScriptRoot
$manifestPath = "$root\extension\extension.toml"

function Set-Grammar([string] $Url, [string] $Commit) {
    $content = Get-Content $manifestPath -Raw
    # Scoped to the [grammars.isml] block: the manifest also has a top-level
    # `repository` key pointing at the same URL, which must not be touched.
    $section = [regex]::Match($content, '(?ms)\[grammars\.isml\].*?(?=(\r?\n\[)|\z)')
    if (-not $section.Success) { throw 'no [grammars.isml] section in extension.toml' }

    $block = $section.Value
    $block = $block -replace '(?m)^repository = ".*"$', "repository = `"$Url`""
    $block = $block -replace '(?m)^commit = ".*"$', "commit = `"$Commit`""

    $content = $content.Remove($section.Index, $section.Length).Insert($section.Index, $block)
    Set-Content -Path $manifestPath -Value $content -NoNewline
    Write-Host "    grammar -> $Url @ $Commit" -ForegroundColor DarkGray
}

if (-not $SkipServer) {
    # Zed keeps the server running, and Windows will not let cargo overwrite a
    # running executable.
    if (Get-Process isml-lsp -ErrorAction SilentlyContinue) {
        throw 'isml-lsp is running. Quit Zed (or `Stop-Process -Name isml-lsp`) and retry, or pass -SkipServer.'
    }

    Write-Host '==> Building isml-lsp' -ForegroundColor Cyan
    cargo install --path "$root\isml-lsp" --force
    if ($LASTEXITCODE -ne 0) { throw "cargo install failed ($LASTEXITCODE)" }
}

if ($LocalGrammar) {
    # Zed fetches grammars with git, so a work-in-progress grammar needs a
    # throwaway repo of its own. It is a build artifact, kept out of this tree.
    $staging = "$(Split-Path $root -Parent)\zed-isml-grammar"
    Write-Host "==> Staging the grammar in $staging" -ForegroundColor Cyan
    # Mirrors this repo's layout so that `path = "grammar"` holds either way.
    New-Item -ItemType Directory -Force -Path "$staging\grammar" | Out-Null
    foreach ($item in @('grammar.js', 'tree-sitter.json', 'package.json', 'LICENSE')) {
        Copy-Item "$root\grammar\$item" "$staging\grammar" -Force
    }
    if (Test-Path "$staging\grammar\src") { Remove-Item -Recurse -Force "$staging\grammar\src" }
    Copy-Item "$root\grammar\src" "$staging\grammar" -Recurse -Force

    Push-Location $staging
    try {
        if (-not (Test-Path '.git')) {
            git init --quiet
            git config user.email 'zed-isml@local'
            git config user.name 'zed-isml'
        }
        git add -A
        git commit --quiet --allow-empty -m "grammar $(Get-Date -Format s)"
        $commit = (git rev-parse HEAD).Trim()
    } finally {
        Pop-Location
    }
    Set-Grammar ('file:///' + ($staging -replace '\\', '/')) $commit
}

if ($PinGrammar) {
    Write-Host '==> Pinning the grammar to this repo' -ForegroundColor Cyan
    $commit = (git -C $root rev-parse HEAD).Trim()
    Set-Grammar 'https://github.com/salva-sm/sfcc-zed-isml' $commit
    Write-Host '    commit the manifest and push before tagging a release' -ForegroundColor DarkGray
}

Write-Host ''
Write-Host 'Done. In Zed: run `zed: install dev extension` and choose' -ForegroundColor Green
Write-Host "  $root\extension" -ForegroundColor Green
Write-Host '(already installed? run `zed: reload extensions` instead).' -ForegroundColor Green
