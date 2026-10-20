<#
Package-level end-to-end check: the PACKAGED artifact (app-image) submits a real
draft to a real local node and must get a settled acceptance.

Usage:
  powershell -ExecutionPolicy Bypass -File tools/verify-package-e2e.ps1
  powershell -ExecutionPolicy Bypass -File tools/verify-package-e2e.ps1 -SkipBuild
#>
param(
    [switch]$SkipBuild,
    [int]$HealthTimeoutSeconds = 40
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('e2e-logs\' + $stamp)
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$reportPath = Join-Path $build ('verify-package-e2e-' + $stamp + '.md')
$rows = @()
$failed = 0

function Add-Row($stage, $status, $seconds, $detail) {
    $script:rows += [pscustomobject]@{ Stage = $stage; Status = $status; Seconds = $seconds; Detail = $detail }
    Write-Host ('{0,-24} {1,-6} {2,8:N1}s  {3}' -f $stage, $status, $seconds, $detail)
}

# 1. build app-image
$t0 = Get-Date
if ($SkipBuild) {
    Add-Row 'build' 'SKIP' 0 '-SkipBuild'
} else {
    & cmd /c ('"' + $root + '\gradlew.bat" :apps:desktop:appImageWithNative --offline --console=plain --max-workers=1 > "' + $logDir + '\build.log" 2>&1')
    $elapsed = ((Get-Date) - $t0).TotalSeconds
    if (Test-Path (Join-Path $build 'dist\app-image\Chat\Chat.exe')) {
        Add-Row 'build' 'PASS' $elapsed (Join-Path $build 'dist\app-image\Chat')
    } else {
        Add-Row 'build' 'FAIL' $elapsed 'app-image missing'; $failed++
    }
}

$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) {
    if (-not (Test-Path $p)) { Write-Host ('MISSING: ' + $p); Add-Row 'structure' 'FAIL' 0 ('missing ' + $p); $failed++ }
}

# 2. start node
$t0 = Get-Date
$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) {
    & cmd /c ('"' + $root + '\gradlew.bat" :prepareRustHost --offline --console=plain --max-workers=1 > "' + $logDir + '\rusthost.log" 2>&1')
    & cmd /c ('cargo build --offline -p chat-node > "' + $logDir + '\nodebuild.log" 2>&1')
}
if (-not (Test-Path $nodeExe)) { Add-Row 'node' 'FAIL' 0 'chat-node.exe not built'; $failed++ }

$seedFile = Join-Path $build 'e2e-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
}
$dataDir = Join-Path $build ('e2e-node-data-' + $stamp)
$nodeOut = Join-Path $logDir 'node.out.log'
$nodeErr = Join-Path $logDir 'node.err.log'
$nodeArgList = @('--bind', '127.0.0.1:8099', '--data-dir', $dataDir, '--seed-file', $seedFile, '--node-id', 'node-e2e')
$node = Start-Process -FilePath $nodeExe -ArgumentList $nodeArgList -WorkingDirectory $root -PassThru -RedirectStandardOutput $nodeOut -RedirectStandardError $nodeErr
$healthy = $false
for ($i = 0; $i -lt ($HealthTimeoutSeconds * 4); $i++) {
    Start-Sleep -Milliseconds 250
    try {
        $resp = Invoke-WebRequest -Uri 'http://127.0.0.1:8099/v1/health' -TimeoutSec 2 -UseBasicParsing
        if ($resp.StatusCode -eq 200) { $healthy = $true; break }
    } catch { }
}
$elapsed = ((Get-Date) - $t0).TotalSeconds
if ($healthy) { Add-Row 'node' 'PASS' $elapsed 'http://127.0.0.1:8099 (node-e2e)' }
else { Add-Row 'node' 'FAIL' $elapsed 'health check failed'; $failed++; if (Test-Path $nodeErr) { Get-Content $nodeErr -Tail 5 | Out-Host } }

# 3. packaged artifact submits a real draft
if ($healthy) {
    $t0 = Get-Date
    $out = Join-Path $logDir 'e2e.log'
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' 'http://127.0.0.1:8099' 'node-e2e' *> $out
    $code = $LASTEXITCODE
    $elapsed = ((Get-Date) - $t0).TotalSeconds
    Get-Content $out | Out-Host
    if ($code -eq 0) { Add-Row 'e2e-submit' 'PASS' $elapsed 'settled accepted' }
    else { Add-Row 'e2e-submit' 'FAIL' $elapsed ('exit ' + $code); $failed++ }
} else {
    Add-Row 'e2e-submit' 'SKIP' 0 'node not healthy'
}

# 4. stop node
if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue; Wait-Process -Id $node.Id -Timeout 10 -ErrorAction SilentlyContinue }
Add-Row 'cleanup' 'PASS' 0 'node stopped'

$overall = if ($failed -eq 0) { 'PASS' } else { 'FAIL' }
$md = @('# verify-package-e2e report (' + $stamp + ')', '', ('- overall: **' + $overall + '**'), '')
$md += '| stage | status | seconds | detail |'
$md += '| --- | --- | --- | --- |'
foreach ($r in $rows) { $md += ('| ' + $r.Stage + ' | ' + $r.Status + ' | ' + [math]::Round($r.Seconds, 1) + ' | ' + $r.Detail + ' |') }
Set-Content -Path $reportPath -Value ($md -join "`n") -Encoding UTF8
Write-Host ''
Write-Host ('overall: ' + $overall)
Write-Host ('report:  ' + $reportPath)
if ($failed -eq 0) { exit 0 } else { exit 1 }
