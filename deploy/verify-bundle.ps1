# Verify a copied deployment bundle on the server BEFORE starting it.
# Catches the usual failures: truncated copy, cert/fingerprint mismatch, missing seed, broken exe.
#
# Usage (on the server, inside the bundle folder):
#   powershell -ExecutionPolicy Bypass -File .\verify-bundle.ps1
#   powershell -ExecutionPolicy Bypass -File .\verify-bundle.ps1 -Port 8443 -StartProbe
param(
    [string]$BundleDir = '',
    [int]$Port = 8443,
    [switch]$StartProbe
)
$ErrorActionPreference = 'Continue'
if ([string]::IsNullOrWhiteSpace($BundleDir)) { $BundleDir = Split-Path -Parent $MyInvocation.MyCommand.Path }
$fail = 0
function Check([string]$name, [bool]$ok, [string]$detail) {
    if ($ok) { Write-Host ('PASS ' + $name + ' ' + $detail) }
    else { Write-Host ('FAIL ' + $name + ' ' + $detail); $script:fail = 1 }
}

Write-Host ('== bundle: ' + $BundleDir + ' ==')
foreach ($name in @('chat-node.exe', 'cert.pem', 'key.pem', 'seed.hex', 'run-node.ps1', 'fingerprint.txt')) {
    $path = Join-Path $BundleDir $name
    $exists = Test-Path $path
    $size = ''
    if ($exists) { $size = '(' + [math]::Round((Get-Item $path).Length / 1KB, 1) + ' KB)' } else { $size = 'missing' }
    Check ('file ' + $name) $exists $size
}

# certificate vs declared fingerprint
$cert = Join-Path $BundleDir 'cert.pem'
$fpFile = Join-Path $BundleDir 'fingerprint.txt'
if ((Test-Path $cert) -and (Test-Path $fpFile)) {
    $declared = ([regex]::Match((Get-Content $fpFile -Raw), '([0-9a-fA-F]{64})')).Groups[1].Value.ToLowerInvariant()
    $openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
    if (Test-Path $openssl) {
        $printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
        $actual = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
        $fpOk = $false
        if ($actual -is [string] -and $declared -is [string] -and $actual.Length -eq 64 -and $actual -eq $declared) { $fpOk = $true }
        $fpDetail = 'declared=' + $declared + ' actual=' + $actual
        Check 'certificate fingerprint matches fingerprint.txt' $fpOk $fpDetail
    } else {
        Write-Host 'SKIP certificate fingerprint check (openssl not found on this host)'
    }
}

# exe actually executes (usage error is expected and proves it runs)
$exe = Join-Path $BundleDir 'chat-node.exe'
if (Test-Path $exe) {
    & $exe '--help' 2>&1 | Out-Null
    Check 'chat-node.exe executes' ($LASTEXITCODE -ne 0) ('exit ' + $LASTEXITCODE + ' (usage error expected)')
}

# optional: start it briefly and read /v1/health locally
if ($StartProbe -and $fail -eq 0) {
    $dataDir = Join-Path $BundleDir 'probe-data'
    $proc = Start-Process -FilePath $exe -ArgumentList @(
        '--bind', ('127.0.0.1:' + $Port), '--allow-public-bind',
        '--data-dir', $dataDir,
        '--seed-file', (Join-Path $BundleDir 'seed.hex'),
        '--node-id', 'bundle-probe',
        '--tls-cert', $cert,
        '--tls-key', (Join-Path $BundleDir 'key.pem')
    ) -PassThru -RedirectStandardOutput (Join-Path $env:TEMP 'bundle-probe.out') -RedirectStandardError (Join-Path $env:TEMP 'bundle-probe.err')
    Start-Sleep -Seconds 3
    # 本地探活只看"进程起没起来"：证书 SAN 是公网 IP，用 127.0.0.1 连必然主机名不匹配，
    # 所以这里 -k 跳过校验（证书指纹已在上面单独核对过，不靠这一步）。
    $body = (curl.exe -s -k --max-time 5 ('https://127.0.0.1:' + $Port + '/v1/health')) | Out-String
    $ok = [bool]($body -match 'node_public_key')
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue }
    Check 'start probe (/v1/health)' $ok ''
}

if ($fail -eq 0) {
    Write-Host ''
    Write-Host 'BUNDLE OK - start it with: powershell -ExecutionPolicy Bypass -File .\run-node.ps1'
    exit 0
} else {
    Write-Host ''
    Write-Host 'BUNDLE BROKEN - copy the whole folder again (do not copy only some files)'
    exit 1
}
