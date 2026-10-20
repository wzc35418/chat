# Cross-machine verification against a node (TLS with pinned fingerprint, TOFU, or plain HTTP).
#
# Checks, in order:
#   1. TCP reachability (distinguishes closed port from TLS/fingerprint errors)
#   2. served certificate fingerprint vs the pin (HTTPS)
#   3. /v1/health node_id equals -NodeId (wrong node detection)
#   4. /v1/tls/info readable (days remaining, not expired)
#   5. PACKAGED artifact submits a message (accepted) and pulls entries back
#   6. a WRONG fingerprint is rejected (pinning really enforced)
#
# Exit codes: 0 = PASS, 1 = FAIL, 3 = SKIP (missing artifact/openssl).
param(
    [Parameter(Mandatory = $true)][string]$Endpoint,
    [Parameter(Mandatory = $true)][string]$NodeId,
    [string]$Fingerprint = '',
    [string]$FingerprintFile = '',
    [switch]$DiagnoseOnly,
    [switch]$PlainHttp,
    [switch]$TrustServedCertificate
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('remote-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$Endpoint = $Endpoint.TrimEnd('/')
if ($Endpoint -notmatch '^https?://') { Write-Host 'FAIL remote check: endpoint must start with http:// or https://'; exit 1 }
$isHttps = $Endpoint.StartsWith('https://', [System.StringComparison]::OrdinalIgnoreCase)
$tlsMode = ($isHttps -and -not $PlainHttp)

$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP remote check: packaged artifact missing (' + $p + ')'); exit 3 }
}
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if ($tlsMode -and -not (Test-Path $openssl)) { Write-Host 'SKIP remote check: openssl not installed'; exit 3 }

$hostPart = ($Endpoint -replace '^https?://', '').Split('/')[0]
$hostName = $hostPart.Split(':')[0]
$port = if ($hostPart.Contains(':')) { [int]$hostPart.Split(':')[1] } elseif ($isHttps) { 443 } else { 80 }

$reachable = $false
try { $reachable = Test-NetConnection -ComputerName $hostName -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue } catch { $reachable = $false }
if (-not $reachable) {
    Write-Host ('FAIL remote check: TCP ' + $hostName + ':' + $port + ' unreachable')
    Write-Host '  check order: (1) cloud security group inbound (2) ufw/firewalld (3) node process running (4) ss -ltnp'
    exit 1
}
Write-Host ('PASS remote: TCP ' + $hostName + ':' + $port + ' reachable')
$fp = ($Fingerprint -replace ':', '').Trim().ToLowerInvariant()
if ($tlsMode -and $FingerprintFile -and (Test-Path $FingerprintFile)) {
    $fpText = Get-Content $FingerprintFile -Raw
    $fpMatch = [regex]::Match($fpText, '([0-9a-fA-F]{64})')
    if ($fpMatch.Success) { $fp = $fpMatch.Groups[1].Value.ToLowerInvariant() }
}

if ($tlsMode -and $fp -notmatch '^[0-9a-f]{64}$' -and -not $TrustServedCertificate) {
    Write-Host 'FAIL remote check: HTTPS needs -Fingerprint or -FingerprintFile (or -TrustServedCertificate for first bring-up)'
    exit 1
}

if ($tlsMode) {
    $certOut = Join-Path $logDir 'served-cert.pem'
    $probe = 'cmd /c ""' + $openssl + '" s_client -connect ' + $hostPart + ' -servername ' + $hostName + ' -showcerts < NUL 2>&1 > "' + $certOut + '""'
    cmd /c $probe | Out-Null
    $printed = (& $openssl x509 -in $certOut -noout -fingerprint -sha256 2>&1 | Out-String)
    $served = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
    if ($served -notmatch '^[0-9a-f]{64}$') { Write-Host 'FAIL remote check: could not fetch/parse the served certificate'; exit 1 }

    if ($fp -notmatch '^[0-9a-f]{64}$') {
        $fp = $served
        Write-Host '!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!'
        Write-Host 'WARNING: -TrustServedCertificate (TOFU): accepted the served certificate as the pin.'
        Write-Host ('         fingerprint = ' + $fp)
        Write-Host '         This is NOT pinning. OK for first bring-up; verify manually before production.'
        Write-Host '!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!'
    } elseif ($served -ne $fp) {
        Write-Host 'FAIL remote check: served certificate fingerprint mismatch'
        Write-Host ('  expected: ' + $fp)
        Write-Host ('  actual  : ' + $served)
        exit 1
    }
    Write-Host ('PASS remote: served certificate matches the pin (' + $fp.Substring(0, 16) + '...)')

    $healthReq = Join-Path $logDir 'health.request.txt'
    $healthText = 'GET /v1/health HTTP/1.1' + "`r`n" + 'Host: ' + $hostPart + "`r`n" + 'Connection: close' + "`r`n`r`n"
    Set-Content -Path $healthReq -Value $healthText -NoNewline -Encoding ascii
    $healthCmd = 'cmd /c ""' + $openssl + '" s_client -connect ' + $hostPart + ' -CAfile "' + $certOut + '" -verify_return_error -quiet -ign_eof < "' + $healthReq + '" 2>&1"'
    $healthBody = cmd /c $healthCmd | Out-String
    Set-Content -Path (Join-Path $logDir 'health.response.txt') -Value $healthBody -Encoding utf8
    if ($healthBody -match '"(?:node_id|nodeId)"\s*:\s*"([^"]+)"') {
        $remoteNodeId = $matches[1]
        if ($remoteNodeId -ne $NodeId) {
            Write-Host 'FAIL remote check: node identity mismatch'
            Write-Host ('  expected node_id: ' + $NodeId)
            Write-Host ('  actual node_id  : ' + $remoteNodeId)
            exit 1
        }
        Write-Host ('PASS remote: node identity matches (' + $remoteNodeId + ')')
    } else {
        Write-Host 'FAIL remote check: /v1/health did not expose node_id'
        exit 1
    }
$infoReq = Join-Path $logDir 'tlsinfo.request.txt'
    $infoText = 'GET /v1/tls/info HTTP/1.1' + "`r`n" + 'Host: ' + $hostPart + "`r`n" + 'Connection: close' + "`r`n`r`n"
    Set-Content -Path $infoReq -Value $infoText -NoNewline -Encoding ascii
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
    $plainBody = ''
    try { $plainBody = (Invoke-WebRequest -Uri ($Endpoint + '/v1/health') -TimeoutSec 10 -UseBasicParsing).Content } catch { $plainBody = '' }
    if ($plainBody -match '"(?:node_id|nodeId)"\s*:\s*"([^"]+)"') {
        if ($matches[1] -ne $NodeId) { Write-Host ('FAIL remote check: node identity mismatch (actual ' + $matches[1] + ')'); exit 1 }
        Write-Host ('PASS remote: node identity matches (' + $matches[1] + ')')
    } else {
        Write-Host 'FAIL remote check: plain /v1/health did not expose node_id'
        exit 1
    }
}

if ($DiagnoseOnly) {
    Write-Host 'INFO remote: -DiagnoseOnly, skipping packaged artifact'
    Write-Host ('logs: ' + $logDir)
    Write-Host 'REMOTE DIAGNOSE: PASS'
    exit 0
}

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
