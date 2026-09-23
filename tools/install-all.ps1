# Downloads every command-line tool from the latest release into one folder.
#
#   .\install-all.ps1                        into %LOCALAPPDATA%\sfcc-tools\bin
#   .\install-all.ps1 -Dir C:\tools          somewhere else
#   .\install-all.ps1 -Only log-diff         just the ones named
#
# Each tool installs on its own as well - this only saves typing the curls.
# The folder is added to the user PATH when it is not there yet.

[CmdletBinding()]
param(
    [string] $Dir = "$env:LOCALAPPDATA\sfcc-tools\bin",
    [string[]] $Only = @('sfcc-upload', 'log-diff', 'isml-lsp', 'sfcc-dap')
)

$ErrorActionPreference = 'Stop'
$base = 'https://github.com/salva-sm/sfcc-tools/releases/latest/download'
New-Item -ItemType Directory -Force $Dir | Out-Null

foreach ($tool in $Only) {
    # The two that are installed by hand ship as a bare .exe; the other two
    # are zipped for the Zed extensions, which unpack them.
    if ($tool -in @('sfcc-upload', 'log-diff')) {
        Invoke-WebRequest "$base/$tool-x86_64-windows.exe" -OutFile "$Dir\$tool.exe"
        # A first run puts the tab completion where Git Bash looks for it.
        & "$Dir\$tool.exe" --version | Out-Null
    } else {
        $zip = Join-Path ([IO.Path]::GetTempPath()) "$tool.zip"
        Invoke-WebRequest "$base/$tool-x86_64-windows.zip" -OutFile $zip
        Expand-Archive $zip -DestinationPath $Dir -Force
        Remove-Item $zip
    }
    Write-Host "installed $tool"
}

$path = [Environment]::GetEnvironmentVariable('Path', 'User')
if (-not (($path -split ';') -contains $Dir)) {
    [Environment]::SetEnvironmentVariable('Path', "$path;$Dir", 'User')
    Write-Host "added $Dir to the user PATH - open a new terminal to pick it up"
}
