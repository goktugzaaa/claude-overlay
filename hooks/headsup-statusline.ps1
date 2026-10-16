# Status line wrapper: forwards the status JSON (rate_limits etc.) to the overlay, then
# runs the user's original status line command (if any) the way Claude Code runs it and
# prints its output.
$ErrorActionPreference = 'SilentlyContinue'
# No BOM: the original command gets exactly the bytes Claude Code sent.
try { [Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false } catch {}
try { [System.Net.ServicePointManager]::Expect100Continue = $false } catch {}
# Not %APPDATA%: the Claude app (an MSIX package) redirects AppData for the processes it starts.
$appDir = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.headsup'
$cfg = $null
# ReadAllText decodes UTF-8 with or without a BOM (Get-Content would use the ANSI code page).
try { $cfg = [System.IO.File]::ReadAllText((Join-Path $appDir 'config.json')) | ConvertFrom-Json } catch {}
$port = 47821
if ($cfg -and $cfg.port) { $port = [int]$cfg.port }

# Stdin as raw bytes, passed on unchanged; nothing is written to a temp file.
$inBytes = New-Object byte[] 0
try {
    if ([Console]::IsInputRedirected) {
        $buf = New-Object System.IO.MemoryStream
        [Console]::OpenStandardInput().CopyTo($buf)
        $inBytes = $buf.ToArray()
    }
} catch {}
$raw = [System.Text.Encoding]::UTF8.GetString($inBytes).TrimStart([char]0xFEFF)
if (-not $raw.Trim()) { $raw = '{}' }

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

# Same rules as headsup-send.ps1: not after a tray Quit or an uninstall, never from an
# elevated process, rate-limited with the shared stamps.
function Start-Overlay {
    $exe = $null
    if ($cfg) { $exe = [string]$cfg.overlayExe }
    if (-not $exe -or -not (Test-Path -LiteralPath $exe -PathType Leaf)) { return }
    if ((Test-Path -LiteralPath (Join-Path $appDir 'stopped')) -or (Test-Path -LiteralPath (Join-Path $appDir 'uninstalled'))) { return }
    try {
        $me = New-Object System.Security.Principal.WindowsPrincipal ([System.Security.Principal.WindowsIdentity]::GetCurrent())
        if ($me.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) { return }
    } catch {}
    $spawnStamp = Join-Path $env:TEMP 'headsup-spawn.stamp'
    try { if (((Get-Date) - (Get-Item -LiteralPath (Join-Path $env:TEMP 'headsup-spawn.failed')).LastWriteTime).TotalSeconds -lt 300) { return } } catch {}
    try { if (((Get-Date) - (Get-Item -LiteralPath $spawnStamp).LastWriteTime).TotalSeconds -lt 10) { return } } catch {}
    try { Set-Content -LiteralPath $spawnStamp -Value (Get-Date).Ticks } catch {}
    $argList = @()
    if ($cfg.overlayArgs) { $argList = @($cfg.overlayArgs | ForEach-Object { '"' + $_ + '"' }) }
    $argList += '--from-hook'
    try { Start-Process -FilePath $exe -ArgumentList $argList -WorkingDirectory (Split-Path -Parent $exe) -WindowStyle Hidden -ErrorAction Stop; return } catch {}
    try { Start-Process -FilePath $exe -ArgumentList $argList -WindowStyle Hidden -ErrorAction Stop } catch {}
}

if (Test-Listening) {
    $key = Get-Key
    if ($key) {
        try {
            $nb = New-Object byte[] 16
            [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($nb)
            $nonce = Get-Hex $nb
            $bytes = [System.Text.Encoding]::UTF8.GetBytes('{"status":' + $raw + '}')
            $req = [System.Net.WebRequest]::Create("http://127.0.0.1:$port/status")
            $req.Method = 'POST'
            $req.ContentType = 'application/json; charset=utf-8'
            $req.Timeout = 700
            $req.ReadWriteTimeout = 700
            $req.Proxy = $null
            $req.Headers.Add('X-Overlay-Nonce', $nonce)
            $req.Headers.Add('X-Overlay-Auth', (Get-Mac $key "req`nstatus`n$nonce`n" $bytes))
            $req.ContentLength = $bytes.Length
            $s = $req.GetRequestStream(); $s.Write($bytes, 0, $bytes.Length); $s.Close()
            $req.GetResponse().Close()
        } catch {}
    }
} else {
    Start-Overlay
}

# CommandLineToArgvW quoting of one argument (what Claude Code's spawn does too).
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

# Git Bash, found the way Claude Code finds it; never WSL's bash.exe in System32.
function Find-Bash {
    $c = @()
    if ($env:CLAUDE_CODE_GIT_BASH_PATH) { $c += $env:CLAUDE_CODE_GIT_BASH_PATH }
    try {
        $git = (Get-Command git.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
        $d = Split-Path -Parent $git
        # <Git>\cmd\git.exe, <Git>\bin\git.exe or <Git>\mingw64\bin\git.exe
        $c += Join-Path $d 'bash.exe'
        $c += Join-Path (Split-Path -Parent $d) 'bin\bash.exe'
        $c += Join-Path (Split-Path -Parent (Split-Path -Parent $d)) 'bin\bash.exe'
    } catch {}
    foreach ($base in @($env:ProgramFiles, ${env:ProgramFiles(x86)}, $env:LOCALAPPDATA)) {
        if ($base) { $c += Join-Path $base 'Git\bin\bash.exe'; $c += Join-Path $base 'Programs\Git\bin\bash.exe' }
    }
    foreach ($p in $c) {
        if ($p -and $p -notlike "$env:SystemRoot\*" -and (Test-Path -LiteralPath $p -PathType Leaf)) { return $p }
    }
    return $null
}

# Runs the command through Git Bash (as Claude Code does when it has Git Bash), else cmd.
# Stdin and stdout are copied as bytes, so no character is re-encoded on the way.
function Invoke-Original([string]$cmd) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $bash = Find-Bash
    if ($bash) {
        $psi.FileName = $bash
        $psi.Arguments = '-c ' + (Quote-Arg $cmd)
    } else {
        $psi.FileName = Join-Path $env:SystemRoot 'System32\cmd.exe'
        $psi.Arguments = '/d /s /c "' + $cmd + '"'
    }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    $out = New-Object System.IO.MemoryStream
    # Read while writing: a command that prints before reading all of stdin cannot block.
    $copy = $p.StandardOutput.BaseStream.CopyToAsync($out)
    try { $p.StandardInput.BaseStream.Write($inBytes, 0, $inBytes.Length) } catch {}
    try { $p.StandardInput.Close() } catch {}
    if (-not $copy.Wait(15000)) { try { $p.Kill() } catch {}; return }
    $bytes = $out.ToArray()
    $stdout = [Console]::OpenStandardOutput()
    $stdout.Write($bytes, 0, $bytes.Length)
    $stdout.Flush()
}

$orig = $null
if ($cfg) { $orig = [string]$cfg.originalStatusLineCommand }
if ($orig) {
    try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false } catch {}
    try {
        # Fast path: "powershell ... -File X.ps1" runs in this process (saves a PowerShell
        # startup). pwsh is PowerShell 7 and goes the normal way.
        $script = $null
        if ($orig -match '^\s*powershell(\.exe)?\s+.*-File\s+"?([^"]+?\.ps1)"?\s*$') { $script = $Matches[2] }
        if ($script -and (Test-Path -LiteralPath $script -PathType Leaf)) { $raw | & $script }
        else { Invoke-Original $orig }
    } catch {}
}
