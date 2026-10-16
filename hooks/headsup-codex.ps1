# Codex CLI adapter. Codex runs the `notify` program from ~/.codex/config.toml after every
# turn and passes the event as a JSON string in the LAST argument:
#   notify = ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "<this script>"]
# Only "agent-turn-complete" exists today; it becomes a task-finished banner titled "Codex".
param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Rest)

$ErrorActionPreference = 'SilentlyContinue'
try { [Console]::InputEncoding = [System.Text.Encoding]::UTF8 } catch {}
try { [System.Net.ServicePointManager]::Expect100Continue = $false } catch {}

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
$bodyBytes = [System.Text.Encoding]::UTF8.GetBytes($payload)

# Not %APPDATA%: the Claude app (an MSIX package) redirects AppData for the processes it starts.
$appDir = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.headsup'
$cfg = $null
# ReadAllText decodes UTF-8 with or without a BOM (Get-Content would use the ANSI code page).
try { $cfg = [System.IO.File]::ReadAllText((Join-Path $appDir 'config.json')) | ConvertFrom-Json } catch {}
$port = 47821
if ($cfg -and $cfg.port) { $port = [int]$cfg.port }

function Write-Log([string[]]$fields) {
    try {
        $now = (Get-Date).ToUniversalTime().ToString("yyyy-MM-dd'T'HH':'mm':'ss'.'fff'Z'", [System.Globalization.CultureInfo]::InvariantCulture)
        $clean = @($fields | ForEach-Object { ([string]$_) -replace '[\x00-\x1f\x7f]', ' ' })
        [System.IO.File]::AppendAllText((Join-Path $appDir 'events.log'), ($now + "`t" + ($clean -join "`t") + "`n"), (New-Object System.Text.UTF8Encoding $false))
    } catch {}
}

