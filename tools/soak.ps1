# Soak test: repeat the packaged end-to-end flow N times and report a distribution.
#
# Default: starts its OWN local TLS node (no external dependency), runs -Iterations rounds
# of the packaged artifact (identity -> group -> register -> send -> pull), then summarises:
#   success rate, wall-time p50/p95/max, and a histogram of failure codes.
#
# With -Endpoint you can soak an external node instead (it will NOT start a local one).
#
# Exit codes: 0 = all iterations succeeded, 1 = at least one failure, 3 = cannot run.
param(
    [int]$Iterations = 10,
    [string]$Endpoint = '',
    [string]$NodeId = 'soak-node',
    [string]$Fingerprint = '',
    [int]$Port = 8490,
    [int]$Reuse = 0
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$build = Join-Path $root 'build'
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$logDir = Join-Path $build ('soak-' + $stamp)
New-Item -ItemType Directory -Path $logDir -Force | Out-Null

$app = Join-Path $build 'dist\app-image\Chat'
$java = Join-Path $app 'runtime\bin\java.exe'
$jar = Join-Path $app 'app\app.jar'
$native = Join-Path $app 'app\native'
foreach ($p in @($java, $jar, $native)) {
    if (-not (Test-Path $p)) { Write-Host ('SKIP soak: packaged artifact missing (' + $p + ')'); exit 3 }
}

# -Reuse N: 同一进程内同一身份/群连发 N 条（稳态），只跑 1 个进程；否则每次冷启动。
$nodeProc = $null
$dataDir = Join-Path $logDir 'node-data'
if ([string]::IsNullOrWhiteSpace($Endpoint)) {
    $nodeExe = Join-Path $root 'target\debug\chat-node.exe'
    $cert = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-cert.pem'
    $key = Join-Path $root 'crates\node-client\tests\fixtures\tls\test-key.pem'
    foreach ($p in @($nodeExe, $cert, $key)) { if (-not (Test-Path $p)) { Write-Host ('SKIP soak: missing ' + $p); exit 3 } }
    $seed = Join-Path $logDir 'seed.hex'
    $bytes = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes)
    ([System.BitConverter]::ToString($bytes) -replace '-', '').ToLowerInvariant() | Set-Content -Path $seed -NoNewline -Encoding ascii
    $Endpoint = 'https://127.0.0.1:' + $Port
    $NodeId = 'soak-node'
    if ([string]::IsNullOrWhiteSpace($Fingerprint)) {
        $openssl = 'C:\Program Files\OpenSSL-Win64\bin\openssl.exe'
        if (Test-Path $openssl) {
            $printed = (& $openssl x509 -in $cert -noout -fingerprint -sha256 2>&1 | Out-String)
            $Fingerprint = ([regex]::Match($printed, 'Fingerprint=([0-9A-Fa-f:]+)').Groups[1].Value -replace ':', '').ToLowerInvariant()
        }
    }
    Write-Host ('== starting local node on port ' + $Port + ' ==')
    $nodeProc = Start-Process -FilePath $nodeExe -ArgumentList @(
        '--bind', ('0.0.0.0:' + $Port), '--allow-public-bind',
        '--data-dir', $dataDir, '--seed-file', $seed, '--node-id', $NodeId,
        '--tls-cert', $cert, '--tls-key', $key
    ) -PassThru -RedirectStandardOutput (Join-Path $logDir 'node.out') -RedirectStandardError (Join-Path $logDir 'node.err')
    Start-Sleep -Seconds 3
    if ($nodeProc.HasExited) { Write-Host 'SKIP soak: local node failed to start'; exit 3 }
}

