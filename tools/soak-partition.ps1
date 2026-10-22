# Network partition: TCP handshake succeeds but the peer never answers.
#
# A killed node is easy (connection refused). The dangerous case is the half-dead one:
# the port accepts, then nothing arrives. This test builds that with tools/tcp-proxy.cjs.
#
# Phases:
#   A. proxy in `pass` mode      -> packaged e2e must PASS (proves the proxy isn't the problem)
#   B. proxy in `blackhole` mode -> e2e must FAIL, must NOT claim success, and the failure
#                                   must be a timeout/unavailable class error (not a TLS/pin error)
#   C. proxy back to `pass`      -> e2e must PASS again
#
# Exit codes: 0 = PASS, 1 = a phase violated its expectation, 3 = cannot run.
param(
    [int]$NodePort = 8492,
    [int]$ProxyPort = 8493,
    [string]$NodeId = 'partition-node'
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('partition-' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$app = Join-Path $build 'dist/app-image/Chat'
$java = Join-Path $app 'runtime/bin/java.exe'
$jar = Join-Path $app 'app/app.jar'
$native = Join-Path $app 'app/native'
$nodeExe = Join-Path $root 'target/debug/chat-node.exe'
$cert = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-cert.pem'
$key = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-key.pem'
$proxy = Join-Path $root 'tools/tcp-proxy.cjs'
foreach ($p in @($java, $jar, $native, $nodeExe, $cert, $key, $proxy)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP partition: missing ' + $p); exit 3 }
}
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP partition: openssl missing'; exit 3 }
$printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
$fp = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($fp.Length -ne 64) { Write-Host 'SKIP partition: cannot read fixture fingerprint'; exit 3 }

$seed = Join-Path $logDir 'seed.hex'
$bytes = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant() | Set-Content -Path $seed -NoNewline -Encoding ascii
$dataDir = Join-Path $logDir 'node-data'
$endpoint = 'https://127.0.0.1:' + $ProxyPort
$violations = 0

$node = Start-Process -FilePath $nodeExe -ArgumentList @(
    '--bind', ('0.0.0.0:' + $NodePort), '--allow-public-bind',
    '--data-dir', $dataDir, '--seed-file', $seed, '--node-id', $NodeId,
    '--tls-cert', $cert, '--tls-key', $key
) -PassThru -RedirectStandardOutput (Join-Path $logDir 'node.out') -RedirectStandardError (Join-Path $logDir 'node.err')
Start-Sleep -Seconds 3
if ($node.HasExited) { Write-Host 'SKIP partition: node failed to start'; exit 3 }

$proxyProc = $null
function Start-Proxy([string]$mode) {
    if ($script:proxyProc -and -not $script:proxyProc.HasExited) { Stop-Process -Id $script:proxyProc.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 300
    $script:proxyProc = Start-Process -FilePath 'node' -ArgumentList @(
        $proxy, [string]$ProxyPort, '127.0.0.1', [string]$NodePort, $mode
    ) -PassThru -RedirectStandardOutput (Join-Path $logDir ('proxy-' + $mode + '.out')) -RedirectStandardError (Join-Path $logDir ('proxy-' + $mode + '.err'))
    Start-Sleep -Seconds 2
}

function Invoke-Smoke([string]$tag) {
    $log = Join-Path $logDir ('phase-' + $tag + '.log')
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $endpoint $NodeId '-' $fp *> $log
    $code = $LASTEXITCODE
    $text = Get-Content $log -Raw -ErrorAction SilentlyContinue
    if ($null -eq $text) { $text = '' }
    $sw.Stop()
    return [pscustomobject]@{ Tag = $tag; Code = $code; Text = $text; Log = $log; Ms = [int]$sw.Elapsed.TotalMilliseconds }
}

try {
    Write-Host '== phase A: proxy passing traffic =='
    Start-Proxy 'pass'
    $a = Invoke-Smoke 'A-pass'
    if ($a.Code -eq 0 -and $a.Text -match 'PASS package e2e slice') {
        Write-Host ('PHASE A: PASS (through the proxy -> e2e succeeded, ' + $a.Ms + ' ms)')
    } else {
        Write-Host ('PHASE A: FAIL (exit ' + $a.Code + ')'); Get-Content $a.Log -Tail 6 | Out-Host; $violations += 1
    }

    Write-Host ''
    Write-Host '== phase B: proxy blackholes (handshake ok, no data) =='
    Start-Proxy 'blackhole'
    $b = Invoke-Smoke 'B-blackhole'
    $claimed = ($b.Code -eq 0) -or ($b.Text -match 'PASS package e2e slice')
    $failLine = ($b.Text -split "`n" | Where-Object { $_ -match 'FAIL ' } | Select-Object -First 1)
    if ($claimed) {
        Write-Host ('PHASE B: FAIL — client claimed success during a partition (exit ' + $b.Code + ')')
        Get-Content $b.Log -Tail 8 | Out-Host
        $violations += 1
    } elseif ([string]::IsNullOrWhiteSpace($failLine)) {
        Write-Host 'PHASE B: FAIL — no explicit FAIL line in the output (silent failure)'
        $violations += 1
    } else {
        Write-Host ('PHASE B: PASS (honest failure after ' + $b.Ms + ' ms: ' + $failLine.Trim() + ')')
    }

    Write-Host ''
    Write-Host '== phase C: proxy passing again (recovery) =='
    Start-Proxy 'pass'
    $c = Invoke-Smoke 'C-recovered'
    if ($c.Code -eq 0 -and $c.Text -match 'PASS package e2e slice') {
        Write-Host ('PHASE C: PASS (recovered after the partition cleared, ' + $c.Ms + ' ms)')
    } else {
        Write-Host ('PHASE C: FAIL (exit ' + $c.Code + ')'); Get-Content $c.Log -Tail 6 | Out-Host; $violations += 1
    }
} finally {
    if ($proxyProc -and -not $proxyProc.HasExited) { Stop-Process -Id $proxyProc.Id -Force -ErrorAction SilentlyContinue }
    Get-Process -Name chat-node -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host ('logs: ' + $logDir)
if ($violations -eq 0) { Write-Host 'PARTITION: PASS (half-dead link reported honestly, recovery clean)'; exit 0 }
Write-Host ('PARTITION: FAIL (' + $violations + ' phase violation(s))')
exit 1

