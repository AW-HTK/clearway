﻿# Opens the window of a test instance off screen (nothing visible) and saves it as PNG:
# window-capture.ps1 <output.png> [-SourceExe <built clearway.exe>]
# Uses the test copy %LOCALAPPDATA%\clearway-test\bin\clearway.exe, its own state dir and pipe; never the installed Clearway.
param([Parameter(Mandatory)][string]$Output, [string]$SourceExe)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string c, string t);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint f);
[DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
public struct RECT { public int L, T, R, B; }
'@
$dir = Join-Path $env:LOCALAPPDATA 'clearway-test'
$exe = Join-Path $dir 'bin\clearway.exe'
$pipe = 'clearway-test'
$Output = [IO.Path]::GetFullPath($Output)
function Call($m, $path, $body) {
  $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut); $p.Connect(5000)
  $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
  $w.WriteLine((@{ method = $m; path = $path; params = @{}; body = $body } | ConvertTo-Json -Compress -Depth 5))
  $l = [IO.StreamReader]::new($p).ReadLine(); $p.Dispose(); $l | ConvertFrom-Json
}
function Ticket($r, $ram, $d, $manual) { Call POST '/tickets' @{ request_id=$r; side='wsl'; ram_mb=[int64]$ram; cpu=2; duration_s=900; description=$d; session='capture-test'; claude_session_id=$null; claude_pid=$null; dir='/'; signature=''; measures=$null; manual=$manual } | Out-Null }
# The window of the test process only, even if the installed Clearway has its own open.
function TestWindow($procId) {
  $h = [IntPtr]::Zero
  while (($h = [W.U]::FindWindowExW([IntPtr]::Zero, $h, 'ClearwayWindow', [NullString]::Value)) -ne [IntPtr]::Zero) {
    $owner = [uint32]0; [W.U]::GetWindowThreadProcessId($h, [ref]$owner) | Out-Null
    if ($owner -eq $procId) { return $h }
  }
  [IntPtr]::Zero
}

Get-Process clearway -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process -Force
New-Item -ItemType Directory -Force (Split-Path $exe) | Out-Null
if ($SourceExe) { Copy-Item $SourceExe $exe -Force }
elseif (-not (Test-Path $exe)) { throw "no test copy at ${exe}: pass -SourceExe <path to the built clearway.exe>" }
Remove-Item (Join-Path $dir 'state.json') -ErrorAction SilentlyContinue
Set-Content (Join-Path $dir 'config.toml') "pipe = `"$pipe`"`nlegacy_pipe = false`nlanguage = `"en`"" -Encoding ASCII
$proc = Start-Process -FilePath $exe -ArgumentList 'serve', '--dir', "`"$dir`"", '--test' -PassThru
$deadline = (Get-Date).AddSeconds(10)
while ($true) {
  try { Call GET '/status' $null | Out-Null; break } catch { if ((Get-Date) -gt $deadline) { throw "no response on pipe $pipe" }; Start-Sleep -Milliseconds 200 }
}
Ticket 'c1' 2048 'Differential Infection run' $false
Ticket 'c2' 1536 'Godot headless: GUT suite' $true
Ticket 'c3' 9000 'docker compose up of the full stack' $false
Call POST '/window' @{ open = $true } | Out-Null
Start-Sleep 4
$h = TestWindow $proc.Id
"window found: $($h -ne [IntPtr]::Zero) / visible: $([W.U]::IsWindowVisible($h))"
if ($h -eq [IntPtr]::Zero) { Call POST '/quit' $null | Out-Null; $proc.WaitForExit(10000) | Out-Null; exit 1 }
$r = New-Object W.U+RECT; [W.U]::GetWindowRect($h, [ref]$r) | Out-Null
$bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
$g = [Drawing.Graphics]::FromImage($bmp); $dc = $g.GetHdc()
"PrintWindow: " + [W.U]::PrintWindow($h, $dc, 2)
$g.ReleaseHdc($dc); $bmp.Save($Output, [Drawing.Imaging.ImageFormat]::Png); $g.Dispose(); $bmp.Dispose()
Call POST '/quit' $null | Out-Null
$proc.WaitForExit(10000) | Out-Null
"exit code: $($proc.ExitCode)"
