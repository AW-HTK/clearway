# Replaces the running exe without letting a client relaunch the old one in between:
# first copy the new exe to clearway.new.exe in %LOCALAPPDATA%\Programs\clearway, then run this script.
# Running relays lock the exe: it is renamed (allowed while running) instead of overwritten.
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\clearway'
$exe = Join-Path $dir 'clearway.exe'
$new = Join-Path $dir 'clearway.new.exe'
if (-not (Test-Path $new)) { throw "Not found: $new" }
Get-ChildItem $dir -Filter 'clearway.old-*.exe' | ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
$old = Join-Path $dir ("clearway.old-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date))
# During the switch, clients wait instead of launching the exe themselves (they ignore a file older than 5 min).
$maintenance = Join-Path $env:LOCALAPPDATA 'clearway\maintenance'
Set-Content $maintenance (Get-Date -Format o)
try {
Rename-Item $exe $old
Move-Item $new $exe
"New exe in place; the old one stays as $(Split-Path $old -Leaf) until its relays end."
try {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', 'clearway', [IO.Pipes.PipeDirection]::InOut); $p.Connect(2000)
    $w = [IO.StreamWriter]::new($p); $w.AutoFlush = $true; $w.WriteLine('{"method":"POST","path":"/quit"}')
    [IO.StreamReader]::new($p).ReadLine() | Out-Null; $p.Dispose()
} catch { "No Clearway to stop." }
$deadline = (Get-Date).AddSeconds(10)
while ((Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'clearway.exe' -and $_.CommandLine -notmatch 'relay' }) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
Start-ScheduledTask -TaskName 'Clearway'
$deadline = (Get-Date).AddSeconds(15)
while (-not (Test-Path '\\.\pipe\clearway') -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 200 }
} finally {
    Remove-Item $maintenance -ErrorAction SilentlyContinue
}
Start-Sleep 2
Get-CimInstance Win32_Process | Where-Object { $_.Name -in 'clearway.exe', 'orchestrateur.exe' } |
    ForEach-Object { "{0}  {1}" -f $_.ProcessId, $(if ($_.CommandLine -match 'relay|relais') { 'relay' } else { 'server: ' + $_.ExecutablePath }) }
