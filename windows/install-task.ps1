# Starts clearway.exe at logon, on the interactive desktop, and restarts it if it crashes.
$exe = Join-Path $env:LOCALAPPDATA 'Programs\clearway\clearway.exe'
if (-not (Test-Path $exe)) { throw "Not found: $exe" }

$action = New-ScheduledTaskAction -Execute $exe -Argument 'serve'
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew

Register-ScheduledTask -TaskName 'Clearway' -Action $action -Trigger $trigger `
    -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName 'Clearway'
Write-Output "Scheduled task 'Clearway' installed and started."
