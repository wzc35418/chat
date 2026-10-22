# Storage fault: what happens when the node cannot write?
#
# Phases:
#   A. normal            -> packaged e2e must PASS (baseline)
#   B. node.db read-only -> e2e must NOT claim success (node cannot persist)
#   C. attribute cleared -> e2e must PASS again
#
# Why a read-only FILE and not a filled disk: filling a disk is destructive and slow;
# setting the read-only attribute makes the same write path fail and is trivially reversible.
#
# Exit codes: 0 = PASS, 1 = a phase violated its expectation, 3 = cannot run.
param(
    [int]$Port = 8494,
    [string]$NodeId = 'disk-node'
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('diskfault-' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$app = Join-Path $build 'dist/app-image/Chat'
$java = Join-Path $app 'runtime/bin/java.exe'
$jar = Join-Path $app 'app/app.jar'
$native = Join-Path $app 'app/native'
$nodeExe = Join-Path $root 'target/debug/chat-node.exe'
$cert = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-cert.pem'
$key = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-key.pem'
foreach ($p in @($java, $jar, $native, $nodeExe, $cert, $key)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP disk fault: missing ' + $p); exit 3 }
}
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP disk fault: openssl missing'; exit 3 }
$printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
$fp = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($fp.Length -ne 64) { Write-Host 'SKIP disk fault: cannot read fixture fingerprint'; exit 3 }

$seed = Join-Path $logDir 'seed.hex'
$bytes = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant() | Set-Content -Path $seed -NoNewline -Encoding ascii
$dataDir = Join-Path $logDir 'node-data'
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
$dbPath = Join-Path $dataDir 'node.db'
$endpoint = 'https://127.0.0.1:' + $Port
$violations = 0
$nodeProc = $null

function Start-Node([string]$tag) {
    if ($script:nodeProc -and -not $script:nodeProc.HasExited) { Stop-Process -Id $script:nodeProc.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Milliseconds 500
    $script:nodeProc = Start-Process -FilePath $nodeExe -ArgumentList @(
        '--bind', ('0.0.0.0:' + $Port), '--allow-public-bind',
        '--data-dir', $dataDir, '--seed-file', $seed, '--node-id', $NodeId,
        '--tls-cert', $cert, '--tls-key', $cert.Replace('test-cert.pem', 'test-key.pem')
    ) -PassThru -RedirectStandardOutput (Join-Path $logDir ('node-' + $tag + '.out')) -RedirectStandardError (Join-Path $logDir ('node-' + $tag + '.err'))
    Start-Sleep -Seconds 3
    return $script:nodeProc
}

function Invoke-Smoke([string]$tag) {
    $log = Join-Path $logDir ('phase-' + $tag + '.log')
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $endpoint $NodeId '-' $fp *> $log
    $code = $LASTEXITCODE
    $text = Get-Content $log -Raw -ErrorAction SilentlyContinue
    if ($null -eq $text) { $text = '' }
    return [pscustomobject]@{ Tag = $tag; Code = $code; Text = $text; Log = $log }
}

try {
    Write-Host '== phase A: normal (baseline) =='
    $null = Start-Node 'A'
    $a = Invoke-Smoke 'A-normal'
    if ($a.Code -eq 0 -and $a.Text -match 'PASS package e2e slice') {
        Write-Host 'PHASE A: PASS (baseline e2e works)'
    } else {
        Write-Host ('PHASE A: FAIL (exit ' + $a.Code + ')'); Get-Content $a.Log -Tail 6 | Out-Host; $violations += 1
    }

    Write-Host ''
    Write-Host '== phase B: node.db read-only (writes must fail) =='
    if ($script:nodeProc -and -not $script:nodeProc.HasExited) { Stop-Process -Id $script:nodeProc.Id -Force -ErrorAction SilentlyContinue }
    Start-Sleep -Seconds 2
    if (-not (Test-Path $dbPath)) { Write-Host 'PHASE B: SKIP (node.db not created yet)'; $violations += 0 } else {
        Set-ItemProperty -Path $dbPath -Name IsReadOnly -Value $true
        $null = Start-Node 'B'
        $b = Invoke-Smoke 'B-readonly'
        $claimed = ($b.Code -eq 0) -or ($b.Text -match 'PASS package e2e slice')
        # 允许两种如实结果：节点起不来（连接失败）或节点起来了但写入失败。
        $nodeDown = -not (Test-NetConnection -ComputerName '127.0.0.1' -Port $Port -InformationLevel Quiet -WarningAction SilentlyContinue)
        $failLine = ($b.Text -split "`n" | Where-Object { $_ -match 'FAIL ' } | Select-Object -First 1)
        if ($claimed) {
            Write-Host 'PHASE B: FAIL — client claimed success while the database was read-only'
            Get-Content $b.Log -Tail 8 | Out-Host
            $violations += 1
        } elseif ($nodeDown) {
            Write-Host 'PHASE B: PASS (node refused to start with a read-only database — honest, no false success)'
        } elseif ([string]::IsNullOrWhiteSpace($failLine)) {
            Write-Host 'PHASE B: FAIL — no explicit FAIL line (silent failure)'
            $violations += 1
        } else {
            Write-Host ('PHASE B: PASS (honest failure: ' + $failLine.Trim() + ')')
        }
    }

    Write-Host ''
    Write-Host '== phase C: attribute cleared (recovery) =='
    if (Test-Path $dbPath) { Set-ItemProperty -Path $dbPath -Name IsReadOnly -Value $false -ErrorAction SilentlyContinue }
    $null = Start-Node 'C'
    $c = Invoke-Smoke 'C-recovered'
    if ($c.Code -eq 0 -and $c.Text -match 'PASS package e2e slice') {
        Write-Host 'PHASE C: PASS (recovered after the write permission came back)'
    } else {
        Write-Host ('PHASE C: FAIL (exit ' + $c.Code + ')'); Get-Content $c.Log -Tail 8 | Out-Host; $violations += 1
    }
} finally {
    if (Test-Path $dbPath) { Set-ItemProperty -Path $dbPath -Name IsReadOnly -Value $false -ErrorAction SilentlyContinue }
    Get-Process -Name chat-node -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

Write-Host ''
Write-Host ('logs: ' + $logDir)
if ($violations -eq 0) { Write-Host 'DISK FAULT: PASS (no false success when the database cannot be written)'; exit 0 }
Write-Host ('DISK FAULT: FAIL (' + $violations + ' phase violation(s))')
exit 1

