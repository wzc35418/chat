<#
Two-device demo: two independent identities/DBs on ONE machine, real node.

  powershell -ExecutionPolicy Bypass -File tools/demo-two-devices.ps1
  powershell -ExecutionPolicy Bypass -File tools/demo-two-devices.ps1 -SkipBuild
#>
param([switch]$SkipBuild)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logs = Join-Path $build ('two-device-logs\' + $stamp)
$work = Join-Path $build ('two-device\' + $stamp)
New-Item -ItemType Directory -Force -Path $logs, $work | Out-Null
$failed = 0
function Step($name, $ok, $detail) {
    $status = if ($ok) { 'PASS' } else { 'FAIL' }
    Write-Host ('{0,-14} {1,-6} {2}' -f $name, $status, $detail)
    if (-not $ok) { $script:failed++ }
}

# 1) build app image (skip with -SkipBuild)
if ($SkipBuild) { Step 'build' $true '-SkipBuild' }
else {
    & cmd /c ('"' + $root + '\gradlew.bat" :apps:desktop:appImageWithNative --offline --console=plain --max-workers=1 > "' + $logs + '\build.log" 2>&1')
    Step 'build' (Test-Path (Join-Path $build 'dist\app-image\Chat\Chat.exe')) 'app-image'
}
$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
if (-not (Test-Path $java)) { Write-Host 'MISSING packaged runtime'; exit 1 }

# 2) start node
$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) { & cmd /c ('cargo build --offline -p chat-node > "' + $logs + '\nodebuild.log" 2>&1') }
$seedFile = Join-Path $build 'demo-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
}
$dataDir = Join-Path $logs 'node-data'
$node = Start-Process -FilePath $nodeExe -ArgumentList @('--bind','127.0.0.1:8099','--data-dir',$dataDir,'--seed-file',$seedFile,'--node-id','node-e2e') -PassThru -RedirectStandardOutput (Join-Path $logs 'node.out') -RedirectStandardError (Join-Path $logs 'node.err')
$healthy = $false
for ($i = 0; $i -lt 160; $i++) {
    Start-Sleep -Milliseconds 250
    try { $r = Invoke-WebRequest 'http://127.0.0.1:8099/v1/health' -UseBasicParsing -TimeoutSec 2; if ($r.StatusCode -eq 200) { $healthy = $true; break } } catch { }
}
Step 'node' $healthy 'http://127.0.0.1:8099'

$endpoint = 'http://127.0.0.1:8099'
$nodeId = 'node-e2e'
function Run-Smoke($mode, $logName) {
    $out = Join-Path $logs $logName
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.TwoDeviceSmokeKt' $mode $work $endpoint $nodeId *> $out
    return $LASTEXITCODE
}

# 3) identities
Step 'identities' ((Run-Smoke 'identities' 'identities.log') -eq 0) (Join-Path $logs 'identities.log')

# 4) bob waits for the invite in the background
$bobLog = Join-Path $logs 'bob.log'
$bob = Start-Process -FilePath $java -ArgumentList @(('-Djna.library.path=' + $native), '-cp', $jar, 'chat.desktop.TwoDeviceSmokeKt', 'bob-wait', $work, $endpoint, $nodeId) -PassThru -RedirectStandardOutput $bobLog -RedirectStandardError (Join-Path $logs 'bob.err')
$kp = Join-Path $work 'bob.kp'
for ($i = 0; $i -lt 240; $i++) { Start-Sleep -Milliseconds 250; if (Test-Path $kp) { break } }
Step 'bob-keypackage' (Test-Path $kp) 'bob.kp'

# 5) alice creates the group, registers, hands off the invite, sends a message
Step 'alice' ((Run-Smoke 'alice' 'alice.log') -eq 0) (Join-Path $logs 'alice.log')

# 6) bob joins and pulls
$bob.WaitForExit(120000) | Out-Null
$bobCode = if ($bob.HasExited) { $bob.ExitCode } else { Stop-Process -Id $bob.Id -Force; -1 }
Step 'bob-join-pull' ($bobCode -eq 0) ('exit ' + $bobCode + ' (see ' + $bobLog + ')')

if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue }
Write-Host ''
foreach ($name in @('alice.log','bob.log')) {
    $path = Join-Path $logs $name
    if (Test-Path $path) { Write-Host ('--- ' + $name + ' ---'); Get-Content $path | Select-Object -Last 12 | Out-Host }
}
Write-Host ''
if ($failed -eq 0) { Write-Host 'two-device demo: PASS'; exit 0 } else { Write-Host ('two-device demo: FAIL (' + $failed + ' steps)'); exit 1 }
