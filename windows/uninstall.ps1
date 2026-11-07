# Removes clearway.exe for the current user: stops Clearway, removes the 'Clearway' task, the PATH entry, the
# %LOCALAPPDATA%\Programs\clearway folder and the "Installed apps" entry. clearway-setup.exe --uninstall runs it.
# Variables: CLEARWAY_INSTALL_DIR, CLEARWAY_NO_TASK, CLEARWAY_NO_PATH, CLEARWAY_PIPE, CLEARWAY_UNINSTALL_KEY as for install.ps1;
# CLEARWAY_PURGE=1 also deletes the settings and the log (%LOCALAPPDATA%\clearway, or CLEARWAY_STATE_DIR).
$ErrorActionPreference = 'Stop'
$fr = (Get-UICulture).TwoLetterISOLanguageName -eq 'fr'
function Say([string]$en, [string]$french) { if ($fr) { Write-Output $french } else { Write-Output $en } }

$dir = if ($env:CLEARWAY_INSTALL_DIR) { $env:CLEARWAY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\clearway' }
$state = if ($env:CLEARWAY_STATE_DIR) { $env:CLEARWAY_STATE_DIR } else { Join-Path $env:LOCALAPPDATA 'clearway' }
$failed = @()
$pipe = if ($env:CLEARWAY_PIPE) { $env:CLEARWAY_PIPE } else { 'clearway' }

function Send-Pipe([string]$request) {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut); $p.Connect(1000)
    try {
        $w = [IO.StreamWriter]::new($p); $w.AutoFlush = $true; $w.WriteLine($request)
        [IO.StreamReader]::new($p).ReadLine()
    } finally { $p.Dispose() }
}

# Every clearway process started from the folder: the server, and the MCP servers of sessions still open. Not the
# setup, which may be waiting there for its temporary copy running this script.
function Get-Clearway {
    Get-CimInstance Win32_Process -Filter "Name LIKE 'clearway%' AND NOT Name LIKE 'clearway-setup%'" |
        Where-Object { $_.ExecutablePath -like (Join-Path $dir '*') }
}

# Sessions still open relaunch a stopped Clearway; this file makes them wait (they ignore it after 5 min).
$maintenance = Join-Path $state 'maintenance'
if (Test-Path (Join-Path $dir 'clearway.exe')) {
    New-Item -ItemType Directory -Force $state | Out-Null
    Set-Content $maintenance (Get-Date -Format o)
}

# The pipe is shared by any Clearway: /quit only when the server running is the one of this folder.
$server = @(Get-Clearway | Where-Object { $_.CommandLine -notmatch '\b(relay|mcp|hook)\b' })
if ($server) {
    try { Send-Pipe '{"method":"POST","path":"/quit"}' | Out-Null } catch { }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Clearway | Where-Object { $_.CommandLine -notmatch '\b(relay|mcp|hook)\b' }) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
    }
}
$left = @(Get-Clearway)
$left | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
if ($left) { Start-Sleep -Milliseconds 500 }
if ($server -or $left) { Say 'Clearway stopped.' 'Clearway arrêté.' }

if (-not $env:CLEARWAY_NO_TASK -and (Get-ScheduledTask -TaskName 'Clearway' -ErrorAction SilentlyContinue)) {
    Unregister-ScheduledTask -TaskName 'Clearway' -Confirm:$false
    Say "Task 'Clearway' removed." "Tâche « Clearway » supprimée."
}

if (-not $env:CLEARWAY_NO_PATH) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ })
    $kept = @($parts | Where-Object { $_.TrimEnd('\') -ne $dir.TrimEnd('\') })
    if ($kept.Count -ne $parts.Count) {
        [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
        Say 'Removed from your PATH.' 'Retiré de votre PATH.'
    }
}

# Deletes `files`, then the folder, once the process holding them ends (at most 2 min later).
function Remove-Later([string[]]$files, [string]$folder) {
    $quote = { "'" + $args[0].Replace("'", "''") + "'" }
    $list = ($files | ForEach-Object { & $quote $_ }) -join ','
    $script = "foreach (`$i in 1..120) { `$left = @(@($list) | Where-Object { Test-Path -LiteralPath `$_ }); " +
        "`$left | ForEach-Object { Remove-Item -LiteralPath `$_ -Force -ErrorAction SilentlyContinue }; " +
        "if (-not (@($list) | Where-Object { Test-Path -LiteralPath `$_ })) { Remove-Item -LiteralPath $(& $quote $folder) -ErrorAction SilentlyContinue; break }; " +
        "Start-Sleep 1 }"
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
    Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoProfile', '-EncodedCommand', $encoded
}

# Files one by one: a file still locked is renamed, so that the folder no longer holds a working clearway.exe. The
# setup that started this script cannot be deleted yet: it goes as soon as it ends.
function Remove-Folder([string]$path) {
    if (-not (Test-Path $path)) { return $true }
    $ok = $true
    $later = @()
    # A process relaunched meanwhile (an app restarting its MCP server) holds clearway.exe again: stop it and retry.
    for ($try = 0; $try -lt 5; $try++) {
        $locked = @(Get-ChildItem $path -File -Recurse -Force | Where-Object { $_.Name -notlike 'clearway-setup*.exe' } |
            Where-Object { try { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction Stop; $false } catch { $true } })
        if (-not $locked) { break }
        Get-Clearway | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
        Start-Sleep -Milliseconds 700
    }
    foreach ($f in @(Get-ChildItem $path -File -Recurse -Force)) {
        try { Remove-Item -LiteralPath $f.FullName -Force -ErrorAction Stop }
        catch {
            if ($f.Name -like 'clearway-setup*.exe') { $later += $f.FullName; continue }
            try { Rename-Item -LiteralPath $f.FullName ($f.Name + '.deleted') -ErrorAction Stop } catch { }
            $ok = $false
        }
    }
    Get-ChildItem $path -Directory -Recurse -Force | Sort-Object { $_.FullName.Length } -Descending |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -ErrorAction SilentlyContinue }
    if ($later) { Remove-Later $later $path; return $ok }
    Remove-Item -LiteralPath $path -ErrorAction SilentlyContinue
    $ok -and -not (Test-Path $path)
}

if (Remove-Folder $dir) {
    Say "$dir deleted." "$dir supprimé."
} else {
    $failed += $dir
    Say "Some files of $dir are still in use: close the programs using them, then delete the folder." `
        "Des fichiers de $dir sont encore utilisés : fermez les programmes concernés, puis supprimez le dossier."
}

$name = if ($env:CLEARWAY_UNINSTALL_KEY) { $env:CLEARWAY_UNINSTALL_KEY } else { 'Clearway' }
$key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$name"
if (Test-Path $key) {
    Remove-Item $key
    Say 'Removed from "Installed apps".' 'Retiré des « Applications installées ».'
}

Remove-Item $maintenance -ErrorAction SilentlyContinue

if ($env:CLEARWAY_PURGE -eq '1') {
    if (Remove-Folder $state) { Say "Settings and log deleted ($state)." "Réglages et journal supprimés ($state)." }
    else { $failed += $state }
} elseif (Test-Path $state) {
    Say "Settings and log kept in $state." "Réglages et journal conservés dans $state."
}

if ($failed) {
    if ($fr) { throw "Non supprimé : $($failed -join ', ')" }
    throw "Not deleted: $($failed -join ', ')"
}
