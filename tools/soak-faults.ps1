# Fault injection: does the client tell the truth when the node is DOWN?
#
# Phases:
#   A. node up     -> packaged e2e must PASS
#   B. node killed -> packaged e2e must FAIL (and must NOT claim success)
#   C. node back   -> packaged e2e must PASS again (recovery)
#
# Why: every other test runs with a healthy node. This one asserts the *failure* semantics:
# a dead node must produce a stable error, never a fake success.
#
# Exit codes: 0 = all phases behaved as required, 1 = a phase violated its expectation, 3 = cannot run.
param(
    [int]$Port = 8491,
    [string]$NodeId = 'fault-node'
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('faults-' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$app = Join-Path $build 'dist/app-image/Chat'
$java = Join-Path $app 'runtime/bin/java.exe'
$jar = Join-Path $app 'app/app.jar'
$native = Join-Path $app 'app/native'
$nodeExe = Join-Path $root 'target/debug/chat-node.exe'
$cert = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-cert.pem'
$key = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-key.pem'
foreach ($p in @($java, $jar, $native, $nodeExe, $cert, $key)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP faults: missing ' + $p); exit 3 }
}
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP faults: openssl missing (needed for the TLS fingerprint)'; exit 3 }
$printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
$fp = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($fp.Length -ne 64) { Write-Host 'SKIP faults: cannot read the fixture certificate fingerprint'; exit 3 }

$seed = Join-Path $logDir 'seed.hex'
$bytes = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant() | Set-Content -Path $seed -NoNewline -Encoding ascii
$dataDir = Join-Path $logDir 'node-data'
$endpoint = 'https://127.0.0.1:' + $Port

function Start-Node {
    $proc = Start-Process -FilePath $nodeExe -ArgumentList @(
        '--bind', ('0.0.0.0:' + $Port), '--allow-public-bind',
        '--data-dir', $dataDir, '--seed-file', $seed, '--node-id', $NodeId,
        '--tls-cert', $cert, '--tls-key', $key
    ) -PassThru -RedirectStandardOutput (Join-Path $logDir 'node.out') -RedirectStandardError (Join-Path $logDir 'node.err')
    Start-Sleep -Seconds 3
    return $proc
}

function Invoke-Smoke([string]$tag) {
    $log = Join-Path $logDir ('phase-' + $tag + '.log')
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $endpoint $NodeId '-' $fp *> $log
    $code = $LASTEXITCODE
    $text = Get-Content $log -Raw -ErrorAction SilentlyContinue
    if ($null -eq $text) { $text = '' }
    return [pscustomobject]@{ Tag = $tag; Code = $code; Text = $text; Log = $log }
}

$violations = 0
try {
    Write-Host ('== phase A: node UP (' + $endpoint + ') ==')
    $node = Start-Node
    if ($node.HasExited) { Write-Host 'SKIP faults: node failed to start'; exit 3 }
    $a = Invoke-Smoke 'A-up'
    if ($a.Code -eq 0 -and $a.Text -match 'PASS package e2e slice') {
        Write-Host 'PHASE A: PASS (healthy node -> e2e succeeded, as expected)'
    } else {
        Write-Host ('PHASE A: FAIL (healthy node but e2e failed, exit ' + $a.Code + ')')
        Get-Content $a.Log -Tail 8 | Out-Host
        $violations += 1
    }

    Write-Host ''
    Write-Host '== phase B: node KILLED (the interesting one) =='
    Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue
    Start-Sleep -Seconds 2
    $b = Invoke-Smoke 'B-down'
    $claimedSuccess = ($b.Code -eq 0) -or ($b.Text -match 'PASS package e2e slice')
    if ($claimedSuccess) {
        Write-Host ('PHASE B: FAIL — client claimed success while the node was down (exit ' + $b.Code + ')')
        Get-Content $b.Log -Tail 10 | Out-Host
        $violations += 1
    } else {
        $codeLine = ($b.Text -split "`n" | Where-Object { $_ -match 'FAIL ' } | Select-Object -First 1)
        Write-Host ('PHASE B: PASS (node down -> honest failure: ' + $codeLine.Trim() + ')')
    }

    Write-Host ''
    Write-Host '== phase C: node BACK (recovery) =='
    $node = Start-Node
    if ($node.HasExited) { Write-Host 'PHASE C: FAIL (node did not come back)'; $violations += 1 }
    $c = Invoke-Smoke 'C-recovered'
    if ($c.Code -eq 0 -and $c.Text -match 'PASS package e2e slice') {
        Write-Host 'PHASE C: PASS (node back -> e2e succeeded again)'
    } else {
        Write-Host ('PHASE C: FAIL (node back but e2e failed, exit ' + $c.Code + ')')
        Get-Content $c.Log -Tail 8 | Out-Host
        $violations += 1
    }
    Write-Host ''
} finally {
    Get-Process -Name chat-node -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host ('logs: ' + $logDir)
if ($violations -eq 0) { Write-Host 'FAULT INJECTION: PASS (honest failures, clean recovery)'; exit 0 }
Write-Host ('FAULT INJECTION: FAIL (' + $violations + ' phase violation(s))')
exit 1

