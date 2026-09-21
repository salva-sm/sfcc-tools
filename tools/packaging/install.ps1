# Installs a packaged Zed extension. No toolchain required.
#
# Shipped inside the release zip next to the extension folder. Zed watches its
# installed directory, so it picks the extension up without a restart.
#
# Kept as a file rather than generated, so the zip built locally by package.ps1
# and the one built in CI are byte-for-byte the same thing.

$ErrorActionPreference = 'Stop'

$payload = Get-ChildItem $PSScriptRoot -Directory |
    Where-Object { $_.Name -ne 'bin' -and (Test-Path (Join-Path $_.FullName 'extension.toml')) } |
    Select-Object -First 1
if (-not $payload) { throw "no extension folder next to $PSScriptRoot" }

$id = $payload.Name
$installed = "$env:LOCALAPPDATA\Zed\extensions\installed\$id"

if (Test-Path $installed) { Remove-Item -Recurse -Force $installed }
New-Item -ItemType Directory -Force -Path (Split-Path $installed) | Out-Null
Copy-Item $payload.FullName $installed -Recurse
Write-Host "Installed $id into $installed" -ForegroundColor Green

if (Test-Path "$PSScriptRoot\bin") {
    $binDir = "$env:LOCALAPPDATA\Zed\extensions\bin"
    New-Item -ItemType Directory -Force -Path $binDir | Out-Null
    Copy-Item "$PSScriptRoot\bin\*" $binDir -Force

    Write-Host ''
    Write-Host 'This extension ships a language server:' -ForegroundColor Yellow
    Get-ChildItem "$PSScriptRoot\bin" -Filter *.exe | ForEach-Object {
        Write-Host ('  ' + (Join-Path $binDir $_.Name)) -ForegroundColor Yellow
    }
    Write-Host 'The extension finds it on PATH. Add the folder once:' -ForegroundColor Yellow
    Write-Host ('  setx PATH "' + $binDir + ';%PATH%"') -ForegroundColor Yellow
    Write-Host 'or set lsp.<server-name>.binary.path to that file in settings.json.' -ForegroundColor Yellow
    Write-Host 'Restart Zed afterwards so it picks up the new PATH.' -ForegroundColor Yellow
}

Write-Host ''
Write-Host 'Zed watches that folder, so it picks the extension up right away.' -ForegroundColor Green
