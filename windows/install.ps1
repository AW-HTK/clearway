# Installs or updates Clearway for the current user, without administrator rights:
# clearway.exe in %LOCALAPPDATA%\Programs\clearway, on the user PATH, started at logon by the 'Clearway' task.
#   iex ([Text.Encoding]::UTF8.GetString((iwr -UseBasicParsing https://github.com/AW-HTK/clearway/releases/latest/download/install.ps1).Content).TrimStart([char]0xFEFF))
# (decoded as UTF-8 by hand: Windows PowerShell would read this BOM-marked UTF-8 file as ANSI).
# Variables for tests: CLEARWAY_SOURCE (URL or folder of the release files), CLEARWAY_INSTALL_DIR, CLEARWAY_NO_TASK,
# CLEARWAY_NO_PATH, CLEARWAY_QUIET (no closing instructions, for clearway-setup).
$ErrorActionPreference = 'Stop'
$fr = (Get-UICulture).TwoLetterISOLanguageName -eq 'fr'
function Say([string]$en, [string]$french) { if ($fr) { Write-Output $french } else { Write-Output $en } }

$source = if ($env:CLEARWAY_SOURCE) { $env:CLEARWAY_SOURCE } else { 'https://github.com/AW-HTK/clearway/releases/latest/download' }
$dir = if ($env:CLEARWAY_INSTALL_DIR) { $env:CLEARWAY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\clearway' }
$exe = Join-Path $dir 'clearway.exe'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Fetch([string]$name, [string]$to) {
    if ($source -match '^https?://') { Invoke-WebRequest -UseBasicParsing -Uri "$source/$name" -OutFile $to }
    else { Copy-Item (Join-Path $source $name) $to }
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("clearway-install-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $tmp | Out-Null
try {
    $new = Join-Path $tmp 'clearway.exe'
    Fetch 'clearway.exe' $new
    Fetch 'SHA256SUMS' (Join-Path $tmp 'SHA256SUMS')
    Fetch 'VERSION' (Join-Path $tmp 'VERSION')
    $version = (Get-Content (Join-Path $tmp 'VERSION') -Raw).Trim()
    $line = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match '\s\*?clearway\.exe$' } | Select-Object -First 1
    if (-not $line) { throw 'SHA256SUMS has no line for clearway.exe' }
    $expected = ($line -split '\s+')[0].ToLower()
    # .NET rather than Get-FileHash, whose module is not always loadable (inherited PSModulePath).
    $sha = [Security.Cryptography.SHA256]::Create()
    $actual = -join ($sha.ComputeHash([IO.File]::ReadAllBytes($new)) | ForEach-Object { $_.ToString('x2') })
    if ($expected -ne $actual) { throw "clearway.exe: SHA-256 $actual, expected $expected" }

    New-Item -ItemType Directory -Force $dir | Out-Null
    $server = Get-CimInstance Win32_Process -Filter "Name='clearway.exe'" |
        Where-Object { $_.ExecutablePath -eq $exe -and $_.CommandLine -notmatch '\brelay\b|\bmcp\b|\bhook\b' }
    $maintenance = Join-Path $env:LOCALAPPDATA 'clearway\maintenance'
    if ($server) {
        # During the switch, clients wait instead of launching the exe themselves (they ignore a file older than 5 min).
        New-Item -ItemType Directory -Force (Split-Path $maintenance) | Out-Null
        Set-Content $maintenance (Get-Date -Format o)
    }
    $previous = $null
    if (Test-Path $exe) {
        # A running exe cannot be overwritten, but it can be renamed; its relays keep the old file until they end.
        Get-ChildItem $dir -Filter 'clearway.old-*.exe' | ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
        $previous = Join-Path $dir ("clearway.old-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date))
        Rename-Item $exe $previous
    }
    Move-Item $new $exe
    Say "clearway.exe $version installed in $dir." "clearway.exe $version installé dans $dir."
} finally {
    Get-ChildItem $tmp -File -ErrorAction SilentlyContinue | Remove-Item -ErrorAction SilentlyContinue
    Remove-Item $tmp -ErrorAction SilentlyContinue
}

if (-not $env:CLEARWAY_NO_PATH) {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $parts = @($userPath -split ';' | Where-Object { $_ })
    if ($parts -notcontains $dir) {
        [Environment]::SetEnvironmentVariable('Path', (($parts + $dir) -join ';'), 'User')
        Say "Added to your PATH (for programs started from now on)." "Ajouté à votre PATH (pour les programmes lancés à partir de maintenant)."
    }
}

function Send-Pipe([string]$request) {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', 'clearway', [IO.Pipes.PipeDirection]::InOut); $p.Connect(1000)
    try {
        $w = [IO.StreamWriter]::new($p); $w.AutoFlush = $true; $w.WriteLine($request)
        [IO.StreamReader]::new($p).ReadLine()
    } finally { $p.Dispose() }
}

function Get-ServerVersion {
    try { (Send-Pipe '{"method":"GET","path":"/status"}' | ConvertFrom-Json).body.version } catch { $null }
}

function Get-Server {
    Get-CimInstance Win32_Process -Filter "Name='clearway.exe' OR Name LIKE 'clearway.old-%'" |
        Where-Object { $_.CommandLine -notmatch '\brelay\b|\bmcp\b|\bhook\b' -and $_.ExecutablePath -like (Join-Path $dir '*') }
}

function Stop-Server {
    try { Send-Pipe '{"method":"POST","path":"/quit"}' | Out-Null } catch { }
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Server) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    Get-Server | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 500
}

# Healthy only when the server answering on the pipe is the version just installed, not a previous one still alive.
function Wait-Version([string]$wanted) {
    $deadline = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $deadline) {
        if ((Get-ServerVersion) -eq $wanted) { return $true }
        Start-Sleep -Milliseconds 300
    }
    $false
}

if (-not $env:CLEARWAY_NO_TASK) {
    if ($server) { Stop-Server }
    $action = New-ScheduledTaskAction -Execute $exe -Argument 'serve'
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName 'Clearway' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName 'Clearway'
    $healthy = Wait-Version $version
    if (-not $healthy -and $previous) {
        Stop-Server
        Rename-Item $exe ("clearway.failed-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date))
        Rename-Item $previous $exe
        Start-ScheduledTask -TaskName 'Clearway'
        Remove-Item $maintenance -ErrorAction SilentlyContinue
        if ($fr) { throw "Clearway $version n'a pas démarré : l'ancienne version est remise en place." }
        throw "Clearway $version did not start: the previous version is back in place."
    }
    Remove-Item $maintenance -ErrorAction SilentlyContinue
    if (-not $healthy) {
        if ($fr) { throw "La tâche « Clearway » est installée mais Clearway $version n'a pas répondu en 20 s." }
        throw "The 'Clearway' task is installed but Clearway $version did not answer within 20 s."
    }
    Say "Clearway $version is running (tray icon) and starts at each logon." "Clearway $version tourne (icône de la zone de notification) et démarre à chaque ouverture de session."
}

if (-not $env:CLEARWAY_QUIET) {
Say @"

Next steps:
  WSL (in each distribution where AI sessions run):
    curl -fsSL https://github.com/AW-HTK/clearway/releases/latest/download/install.sh | sh
  Claude Code and Codex (in a new terminal, Windows and/or WSL):
    claude plugin marketplace add AW-HTK/clearway; claude plugin install clearway@clearway --scope user
    codex plugin marketplace add AW-HTK/clearway; codex plugin add clearway@clearway
"@ @"

Étapes suivantes :
  WSL (dans chaque distribution où tournent des sessions d'IA) :
    curl -fsSL https://github.com/AW-HTK/clearway/releases/latest/download/install.sh | sh
  Claude Code et Codex (dans un nouveau terminal, Windows et/ou WSL) :
    claude plugin marketplace add AW-HTK/clearway; claude plugin install clearway@clearway --scope user
    codex plugin marketplace add AW-HTK/clearway; codex plugin add clearway@clearway
"@
}
