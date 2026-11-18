[CmdletBinding()]
param(
    [string]$Compiler = (Join-Path $env:LOCALAPPDATA 'Programs/Inno Setup 6/ISCC.exe')
)
$ErrorActionPreference = 'Stop'
if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'Run this script with PowerShell 7 (pwsh).' }
if (!(Test-Path -LiteralPath $Compiler)) { throw 'Install Inno Setup 6 or pass -Compiler with the path to ISCC.exe.' }
$root = Split-Path $PSScriptRoot
Push-Location $root
try {
    & "$PSScriptRoot/release.ps1"
    & "$PSScriptRoot/prepare-assets.ps1"
    $version = (Get-Content Cargo.toml -Raw | Select-String '(?m)^version = "([^"]+)"').Matches[0].Groups[1].Value
    $assetVersion = (Get-Content src/reference.json -Raw -Encoding UTF8 | ConvertFrom-Json).version
    & $Compiler "/DAppVersion=$version" "/DAssetVersion=$assetVersion" 'installer/lol-records.iss'
    if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
    $setup = Join-Path $root "dist/lol-records-setup-v$version-x64.exe"
    $hash = (Get-FileHash -LiteralPath $setup -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $([IO.Path]::GetFileName($setup))" | Set-Content "$setup.sha256" -Encoding ascii
    Get-Item -LiteralPath $setup | Select-Object FullName,Length
} finally { Pop-Location }
