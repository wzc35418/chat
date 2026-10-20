# LAN (non-loopback) end-to-end check: the PACKAGED artifact talks to the node over the
# machine's real LAN IPv4 instead of 127.0.0.1. This proves the transport path does not
# depend on the loopback interface (a prerequisite for cross-machine use).
#
# Honest boundary: this is still ONE host. It does NOT prove NAT traversal, a relay,
# or TLS. Traffic is plaintext HTTP: only run this on a network you trust.
#
# Exit codes: 0 = PASS, 1 = FAIL, 3 = SKIP (no non-loopback IPv4 available).
param(
    [int]$Port = 8097,
    [int]$HealthTimeoutSeconds = 20,
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('lan-logs\' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

# 1. pick a non-loopback IPv4 (prefer private ranges).
$candidates = Get-NetIPAddress -AddressFamily IPv4 -ErrorAction SilentlyContinue |
    Where-Object {
        $_.IPAddress -ne '127.0.0.1' -and
        $_.IPAddress -notlike '169.254.*' -and
        ($_.IPAddress -like '192.168.*' -or $_.IPAddress -like '10.*' -or $_.IPAddress -like '172.1[6-9].*' -or $_.IPAddress -like '172.2*' -or $_.IPAddress -like '172.3*')
    } | Select-Object -ExpandProperty IPAddress -Unique
if (-not $candidates) {
    Write-Host 'SKIP lan check: no non-loopback IPv4 address on this machine'
    exit 3
}
$lanIp = @($candidates)[0]
$endpoint = 'http://' + $lanIp + ':' + $Port
Write-Host ('lan address: ' + $lanIp + ' -> ' + $endpoint)

# 2. packaged artifact
$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
if (-not (Test-Path $jar)) {
    Write-Host ('FAIL lan check: packaged artifact missing at ' + $app)
    exit 1
}

# 3. node binary
$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) {
    Write-Host ('FAIL lan check: chat-node.exe missing (build it first)')
    exit 1
}
$seedFile = Join-Path $build 'lan-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
}
$dataDir = Join-Path $build ('lan-node-data-' + $stamp)
$nodeOut = Join-Path $logDir 'node.out.log'
$nodeErr = Join-Path $logDir 'node.err.log'
$env:CHAT_NODE_ALLOW_PUBLIC_BIND = '1'
$nodeArgs = @('--bind', ($lanIp + ':' + $Port), '--data-dir', $dataDir, '--seed-file', $seedFile, '--node-id', 'node-lan')
$node = Start-Process -FilePath $nodeExe -ArgumentList $nodeArgs -WorkingDirectory $root -PassThru -RedirectStandardOutput $nodeOut -RedirectStandardError $nodeErr
$healthy = $false
try {
    for ($i = 0; $i -lt ($HealthTimeoutSeconds * 4); $i++) {
        Start-Sleep -Milliseconds 250
        try {
            $resp = Invoke-WebRequest -Uri ($endpoint + '/v1/health') -TimeoutSec 2 -UseBasicParsing
            if ($resp.StatusCode -eq 200) { $healthy = $true; break }
        } catch { }
    }
    if (-not $healthy) {
        Write-Host ('FAIL lan check: node not healthy on ' + $endpoint)
        if (Test-Path $nodeErr) { Get-Content $nodeErr -Tail 10 | Out-Host }
        exit 1
    }
    Write-Host ('PASS lan node: ' + $endpoint + ' (node-lan)')

    # 4. packaged artifact sends + pulls over the LAN address
    $out = Join-Path $logDir 'e2e-lan.log'
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $endpoint 'node-lan' *> $out
    $code = $LASTEXITCODE
    Get-Content $out -Tail 12 | Out-Host
    if ($code -ne 0) {
        Write-Host ('FAIL lan check: packaged e2e over LAN failed (exit ' + $code + ')')
        exit 1
    }
    $log = Get-Content $out -Raw
    if ($log -notmatch 'accepted') {
        Write-Host 'FAIL lan check: no node acceptance observed over LAN'
        exit 1
    }
    Write-Host 'PASS lan e2e: draft accepted + sync applied over non-loopback interface'
    Write-Host ('logs: ' + $logDir)
    exit 0
} finally {
    if ($node -and -not $node.HasExited) { Stop-Process -Id $node.Id -Force -ErrorAction SilentlyContinue }
    Remove-Item Env:CHAT_NODE_ALLOW_PUBLIC_BIND -ErrorAction SilentlyContinue
}
