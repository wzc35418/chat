# Offline (store-and-forward) delivery check.
#
# Runs THREE sequential processes against one real node:
#   1) bob-prepare  : bob registers + exports key package, then EXITS (offline)
#   2) alice-send   : alice creates the group, adds the offline bob as member,
#                     submits the invite and sends a message -> node accepts
#   3) bob-receive  : a NEW bob process joins from the handoff and pulls
#
# Because the sender and receiver processes never overlap, delivery can only come
# from the node's store-and-forward path, not from in-memory hand-off.
#
# Exit codes: 0 = PASS, 1 = FAIL.
param(
    [int]$Port = 8098,
    [int]$HealthTimeoutSeconds = 20
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('offline-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
$work = Join-Path $build ('offline-work\' + $stamp)
New-Item -ItemType Directory -Path $work -Force | Out-Null

$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) { if (-not (Test-Path $p)) { Write-Host ('FAIL offline check: missing ' + $p); exit 1 } }
$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) { Write-Host 'FAIL offline check: chat-node.exe missing'; exit 1 }

$seedFile = Join-Path $build 'offline-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
}
$dataDir = Join-Path $build ('offline-node-data-' + $stamp)
$nodeOut = Join-Path $logDir 'node.out.log'
$nodeErr = Join-Path $logDir 'node.err.log'
$node = Start-Process -FilePath $nodeExe -ArgumentList @('--bind', ('127.0.0.1:' + $Port), '--data-dir', $dataDir, '--seed-file', $seedFile, '--node-id', 'node-offline') -WorkingDirectory $root -PassThru -RedirectStandardOutput $nodeOut -RedirectStandardError $nodeErr
try {
    $healthy = $false
    for ($i = 0; $i -lt ($HealthTimeoutSeconds * 4); $i++) {
        Start-Sleep -Milliseconds 250
        try {
            $resp = Invoke-WebRequest -Uri ('http://127.0.0.1:' + $Port + '/v1/health') -TimeoutSec 2 -UseBasicParsing
            if ($resp.StatusCode -eq 200) { $healthy = $true; break }
        } catch { }
    }
    if (-not $healthy) { Write-Host 'FAIL offline check: node not healthy'; exit 1 }
    Write-Host ('PASS offline node: http://127.0.0.1:' + $Port + ' (node-offline)')

    $endpoint = 'http://127.0.0.1:' + $Port
    $steps = @(
        @{ Name = 'identities'; Mode = 'identities'; Expect = 'PASS identities created' },
        @{ Name = 'bob-prepare'; Mode = 'bob-prepare'; Expect = 'PASS bob prepared and goes OFFLINE' },
        @{ Name = 'alice-send'; Mode = 'alice-send'; Expect = 'PASS alice message accepted by node while bob was offline' },
        @{ Name = 'bob-receive'; Mode = 'bob-receive'; Expect = 'PASS bob received' }
    )
    foreach ($step in $steps) {
        $stepLog = Join-Path $logDir ($step.Name + '.log')
        & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.OfflineDeliverySmokeKt' $step.Mode $work $endpoint 'node-offline' *> $stepLog
        $code = $LASTEXITCODE
        $text = Get-Content $stepLog -Raw -ErrorAction SilentlyContinue
        if ($null -eq $text) { $text = '' }
        if ($code -ne 0 -or $text -notmatch [regex]::Escape($step.Expect)) {
            Write-Host ('FAIL offline check: step ' + $step.Name + ' (exit ' + $code + ')')
            Get-Content $stepLog -Tail 12 | Out-Host
            exit 1
        }
        Write-Host ('PASS offline step: ' + $step.Name)
    }

    Write-Host 'PASS offline delivery: message sent with receiver offline, delivered on next sync'
    Write-Host ('logs: ' + $logDir)
    exit 0
} finally {
    if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue }
}
