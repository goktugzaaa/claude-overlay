# Builds the release exe and the zip that goes on the GitHub release.
#   powershell -ExecutionPolicy Bypass -File scripts/dist.ps1 [-SkipBuild]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $SkipBuild) {
    Push-Location "$root\src-tauri"
    try {
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally {
        Pop-Location
    }
}

$exe = "$root\src-tauri\target\release\ClaudeOverlay.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }
New-Item -ItemType Directory -Force "$root\dist" | Out-Null
Copy-Item $exe "$root\dist\ClaudeOverlay.exe" -Force
$zip = "$root\dist\ClaudeOverlay-win64.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$root\dist\ClaudeOverlay.exe" -DestinationPath $zip
"exe: {0:N1} MB" -f ((Get-Item $exe).Length / 1MB)
"zip: {0:N1} MB" -f ((Get-Item $zip).Length / 1MB)
