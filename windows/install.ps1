# Installs or updates Clearway for the current user, without administrator rights:
# clearway.exe in %LOCALAPPDATA%\Programs\clearway, on the user PATH, started at logon by the 'Clearway' task.
#   irm https://github.com/AW-HTK/clearway/releases/latest/download/install.ps1 | iex
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
    if (Test-Path $exe) {
        # A running exe cannot be overwritten, but it can be renamed; its relays keep the old file until they end.
        Get-ChildItem $dir -Filter 'clearway.old-*.exe' | ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
        Rename-Item $exe ("clearway.old-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date))
    }
    Move-Item $new $exe
    Say "clearway.exe installed in $dir." "clearway.exe installé dans $dir."
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

if (-not $env:CLEARWAY_NO_TASK) {
    if ($server) {
        try {
            $p = [IO.Pipes.NamedPipeClientStream]::new('.', 'clearway', [IO.Pipes.PipeDirection]::InOut); $p.Connect(2000)
            $w = [IO.StreamWriter]::new($p); $w.AutoFlush = $true; $w.WriteLine('{"method":"POST","path":"/quit"}')
            [IO.StreamReader]::new($p).ReadLine() | Out-Null; $p.Dispose()
        } catch { }
        $deadline = (Get-Date).AddSeconds(10)
        while ((Get-Process -Id $server.ProcessId -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    }
    $action = New-ScheduledTaskAction -Execute $exe -Argument 'serve'
    $trigger = New-ScheduledTaskTrigger -AtLogOn -User $env:USERNAME
    $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive -RunLevel Limited
    $settings = New-ScheduledTaskSettingsSet -RestartCount 5 -RestartInterval (New-TimeSpan -Minutes 1) `
        -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
    Register-ScheduledTask -TaskName 'Clearway' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
    Start-ScheduledTask -TaskName 'Clearway'
    $deadline = (Get-Date).AddSeconds(15)
    while (-not (Test-Path '\\.\pipe\clearway') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
    Remove-Item $maintenance -ErrorAction SilentlyContinue
    if (Test-Path '\\.\pipe\clearway') {
        Say "Clearway is running (tray icon) and starts at each logon." "Clearway tourne (icône de la zone de notification) et démarre à chaque ouverture de session."
    } else {
        Say "The 'Clearway' task is installed but the server did not answer within 15 s." "La tâche « Clearway » est installée mais le serveur n'a pas répondu en 15 s."
    }
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
