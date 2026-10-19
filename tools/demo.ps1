<#
One-click demo: start a local node, print the values to paste into the preview app,
and optionally launch the desktop preview.

Usage:
  powershell -ExecutionPolicy Bypass -File tools/demo.ps1            # node only
  powershell -ExecutionPolicy Bypass -File tools/demo.ps1 -Preview   # node + preview window
  powershell -ExecutionPolicy Bypass -File tools/demo.ps1 -Stop      # stop a running demo node
#>
param(
    [switch]$Preview,
    [switch]$Stop,
    # Detach: return immediately after the node is healthy (default keeps this
    # window hosting the node so it does not die with the shell).
    [switch]$Background
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$pidFile = Join-Path $root 'build\demo-node.pid'
$logFile = Join-Path $root 'build\demo-node.log'
New-Item -ItemType Directory -Force -Path (Join-Path $root 'build') | Out-Null

function Stop-DemoNode {
    if (Test-Path $pidFile) {
        $nodePid = [int](Get-Content $pidFile | Select-Object -First 1)
        $existing = Get-Process -Id $nodePid -ErrorAction SilentlyContinue
        if ($existing) {
            Stop-Process -Id $nodePid -Force -ErrorAction SilentlyContinue
            Write-Host ("stopped demo node pid={0}" -f $nodePid)
        } else {
            Write-Host 'demo node was not running'
        }
        Remove-Item $pidFile -Force -ErrorAction SilentlyContinue
    } else {
        Write-Host 'no demo node recorded'
    }
}

if ($Stop) { Stop-DemoNode; exit 0 }

# 1) disk guard
powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'tools\check-disk.ps1') | Out-Host
if ($LASTEXITCODE -ne 0) { throw 'not enough disk space' }

# 2) build the node binary once
Push-Location $root
try {
    & cmd /c 'cargo build --offline -p chat-node > build\demo-build.log 2>&1'
    if ($LASTEXITCODE -ne 0) { throw 'cargo build -p chat-node failed (see build\demo-build.log)' }
} finally { Pop-Location }

$nodeExe = Join-Path $root 'target\debug\chat-node.exe'
if (-not (Test-Path $nodeExe)) { throw 'chat-node.exe not found after build' }

# 3) create a demo seed file if missing (NEVER reuse a production seed)
$seedFile = Join-Path $root 'build\demo-node-seed.bin'
if (-not (Test-Path $seedFile)) {
    $seed = New-Object byte[] 32
    [System.Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($seed)
    # chat-node accepts exactly 32 raw bytes or 64 hex characters -> write hex.
    [System.IO.File]::WriteAllText($seedFile, ([System.BitConverter]::ToString($seed) -replace '-', '').ToLowerInvariant())
    Write-Host ("created demo seed file: {0} (64 hex chars)" -f $seedFile)
}

# 4) start node in background
Stop-DemoNode
$env:CHAT_NODE_SEED_FILE = $seedFile
$process = Start-Process -FilePath $nodeExe -WorkingDirectory $root -PassThru -RedirectStandardOutput $logFile -RedirectStandardError "$logFile.err"
Set-Content -Path $pidFile -Value $process.Id
Write-Host ("started demo node pid={0}, log: {1}" -f $process.Id, $logFile)

# 5) wait for health
$healthy = $false
for ($i = 0; $i -lt 40; $i++) {
    Start-Sleep -Milliseconds 250
    try {
        $response = Invoke-WebRequest -Uri 'http://127.0.0.1:8080/v1/health' -TimeoutSec 2 -UseBasicParsing
        if ($response.StatusCode -eq 200) { $healthy = $true; break }
    } catch { }
}
if (-not $healthy) {
    Write-Host 'node did not become healthy; tail of log:'
    if (Test-Path $logFile) { Get-Content $logFile -Tail 20 | Out-Host }
    throw 'demo node failed to start'
}
Write-Host 'node health: OK (http://127.0.0.1:8080/v1/health)'

# 6) print values for the preview app
Write-Host ''
Write-Host 'Paste into the preview app (node panel):'
Write-Host '  node endpoint : http://127.0.0.1:8080'
Write-Host '  request id    : returned as header x-chat-request-id'
Write-Host '  audit         : GET /v1/audit?limit=20'
Write-Host ''
Write-Host 'Stop the node later with: powershell -File tools\demo.ps1 -Stop'

if (-not $Background) {
    Write-Host ''
    Write-Host 'node is running in this window; press Ctrl+C to stop (or use -Stop in another shell).'
    Write-Host 'NOTE: with -Background the node may be killed when this shell exits; prefer this window.'
    Wait-Process -Id $process.Id
}

if ($Preview) {
    Write-Host 'launching desktop preview (close the window to return)...'
    Push-Location $root
    try {
        & .\gradlew.bat :apps:desktop:previewApp --offline --console=plain --max-workers=1
    } finally { Pop-Location }
}