Write-Host ('endpoint: ' + $Endpoint + ' (node ' + $NodeId + ')')
if ($Reuse -gt 0) {
    Write-Host ('mode: steady-state (one process, ' + $Reuse + ' messages on the same identity)')
    $env:CHAT_REPEAT = [string]$Reuse
    $log = Join-Path $logDir 'reuse.log'
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $Endpoint $NodeId '-' $Fingerprint *> $log
    $code = $LASTEXITCODE
    $sw.Stop()
    Remove-Item Env:CHAT_REPEAT -ErrorAction SilentlyContinue
    if ($nodeProc -and -not $nodeProc.HasExited) { Stop-Process -Id $nodeProc.Id -Force -ErrorAction SilentlyContinue }
    $text = Get-Content $log -Raw -ErrorAction SilentlyContinue
    if ($null -eq $text) { $text = '' }
    $latencies = [regex]::Matches($text, 'REPEAT \d+ ok ms=(\d+)') | ForEach-Object { [int]$_.Groups[1].Value } | Sort-Object
    $sentMatch = [regex]::Match($text, 'PASS repeat flow: (\d+)/(\d+) sent')
    Get-Content $log -Tail 12 | Out-Host
    Write-Host ''
    Write-Host '===== soak summary (steady-state) ====='
    if ($sentMatch.Success) {
        Write-Host ('messages sent: ' + $sentMatch.Groups[1].Value + '/' + $sentMatch.Groups[2].Value)
    } else {
        Write-Host 'messages sent: unknown (repeat flow line missing)'
    }
    if ($latencies.Count -gt 0) {
        $p50 = $latencies[[int][math]::Floor(($latencies.Count - 1) * 0.5)]
        $p95 = $latencies[[int][math]::Ceiling(($latencies.Count - 1) * 0.95)]
        Write-Host ('per-message ms: p50=' + $p50 + ' p95=' + $p95 + ' max=' + $latencies[$latencies.Count - 1])
    }
    Write-Host ('wall: ' + [math]::Round($sw.Elapsed.TotalSeconds, 1) + ' s')
    Write-Host ('logs: ' + $logDir)
    if ($code -eq 0 -and $sentMatch.Success -and $sentMatch.Groups[1].Value -eq $sentMatch.Groups[2].Value) { Write-Host 'SOAK: PASS'; exit 0 }
    Write-Host 'SOAK: FAIL'
    exit 1
}

Write-Host ('iterations: ' + $Iterations)
$results = @()
$failCodes = @{}
try {
    for ($i = 1; $i -le $Iterations; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $log = Join-Path $logDir ('iter-' + $i.ToString('000') + '.log')
        & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.PackageE2eSmokeKt' $Endpoint $NodeId '-' $Fingerprint *> $log
        $code = $LASTEXITCODE
        $sw.Stop()
        $ms = [int]$sw.Elapsed.TotalMilliseconds
        $text = Get-Content $log -Raw -ErrorAction SilentlyContinue
        if ($null -eq $text) { $text = '' }
        $ok = ($code -eq 0 -and $text -match 'PASS package e2e slice')
        $results += [pscustomobject]@{ Iteration = $i; Ok = $ok; Ms = $ms; Code = $code }
        if (-not $ok) {
            $match = [regex]::Match($text, 'FAIL ([A-Z_]+)')
            $key = if ($match.Success) { $match.Groups[1].Value } else { 'exit-' + $code }
            if ($failCodes.ContainsKey($key)) { $failCodes[$key] = $failCodes[$key] + 1 } else { $failCodes[$key] = 1 }
            Write-Host ('  iter ' + $i + ' FAIL (' + $key + ', ' + $ms + ' ms)')
        } else {
            Write-Host ('  iter ' + $i + ' PASS (' + $ms + ' ms)')
        }
    }
} finally {
    if ($nodeProc -and -not $nodeProc.HasExited) { Stop-Process -Id $nodeProc.Id -Force -ErrorAction SilentlyContinue }
}

$okCount = @($results | Where-Object { $_.Ok }).Count
$times = @($results | ForEach-Object { $_.Ms } | Sort-Object)
function Percentile($values, [double]$p) {
    if ($values.Count -eq 0) { return 0 }
    $index = [int][math]::Ceiling($p * $values.Count) - 1
    if ($index -lt 0) { $index = 0 }
    if ($index -ge $values.Count) { $index = $values.Count - 1 }
    return $values[$index]
}
$summary = [pscustomobject]@{
    Iterations = $Iterations
    Succeeded = $okCount
    Failed = $Iterations - $okCount
    SuccessRate = [math]::Round(100.0 * $okCount / [math]::Max($Iterations, 1), 1)
    P50Ms = (Percentile $times 0.50)
    P95Ms = (Percentile $times 0.95)
    MaxMs = (Percentile $times 1.0)
    FailCodes = ($failCodes.GetEnumerator() | ForEach-Object { $_.Key + '=' + $_.Value }) -join ',' 
}
$summary | ConvertTo-Json | Set-Content -Path (Join-Path $logDir 'summary.json') -Encoding utf8
Write-Host ''
Write-Host '===== soak summary ====='
Write-Host ('iterations   : ' + $summary.Iterations)
Write-Host ('succeeded    : ' + $summary.Succeeded + ' (' + $summary.SuccessRate + '%)')
Write-Host ('failed       : ' + $summary.Failed)
Write-Host ('latency ms   : p50=' + $summary.P50Ms + ' p95=' + $summary.P95Ms + ' max=' + $summary.MaxMs)
if ($failCodes.Count -gt 0) { Write-Host ('failure codes: ' + $summary.FailCodes) }
Write-Host ('logs         : ' + $logDir)
if ($summary.Failed -eq 0) { Write-Host 'SOAK: PASS'; exit 0 }
Write-Host 'SOAK: FAIL (see failure codes above)'
exit 1

