# Ouvre la fenêtre d'une instance de test hors écran (rien de visible) et l'enregistre en PNG : capture-fenetre.ps1 <sortie.png>
param($sortie)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, string t);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
public struct RECT { public int L, T, R, B; }
'@
$exe = Join-Path $env:LOCALAPPDATA 'Programs\orchestrateur\orchestrateur.exe'
$dossier = Join-Path $env:LOCALAPPDATA 'orchestrateur-test'
$tube = 'orchestrateur-test'
function Appel($m, $c, $corps) {
  $p = [IO.Pipes.NamedPipeClientStream]::new('.', $tube, [IO.Pipes.PipeDirection]::InOut); $p.Connect(5000)
  $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
  $w.WriteLine((@{ methode = $m; chemin = $c; corps = $corps } | ConvertTo-Json -Compress -Depth 5))
  $l = [IO.StreamReader]::new($p).ReadLine(); $p.Dispose(); $l | ConvertFrom-Json
}
function Ticket($r, $ram, $d, $manuel) { Appel POST '/tickets' @{ requete_id=$r; cote='wsl'; ram_mo=[int64]$ram; cpu=2; duree_s=900; description=$d; session='essai-capture'; claude_session_id=$null; claude_pid=$null; dossier='/'; signature=''; mesures=$null; manuel=$manuel } | Out-Null }
$proc = Start-Process -FilePath $exe -ArgumentList '--dossier', "`"$dossier`"", '--test' -PassThru
Start-Sleep 2
Ticket 'c1' 2048 'Infection différentielle' $false
Ticket 'c2' 1536 'Godot headless : suite GUT' $true
Ticket 'c3' 9000 'docker compose up de la pile complète' $false
Appel POST '/fenetre' @{ ouvrir = $true } | Out-Null
Start-Sleep 4
$h = [W.U]::FindWindowW('OrchestrateurFenetre', [NullString]::Value)
"fenetre trouvee : $($h -ne [IntPtr]::Zero) / visible : $([W.U]::IsWindowVisible($h))"
if ($h -eq [IntPtr]::Zero) { Appel POST '/quitter' $null | Out-Null; exit 1 }
$r = New-Object W.U+RECT; [W.U]::GetWindowRect($h, [ref]$r) | Out-Null
$bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$g = [Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
"PrintWindow : " + [W.U]::PrintWindow($h, $dc, 2)
$g.ReleaseHdc($dc); $bmp.Save($sortie, [Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
Appel POST '/quitter' $null | Out-Null
$proc.WaitForExit(10000) | Out-Null
"code de sortie : $($proc.ExitCode)"
