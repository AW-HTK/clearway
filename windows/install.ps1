# Installs or updates clearway.exe for the current user, without administrator rights: in
# %LOCALAPPDATA%\Programs\clearway, on the user PATH, started at logon by the 'Clearway' task.
# clearway-setup.exe runs it; to run it by hand, point CLEARWAY_SOURCE at a dist/ folder (scripts/build-release.sh).
# Other variables: CLEARWAY_INSTALL_DIR, CLEARWAY_NO_TASK, CLEARWAY_NO_PATH, CLEARWAY_PIPE,
# CLEARWAY_QUIET (no closing instructions), CLEARWAY_SETUP_EXE (setup copied next to clearway.exe and registered in
# "Installed apps" as CLEARWAY_UNINSTALL_KEY).
$ErrorActionPreference = 'Stop'
$fr = (Get-UICulture).TwoLetterISOLanguageName -eq 'fr'
function Say([string]$en, [string]$french) { if ($fr) { Write-Output $french } else { Write-Output $en } }

if (-not $env:CLEARWAY_SOURCE) {
    if ($fr) { throw 'Lancez clearway-setup.exe (https://github.com/AW-HTK/clearway/releases/latest), ou définissez CLEARWAY_SOURCE vers un dossier dist/.' }
    throw 'Run clearway-setup.exe (https://github.com/AW-HTK/clearway/releases/latest), or set CLEARWAY_SOURCE to a dist/ folder.'
}
$source = $env:CLEARWAY_SOURCE
$dir = if ($env:CLEARWAY_INSTALL_DIR) { $env:CLEARWAY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\clearway' }
$exe = Join-Path $dir 'clearway.exe'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

function Fetch([string]$name, [string]$to) {
    if ($source -match '^https?://') { Invoke-WebRequest -UseBasicParsing -Uri "$source/$name" -OutFile $to }
    else { Copy-Item (Join-Path $source $name) $to }
}

# Entry of "Installed apps": a copy of the setup next to clearway.exe uninstalls (--uninstall) or updates (Modify).
function Register-Uninstall([string]$setup) {
    $copy = Join-Path $dir 'clearway-setup.exe'
    if ([IO.Path]::GetFullPath($setup) -ne [IO.Path]::GetFullPath($copy)) {
        $next = Join-Path $dir '.clearway-setup.new.exe'
        Copy-Item $setup $next -Force
        if (Test-Path $copy) {
            Get-ChildItem $dir -Filter 'clearway-setup.old-*.exe' | ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
            Rename-Item $copy ("clearway-setup.old-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date))
        }
        Rename-Item $next 'clearway-setup.exe'
    }
    $name = if ($env:CLEARWAY_UNINSTALL_KEY) { $env:CLEARWAY_UNINSTALL_KEY } else { 'Clearway' }
    $key = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$name"
    New-Item -Path $key -Force | Out-Null
    $size = [int]((Get-ChildItem $dir -File | Measure-Object Length -Sum).Sum / 1KB)
    $values = [ordered]@{
        DisplayName = 'Clearway'; DisplayVersion = $version; Publisher = 'AW-HTK'; DisplayIcon = "$exe,0"
        InstallLocation = $dir; URLInfoAbout = 'https://github.com/AW-HTK/clearway'
        UninstallString = "`"$copy`" --uninstall"; QuietUninstallString = "`"$copy`" --uninstall --yes"
        ModifyPath = "`"$copy`""; InstallDate = (Get-Date -Format 'yyyyMMdd')
    }
    foreach ($v in $values.GetEnumerator()) { New-ItemProperty -Path $key -Name $v.Key -Value $v.Value -PropertyType String -Force | Out-Null }
    New-ItemProperty -Path $key -Name EstimatedSize -Value $size -PropertyType DWord -Force | Out-Null
    New-ItemProperty -Path $key -Name NoRepair -Value 1 -PropertyType DWord -Force | Out-Null
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
    # Left by an uninstallation that found the file locked.
    Get-ChildItem $dir -Filter '*.deleted' | ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
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
    # Font licences travel with the setup, including when installing offline.
    if ($env:CLEARWAY_SOURCE -and $env:CLEARWAY_SOURCE -notmatch '^https?://' -and (Test-Path (Join-Path $env:CLEARWAY_SOURCE 'FONT-LICENSES.txt'))) {
        Copy-Item (Join-Path $env:CLEARWAY_SOURCE 'FONT-LICENSES.txt') (Join-Path $dir 'FONT-LICENSES.txt') -Force
    }
    Say "clearway.exe $version installed in $dir." "clearway.exe $version installé dans $dir."
    if ($env:CLEARWAY_SETUP_EXE) { Register-Uninstall $env:CLEARWAY_SETUP_EXE }
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

$pipe = if ($env:CLEARWAY_PIPE) { $env:CLEARWAY_PIPE } else { 'clearway' }
function Send-Pipe([string]$request) {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut); $p.Connect(1000)
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
