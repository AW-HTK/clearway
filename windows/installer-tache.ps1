# Démarre orchestrateur.exe à l'ouverture de session, sur le bureau interactif, et le relance s'il plante.
$exe = Join-Path $env:LOCALAPPDATA 'Programs\orchestrateur\orchestrateur.exe'
if (-not (Test-Path $exe)) { throw "Introuvable : $exe" }

$action = New-ScheduledTaskAction -Execute $exe
$declencheur = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
$principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
$reglages = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew

Register-ScheduledTask -TaskName 'Orchestrateur de charge' -Action $action -Trigger $declencheur `
    -Principal $principal -Settings $reglages -Force | Out-Null
Start-ScheduledTask -TaskName 'Orchestrateur de charge'
Write-Output "Tâche « Orchestrateur de charge » installée et lancée."
