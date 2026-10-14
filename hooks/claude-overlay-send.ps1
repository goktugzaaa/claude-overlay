# Usage: powershell -File claude-overlay-send.ps1 <stop|notification|test-notify|test-logo|reload-config|quit>
# Forwards the Claude Code hook JSON from stdin to the overlay; starts the overlay if needed.
param([string]$Type = 'stop')

$ErrorActionPreference = 'SilentlyContinue'
$Type = ($Type -replace '[^a-z-]', '')
$cfgPath = Join-Path $env:APPDATA 'claude-notification\config.json'
$cfg = $null
try { $cfg = Get-Content -Raw -LiteralPath $cfgPath | ConvertFrom-Json } catch {}
$port = 47821
if ($cfg -and $cfg.port) { $port = [int]$cfg.port }

$raw = ''
try { if ([Console]::IsInputRedirected) { $raw = [Console]::In.ReadToEnd() } } catch {}
if (-not $raw -or -not $raw.Trim()) { $raw = '{}' }

function Send-Event([int]$timeoutMs) {
    $req = [System.Net.WebRequest]::Create("http://127.0.0.1:$port/$Type")
    $req.Method = 'POST'
    $req.ContentType = 'application/json; charset=utf-8'
    $req.Timeout = $timeoutMs
    $req.Proxy = $null
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($raw)
    $req.ContentLength = $bytes.Length
    $stream = $req.GetRequestStream()
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Close()
    $resp = $req.GetResponse()
    $resp.Close()
}

try { Send-Event 1500; exit 0 } catch {}
if ($Type -eq 'quit') { exit 0 }

# Overlay not running: start it (rate-limited) and retry.
$exe = $null
if ($cfg) { $exe = $cfg.overlayExe }
if (-not $exe -or -not (Test-Path -LiteralPath $exe)) { exit 0 }
$stamp = Join-Path $env:TEMP 'claude-overlay-spawn.stamp'
$recent = $false
try { $recent = ((Get-Date) - (Get-Item -LiteralPath $stamp).LastWriteTime).TotalSeconds -lt 10 } catch {}
if (-not $recent) {
    try { Set-Content -LiteralPath $stamp -Value (Get-Date).Ticks } catch {}
    $argList = @()
    if ($cfg.overlayArgs) { $argList = @($cfg.overlayArgs | ForEach-Object { '"' + $_ + '"' }) }
    try {
        if ($argList.Count -gt 0) { Start-Process -FilePath $exe -ArgumentList $argList -WindowStyle Hidden }
        else { Start-Process -FilePath $exe -WindowStyle Hidden }
    } catch {}
}
for ($i = 0; $i -lt 10; $i++) {
    Start-Sleep -Milliseconds 500
    try { Send-Event 1000; exit 0 } catch {}
}
exit 0
