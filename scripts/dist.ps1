# Builds the release exe and the zip that goes on the GitHub release.
#   powershell -ExecutionPolicy Bypass -File scripts/dist.ps1 [-SkipBuild]
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $SkipBuild) {
    Push-Location "$root\src-tauri"
    $savedFlags = $env:CARGO_ENCODED_RUSTFLAGS
    try {
        # Panic locations embed source paths: keep the user name (profile, cargo home, this
        # checkout) out of the exe. The last matching prefix wins, so the general one goes
        # first. Encoded flags (0x1F-separated) survive paths with spaces.
        $cargoHome = $env:CARGO_HOME
        if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
        $flags = @(
            "--remap-path-prefix=$env:USERPROFILE=~",
            "--remap-path-prefix=$cargoHome=cargo",
            "--remap-path-prefix=$root=."
        )
        $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
        cargo build --release
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
    } finally {
        $env:CARGO_ENCODED_RUSTFLAGS = $savedFlags
        Pop-Location
    }
}

$exe = "$root\src-tauri\target\release\HeadsUp.exe"
if (-not (Test-Path $exe)) { throw "missing $exe" }
New-Item -ItemType Directory -Force "$root\dist" | Out-Null
Copy-Item $exe "$root\dist\HeadsUp.exe" -Force
$zip = "$root\dist\HeadsUp-win64.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path "$root\dist\HeadsUp.exe" -DestinationPath $zip
"exe: {0:N1} MB" -f ((Get-Item $exe).Length / 1MB)
"zip: {0:N1} MB" -f ((Get-Item $zip).Length / 1MB)
