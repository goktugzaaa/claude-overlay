# Codex CLI adapter. Codex runs the `notify` program from ~/.codex/config.toml after every
# turn and passes the event as a JSON string in the LAST argument:
#   notify = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "<this script>"]
# Only "agent-turn-complete" exists today; it becomes a task-finished banner titled "Codex".
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest)

$ErrorActionPreference = 'SilentlyContinue'
try { [Console]::InputEncoding = [System.Text.Encoding]::UTF8 } catch {}

$json = ''
if ($Rest -and $Rest.Count -gt 0) { $json = [string]$Rest[-1] }
if (-not $json -or -not $json.Trim().StartsWith('{')) {
    try { if ([Console]::IsInputRedirected) { $json = [Console]::In.ReadToEnd() } } catch {}
}
if (-not $json) { exit 0 }
$ev = $null
try { $ev = $json | ConvertFrom-Json } catch { exit 0 }
if (-not $ev -or $ev.type -ne 'agent-turn-complete') { exit 0 }

$msg = ''
try { $msg = [string]$ev.'last-assistant-message' } catch {}
$cwd = ''
try { $cwd = (Get-Location).Path } catch {}
$payload = @{
    hook_event_name        = 'Stop'
    agent                  = 'codex'
    title                  = 'Codex'
    cwd                    = $cwd
    last_assistant_message = $msg
} | ConvertTo-Json -Compress

$cfgPath = Join-Path $env:APPDATA 'claude-notification\config.json'
$cfg = $null
try { $cfg = Get-Content -Raw -LiteralPath $cfgPath | ConvertFrom-Json } catch {}
$port = 47821
if ($cfg -and $cfg.port) { $port = [int]$cfg.port }

# Chain the notify command Codex had before the overlay was enabled (for example the Codex
# app's own "turn-ended" hook), so enabling the overlay never switches other behaviour off.
$orig = $null
if ($cfg) { $orig = $cfg.originalCodexNotify }
if ($orig) {
    $parts = @()
    try {
        foreach ($m in [regex]::Matches([string]$orig, '"((?:[^"\\]|\\.)*)"')) {
            $parts += ($m.Groups[1].Value -replace '\\\\', '\' -replace '\\"', '"')
        }
    } catch {}
    if ($parts.Count -gt 0) {
        $rest = @()
        if ($parts.Count -gt 1) { $rest = @($parts[1..($parts.Count - 1)]) }
        $rest += $json
        $quoted = $rest | ForEach-Object { '"' + ($_ -replace '"', '\"') + '"' }
        try { Start-Process -FilePath $parts[0] -ArgumentList $quoted -WindowStyle Hidden } catch {}
    }
}

function Send-Event([int]$timeoutMs) {
    $req = [System.Net.WebRequest]::Create("http://127.0.0.1:$port/stop")
    $req.Method = 'POST'
    $req.ContentType = 'application/json; charset=utf-8'
    $req.Timeout = $timeoutMs
    $req.Proxy = $null
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
    $req.ContentLength = $bytes.Length
    $stream = $req.GetRequestStream()
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Close()
    $req.GetResponse().Close()
}

try { Send-Event 1500; exit 0 } catch {}

# Overlay not running: start it (rate-limited, shared stamp with the Claude hooks) and retry.
$exe = $null
if ($cfg) { $exe = $cfg.overlayExe }
if (-not $exe -or -not (Test-Path -LiteralPath $exe)) { exit 0 }
$stamp = Join-Path $env:TEMP 'claude-overlay-spawn.stamp'
$recent = $false
try { $recent = ((Get-Date) - (Get-Item -LiteralPath $stamp).LastWriteTime).TotalSeconds -lt 10 } catch {}
if (-not $recent) {
    try { Set-Content -LiteralPath $stamp -Value (Get-Date).Ticks } catch {}
    try { Start-Process -FilePath $exe -WindowStyle Hidden } catch {}
}
for ($i = 0; $i -lt 10; $i++) {
    Start-Sleep -Milliseconds 500
    try { Send-Event 1000; exit 0 } catch {}
}
exit 0
