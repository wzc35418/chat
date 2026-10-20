# Wait until the remote node port opens, then verify automatically.
#
# Typical use: start run-node.ps1 on the Windows server, then run this locally and
# walk away — it will detect the open port and run verify-remote.ps1 for you.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File tools\watch-remote.ps1 `
#     -Endpoint https://106.54.223.171:8443 -NodeId node-public `
#     -FingerprintFile build\node-bundle\fingerprint.txt [-TimeoutMinutes 20] [-DiagnoseOnly]
param(
    [Parameter(Mandatory = $true)][string]$Endpoint,
    [Parameter(Mandatory = $true)][string]$NodeId,
    [string]$FingerprintFile = '',
    [int]$TimeoutMinutes = 20,
    [int]$PollSeconds = 5,
    [switch]$DiagnoseOnly
)
$ErrorActionPreference = 'Continue'
$hostPart = ($Endpoint -replace '^https?://', '').Split('/')[0]
$hostName = $hostPart.Split(':')[0]
$isHttps = $Endpoint.StartsWith('https://', [System.StringComparison]::OrdinalIgnoreCase)
$port = if ($hostPart.Contains(':')) { [int]$hostPart.Split(':')[1] } elseif ($isHttps) { 443 } else { 80 }

Write-Host ('watching ' + $hostName + ':' + $port + ' (timeout ' + $TimeoutMinutes + ' min, poll ' + $PollSeconds + 's)')
$deadline = (Get-Date).AddMinutes($TimeoutMinutes)
$attempt = 0
$opened = $false
while ((Get-Date) -lt $deadline) {
    $attempt++
    $ok = $false
    try { $ok = Test-NetConnection -ComputerName $hostName -Port $port -InformationLevel Quiet -WarningAction SilentlyContinue } catch { $ok = $false }
    if ($ok) { $opened = $true; break }
    if ($attempt % 6 -eq 1) { Write-Host ('  still closed (attempt ' + $attempt + ')') }
    Start-Sleep -Seconds $PollSeconds
}

if (-not $opened) {
    Write-Host ('FAIL watch-remote: port never opened within ' + $TimeoutMinutes + ' min')
    Write-Host '  check: server run-node.ps1 window, New-NetFirewallRule, cloud security group'
    exit 1
}

Write-Host ('PORT OPEN after ' + $attempt + ' attempts — running verification')
$root = Split-Path -Parent $PSScriptRoot
$verify = Join-Path $root 'tools\verify-remote.ps1'
$args = @('-Endpoint', $Endpoint, '-NodeId', $NodeId)
if ($FingerprintFile) { $args += @('-FingerprintFile', $FingerprintFile) }
if ($DiagnoseOnly) { $args += '-DiagnoseOnly' }
if ($isHttps -and -not $FingerprintFile -and -not $DiagnoseOnly) {
    Write-Host 'INFO watch-remote: no -FingerprintFile given; verify-remote will ask for -Fingerprint on HTTPS'
}
& powershell -NoProfile -ExecutionPolicy Bypass -File $verify @args
exit $LASTEXITCODE