# CommandLineToArgvW quoting of one argument (what Codex's own spawn produces).
function Quote-Arg([string]$a) {
    $sb = New-Object System.Text.StringBuilder
    [void]$sb.Append('"')
    $bs = 0
    foreach ($ch in $a.ToCharArray()) {
        if ($ch -eq [char]'\') { $bs++; continue }
        if ($ch -eq [char]'"') { [void]$sb.Append([char]'\', 2 * $bs + 1) }
        elseif ($bs -gt 0) { [void]$sb.Append([char]'\', $bs) }
        $bs = 0
        [void]$sb.Append($ch)
    }
    [void]$sb.Append([char]'\', 2 * $bs)
    [void]$sb.Append('"')
    return $sb.ToString()
}

# Chain the notify command Codex had before the overlay was enabled (for example the Codex
# app's own "turn-ended" hook), so enabling the overlay never switches other behaviour off.
# The overlay stores it as an argument list when it edits config.toml.
$parts = @()
if ($cfg -and $cfg.PSObject.Properties['originalCodexNotifyArgs']) {
    # null: the earlier notify was not a plain list of strings, so it is not chained.
    $parts = @($cfg.originalCodexNotifyArgs | Where-Object { $null -ne $_ } | ForEach-Object { [string]$_ })
} elseif ($cfg -and $cfg.originalCodexNotify) {
    # Stored by an older version as the TOML line: read its "basic" strings.
    try {
        foreach ($m in [regex]::Matches([string]$cfg.originalCodexNotify, '"((?:[^"\\]|\\.)*)"')) {
            $parts += ($m.Groups[1].Value -replace '\\(["\\])', '$1')
        }
    } catch {}
}
if ($parts.Count -gt 0 -and $parts[0]) {
    $name = ''
    # Windows ignores trailing dots and spaces: "x.bat." is x.bat.
    try { $name = [System.IO.Path]::GetFileName($parts[0].Trim()).TrimEnd([char[]]'. ').ToLowerInvariant() } catch {}
    if (-not $name -or $name.Contains(':') -or $name -match '\.(bat|cmd)$' -or $name -eq 'cmd' -or $name -eq 'cmd.exe') {
        # A batch file runs through cmd.exe, which would read the assistant's message as
        # commands: never hand it over.
        Write-Log @('codex-chain-refused', 'batch or cmd target')
    } else {
        $chainArgs = @()
        if ($parts.Count -gt 1) { $chainArgs = @($parts[1..($parts.Count - 1)]) }
        $chainArgs += $json
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $parts[0]
        $psi.Arguments = (@($chainArgs | ForEach-Object { Quote-Arg $_ }) -join ' ')
        # CreateProcess directly: no shell in between.
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        try { [void][System.Diagnostics.Process]::Start($psi) } catch { Write-Log @('codex-chain-failed', $_.Exception.Message) }
    }
}

function Get-Key {
    try {
        $k = [System.IO.File]::ReadAllText((Join-Path $appDir 'secret.key')).Trim()
        if ($k -cmatch '^[0-9a-f]{64}$') { return $k }
    } catch {}
    return $null
}

function Get-Hex([byte[]]$b) { return [BitConverter]::ToString($b).Replace('-', '').ToLowerInvariant() }

function Get-Mac([string]$key, [string]$prefix, [byte[]]$data) {
    $h = New-Object System.Security.Cryptography.HMACSHA256 (, [System.Text.Encoding]::UTF8.GetBytes($key))
    $head = [System.Text.Encoding]::UTF8.GetBytes($prefix)
    $msg = New-Object byte[] ($head.Length + $data.Length)
    [Array]::Copy($head, 0, $msg, 0, $head.Length)
    [Array]::Copy($data, 0, $msg, $head.Length, $data.Length)
    return Get-Hex ($h.ComputeHash($msg))
}

function Test-Listening {
    try {
        foreach ($l in [System.Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties().GetActiveTcpListeners()) {
            if ($l.Port -eq $port) { return $true }
        }
        return $false
    } catch { return $true }
}

function Send-Event([string]$key, [int]$timeoutMs) {
    $nb = New-Object byte[] 16
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($nb)
    $nonce = Get-Hex $nb
    $req = [System.Net.WebRequest]::Create("http://127.0.0.1:$port/stop")
    $req.Method = 'POST'
    $req.ContentType = 'application/json; charset=utf-8'
    $req.Timeout = $timeoutMs
    $req.ReadWriteTimeout = $timeoutMs
    $req.Proxy = $null
    $req.Headers.Add('X-Overlay-Nonce', $nonce)
    $req.Headers.Add('X-Overlay-Auth', (Get-Mac $key "req`nstop`n$nonce`n" $bodyBytes))
    $req.ContentLength = $bodyBytes.Length
    $stream = $req.GetRequestStream()
    $stream.Write($bodyBytes, 0, $bodyBytes.Length)
    $stream.Close()
    $req.GetResponse().Close()
}

# Same rules as headsup-send.ps1: not after a tray Quit or an uninstall, never from an
# elevated process, rate-limited with the shared stamps.
function Start-Overlay {
    $exe = $null
    if ($cfg) { $exe = [string]$cfg.overlayExe }
    if (-not $exe -or -not (Test-Path -LiteralPath $exe -PathType Leaf)) { return $false }
    if ((Test-Path -LiteralPath (Join-Path $appDir 'stopped')) -or (Test-Path -LiteralPath (Join-Path $appDir 'uninstalled'))) { return $false }
    try {
        $me = New-Object System.Security.Principal.WindowsPrincipal ([System.Security.Principal.WindowsIdentity]::GetCurrent())
        if ($me.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) { return $false }
    } catch {}
    try { if (((Get-Date) - (Get-Item -LiteralPath $failStamp).LastWriteTime).TotalSeconds -lt 300) { return $false } } catch {}
    try { if (((Get-Date) - (Get-Item -LiteralPath $spawnStamp).LastWriteTime).TotalSeconds -lt 10) { return $true } } catch {}
    try { Set-Content -LiteralPath $spawnStamp -Value (Get-Date).Ticks } catch {}
    $argList = @('--from-hook')
    try { Start-Process -FilePath $exe -ArgumentList $argList -WorkingDirectory (Split-Path -Parent $exe) -WindowStyle Hidden -ErrorAction Stop; return $true } catch {}
    try { Start-Process -FilePath $exe -ArgumentList $argList -WindowStyle Hidden -ErrorAction Stop; return $true } catch { return $false }
}

$spawnStamp = Join-Path $env:TEMP 'headsup-spawn.stamp'
$failStamp = Join-Path $env:TEMP 'headsup-spawn.failed'
if (-not (Test-Listening)) {
    if (-not (Start-Overlay)) { exit 0 }
    $until = (Get-Date).AddMilliseconds(3000)
    while (-not (Test-Listening)) {
        if ((Get-Date) -gt $until) {
            try { Set-Content -LiteralPath $failStamp -Value (Get-Date).Ticks } catch {}
            exit 0
        }
        Start-Sleep -Milliseconds 100
    }
}
# The key appears right after the overlay binds the port.
$key = $null
$until = (Get-Date).AddMilliseconds(2000)
while (-not ($key = Get-Key) -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 100 }
if (-not $key) { exit 0 }
try { Send-Event $key 2500 } catch {}
exit 0
