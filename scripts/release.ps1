# Builds a release of Hopper into dist\: hopper.exe and a zip of it, the two
# files a GitHub release needs for Hopper's self-update (Settings → Hopper
# updates) to find it. Tag the release v<version> from Cargo.toml.
#
#   .\scripts\release.ps1             # the CurseForge key comes from Settings
#   .\scripts\release.ps1 -EmbedKey   # builds CF_API_KEY (.env) into the exe
#
# -EmbedKey suits copies for friends; anyone with the exe can read the key.

param([switch]$EmbedKey)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
Write-Host "Hopper $version"

if ($EmbedKey) { $env:HOPPER_EMBED_CF_KEY = '1' } else { Remove-Item Env:HOPPER_EMBED_CF_KEY -ErrorAction SilentlyContinue }
cargo build --release --bin hopper
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$dist = Join-Path $root 'dist'
New-Item -ItemType Directory -Force $dist | Out-Null
$exe = Join-Path $dist 'hopper.exe'
Copy-Item target\release\hopper.exe $exe -Force
$zip = Join-Path $dist "hopper-$version-win64.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path $exe -DestinationPath $zip

$size = [math]::Round((Get-Item $exe).Length / 1MB, 1)
Write-Host "dist\hopper.exe ($size MB) and dist\$(Split-Path $zip -Leaf)"
Write-Host "Attach both to a GitHub release tagged v$version."
