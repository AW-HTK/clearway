# Remplace l'exe en service sans laisser un client relancer l'ancien entre-temps :
# copier d'abord le nouvel exe en orchestrateur.nouveau.exe dans %LOCALAPPDATA%\Programs\orchestrateur, puis lancer ce script.
# Les relais en cours verrouillent l'exe : on le renomme (permis même en cours d'exécution) au lieu de l'écraser.
$ErrorActionPreference = 'Stop'
$dir = Join-Path $env:LOCALAPPDATA 'Programs\orchestrateur'
$exe = Join-Path $dir 'orchestrateur.exe'
$neuf = Join-Path $dir 'orchestrateur.nouveau.exe'
if (-not (Test-Path $neuf)) { throw "Introuvable : $neuf" }
Get-ChildItem $dir -Filter 'orchestrateur.ancien-*.exe' | ForEach-Object { Remove-Item $_.FullName -ErrorAction SilentlyContinue }
$ancien = Join-Path $dir ("orchestrateur.ancien-{0:yyyyMMdd-HHmmss}.exe" -f (Get-Date))
# Pendant la coupure, les clients attendent au lieu de lancer l'exe eux-mêmes (ils ignorent un fichier de plus de 5 min).
$maintenance = Join-Path $env:LOCALAPPDATA 'orchestrateur\maintenance'
Set-Content $maintenance (Get-Date -Format o)
try {
Rename-Item $exe $ancien
Move-Item $neuf $exe
"Nouvel exe en place ; l'ancien reste sous $(Split-Path $ancien -Leaf) le temps que ses relais se terminent."
try {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', 'orchestrateur', [IO.Pipes.PipeDirection]::InOut); $p.Connect(2000)
    $w = [IO.StreamWriter]::new($p); $w.AutoFlush = $true; $w.WriteLine('{"methode":"POST","chemin":"/quitter"}')
    [IO.StreamReader]::new($p).ReadLine() | Out-Null; $p.Dispose()
} catch { "Aucun orchestrateur à arrêter." }
$fin = (Get-Date).AddSeconds(10)
while ((Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'orchestrateur.exe' -and $_.CommandLine -notmatch 'relais' }) -and (Get-Date) -lt $fin) { Start-Sleep -Milliseconds 100 }
Start-ScheduledTask -TaskName 'Orchestrateur de charge'
$fin = (Get-Date).AddSeconds(15)
while (-not (Test-Path '\\.\pipe\orchestrateur') -and (Get-Date) -lt $fin) { Start-Sleep -Milliseconds 200 }
} finally {
    Remove-Item $maintenance -ErrorAction SilentlyContinue
}
Start-Sleep 2
Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'orchestrateur.exe' } |
    ForEach-Object { "{0}  {1}" -f $_.ProcessId, $(if ($_.CommandLine -match 'relais') { 'relais' } else { 'serveur : ' + $_.ExecutablePath }) }
