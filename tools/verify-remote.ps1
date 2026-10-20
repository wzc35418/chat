# Cross-machine verification against a PUBLIC node (TLS with pinned fingerprint, or plain HTTP).
#
# Proves, over the real network:
#   1. TCP reachability (distinguishes closed port from TLS/fingerprint errors)
#   2. served certificate fingerprint matches the pinned one (HTTPS mode)
#   3. /v1/tls/info readable (HTTPS mode)
#   4. the PACKAGED artifact submits a message -> node accepts, and pulls entries back
#   5. a WRONG fingerprint is rejected (HTTPS mode)
#
# Exit codes: 0 = PASS, 1 = FAIL, 3 = SKIP (missing artifact/openssl).
param(
    [Parameter(Mandatory = $true)][string]$Endpoint,
    [Parameter(Mandatory = $true)][string]$NodeId,
    [string]$Fingerprint = '',
    # 从文件读取指纹（避免手抄出错）：文件里第一处 64 位 hex 即可。
    [string]$FingerprintFile = '',
    # 只做连通性/证书诊断，不跑打包产物（快速排查用）。
    [switch]$DiagnoseOnly,
    [switch]$PlainHttp
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('remote-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$Endpoint = $Endpoint.TrimEnd('/')
if ($Endpoint -notmatch '^https?://') { Write-Host 'FAIL remote check: endpoint must start with http:// or https://'; exit 1 }
$isHttps = $Endpoint.StartsWith('https://', [System.StringComparison]::OrdinalIgnoreCase)
$tlsMode = ($isHttps -and -not $PlainHttp)

$fp = ''
if ($tlsMode) {
    if ($FingerprintFile -and (Test-Path $FingerprintFile)) {
        $fpText = Get-Content $FingerprintFile -Raw
        $fpMatch = [regex]::Match($fpText, '([0-9a-fA-F]{64})')
        if ($fpMatch.Success) { $Fingerprint = $fpMatch.Groups[1].Value }
    }
    $fp = ($Fingerprint -replace ':', '').Trim().ToLowerInvariant()
    if ($fp -notmatch '^[0-9a-f]{64}$') {
        Write-Host 'FAIL remote check: HTTPS endpoint needs -Fingerprint (64 lowercase hex). Use -PlainHttp only if the node has no TLS.'
        exit 1
    }
}
Write-Host ('endpoint: ' + $Endpoint + ' (node ' + $NodeId + ')')
if ($tlsMode) { Write-Host ('fingerprint: ' + $fp.Substring(0, 16) + '...') }

# 0. packaged artifact + openssl presence
$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP remote check: packaged artifact missing (' + $p + ')'); exit 3 }
}
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if ($tlsMode -and -not (Test-Path $openssl)) { Write-Host 'SKIP remote check: openssl not installed'; exit 3 }

# 1. TCP reachability
$hostPart = ($Endpoint -replace '^https?://', '').Split('/')[0]
$hostName = $hostPart.Split(':')[0]
$port = if ($hostPart.Contains(':')) { [int]$hostPart.Split(':')[1] } elseif ($isHttps) { 443 } else { 80 }
$reachable = $false
try { $reachable = Test-NetConnection -ComputerName $hostName -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue } catch { $reachable = $false }
if (-not $reachable) {
    Write-Host ('FAIL remote check: TCP ' + $hostName + ':' + $port + ' unreachable')
    Write-Host '  check order: (1) cloud security group inbound (2) ufw/firewalld (3) systemctl status chat-node (4) ss -ltnp'
    exit 1
}
Write-Host ('PASS remote: TCP ' + $hostName + ':' + $port + ' reachable')

if ($tlsMode) {
    # 2. served certificate fingerprint
    $certOut = Join-Path $logDir 'served-cert.pem'
    $probe = 'cmd /c ""' + $openssl + '" s_client -connect ' + $hostPart + ' -servername ' + $hostName + ' -showcerts < NUL 2>&1 > "' + $certOut + '""'
    cmd /c $probe | Out-Null
    $served = Get-Content $certOut -Raw -ErrorAction SilentlyContinue
    if ($null -eq $served) { $served = '' }
    if ($served -notmatch 'BEGIN CERTIFICATE') { Write-Host 'FAIL remote check: could not fetch the served certificate'; exit 1 }
    $printed = (& $openssl x509 -in $certOut -noout -fingerprint -sha256 2>&1 | Out-String)
    $m = [regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)')
    if (-not $m.Success) { Write-Host 'FAIL remote check: could not read served fingerprint'; exit 1 }
    $actual = ($m.Groups[1].Value -replace ':', '').ToLowerInvariant()
    if ($actual -ne $fp) {
        Write-Host 'FAIL remote check: served certificate fingerprint mismatch'
        Write-Host ('  expected: ' + $fp)
        Write-Host ('  actual  : ' + $actual)
        exit 1
    }
    Write-Host 'PASS remote: served certificate matches the pinned fingerprint'

    # 3. /v1/tls/info
    $infoReq = Join-Path $logDir 'tlsinfo.request.txt'
    $requestText = 'GET /v1/tls/info HTTP/1.1' + "`r`n" + 'Host: ' + $hostPart + "`r`n" + 'Connection: close' + "`r`n`r`n"
    Set-Content -Path $infoReq -Value $requestText -NoNewline -Encoding ascii
    $infoCmd = 'cmd /c ""' + $openssl + '" s_client -connect ' + $hostPart + ' -CAfile "' + $certOut + '" -verify_return_error -quiet -ign_eof < "' + $infoReq + '" 2>&1"'
    $infoResponse = cmd /c $infoCmd | Out-String
    Set-Content -Path (Join-Path $logDir 'tlsinfo.response.txt') -Value $infoResponse -Encoding utf8
    $daysMatch = [regex]::Match($infoResponse, '"days_remaining"\s*:\s*(-?[0-9]+)')
    if (-not $daysMatch.Success) { Write-Host 'FAIL remote check: /v1/tls/info unusable'; exit 1 }
    $days = [int]$daysMatch.Groups[1].Value
    if ($days -le 0) { Write-Host ('FAIL remote check: remote certificate expired (' + $days + ' days)'); exit 1 }
    Write-Host ('PASS remote: /v1/tls/info ok (days_remaining=' + $days + ')')
} else {
    Write-Host 'INFO remote: plain HTTP mode, skipping certificate/fingerprint checks'
}

if ($DiagnoseOnly) {
    Write-Host 'INFO remote: -DiagnoseOnly，跳过打包产物（连通性/证书已检查）'
    Write-Host ('logs: ' + $logDir)
    Write-Host 'REMOTE DIAGNOSE: PASS'
    exit 0
}

# 4. packaged artifact submit + pull
$goodLog = Join-Path $logDir 'e2e-good.log'
& $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $Endpoint $NodeId '-' $fp *> $goodLog
$goodCode = $LASTEXITCODE
$goodText = Get-Content $goodLog -Raw -ErrorAction SilentlyContinue
if ($null -eq $goodText) { $goodText = '' }
if ($goodCode -ne 0 -or $goodText -notmatch 'accepted' -or $goodText -notmatch 'syncOnce') {
    Write-Host ('FAIL remote check: packaged artifact did not complete submit+sync (exit ' + $goodCode + ')')
    Get-Content $goodLog -Tail 15 | Out-Host
    exit 1
}
Write-Host 'PASS remote: packaged artifact submitted (accepted) and pulled entries back'

if (-not $tlsMode) {
    Write-Host ('logs: ' + $logDir)
    Write-Host 'REMOTE CONNECTIVITY: PASS (plaintext)'
    exit 0
}

# 5. wrong fingerprint must fail
$badLog = Join-Path $logDir 'e2e-bad.log'
& $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $Endpoint $NodeId '-' ('0' * 64) *> $badLog
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
