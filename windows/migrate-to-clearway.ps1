# One-time switch from the pre-Clearway server (orchestrateur.exe) to Clearway, without disturbing running sessions.
# Usage: copy the new clearway.exe next to this script (or pass -Exe), then run it once.
# Rollback: Stop-ScheduledTask Clearway; Disable-ScheduledTask Clearway; restore orchestrateur.old-*.exe as
# orchestrateur.exe; Enable-ScheduledTask 'Orchestrateur de charge'; Start-ScheduledTask 'Orchestrateur de charge'.
param([string]$Exe = (Join-Path $PSScriptRoot 'clearway.exe'))
$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Exe)) { throw "Not found: $Exe" }
$local = $env:LOCALAPPDATA
$newDir = Join-Path $local 'Programs\clearway'
$oldDir = Join-Path $local 'Programs\orchestrateur'
$oldExe = Join-Path $oldDir 'orchestrateur.exe'

function Stop-Server($pipe, $request, $name) {
    try {
        $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut); $p.Connect(2000)
        $w = [IO.StreamWriter]::new($p); $w.AutoFlush = $true; $w.WriteLine($request)
        [IO.StreamReader]::new($p).ReadLine() | Out-Null; $p.Dispose()
    } catch { "No server on pipe $pipe." }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-CimInstance Win32_Process | Where-Object { $_.Name -eq $name -and $_.CommandLine -notmatch 'relais|relay' }) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 100
    }
}

# 1. Clients of both versions wait during the switch instead of launching a server themselves.
$maintenance = @((Join-Path $local 'orchestrateur\maintenance'), (Join-Path $local 'clearway\maintenance'))
New-Item -ItemType Directory -Force (Join-Path $local 'clearway') | Out-Null
$maintenance | ForEach-Object { Set-Content $_ (Get-Date -Format o) }
try {
    # 2. New exe in place.
    New-Item -ItemType Directory -Force $newDir | Out-Null
    Copy-Item $Exe (Join-Path $newDir 'clearway.exe') -Force

    # 3. Old server stopped (French request: it only speaks the old dialect), its task disabled but kept.
    Stop-Server 'orchestrateur' '{"methode":"POST","chemin":"/quitter"}' 'orchestrateur.exe'
    if (Get-ScheduledTask -TaskName 'Orchestrateur de charge' -ErrorAction SilentlyContinue) {
        Disable-ScheduledTask -TaskName 'Orchestrateur de charge' | Out-Null
    }

    # 4. Old clients launch the old path: it becomes a copy of Clearway (relais --tube orchestrateur still works).
    if (Test-Path $oldExe) {
        Rename-Item $oldExe (Join-Path $oldDir ("orchestrateur.old-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date)))
    }
    New-Item -ItemType Directory -Force $oldDir | Out-Null
    Copy-Item $Exe $oldExe -Force

    # 5. Clearway task. At its first start the server migrates %LOCALAPPDATA%\orchestrateur into ...\clearway.
    & (Join-Path $PSScriptRoot 'install-task.ps1')
    $deadline = (Get-Date).AddSeconds(15)
    while (-not ((Test-Path '\\.\pipe\clearway') -and (Test-Path '\\.\pipe\orchestrateur')) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 200
    }
} finally {
    $maintenance | ForEach-Object { Remove-Item $_ -ErrorAction SilentlyContinue }
}
"Pipes: clearway $(Test-Path '\\.\pipe\clearway'), orchestrateur (old clients) $(Test-Path '\\.\pipe\orchestrateur')"
Get-Content (Join-Path $local 'clearway\clearway.log') -Tail 5 -Encoding UTF8
Get-CimInstance Win32_Process | Where-Object { $_.Name -in 'clearway.exe', 'orchestrateur.exe' } |
    ForEach-Object { "{0}  {1}" -f $_.ProcessId, $_.CommandLine }
