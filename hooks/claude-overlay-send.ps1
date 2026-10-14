# Usage: powershell -File claude-overlay-send.ps1 <stop|notification|test-notify|test-logo|reload-config|quit>
# Forwards the Claude Code hook JSON from stdin to the overlay; starts the overlay if needed.
param([string]$Type = 'stop')

$ErrorActionPreference = 'SilentlyContinue'
try { [Console]::InputEncoding = [System.Text.Encoding]::UTF8 } catch {}
$Type = ($Type -replace '[^a-z-]', '')
$cfgPath = Join-Path $env:APPDATA 'claude-notification\config.json'
$cfg = $null
try { $cfg = Get-Content -Raw -LiteralPath $cfgPath | ConvertFrom-Json } catch {}
$port = 47821
if ($cfg -and $cfg.port) { $port = [int]$cfg.port }

$raw = ''
try { if ([Console]::IsInputRedirected) { $raw = [Console]::In.ReadToEnd() } } catch {}
if (-not $raw -or -not $raw.Trim()) { $raw = '{}' }

# Interactive types (question, permission) block until the user clicks a button on the
# overlay or the wait expires; the overlay's reply is printed as the hook's JSON output.
$interactive = ($Type -eq 'question' -or $Type -eq 'permission')
$waitMs = 20000
if ($cfg -and $cfg.interactive -and $cfg.interactive.waitMs) { $waitMs = [int]$cfg.interactive.waitMs }

function Send-Event([int]$timeoutMs) {
    $req = [System.Net.WebRequest]::Create("http://127.0.0.1:$port/$Type")
    $req.Method = 'POST'
    $req.ContentType = 'application/json; charset=utf-8'
    $req.Timeout = $timeoutMs
    $req.ReadWriteTimeout = $timeoutMs
    $req.Proxy = $null
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($raw)
    $req.ContentLength = $bytes.Length
    $stream = $req.GetRequestStream()
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Close()
    $resp = $req.GetResponse()
    $reader = New-Object System.IO.StreamReader($resp.GetResponseStream(), [System.Text.Encoding]::UTF8)
    $body = $reader.ReadToEnd()
    $reader.Close()
    $resp.Close()
    if ($interactive -and $body -and $body.Trim().StartsWith('{') -and $body.Contains('hookSpecificOutput')) {
        [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
        [Console]::Out.Write($body)
        [Console]::Out.Flush()
    }
}

$firstTimeout = 1500
if ($interactive) { $firstTimeout = $waitMs + 5000 }
try { Send-Event $firstTimeout; exit 0 } catch {}
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
$retryTimeout = 1000
if ($interactive) { $retryTimeout = $waitMs + 5000 }
for ($i = 0; $i -lt 10; $i++) {
    Start-Sleep -Milliseconds 500
    try { Send-Event $retryTimeout; exit 0 } catch {}
}
exit 0
