# Requires PowerShell 7. Downloads build-time assets; no game credentials are used.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
$reference = Get-Content (Join-Path $root 'src/reference.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$stage = Join-Path $root 'dist/assets'
$version = $reference.version
$jobs = @(
    foreach ($entry in $reference.champions.PSObject.Properties) {
        [pscustomobject]@{ Relative = "portraits/$version/$($entry.Name).png"; Url = "https://ddragon.leagueoflegends.com/cdn/$version/img/champion/$($entry.Value[1])" }
    }
    foreach ($entry in $reference.items.PSObject.Properties) {
        [pscustomobject]@{ Relative = "items/$version/$($entry.Name).png"; Url = "https://ddragon.leagueoflegends.com/cdn/$version/img/item/$($entry.Name).png" }
    }
    $combat = Get-Content (Join-Path $root 'src/combat-reference.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    foreach ($group in 'spells','runes','ranks') {
        foreach ($entry in $combat.$group.PSObject.Properties) {
            [pscustomobject]@{ Relative = "$group/$version/$($entry.Name).png"; Url = $entry.Value[1] }
        }
    }
)
$results = $jobs | ForEach-Object -ThrottleLimit 6 -Parallel {
    $ErrorActionPreference = 'Stop'
    function Test-Png($path) {
        if (!(Test-Path -LiteralPath $path)) { return $false }
        $bytes = [IO.File]::ReadAllBytes($path)
        return ($bytes.Length -ge 8 -and $bytes.Length -le 1048576 -and [Convert]::ToHexString($bytes[0..7]) -eq '89504E470D0A1A0A')
    }
    $job = $_
    $target = Join-Path $using:stage $job.Relative
    try {
        New-Item -ItemType Directory -Path (Split-Path $target) -Force | Out-Null
        if (!(Test-Png $target)) {
            $cached = Join-Path (Join-Path $env:LOCALAPPDATA 'LolRecords') $job.Relative
            if (Test-Png $cached) { Copy-Item -LiteralPath $cached -Destination $target }
            else {
                $temporary = "$target.download"
                try {
                    for ($attempt = 0; $attempt -lt 3; $attempt++) {
                        try {
                            Invoke-WebRequest $job.Url -OutFile $temporary -TimeoutSec 20
                            if (!(Test-Png $temporary)) { throw 'Invalid PNG response' }
                            break
                        } catch {
                            if ($attempt -eq 2) { throw }
                            Start-Sleep -Seconds 1
                        }
                    }
                    Move-Item -LiteralPath $temporary -Destination $target -Force
                } finally { Remove-Item -LiteralPath $temporary -ErrorAction SilentlyContinue }
            }
        }
        [pscustomobject]@{ Path = $job.Relative; Sha256 = (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash; Error = $null }
    } catch { [pscustomobject]@{ Path = $job.Relative; Error = $_.Exception.Message } }
}
$failed = @($results | Where-Object Error)
if ($failed.Count) {
    $failed | Format-Table Path,Error | Out-Host
    throw "Asset preparation failed: $($failed.Count)/$($jobs.Count). Run again to resume."
}
@{ Version = $version; Files = @($results | Sort-Object Path | Select-Object Path,Sha256) } |
    ConvertTo-Json -Depth 4 | Set-Content (Join-Path $root 'dist/assets-manifest.json') -Encoding UTF8
Write-Output "Assets ready: $($results.Count), Data Dragon $version"
