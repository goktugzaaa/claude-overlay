# Status line wrapper: forwards the status JSON (rate_limits etc.) to the overlay, then
# runs the user's original status line command (if any) and prints its output.
$ErrorActionPreference = 'SilentlyContinue'
$cfgPath = Join-Path $env:APPDATA 'claude-notification\config.json'
$cfg = $null
try { $cfg = Get-Content -Raw -LiteralPath $cfgPath | ConvertFrom-Json } catch {}
$port = 47821
if ($cfg -and $cfg.port) { $port = [int]$cfg.port }

$raw = ''
try { $raw = [Console]::In.ReadToEnd() } catch {}
if (-not $raw) { $raw = '{}' }

try {
    $req = [System.Net.WebRequest]::Create("http://127.0.0.1:$port/status")
    $req.Method = 'POST'
    $req.ContentType = 'application/json; charset=utf-8'
    $req.Timeout = 700
    $req.Proxy = $null
    $bytes = [System.Text.Encoding]::UTF8.GetBytes('{"status":' + $raw + '}')
    $req.ContentLength = $bytes.Length
    $s = $req.GetRequestStream(); $s.Write($bytes, 0, $bytes.Length); $s.Close()
    $req.GetResponse().Close()
} catch {
    # Overlay down: start it, rate-limited (same stamp as claude-overlay-send.ps1).
    $exe = $null
    if ($cfg) { $exe = $cfg.overlayExe }
    if ($exe -and (Test-Path -LiteralPath $exe)) {
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
    }
}

$orig = $null
if ($cfg) { $orig = $cfg.originalStatusLineCommand }
if ($orig) {
    $tmp = Join-Path $env:TEMP ("claude-statusline-" + [guid]::NewGuid().ToString('N') + ".json")
    try {
        [System.IO.File]::WriteAllText($tmp, $raw, [System.Text.Encoding]::UTF8)
        # Fast path: "powershell ... -File X.ps1" runs in this process (saves a PowerShell startup).
        if ($orig -match '^\s*(powershell(\.exe)?|pwsh(\.exe)?)\s+.*-File\s+"?([^"]+?\.ps1)"?\s*$') {
            $script = $Matches[4]
            if (Test-Path -LiteralPath $script) { Get-Content -Raw -LiteralPath $tmp | & $script }
            else { Invoke-Expression ("Get-Content -Raw -LiteralPath '" + $tmp.Replace("'", "''") + "' | " + $orig) }
        } else {
            Invoke-Expression ("Get-Content -Raw -LiteralPath '" + $tmp.Replace("'", "''") + "' | " + $orig)
        }
    } catch {}
    try { Remove-Item -LiteralPath $tmp -Force } catch {}
}
