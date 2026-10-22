# Rotate the TLS certificate of a deployed chat-node bundle WITHOUT losing identity.
#
# Certificates expire and public IPs change; the node seed (identity) must stay the same,
# otherwise every client's member records stop matching.
#
# What it does:
#   1. keeps seed.hex and the data directory untouched;
#   2. generates a NEW self-signed cert (SAN = -PublicIp) valid for -Days;
#   3. appends the OLD fingerprint to fingerprint-history.txt (audit trail);
#   4. rewrites fingerprint.txt with the new values;
#   5. runs verify-bundle.ps1 to confirm cert/fingerprint consistency.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File deploy\rotate-certificate.ps1 `
#       -BundleDir build\node-bundle -PublicIp 106.54.223.171 -Port 8443
#
# Then copy cert.pem + key.pem + fingerprint.txt to the server and restart the node.
# Clients need the NEW fingerprint; keep the old one during the transition window.
param(
    [Parameter(Mandatory = $true)][string]$BundleDir,
    [Parameter(Mandatory = $true)][string]$PublicIp,
    [int]$Port = 8443,
    [string]$NodeId = 'node-public',
    [int]$Days = 3650
)
$ErrorActionPreference = 'Continue'
if (-not (Test-Path $BundleDir)) { Write-Host ('FAIL rotate: bundle dir not found: ' + $BundleDir); exit 1 }
$BundleDir = (Resolve-Path $BundleDir).Path
$cert = Join-Path $BundleDir 'cert.pem'
$key = Join-Path $BundleDir 'key.pem'
$seed = Join-Path $BundleDir 'seed.hex'
if (-not (Test-Path $seed)) { Write-Host 'FAIL rotate: seed.hex missing (refusing to rotate without node identity)'; exit 1 }
$openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
if (-not (Test-Path $openssl)) { Write-Host 'FAIL rotate: openssl not found'; exit 1 }

# 1. record the old fingerprint (audit trail)
$oldFp = ''
if (Test-Path $cert) {
    $printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
    $oldFp = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
}
$oldFpText = '(none)'
if ($oldFp.Length -eq 64) { $oldFpText = $oldFp }
Write-Host ('old fingerprint: ' + $oldFpText)

# 2. new certificate (same CN/SAN rules as prepare-node-bundle.ps1)
$cnf = Join-Path $BundleDir 'openssl.cnf'
$cnfLines = @(
    '[req]',
    'distinguished_name = dn',
    'x509_extensions = ext',
    'prompt = no',
    '[dn]',
    ('CN = ' + $PublicIp),
    '[ext]',
    ('subjectAltName = IP:' + $PublicIp + ',DNS:localhost'),
    'basicConstraints = critical,CA:TRUE',
    'keyUsage = critical,digitalSignature,keyCertSign',
    'extendedKeyUsage = serverAuth'
)
Set-Content -Path $cnf -Value $cnfLines -Encoding ascii
& $openssl req -x509 -newkey rsa:2048 -nodes -days $Days -keyout $key -out $cert -config $cnf -extensions ext 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Write-Host 'FAIL rotate: certificate generation failed'; exit 1 }

$newPrinted = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
$newFp = ([regex]::Match($newPrinted, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
if ($newFp.Length -ne 64) { Write-Host 'FAIL rotate: could not read new fingerprint'; exit 1 }
if ($newFp -eq $oldFpText) { Write-Host 'FAIL rotate: new certificate equals the old one (nothing rotated)'; exit 1 }

# 3. audit history + 4. new fingerprint file
$history = Join-Path $BundleDir 'fingerprint-history.txt'
$stamp = Get-Date -Format 'yyyy-MM-ddTHH:mm:sszzz'
Add-Content -Path $history -Value ($stamp + ' old=' + $oldFpText + ' new=' + $newFp + ' ip=' + $PublicIp) -Encoding utf8
$fpLines = @(
    ('endpoint    : https://' + $PublicIp + ':' + $Port),
    ('node_id     : ' + $NodeId),
    ('fingerprint : ' + $newFp)
)
Set-Content -Path (Join-Path $BundleDir 'fingerprint.txt') -Value $fpLines -Encoding utf8

Write-Host ''
Write-Host '================ rotated ================'
Write-Host ('old fingerprint : ' + $oldFpText)
Write-Host ('new fingerprint : ' + $newFp)
Write-Host ('endpoint        : https://' + $PublicIp + ':' + $Port)
Write-Host '========================================'
Write-Host 'Next: copy cert.pem + key.pem + fingerprint.txt to the server, restart the node,'
Write-Host '      then add the NEW fingerprint on clients (old ones may stay during transition).'

# 5. self-check the rotated bundle
$verifier = Join-Path $BundleDir 'verify-bundle.ps1'
if (Test-Path $verifier) {
    & powershell -NoProfile -ExecutionPolicy Bypass -File $verifier -BundleDir $BundleDir -Port $Port
    exit $LASTEXITCODE
}
Write-Host 'INFO rotate: verify-bundle.ps1 not in bundle, skipped self-check'
exit 0
