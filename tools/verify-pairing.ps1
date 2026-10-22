# Two-party pairing over copy/paste packs: does the invite chain really work?
#
# A (alice) creates a group from B's KeyPackage pack, registers it on a node (the
# verified pull anchor only exists after registration), and produces an invite pack.
# B (bob) joins from that pack and must see the group in its list.
#
# Exit: 0 = PASS, 1 = FAIL, 3 = cannot run.
param(
    [int]$Port = 8496,
    [string]$NodeId = 'pairing-node'
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('pairing-' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null
$cert = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-cert.pem'
$key = Join-Path $root 'crates/node-client/tests/fixtures/tls/test-key.pem'
$nodeExe = Join-Path $root 'target/debug/chat-node.exe'
foreach ($p in @($cert, $key, $nodeExe)) { if (-not (Test-Path $p)) { Write-Host ('SKIP pairing: missing ' + $p); exit 3 } }
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'SKIP pairing: openssl missing'; exit 3 }
$dataDir = Join-Path $logDir 'node-data'
New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
$seed = Join-Path $logDir 'seed.hex'
$bytes = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant() | Set-Content -Path $seed -NoNewline -Encoding ascii
$node = Start-Process -FilePath $nodeExe -ArgumentList @(
    '--bind', ('0.0.0.0:' + $Port), '--allow-public-bind',
    '--data-dir', $dataDir, '--seed-file', $seed, '--node-id', $NodeId,
    '--tls-cert', $cert, '--tls-key', $key
) -PassThru -RedirectStandardOutput (Join-Path $logDir 'node.out') -RedirectStandardError (Join-Path $logDir 'node.err')
$exit = 1
try {
    Start-Sleep -Seconds 3
    $endpoint = 'https://127.0.0.1:' + $Port
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    if (-not $curl) { Write-Host 'SKIP pairing: curl.exe missing'; exit 3 }
    $body = (& curl.exe -s --insecure ($endpoint + '/v1/health') 2>&1 | Out-String)
    $match = [regex]::Match($body, '"node_public_key"\s*:\s*"([0-9a-fA-F]{64})"')
    if (-not $match.Success) { Write-Host 'FAIL pairing: node public key not found'; exit 1 }
    $nodeKey = $match.Groups[1].Value
    $printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
    $fp = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
    if ($fp.Length -ne 64) { Write-Host 'FAIL pairing: cannot read fixture fingerprint'; exit 1 }
    Write-Host ('node: ' + $endpoint + ' key=' + $nodeKey.Substring(0, 12) + '…')
    . (Join-Path $root 'tools/activate.ps1')
    cmd /c (".\gradlew.bat :apps:desktop:pairingSmoke --offline --console=plain --max-workers=1 " +
        "-Ppairing.endpoint=" + $endpoint + " -Ppairing.nodeId=" + $NodeId + " -Ppairing.nodeKey=" + $nodeKey + " -Ppairing.fingerprint=" + $fp +
        " > " + (Join-Path $logDir 'smoke.log') + " 2>&1")
    $exit = $LASTEXITCODE
    if ($exit -eq 0) {
        Write-Host 'PAIRING: PASS (invite pack round-trip works: KeyPackage -> group -> Welcome -> joined)'
    } else {
        Write-Host 'PAIRING: FAIL'
        Get-Content (Join-Path $logDir 'smoke.log') -Tail 12 | Out-Host
    }
} finally {
    if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue }
}
Write-Host ('logs: ' + $logDir)
exit $exit
