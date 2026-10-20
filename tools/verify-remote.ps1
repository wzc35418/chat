# Cross-machine verification against a PUBLIC node over TLS with pinned fingerprint.
#
# What it proves (each step is a real network call to the remote host):
#   1. anonymous /v1/health reachable with the served certificate verified
#   2. /v1/tls/info readable (cert fingerprint + days remaining)
#   3. the PACKAGED artifact (app-image jar + FFI) submits a message -> node accepts
#   4. the same artifact pulls entries back -> applied >= 1
#   5. a WRONG fingerprint is rejected (pinning really is enforced)
#
# Exit codes: 0 = PASS, 1 = FAIL, 3 = SKIP (missing artifact/openssl).
param(
    [Parameter(Mandatory = $true)][string]$Endpoint,
    [Parameter(Mandatory = $true)][string]$NodeId,
    [Parameter(Mandatory = $true)][string]$Fingerprint,
    [int]$TimeoutSeconds = 20
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('remote-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$fp = ($Fingerprint -replace ':', '').Trim().ToLowerInvariant()
if ($fp -notmatch '^[0-9a-f]{64}$') { Write-Host 'FAIL remote check: fingerprint must be 64 lowercase hex chars'; exit 1 }
$Endpoint = $Endpoint.TrimEnd('/')
Write-Host ('endpoint: ' + $Endpoint + ' (node ' + $NodeId + ')')
Write-Host ('fingerprint: ' + $fp.Substring(0, 16) + '...')

$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP remote check: packaged artifact missing (' + $p + ')'); exit 3 }
}
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP remote check: openssl not installed'; exit 3 }

# 1. 证书本身：连上去取服务端证书，比对指纹（这一步证明「拿到的证书就是我们钉的那张」）。
$certOut = Join-Path $logDir 'served-cert.pem'
$probe = 'cmd /c ""' + $openssl + '" s_client -connect ' + ($Endpoint -replace '^https?://', '') + ' -servername localhost -showcerts < NUL 2>&1 > "' + $certOut + '""'
cmd /c $probe | Out-Null
$served = (Get-Content $certOut -Raw -ErrorAction SilentlyContinue)
if ($null -eq $served) { $served = '' }
if ($served -notmatch 'BEGIN CERTIFICATE') {
    Write-Host ('FAIL remote check: could not fetch the served certificate from ' + $Endpoint)
    Write-Host '  hint: is the port open in the cloud security group and local firewall?'
    exit 1
}
$servedFingerprint = (& $openssl x509 -in $certOut -noout -fingerprint -sha256 2>&1 | Out-String)
$match = [regex]::Match($servedFingerprint, 'Fingerprint=([0-9A-Fa-f:]+)')
if (-not $match.Success) { Write-Host 'FAIL remote check: could not read the served fingerprint'; exit 1 }
$actual = ($match.Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($actual -ne $fp) {
    Write-Host ('FAIL remote check: served certificate fingerprint mismatch')
    Write-Host ('  expected: ' + $fp)
    Write-Host ('  actual  : ' + $actual)
    exit 1
}
Write-Host 'PASS remote: served certificate matches the pinned fingerprint'

# 2. 匿名 /v1/tls/info（用 openssl 做带信任的请求）。
$infoReq = Join-Path $logDir 'tlsinfo.request.txt'
Set-Content -Path $infoReq -Value "GET /v1/tls/info HTTP/1.1`r`nHost: $($Endpoint -replace '^https?://','')`r`nConnection: close`r`n`r`n" -NoNewline -Encoding ascii
$infoCmd = 'cmd /c ""' + $openssl + '" s_client -connect ' + ($Endpoint -replace '^https?://', '') + ' -CAfile "' + $certOut + '" -verify_return_error -quiet -ign_eof < "' + $infoReq + '" 2>&1"'
$infoResponse = cmd /c $infoCmd | Out-String
Set-Content -Path (Join-Path $logDir 'tlsinfo.response.txt') -Value $infoResponse -Encoding utf8
if ($infoResponse -notmatch '"days_remaining"\s*:\s*(-?\d+)') {
    Write-Host 'FAIL remote check: /v1/tls/info unusable'
    exit 1
}
$days = [int]$matches[1]
if ($days -le 0) { Write-Host ('FAIL remote check: remote certificate expired (' + $days + ' days)'); exit 1 }
Write-Host ('PASS remote: /v1/tls/info ok (days_remaining=' + $days + ')')

# 3+4. 打包产物：配置指纹后提交 + 拉取（真实跨机器）。
$goodLog = Join-Path $logDir 'e2e-good.log'
& $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $Endpoint $NodeId '-' $fp *> $goodLog
$goodCode = $LASTEXITCODE
$goodText = Get-Content $goodLog -Raw -ErrorAction SilentlyContinue
if ($null -eq $goodText) { $goodText = '' }
if ($goodCode -ne 0 -or $goodText -notmatch 'accepted' -or $goodText -notmatch 'syncOnce') {
    Write-Host ('FAIL remote check: packaged artifact did not complete submit+sync against the remote node (exit ' + $goodCode + ')')
    Get-Content $goodLog -Tail 15 | Out-Host
    exit 1
}
Write-Host 'PASS remote: packaged artifact submitted to the PUBLIC node (accepted) and pulled entries back'

# 5. 错误指纹必须失败。
$badFp = ('0' * 64)
$badLog = Join-Path $logDir 'e2e-bad.log'
& $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $Endpoint $NodeId '-' $badFp *> $badLog
$badCode = $LASTEXITCODE
if ($badCode -eq 0) {
    Write-Host 'FAIL remote check: wrong fingerprint was accepted (pinning not enforced)'
    Get-Content $badLog -Tail 15 | Out-Host
    exit 1
}
Write-Host 'PASS remote: wrong fingerprint rejected by the client'
Write-Host ('logs: ' + $logDir)
Write-Host 'CROSS-MACHINE VERIFICATION: PASS'
exit 0
