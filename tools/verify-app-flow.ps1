# Does the app's own flow work against the public node with the user's settings?
#
# Mirrors ChatApp exactly: transport pin -> identity -> group (+register) -> store
# -> configureSendNode/Group -> draft -> prepare -> dispatch(accepted) -> close/sync/reopen.
param(
    [string]$Endpoint = 'https://106.54.223.171:8443',
    [string]$NodeId = 'node-public',
    [string]$Fingerprint = '13be2f93162bd121cba6a91d4e05c22900dcc06abcd92cf843bd5ed772b8a053'
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('appflow-' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
. (Join-Path $root 'tools/activate.ps1')
$body = (& curl.exe -s --insecure ($Endpoint.TrimEnd('/') + '/v1/health') 2>&1 | Out-String)
$match = [regex]::Match($body, '"node_public_key"\s*:\s*"([0-9a-fA-F]{64})"')
if (-not $match.Success) { Write-Host 'FAIL app flow: node /v1/health gave no public key'; exit 1 }
$nodeKey = $match.Groups[1].Value
$servedNodeId = if ($body -match '"node_id"\s*:\s*"([^"]+)"') { $matches[1] } else { '' }
Write-Host ('node: ' + $Endpoint + ' node_id=' + $servedNodeId)
if ($servedNodeId -ne '' -and $servedNodeId -ne $NodeId) {
    Write-Host ('NOTE: passed node_id ' + $NodeId + ' differs from the served one (' + $servedNodeId + '); using the served value')
    $NodeId = $servedNodeId
}
cmd /c (".\gradlew.bat :apps:desktop:appFlowSmoke --offline --console=plain --max-workers=1 " +
    "-Papp.endpoint=" + $Endpoint + " -Papp.nodeId=" + $NodeId +
    " -Papp.nodeKey=" + $nodeKey + " -Papp.fingerprint=" + $Fingerprint +
    " > " + (Join-Path $logDir 'smoke.log') + " 2>&1")
$exit = $LASTEXITCODE
Get-Content (Join-Path $logDir 'smoke.log') | Select-String -Pattern 'PASS |FAIL |sync:|send ' | ForEach-Object { $_.Line }
Write-Host ('logs: ' + $logDir)
if ($exit -eq 0) { Write-Host 'APP FLOW: PASS' } else { Write-Host 'APP FLOW: FAIL'; Get-Content (Join-Path $logDir 'smoke.log') -Tail 15 | Out-Host }
exit $exit
