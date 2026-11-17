param(
    [string]$SigningKey = (Join-Path $env:LOCALAPPDATA 'LolRecordsSigning/release-ed25519.pem')
)

$ErrorActionPreference = 'Stop'
Push-Location (Split-Path $PSScriptRoot)
try {
    $metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Unable to read Cargo metadata' }
    $tag = 'v' + $metadata.packages[0].version
    cargo build --release --locked --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }

    $destination = Join-Path $PWD 'dist'
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    $exe = Join-Path $destination 'lol-records.exe'
    Copy-Item -LiteralPath 'target/x86_64-pc-windows-msvc/release/lol-records.exe' -Destination $exe
    $payload = Join-Path $destination 'signing-payload.tmp'
    try {
        $header = [Text.Encoding]::UTF8.GetBytes("lol-records $tag windows-x64`n")
        [IO.File]::WriteAllBytes($payload, [byte[]]($header + [IO.File]::ReadAllBytes($exe)))
        openssl pkeyutl -sign -rawin -inkey $SigningKey -in $payload -out "$exe.sig"
        if ($LASTEXITCODE -ne 0) { throw 'Signing failed' }
        $publicDer = Join-Path $destination 'signing-public.tmp'
        try {
            openssl pkey -in $SigningKey -pubout -outform DER -out $publicDer
            if ($LASTEXITCODE -ne 0) { throw 'Unable to derive the release public key' }
            $bytes = [IO.File]::ReadAllBytes($publicDer)
            $publicKey = [Convert]::ToBase64String($bytes[12..43])
            if (!(Select-String -LiteralPath '.cargo/config.toml' -SimpleMatch $publicKey -Quiet)) {
                Remove-Item -LiteralPath "$exe.sig"
                throw 'Signing key does not match the public key compiled into the application'
            }
        } finally { Remove-Item -LiteralPath $publicDer -ErrorAction SilentlyContinue }
    } finally { Remove-Item -LiteralPath $payload -ErrorAction SilentlyContinue }
    Write-Output "$tag $destination"
} finally { Pop-Location }
