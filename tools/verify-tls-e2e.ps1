# TLS end-to-end check for the PACKAGED artifact (real handshake + strict pinning).
#
# 1) start a node with TLS terminated in-process
# 2) packaged app connects over https with the CORRECT fingerprint -> accepted + applied
# 3) same app with a WRONG fingerprint -> must FAIL (this is the proof that pinning is real)
#
# Exit codes: 0 = PASS, 1 = FAIL, 3 = SKIP (openssl missing).
param(
    [int]$Port = 8096,
    [int]$HealthTimeoutSeconds = 20
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('tls-e2e-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP tls e2e: openssl not installed'; exit 3 }

$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) { if (-not (Test-Path $p)) { Write-Host ('FAIL tls e2e: missing ' + $p); exit 1 } }
$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) { Write-Host 'FAIL tls e2e: chat-node.exe missing'; exit 1 }
$cert = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-cert.pem'
$otherCert = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-other-cert.pem'
$key = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-key.pem'

function Get-Fingerprint([string]$pem) {
    $text = & $openssl x509 -in $pem -noout -fingerprint -sha256 2>&1 | Out-String
    $match = [regex]::Match($text, 'Fingerprint=([0-9A-Fa-f:]+)')
    if (-not $match.Success) { throw ('could not read fingerprint from ' + $pem) }
    return ($match.Groups[1].Value -replace ':', '').ToLowerInvariant()
}
$goodFingerprint = Get-Fingerprint $cert
$wrongFingerprint = Get-Fingerprint $otherCert
Write-Host ('fingerprint (served cert): ' + $goodFingerprint.Substring(0, 16) + '...')

$seedFile = Join-Path $build 'tls-e2e-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
}
$dataDir = Join-Path $build ('tls-e2e-node-data-' + $stamp)
$nodeOut = Join-Path $logDir 'node.out.log'
$nodeErr = Join-Path $logDir 'node.err.log'
$node = Start-Process -FilePath $nodeExe -ArgumentList @('--bind', ('127.0.0.1:' + $Port), '--data-dir', $dataDir, '--seed-file', $seedFile, '--node-id', 'node-tls-e2e', '--tls-cert', $cert, '--tls-key', $key) -WorkingDirectory $root -PassThru -RedirectStandardOutput $nodeOut -RedirectStandardError $nodeErr
try {
    $healthy = $false
    for ($i = 0; $i -lt ($HealthTimeoutSeconds * 4); $i++) {
        Start-Sleep -Milliseconds 250
        $probe = 'cmd /c ""' + $openssl + '" s_client -connect 127.0.0.1:' + $Port + ' -CAfile "' + $cert + '" -verify_return_error < NUL 2>&1"'
        $out = cmd /c $probe | Out-String
        if ($out -match 'Verify return code: 0') { $healthy = $true; break }
    }
    if (-not $healthy) { Write-Host 'FAIL tls e2e: node did not complete a TLS handshake'; exit 1 }
    Write-Host ('PASS tls e2e node: https://127.0.0.1:' + $Port)
    $endpoint = 'https://127.0.0.1:' + $Port

    # 1. 正确指纹 -> 必须成功（accepted + applied）
    $goodLog = Join-Path $logDir 'e2e-good.log'
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $endpoint 'node-tls-e2e' '-' $goodFingerprint *> $goodLog
    $goodCode = $LASTEXITCODE
    $goodText = Get-Content $goodLog -Raw -ErrorAction SilentlyContinue
    if ($null -eq $goodText) { $goodText = '' }
    if ($goodCode -ne 0 -or $goodText -notmatch 'accepted') {
        Write-Host ('FAIL tls e2e: correct fingerprint did not reach the node (exit ' + $goodCode + ')')
        Get-Content $goodLog -Tail 12 | Out-Host
        exit 1
    }
    Write-Host 'PASS tls e2e: correct fingerprint -> send accepted + sync applied over https'

    # 2. 错误指纹 -> 必须失败（否则 pinning 就是摆设）
    $badLog = Join-Path $logDir 'e2e-bad.log'
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $endpoint 'node-tls-e2e' '-' $wrongFingerprint *> $badLog
    $badCode = $LASTEXITCODE
    if ($badCode -eq 0) {
        Write-Host 'FAIL tls e2e: wrong fingerprint was accepted (pinning is not enforced)'
        Get-Content $badLog -Tail 12 | Out-Host
        exit 1
    }
    Write-Host 'PASS tls e2e: wrong fingerprint is rejected'
    Write-Host ('logs: ' + $logDir)
    exit 0
} finally {
    if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue }
}
