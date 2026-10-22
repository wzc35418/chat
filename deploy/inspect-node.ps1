# Inspect a chat-node deployment: one command, one page, actionable.
#
# Two modes:
#   Remote (from any machine):  -Endpoint https://host:8443 [-Fingerprint <hex>]
#   Local  (on the server):     -Local [-BundleDir C:\chat-node]
#
# Exit codes: 0 = healthy, 1 = something needs attention, 3 = cannot check (missing tool).
param(
    [string]$Endpoint = '',
    [string]$Fingerprint = '',
    [switch]$Local,
    [string]$BundleDir = 'C:\chat-node',
    [int]$LogTailLines = 8
)
$ErrorActionPreference = 'Continue'
$issues = 0
function Line([string]$name, [string]$value) { Write-Host ($name.PadRight(22) + $value) }
function Bad([string]$name, [string]$value) { $script:issues++; Write-Host ($name.PadRight(22) + 'NEEDS ATTENTION: ' + $value) }

$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
$hasOpenssl = Test-Path $openssl

if ($Local) {
    Write-Host ('== local node inspection: ' + $BundleDir + ' ==')
    $exe = Join-Path $BundleDir 'chat-node.exe'
    $cert = Join-Path $BundleDir 'cert.pem'
    $seed = Join-Path $BundleDir 'seed.hex'
    $fpFile = Join-Path $BundleDir 'fingerprint.txt'
    foreach ($name in @('chat-node.exe', 'cert.pem', 'key.pem', 'seed.hex', 'fingerprint.txt')) {
        if (Test-Path (Join-Path $BundleDir $name)) { Line ('file ' + $name) 'present' } else { Bad ('file ' + $name) 'missing' }
    }
    $proc = Get-Process -Name 'chat-node' -ErrorAction SilentlyContinue
    if ($proc) { Line 'process' ('running pid=' + ($proc | Select-Object -First 1).Id) } else { Bad 'process' 'chat-node.exe is not running' }
    $listening = Get-NetTCPConnection -State Listen -ErrorAction SilentlyContinue | Where-Object { $_.LocalPort -eq 8443 }
    if ($listening) { Line 'listening on 8443' 'yes' } else { Bad 'listening on 8443' 'nothing is listening (firewall or node down)' }
    $task = Get-ScheduledTask -TaskName 'chat-node' -ErrorAction SilentlyContinue
    if ($task) { Line 'boot task' ('registered, state=' + $task.State) } else { Line 'boot task' 'NOT registered (node dies on reboot; run register-service.ps1)' }
    if ((Test-Path $cert) -and $hasOpenssl) {
        $printed = (& $openssl x509 -in $cert -noout -enddate 2>&1 | Out-String)
        $notAfter = ([regex]::Match($printed, 'notAfter=(.+)')).Groups[1].Value.Trim()
        if ($notAfter) { Line 'certificate notAfter' $notAfter }
        $fpPrinted = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
        $fp = ([regex]::Match($fpPrinted, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
        if ($fp) { Line 'fingerprint (sha256)' $fp }
        if (Test-Path $fpFile) {
            $declared = ([regex]::Match((Get-Content $fpFile -Raw), '([0-9a-fA-F]{64})')).Groups[1].Value.ToLowerInvariant()
            if ($declared -eq $fp) { Line 'fingerprint.txt' 'matches cert' } else { Bad 'fingerprint.txt' 'does NOT match cert (clients will fail)' }
        }
    }
    $db = Join-Path $BundleDir 'data\node.db'
    if (Test-Path $db) { Line 'database' (([math]::Round((Get-Item $db).Length / 1MB, 2)).ToString() + ' MB') } else { Line 'database' 'not created yet' }
    $log = Join-Path $BundleDir 'node.log'
    if (Test-Path $log) {
        Line 'log tail' ((Get-Content $log -Tail $LogTailLines) -join ' | ')
    } else {
        Line 'log tail' '(no node.log; run-node.ps1 prints to the console window)'
    }
    if ($issues -eq 0) { Write-Host ''; Write-Host 'INSPECT: HEALTHY'; exit 0 }
    Write-Host ''; Write-Host ('INSPECT: ' + $issues + ' issue(s) need attention')
    exit 1
}

if ([string]::IsNullOrWhiteSpace($Endpoint)) { Write-Host 'usage: inspect-node.ps1 -Endpoint https://host:8443 [-Fingerprint hex]  |  -Local -BundleDir C:\chat-node'; exit 3 }
$Endpoint = $Endpoint.TrimEnd('/')
$hostPart = ($Endpoint -replace '^https?://', '').Split('/')[0]
$hostName = $hostPart.Split(':')[0]
$port = if ($hostPart.Contains(':')) { [int]$hostPart.Split(':')[1] } else { 443 }
Write-Host ('== remote node inspection: ' + $Endpoint + ' ==')

$reachable = $false
try { $reachable = Test-NetConnection -ComputerName $hostName -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue } catch { $reachable = $false }
if (-not $reachable) { Bad 'tcp' ('port ' + $port + ' unreachable (security group / firewall / node down)') ; Write-Host ''; Write-Host 'INSPECT: 1 issue(s) need attention'; exit 1 }
Line 'tcp' ('reachable')
if (-not $hasOpenssl) { Write-Host 'INSPECT: cannot check TLS (openssl missing)'; exit 3 }

$certOut = Join-Path $env:TEMP ('inspect-' + [guid]::NewGuid().ToString('N') + '.pem')
$probe = 'cmd /c ""' + $openssl + '" s_client -connect ' + $hostPart + ' -showcerts < NUL 2>&1 > "' + $certOut + '""'
cmd /c $probe | Out-Null
$printed = (& $openssl x509 -in $certOut -noout -fingerprint -sha256 2>&1 | Out-String)
$served = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($served) {
    Line 'fingerprint (sha256)' $served
    if ($Fingerprint) {
        $expected = ($Fingerprint -replace ':', '').Trim().ToLowerInvariant()
        if ($expected -eq $served) { Line 'pinned fingerprint' 'matches served certificate' } else { Bad 'pinned fingerprint' ('MISMATCH expected ' + $expected.Substring(0, 16) + '... actual ' + $served.Substring(0, 16) + '...') }
    } else {
        Line 'pinned fingerprint' '(not given: pass -Fingerprint to verify)'
    }
} else {
    Bad 'fingerprint' 'could not read the served certificate'
}

# health + tls/info over TLS (verify_return_error, so a bad cert fails the check)
function GetJson([string]$path) {
    $req = Join-Path $env:TEMP ('inspect-req-' + [guid]::NewGuid().ToString('N') + '.txt')
    $text = 'GET ' + $path + ' HTTP/1.1' + "`r`n" + 'Host: ' + $hostPart + "`r`n" + 'Connection: close' + "`r`n`r`n"
    Set-Content -Path $req -Value $text -NoNewline -Encoding ascii
    $cmd = 'cmd /c ""' + $openssl + '" s_client -connect ' + $hostPart + ' -CAfile "' + $certOut + '" -verify_return_error -quiet -ign_eof < "' + $req + '" 2>&1"'
    return (cmd /c $cmd | Out-String)
}
$health = GetJson '/v1/health'
if ($health -match '"node_id"\s*:\s*"([^"]+)"') { Line 'node_id' $matches[1] } else { Bad 'health' '/v1/health unusable (TLS or routing problem)' }
$info = GetJson '/v1/tls/info'
if ($info -match '"days_remaining"\s*:\s*(-?[0-9]+)') {
    $days = [int]$matches[1]
    if ($days -le 0) { Bad 'certificate expiry' ('expired ' + $days + ' days ago') } elseif ($days -le 30) { Bad 'certificate expiry' ($days.ToString() + ' days left - rotate now (deploy/rotate-certificate.ps1)') } else { Line 'certificate expiry' ($days.ToString() + ' days left') }
} else {
    Bad 'tls/info' 'endpoint not readable'
}
Remove-Item -Force $certOut -ErrorAction SilentlyContinue

if ($issues -eq 0) { Write-Host ''; Write-Host 'INSPECT: HEALTHY'; exit 0 }
Write-Host ''; Write-Host ('INSPECT: ' + $issues + ' issue(s) need attention')
exit 1

