# Capture the actual Windows UI in isolation. No installation, uninstallation or real workload.
param([Parameter(Mandatory)][string]$Project, [Parameter(Mandatory)][string]$Output)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -Namespace Preview -Name Native -MemberDefinition @'
[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowExW(IntPtr parent, IntPtr after, string c, string t);
[DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
[DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
[DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr dc, uint flags);
[DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint message, IntPtr w, IntPtr l);
public struct RECT { public int L, T, R, B; }
'@
[Preview.Native]::SetProcessDPIAware() | Out-Null
$id = [Guid]::NewGuid().ToString('N')
$dir = Join-Path ([IO.Path]::GetTempPath()) "clearway-preview-$id"
$pipe = "clearway-preview-$id"
$connections = @()
$server = $null
$setup = $null
New-Item -ItemType Directory -Force $dir, $Output | Out-Null
Copy-Item (Join-Path $Project 'dist\local\clearway.exe') $dir
Copy-Item (Join-Path $Project 'dist\local\clearway-setup.exe') $dir
Set-Content (Join-Path $dir 'config.toml') "pipe = `"$pipe`"`nlegacy_pipe = false`nlanguage = `"fr`"`nupdate.check = `"manual`"" -Encoding ASCII
$env:CLEARWAY_LANG = 'fr'

function Connect {
    $p = [IO.Pipes.NamedPipeClientStream]::new('.', $pipe, [IO.Pipes.PipeDirection]::InOut)
    $p.Connect(1000)
    $w = [IO.StreamWriter]::new($p, [Text.UTF8Encoding]::new($false)); $w.AutoFlush = $true
    @{ Pipe=$p; Writer=$w; Reader=[IO.StreamReader]::new($p) }
}
function Send($connection, $method, $path, $body) {
    $connection.Writer.WriteLine((@{ method=$method; path=$path; params=@{}; body=$body } | ConvertTo-Json -Compress -Depth 8))
    $answer = $connection.Reader.ReadLine() | ConvertFrom-Json
    if ($answer.code -ge 400) { throw "Preview API $path returned $($answer.code): $($answer.body | ConvertTo-Json -Compress -Depth 4)" }
    $answer
}
function Call($method, $path, $body) {
    $c = Connect
    try { Send $c $method $path $body } finally { $c.Pipe.Dispose() }
}
function Window($processId, $class) {
    $deadline = (Get-Date).AddSeconds(12)
    do {
        $h = [IntPtr]::Zero
        while (($h = [Preview.Native]::FindWindowExW([IntPtr]::Zero, $h, $class, [NullString]::Value)) -ne [IntPtr]::Zero) {
            $owner = [uint32]0
            [Preview.Native]::GetWindowThreadProcessId($h, [ref]$owner) | Out-Null
            if ($owner -eq $processId) { return $h }
        }
        Start-Sleep -Milliseconds 100
    } while ((Get-Date) -lt $deadline)
    throw "No preview window for PID $processId ($class)"
}
function Capture($h, $name) {
    $r = New-Object Preview.Native+RECT
    [Preview.Native]::GetWindowRect($h, [ref]$r) | Out-Null
    $bmp = New-Object Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
    $g = [Drawing.Graphics]::FromImage($bmp)
    $dc = $g.GetHdc()
    try { if (-not [Preview.Native]::PrintWindow($h, $dc, 2)) { throw 'PrintWindow failed' } }
    finally { $g.ReleaseHdc($dc) }
    try { $bmp.Save((Join-Path $Output $name), [Drawing.Imaging.ImageFormat]::Png) }
    finally { $g.Dispose(); $bmp.Dispose() }
    Write-Output "Captured $name ($($r.R - $r.L) x $($r.B - $r.T))"
}

try {
    $server = Start-Process (Join-Path $dir 'clearway.exe') -ArgumentList 'serve', '--dir', "`"$dir`"", '--test' -PassThru
    $deadline = (Get-Date).AddSeconds(12)
    while ($true) {
        try { Call GET '/status' $null | Out-Null; break }
        catch { if ((Get-Date) -gt $deadline) { throw }; Start-Sleep -Milliseconds 150 }
    }
    $examples = @(
        @{ session='Claude - apercu'; project='clearway'; branch='readme-refonte'; task='cargo test --locked + clippy'; ram=3072; cpu=4 },
        @{ session='Codex - apercu'; project='nuages-v2'; branch='feat/nuages-v3'; task='Tests backend meteo'; ram=5120; cpu=2 },
        @{ session='Claude - apercu'; project='rdev'; branch='feature/RDEV-816'; task='Verification des types'; ram=4096; cpu=2 }
    )
    $n = 0
    foreach ($e in $examples) {
        $n++
        $c = Connect; $connections += $c
        $measures = @{ mem_avail_mb=6553; mem_total_mb=12288; load1=1.5; at=[DateTimeOffset]::UtcNow.ToUnixTimeSeconds() }
        Send $c POST '/link' @{ identity="preview:$n"; session=$e.session; client='codex mcp 0.1.6'; measures=$measures } | Out-Null
        Send $c POST '/tickets' @{
            request_id="preview-$n"; side='wsl'; ram_mb=$e.ram; cpu=$e.cpu; duration_s=900;
            description=$e.task; session=$e.session; claude_session_id=$null; claude_pid=$null;
            dir='/preview'; signature=''; measures=$measures; manual=$true; project=$e.project; branch=$e.branch
        } | Out-Null
    }
    Call POST '/window' @{ open=$true } | Out-Null
    $mainWindow = Window $server.Id 'ClearwayWindow'
    Start-Sleep -Seconds 2
    Capture $mainWindow 'application.png'

    # --preview takes precedence over every other flag and prevents all setup actions.
    $setup = Start-Process (Join-Path $dir 'clearway-setup.exe') -ArgumentList '--preview' -PassThru
    $setupWindow = Window $setup.Id 'ClearwaySetup'
    Start-Sleep -Seconds 1
    Capture $setupWindow 'installateur.png'
    [Preview.Native]::PostMessage($setupWindow, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null
    Call POST '/quit' $null | Out-Null
} finally {
    foreach ($c in $connections) { $c.Pipe.Dispose() }
    foreach ($process in @($setup, $server)) {
        if ($null -ne $process -and -not $process.HasExited) {
            if (-not $process.WaitForExit(3000)) { $process.Kill(); $process.WaitForExit() }
        }
    }
    Write-Output "Preview state isolated in $dir; installed Clearway was not changed."
}
