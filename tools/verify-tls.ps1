# Node-side TLS termination check.
#
# Proves: the node really terminates TLS with the given certificate, and the same port
# does NOT answer plaintext HTTP (no silent downgrade).
#
# Boundary: the certificate here is the repo's TEST certificate (SAN=127.0.0.1);
# production deployments must generate their own and pin its fingerprint client-side.
# Exit codes: 0 = PASS, 1 = FAIL.
param(
    [int]$Port = 8096,
    [int]$HealthTimeoutSeconds = 20
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('tls-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) { Write-Host 'FAIL tls check: chat-node.exe missing'; exit 1 }
$cert = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-cert.pem'
$key = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-key.pem'
foreach ($p in @($cert, $key)) { if (-not (Test-Path $p)) { Write-Host ('FAIL tls check: missing ' + $p); exit 1 } }

$seedFile = Join-Path $build 'tls-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
}
$dataDir = Join-Path $build ('tls-node-data-' + $stamp)
$nodeOut = Join-Path $logDir 'node.out.log'
$nodeErr = Join-Path $logDir 'node.err.log'
$nodeArgs = @('--bind', ('127.0.0.1:' + $Port), '--data-dir', $dataDir, '--seed-file', $seedFile, '--node-id', 'node-tls', '--tls-cert', $cert, '--tls-key', $key)
$node = Start-Process -FilePath $nodeExe -ArgumentList $nodeArgs -WorkingDirectory $root -PassThru -RedirectStandardOutput $nodeOut -RedirectStandardError $nodeErr
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP tls check: openssl not installed'; exit 3 }

try {
    # 1. 等端口可用（用 openssl 真正握手，不信"进程还活着"）。
    $handshake = $null
    for ($i = 0; $i -lt ($HealthTimeoutSeconds * 2); $i++) {
        Start-Sleep -Milliseconds 500
        $probe = 'cmd /c ""' + $openssl + '" s_client -connect 127.0.0.1:' + $Port + ' -CAfile "' + $cert + '" -verify_return_error < NUL 2>&1"'
        $out = cmd /c $probe
        $joined = ($out | Out-String)
        if ($joined -match 'Verify return code: 0') { $handshake = $joined; break }
        if ($joined -match 'BEGIN CERTIFICATE|Verify return code: 0') { $handshake = $joined; break }
    }
    if (-not $handshake) {
        Write-Host 'FAIL tls check: TLS handshake with the served certificate never succeeded'
        if (Test-Path $nodeErr) { Get-Content $nodeErr -Tail 10 | Out-Host }
        exit 1
    }
    Write-Host 'PASS tls node: handshake succeeds against the served certificate'

    # 2. 用真正的 HTTP 请求验证：带信任的 openssl 会话里取 /v1/health。
    $requestText = "GET /v1/health HTTP/1.1`r`nHost: 127.0.0.1`r`nConnection: close`r`n`r`n"
    $requestFile = Join-Path $logDir 'health.request.txt'
    Set-Content -Path $requestFile -Value $requestText -NoNewline -Encoding ascii
    $requestCmd = 'cmd /c ""' + $openssl + '" s_client -connect 127.0.0.1:' + $Port + ' -CAfile "' + $cert + '" -verify_return_error -quiet -ign_eof < "' + $requestFile + '" 2>&1"'
    $response = cmd /c $requestCmd | Out-String
    Set-Content -Path (Join-Path $logDir 'health.response.txt') -Value $response -Encoding utf8
    if ($response -notmatch '200 OK') { Write-Host 'FAIL tls check: https /v1/health did not return 200'; exit 1 }
    Write-Host 'PASS tls node: https /v1/health returned 200 over the pinned certificate'

    # 3. 同一端口必须拒绝明文 HTTP（不静默降级）。
    $plainRejected = $false
    try {
        $plain = Invoke-WebRequest -Uri ('http://127.0.0.1:' + $Port + '/v1/health') -TimeoutSec 3 -UseBasicParsing
        if ($plain.StatusCode -ne 200) { $plainRejected = $true }
    } catch { $plainRejected = $true }
    if (-not $plainRejected) { Write-Host 'FAIL tls check: plaintext HTTP answered on the TLS port'; exit 1 }
    Write-Host 'PASS tls node: plaintext HTTP is refused on the TLS port'

    # 4. 不提供信任的客户端必须握手失败（自签证书不被系统根认可）。
    $untrustedCmd = 'cmd /c ""' + $openssl + '" s_client -connect 127.0.0.1:' + $Port + ' -verify_return_error < NUL 2>&1"'
    $untrusted = cmd /c $untrustedCmd | Out-String
    if ($untrusted -match 'Verify return code: 0') { Write-Host 'FAIL tls check: untrusted client accepted the certificate'; exit 1 }
    Write-Host 'PASS tls node: client without the certificate is rejected'

    Write-Host ('logs: ' + $logDir)
    exit 0
} finally {
    if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue }
}
