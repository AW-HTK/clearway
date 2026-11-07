﻿# Test of clearway.exe on real Windows, PowerShell client on the named pipe (no WSL), in test mode:
# no balloon, no message box, hidden window. The exe is copied to %LOCALAPPDATA%\clearway-test\bin and runs with its
# own state dir and pipe; the installed Clearway is never touched. The tray icon only shows with -WithIcon (step 11).
# Usage: windows-api.ps1 [-SourceExe <built clearway.exe>] [-WithIcon]
param([string]$SourceExe, [switch]$WithIcon)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$dir = Join-Path $env:LOCALAPPDATA 'clearway-test'
$dir2 = Join-Path $env:LOCALAPPDATA 'clearway-test-pipe'
$exe = Join-Path $dir 'bin\clearway.exe'
$pipe = 'clearway-test'
$takenPipe = 'clearway-test-taken'
$script:failures = 0

function Check($what, [bool]$ok) { if ($ok) { "OK    $what" } else { "FAILED $what"; $script:failures++ } }
function Step($t) { ''; "=== $t" }
# Only processes started from the test copy: an installed Clearway running alongside is neither counted nor stopped.
function TestProcesses { @(Get-Process clearway -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }) }
function Instances { (TestProcesses).Count }
function Log { Get-Content (Join-Path $dir 'clearway.log') -Raw -Encoding UTF8 }
function Call($method, $path, $body, $params) {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut)
    $p.Connect(5000)
    $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
    $r = [IO.StreamReader]::new($p, [Text.UTF8Encoding]::new($false))
    if ($null -eq $params) { $params = @{} }
    $w.WriteLine((@{ method = $method; path = $path; params = $params; body = $body } | ConvertTo-Json -Compress -Depth 6))
    $line = $r.ReadLine()
    $p.Dispose()
    $rep = $line | ConvertFrom-Json
    [pscustomobject]@{ code = [int]$rep.code; json = $rep.body }
}
function Start-Clearway($options) {
    $p = Start-Process -FilePath $exe -ArgumentList (@('serve', '--dir', "`"$dir`"") + $options) -PassThru
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) { try { Call GET '/status' $null | Out-Null; return $p } catch { Start-Sleep -Milliseconds 200 } }
    throw "no response on pipe $pipe"
}
function Stop-Clearway($p) {
    Call POST '/quit' $null | Out-Null
    $p.WaitForExit(10000) | Out-Null
}
function Ticket($requestId, $ram_mb, $description, $manual) {
    Call POST '/tickets' @{ request_id = $requestId; side = 'windows'; ram_mb = [int64]$ram_mb; cpu = 1; duration_s = 600; description = $description;
        session = 'powershell-test'; claude_session_id = $null; claude_pid = $null; dir = 'C:\'; signature = ''; measures = $null; manual = [bool]$manual }
}
function StateOf($id) { ((Call GET '/status' $null).json.tickets | Where-Object id -eq $id).state }
function Memory($p) {
    $p.Refresh()
    $perf = Get-CimInstance Win32_PerfFormattedData_PerfProc_Process | Where-Object IDProcess -eq $p.Id
    '{0:N1} MB working set, {1:N1} MB private resident, {2:N1} MB private commit, {3} threads' -f ($p.WorkingSet64 / 1MB), ($perf.WorkingSetPrivate / 1MB), ($p.PrivateMemorySize64 / 1MB), $p.Threads.Count
    $script:private = $perf.WorkingSetPrivate
}

TestProcesses | Stop-Process -Force
Start-Sleep 1
New-Item -ItemType Directory -Force $dir, $dir2, (Split-Path $exe) | Out-Null
if (-not $SourceExe) { $SourceExe = Join-Path $PSScriptRoot '..\target\x86_64-pc-windows-gnu\release\clearway.exe' }
if (Test-Path $SourceExe) { Copy-Item $SourceExe $exe -Force }
elseif (Test-Path $exe) { "Build not found at $SourceExe; reusing the test copy $exe" }
else { throw "clearway.exe not found: pass -SourceExe <path to the built clearway.exe>" }
Remove-Item (Join-Path $dir 'state.json'), (Join-Path $dir 'clearway.log'), (Join-Path $dir2 'clearway.log') -ErrorAction SilentlyContinue
Set-Content (Join-Path $dir 'config.toml') "pipe = `"$pipe`"`nlegacy_pipe = false`nlanguage = `"en`"`ndelay.silence = `"5s`"`ndelay.long_poll = `"20s`"" -Encoding ASCII
Set-Content (Join-Path $dir2 'config.toml') "pipe = `"$takenPipe`"`nlegacy_pipe = false`nlanguage = `"en`"" -Encoding ASCII
"Binary: {0:N0} bytes" -f (Get-Item $exe).Length

Step '1. Start in test mode'
$p = Start-Clearway @('--test')
Check 'one test instance' ((Instances) -eq 1)
Check 'log: START epoch 1 on the pipe' ((Log) -match "START epoch 1 on pipe $pipe")
Check 'unknown route: 404' ((Call GET '/nothing' $null).code -eq 404)

Step '2. Real measures'
Start-Sleep 3
$e = (Call GET '/status' $null).json
$m = $e.measures
'RAM available {0} / {1} MB - commit {2} / {3} MB - CPU {4:N1} % - vmmemWSL {5}' -f $m.ram_avail_mb, $m.ram_total_mb, $m.commit_mb, $m.commit_limit_mb, $m.cpu_pct, ($m.vmmem | ConvertTo-Json -Compress)
$os = Get-CimInstance Win32_OperatingSystem
Check 'available RAM consistent (gap < 1 GB)' ([math]::Abs($m.ram_avail_mb - $os.FreePhysicalMemory / 1024) -lt 1024)
$vm = Get-Process vmmemWSL -ErrorAction SilentlyContinue
if ($vm) { Check ("vmmemWSL: {0:N0} MB (Get-Process: {1:N0} MB)" -f $m.vmmem.rss_mb, ($vm.WorkingSet64 / 1MB)) ($m.vmmem.pid -eq $vm.Id -and [math]::Abs($m.vmmem.rss_mb - $vm.WorkingSet64 / 1MB) -lt ($vm.WorkingSet64 / 1MB * 0.1 + 64)) }

Step '3. Refusal, clearance, waiting, long-poll, release'
$r = Ticket 'r-too-big' (40 * 1024) 'too big'
Check "refused 422: $($r.json.reason.message)" ($r.code -eq 422)
$free = ($e.reserves | Where-Object key -eq 'ram_windows').free
$a = Ticket 'r-a' ($free * 0.7) 'A'
Check "A: $($a.json.reason.message)" ($a.json.state -eq 'active')
$b = Ticket 'r-b' ($free * 0.5) 'B'
Check "B: $($b.json.reason.message)" ($b.json.state -eq 'waiting')
$z = $b.json.reason
$known = '{0}-{1}-{2}-{3}-{4}' -f $z.decision, $(if ($z.position) { $z.position } else { 0 }), $(if ($z.reserve) { $z.reserve } else { 'x' }), ($z.blockers -join '_'), $(if ($z.unblocked_at) { $z.unblocked_at } else { 0 })
$job = Start-Job -ArgumentList $pipe, $b.json.id, $known {
    param($pipe, $id, $known)
    $t0 = Get-Date
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut)
    $p.Connect(5000)
    $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
    $w.WriteLine((@{ method = 'GET'; path = "/tickets/$id/wait"; params = @{ known = $known }; body = $null } | ConvertTo-Json -Compress))
    $l = [IO.StreamReader]::new($p, [Text.UTF8Encoding]::new($false)).ReadLine()
    [pscustomobject]@{ s = ((Get-Date) - $t0).TotalSeconds; l = $l }
}
Start-Sleep 6
Check 'A released' ((Call POST "/tickets/$($a.json.id)/release" @{ code = 0 }).code -eq 200)
Wait-Job $job -Timeout 30 | Out-Null
$res = Receive-Job $job
$v = ($res.l | ConvertFrom-Json).body
Check ("long-poll of B held {0:N1} s then woken by the release of A: {1}" -f $res.s, $v.reason.message) ($v.state -eq 'active' -and $res.s -ge 1 -and $res.s -lt 15)
Call POST "/tickets/$($b.json.id)/release" @{} | Out-Null

Step '4. Orphan, freeze, back on presence'
$c = Ticket 'r-c' 1024 'C'
Start-Sleep 9
Check 'C orphaned after 5 s without presence' ((StateOf $c.json.id) -eq 'orphaned')
$d = Ticket 'r-d' 1024 'D'
Check "D frozen: $($d.json.reason.message)" ($d.json.reason.decision -eq 'frozen')
Check 'C active again on presence' ((Call POST "/tickets/$($c.json.id)/presence" @{}).json.state -eq 'active')
Check 'D cleared as soon as the freeze lifts' ((StateOf $d.json.id) -eq 'active')
Call DELETE "/tickets/$($c.json.id)" $null | Out-Null
Call DELETE "/tickets/$($d.json.id)" $null | Out-Null

Step '5. Manual freeze'
Call POST '/freeze' @{ frozen = $true } | Out-Null
$g = Ticket 'r-g' 1024 'G'
Check "frozen: $($g.json.reason.message)" ($g.json.reason.decision -eq 'frozen')
Call POST '/freeze' @{ frozen = $false } | Out-Null
Check 'unfrozen: G cleared' ((StateOf $g.json.id) -eq 'active')
Call DELETE "/tickets/$($g.json.id)" $null | Out-Null

Step '6. Hard kill during an active ticket, resume'
$f = Ticket 'r-f' 1024 'F'
Stop-Process -Id $p.Id -Force
Start-Sleep 1
$onDisk = Get-Content (Join-Path $dir 'state.json') -Raw | ConvertFrom-Json
Check 'state.json holds F active' (($onDisk.tickets | Where-Object id -eq $f.json.id).state -eq 'active')
$p = Start-Clearway @('--test')
$e = (Call GET '/status' $null).json
Check 'epoch 2 and F resumed' ($e.epoch -eq 2 -and (($e.tickets | Where-Object id -eq $f.json.id).state -eq 'active'))
Call POST "/tickets/$($f.json.id)/release" @{} | Out-Null

Step '7. Second instance (named mutex)'
$p2 = Start-Process -FilePath $exe -ArgumentList 'serve', '--dir', "`"$dir`"", '--test' -PassThru
Check 'the copy exits with code 3' ($p2.WaitForExit(10000) -and $p2.ExitCode -eq 3)
Check 'log: Already running' ((Log) -match 'INFO Already running: Clearway is running on pipe')
Check 'a single test instance remains' ((Instances) -eq 1)

Step '8. Pipe already taken by another program'
$squatter = [IO.Pipes.NamedPipeServerStream]::new($takenPipe, [IO.Pipes.PipeDirection]::InOut, 1)
$p3 = Start-Process -FilePath $exe -ArgumentList 'serve', '--dir', "`"$dir2`"", '--test' -PassThru
$exited = $p3.WaitForExit(10000)
$squatter.Dispose()
$log2 = Get-Content (Join-Path $dir2 'clearway.log') -Raw -Encoding UTF8
Check 'code 4 and clear message' ($exited -and $p3.ExitCode -eq 4 -and $log2 -match 'ERROR Pipe already taken')
$log2.Trim()

Step '9. Memory: without window, window open (hidden), window closed'
Start-Sleep 20
'without window: ' + (Memory $p); $without = $script:private
Call POST '/window' @{ open = $true } | Out-Null
Start-Sleep 6
'window open: ' + (Memory $p); $with = $script:private
Call POST '/window' @{ open = $false } | Out-Null
Start-Sleep 20
'window closed: ' + (Memory $p); $after = $script:private
Check 'private resident memory without window < 10 MB' ($without -lt 10MB)
Check 'test window closed (nothing left on screen)' ((Get-Process -Id $p.Id).MainWindowHandle -eq 0)

Step '10. Clean stop through the pipe'
Stop-Clearway $p
Check 'exit with code 0' ($p.HasExited -and $p.ExitCode -eq 0)
Check 'log: STOP' ((Log) -match 'STOP requested through the pipe')

if ($WithIcon) {
    Step '11. Tray icon (--test-icon): created, then removed on stop'
    $p = Start-Clearway @('--test-icon')
    Start-Sleep 3
    Check 'no tray icon error' (-not ((Log) -match 'ERROR tray icon'))
    Stop-Clearway $p
    Check 'clean stop (NIM_DELETE): code 0' ($p.HasExited -and $p.ExitCode -eq 0)
} else {
    Step '11. Tray icon: skipped (pass -WithIcon to show it for a few seconds)'
}

Step 'End'
Check 'no test instance left' ((Instances) -eq 0)
"Failures: $script:failures"
exit $script:failures
