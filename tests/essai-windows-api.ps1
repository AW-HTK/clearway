# Essai de orchestrateur.exe sous Windows réel, client PowerShell sur le tube nommé (sans WSL), en mode test :
# ni bulle, ni boîte, fenêtre cachée ; l'icône n'apparaît qu'à l'étape qui la vérifie, puis est retirée.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$exe = Join-Path $env:LOCALAPPDATA 'Programs\orchestrateur\orchestrateur.exe'
$dossier = Join-Path $env:LOCALAPPDATA 'orchestrateur-test'
$dossier2 = Join-Path $env:LOCALAPPDATA 'orchestrateur-test-tube'
$tube = 'orchestrateur-test'
$script:echecs = 0

function Verifie($quoi, [bool]$ok) { if ($ok) { "OK    $quoi" } else { "ÉCHEC $quoi"; $script:echecs++ } }
function Etape($t) { ''; "=== $t" }
function Instances { @(Get-Process orchestrateur -ErrorAction SilentlyContinue).Count }
function Journal { Get-Content (Join-Path $dossier 'journal.log') -Raw -Encoding UTF8 }
function Appel($methode, $chemin, $corps, $parametres) {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $tube, [IO.Pipes.PipeDirection]::InOut)
    $p.Connect(5000)
    $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
    $r = [IO.StreamReader]::new($p, [Text.UTF8Encoding]::new($false))
    if ($null -eq $parametres) { $parametres = @{} }
    $w.WriteLine((@{ methode = $methode; chemin = $chemin; corps = $corps; parametres = $parametres } | ConvertTo-Json -Compress -Depth 6))
    $ligne = $r.ReadLine()
    $p.Dispose()
    $rep = $ligne | ConvertFrom-Json
    [pscustomobject]@{ code = [int]$rep.code; json = $rep.corps }
}
function Lancer($options) {
    $p = Start-Process -FilePath $exe -ArgumentList (@('--dossier', "`"$dossier`"") + $options) -PassThru
    $fin = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $fin) { try { Appel GET '/etat' $null | Out-Null; return $p } catch { Start-Sleep -Milliseconds 200 } }
    throw "pas de réponse sur le tube $tube"
}
function Quitter($p) {
    Appel POST '/quitter' $null | Out-Null
    $p.WaitForExit(10000) | Out-Null
}
function Ticket($requete, $ram_mo, $description, $manuel) {
    Appel POST '/tickets' @{ requete_id = $requete; cote = 'windows'; ram_mo = [int64]$ram_mo; cpu = 1; duree_s = 600; description = $description;
        session = 'essai-powershell'; claude_session_id = $null; claude_pid = $null; dossier = 'C:\'; signature = ''; mesures = $null; manuel = [bool]$manuel }
}
function EtatDe($id) { ((Appel GET '/etat' $null).json.tickets | Where-Object id -eq $id).etat }
function Memoire($p) {
    $p.Refresh()
    $perf = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | Where-Object IDProcess -eq $p.Id
    '{0:N1} Mo de working set, {1:N1} Mo privés résidents, {2:N1} Mo de commit privé, {3} threads' -f ($p.WorkingSet64 / 1MB), ($perf.WorkingSetPrivate / 1MB), ($p.PrivateMemorySize64 / 1MB), $p.Threads.Count
    $script:prive = $perf.WorkingSetPrivate
}

Get-Process orchestrateur -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
Start-Sleep 1
New-Item -ItemType Directory -Force $dossier, $dossier2 | Out-Null
Remove-Item (Join-Path $dossier 'etat.json'), (Join-Path $dossier 'journal.log'), (Join-Path $dossier2 'journal.log') -ErrorAction SilentlyContinue
Set-Content (Join-Path $dossier 'config.toml') "tube = `"$tube`"`ndelai.actif = `"5s`"`ndelai.long_poll = `"20s`"" -Encoding ASCII
Set-Content (Join-Path $dossier2 'config.toml') 'tube = "orchestrateur-test-pris"' -Encoding ASCII
"Binaire : {0:N0} octets" -f (Get-Item $exe).Length

Etape '1. Démarrage en mode test'
$p = Lancer @('--test')
Verifie 'une instance' ((Instances) -eq 1)
Verifie 'journal : DÉMARRAGE époque 1 sur le tube' ((Journal) -match "DÉMARRAGE époque 1 sur le tube $tube")
Verifie 'route inconnue : 404' ((Appel GET '/rien' $null).code -eq 404)

Etape '2. Mesures réelles'
Start-Sleep 3
$e = (Appel GET '/etat' $null).json
$m = $e.mesures
'RAM disponible {0} / {1} Mo · commit {2} / {3} Mo · CPU {4:N1} % · vmmemWSL {5}' -f $m.ram_dispo_mo, $m.ram_totale_mo, $m.commit_mo, $m.commit_limite_mo, $m.cpu_pct, ($m.vmmem | ConvertTo-Json -Compress)
$os = Get-CimInstance Win32_OperatingSystem
Verifie 'RAM disponible cohérente (écart < 1 Go)' ([math]::Abs($m.ram_dispo_mo - $os.FreePhysicalMemory / 1024) -lt 1024)
$vm = Get-Process vmmemWSL -ErrorAction SilentlyContinue
if ($vm) { Verifie ("vmmemWSL : {0:N0} Mo (Get-Process : {1:N0} Mo)" -f $m.vmmem.rss_mo, ($vm.WorkingSet64 / 1MB)) ($m.vmmem.pid -eq $vm.Id -and [math]::Abs($m.vmmem.rss_mo - $vm.WorkingSet64 / 1MB) -lt ($vm.WorkingSet64 / 1MB * 0.1 + 64)) }

Etape '3. Refus, feu vert, attente, long-poll, fin'
$r = Ticket 'r-trop' (40 * 1024) 'trop gros'
Verifie "refus 422 : $($r.json.raison.phrase)" ($r.code -eq 422)
$libre = ($e.reserves | Where-Object cle -eq 'ram_windows').libre
$a = Ticket 'r-a' ($libre * 0.7) 'A'
Verifie "A : $($a.json.raison.phrase)" ($a.json.etat -eq 'actif')
$b = Ticket 'r-b' ($libre * 0.5) 'B'
Verifie "B : $($b.json.raison.phrase)" ($b.json.etat -eq 'attente')
$z = $b.json.raison
$cle = '{0}-{1}-{2}-{3}-{4}' -f $z.decision, $(if ($z.position) { $z.position } else { 0 }), $(if ($z.reserve) { $z.reserve } else { 'x' }), ($z.bloquants -join '_'), $(if ($z.debloque_vers) { $z.debloque_vers } else { 0 })
$job = Start-Job -ArgumentList $tube, $b.json.id, $cle {
    param($tube, $id, $cle)
    $t0 = Get-Date
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $tube, [IO.Pipes.PipeDirection]::InOut)
    $p.Connect(5000)
    $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
    $w.WriteLine((@{ methode = 'GET'; chemin = "/tickets/$id/attente"; parametres = @{ connu = $cle } } | ConvertTo-Json -Compress))
    $l = [IO.StreamReader]::new($p, [Text.UTF8Encoding]::new($false)).ReadLine()
    [pscustomobject]@{ s = ((Get-Date) - $t0).TotalSeconds; l = $l }
}
Start-Sleep 6
Verifie 'A terminé' ((Appel POST "/tickets/$($a.json.id)/fin" @{ code = 0 }).code -eq 200)
Wait-Job $job -Timeout 30 | Out-Null
$res = Receive-Job $job
$v = ($res.l | ConvertFrom-Json).corps
Verifie ("long-poll de B tenu {0:N1} s puis réveillé par la fin de A : {1}" -f $res.s, $v.raison.phrase) ($v.etat -eq 'actif' -and $res.s -ge 1 -and $res.s -lt 15)
Appel POST "/tickets/$($b.json.id)/fin" @{} | Out-Null

Etape '4. Orphelin, gel, retour au signal'
$c = Ticket 'r-c' 1024 'C'
Start-Sleep 9
Verifie 'C orphelin après 5 s sans signal' ((EtatDe $c.json.id) -eq 'orphelin')
$d = Ticket 'r-d' 1024 'D'
Verifie "D gelé : $($d.json.raison.phrase)" ($d.json.raison.decision -eq 'gel')
Verifie 'C de nouveau actif au signal' ((Appel POST "/tickets/$($c.json.id)/presence" @{}).json.etat -eq 'actif')
Verifie 'D accordé dès la levée du gel' ((EtatDe $d.json.id) -eq 'actif')
Appel DELETE "/tickets/$($c.json.id)" $null | Out-Null
Appel DELETE "/tickets/$($d.json.id)" $null | Out-Null

Etape '5. Gel manuel'
Appel POST '/gel' @{ gele = $true } | Out-Null
$g = Ticket 'r-g' 1024 'G'
Verifie "gelé : $($g.json.raison.phrase)" ($g.json.raison.decision -eq 'gel')
Appel POST '/gel' @{ gele = $false } | Out-Null
Verifie 'dégel : G accordé' ((EtatDe $g.json.id) -eq 'actif')
Appel DELETE "/tickets/$($g.json.id)" $null | Out-Null

Etape '6. Arrêt brutal pendant un ticket actif, reprise'
$f = Ticket 'r-f' 1024 'F'
Stop-Process -Id $p.Id -Force
Start-Sleep 1
$disque = Get-Content (Join-Path $dossier 'etat.json') -Raw | ConvertFrom-Json
Verifie 'etat.json contient F actif' (($disque.tickets | Where-Object id -eq $f.json.id).etat -eq 'actif')
$p = Lancer @('--test')
$e = (Appel GET '/etat' $null).json
Verifie 'époque 2 et F repris' ($e.epoque -eq 2 -and (($e.tickets | Where-Object id -eq $f.json.id).etat -eq 'actif'))
Appel POST "/tickets/$($f.json.id)/fin" @{} | Out-Null

Etape '7. Deuxième instance (mutex nommé)'
$p2 = Start-Process -FilePath $exe -ArgumentList '--dossier', "`"$dossier`"", '--test' -PassThru
Verifie 'la copie sort avec le code 3' ($p2.WaitForExit(10000) -and $p2.ExitCode -eq 3)
Verifie 'une seule instance reste' ((Instances) -eq 1)

Etape '8. Tube déjà pris par un autre programme'
$serveur = [IO.Pipes.NamedPipeServerStream]::new('orchestrateur-test-pris', [IO.Pipes.PipeDirection]::InOut, 1)
$p3 = Start-Process -FilePath $exe -ArgumentList '--dossier', "`"$dossier2`"", '--test' -PassThru
$sorti = $p3.WaitForExit(10000)
$serveur.Dispose()
$j2 = Get-Content (Join-Path $dossier2 'journal.log') -Raw -Encoding UTF8
Verifie 'code 4 et message clair' ($sorti -and $p3.ExitCode -eq 4 -and $j2 -match 'Tube déjà pris')
$j2.Trim()

Etape '9. Mémoire : sans fenêtre, fenêtre ouverte (cachée), fenêtre refermée'
Start-Sleep 20
'sans fenêtre : ' + (Memoire $p); $sans = $script:prive
Appel POST '/fenetre' @{ ouvrir = $true } | Out-Null
Start-Sleep 6
'fenêtre ouverte : ' + (Memoire $p); $avec = $script:prive
Appel POST '/fenetre' @{ ouvrir = $false } | Out-Null
Start-Sleep 20
'fenêtre refermée : ' + (Memoire $p); $apres = $script:prive
Verifie 'mémoire privée résidente sans fenêtre < 10 Mo' ($sans -lt 10MB)
Verifie "fenêtre de test fermée (rien ne reste à l'écran)" ((Get-Process -Id $p.Id).MainWindowHandle -eq 0)

Etape '10. Arrêt propre par le tube'
Quitter $p
Verifie 'sortie avec le code 0' ($p.HasExited -and $p.ExitCode -eq 0)
Verifie 'journal : ARRÊT' ((Journal) -match 'ARRÊT demandé par le tube')

Etape "11. Icône (--test-icone) : créée puis retirée à l'arrêt"
$p = Lancer @('--test-icone')
Start-Sleep 3
Verifie "pas d'erreur d'icône" (-not ((Journal) -match 'ERREUR icône'))
Quitter $p
Verifie 'arrêt propre (NIM_DELETE) : code 0' ($p.HasExited -and $p.ExitCode -eq 0)

Etape 'Fin'
Verifie 'plus aucune instance' ((Instances) -eq 0)
"Échecs : $script:echecs"
exit $script:echecs
