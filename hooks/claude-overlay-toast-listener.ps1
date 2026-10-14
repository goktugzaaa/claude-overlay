# Polls the Windows notification center (UserNotificationListener) and prints one JSON
# line per NEW toast notification. The overlay spawns this and filters by app name.
# Requires: Settings > Privacy & security > Notifications > "Let desktop apps access notifications".

[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$ErrorActionPreference = 'Stop'

try {
    [Windows.UI.Notifications.Management.UserNotificationListener, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    [Windows.UI.Notifications.NotificationKinds, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    [Windows.UI.Notifications.KnownNotificationBindings, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    Add-Type -AssemblyName System.Runtime.WindowsRuntime
} catch {
    Write-Output ('{"event":"error","message":"WinRT not available: ' + ($_.Exception.Message -replace '"', "'") + '"}')
    exit 1
}

$asTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
})[0]

function Await($WinRtTask, $ResultType) {
    $asTask = $asTaskGeneric.MakeGenericMethod($ResultType)
    $netTask = $asTask.Invoke($null, @($WinRtTask))
    $netTask.Wait(-1) | Out-Null
    $netTask.Result
}

function Emit($obj) {
    Write-Output (ConvertTo-Json -InputObject $obj -Compress -Depth 4)
    [Console]::Out.Flush()
}

$listener = [Windows.UI.Notifications.Management.UserNotificationListener]::Current
$status = Await ($listener.RequestAccessAsync()) ([Windows.UI.Notifications.Management.UserNotificationListenerAccessStatus])
Emit @{ event = 'access'; status = "$status" }
if ("$status" -ne 'Allowed') { exit 2 }

$kinds = [Windows.UI.Notifications.NotificationKinds]::Toast
$binding = [Windows.UI.Notifications.KnownNotificationBindings]::ToastGeneric
$seen = @{}
$first = $true
$startedAt = [DateTimeOffset]::Now.AddSeconds(-5)

while ($true) {
    try {
        $notifs = Await ($listener.GetNotificationsAsync($kinds)) ([System.Collections.Generic.IReadOnlyList[Windows.UI.Notifications.UserNotification]])
        foreach ($n in $notifs) {
            $id = [string]$n.Id
            if ($seen.ContainsKey($id)) { continue }
            $seen[$id] = $true
            if ($first) { continue }
            if ($n.CreationTime -lt $startedAt) { continue }

            $name = ''; $aumid = ''; $texts = @()
            try { $name = [string]$n.AppInfo.DisplayInfo.DisplayName } catch {}
            try { $aumid = [string]$n.AppInfo.AppUserModelId } catch {}
            try {
                $b = $n.Notification.Visual.GetBinding($binding)
                if ($b) { $texts = @($b.GetTextElements() | ForEach-Object { [string]$_.Text }) }
            } catch {}

            Emit @{
                event = 'toast'
                id    = $id
                app   = $name
                aumid = $aumid
                texts = $texts
                time  = $n.CreationTime.ToUnixTimeSeconds()
            }
        }
        $first = $false
        # Forget ids that are no longer in the center so the table stays small.
        if ($seen.Count -gt 2000) {
            $live = @{}
            foreach ($n in $notifs) { $live[[string]$n.Id] = $true }
            $seen = $live
        }
    } catch {
        Emit @{ event = 'error'; message = ($_.Exception.Message) }
    }
    Start-Sleep -Milliseconds 1000
}
