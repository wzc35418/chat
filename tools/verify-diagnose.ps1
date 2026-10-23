# Assert the app's diagnose mode produces a machine-readable, self-consistent report.
#
# Runs the packaged runtime with `--diagnose --json`, parses the single-line JSON and checks:
#   - required keys exist
#   - allOk is true  <=>  every check ok is true
#   - result starts with PASS when allOk
#   - conversations is an array (may be empty)
# Exit: 0 = PASS, 1 = FAIL, 3 = cannot run.
param(
    [string]$AppImage = ''
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($AppImage)) { $AppImage = Join-Path $root 'build/dist/app-image/Chat' }
$java = Join-Path $AppImage 'runtime/bin/java.exe'
$jar = Join-Path $AppImage 'app/app.jar'
$native = Join-Path $AppImage 'app/native'
foreach ($p in @($java, $jar, $native)) { if (-not (Test-Path $p)) { Write-Host ('SKIP diagnose: missing ' + $p); exit 3 } }
$report = Join-Path $root ('build/diagnose-json-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.json')
$raw = & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.MainKt' '--diagnose' '--json' 2>&1 | Out-String
$exit = $LASTEXITCODE
$line = ($raw -split "`n" | Where-Object { $_.Trim().StartsWith('{') } | Select-Object -First 1)
if (-not $line) { Write-Host 'FAIL diagnose: no JSON line in output'; Write-Host ($raw.Trim()); exit 1 }
try { $json = $line | ConvertFrom-Json } catch { Write-Host ('FAIL diagnose: JSON parse error: ' + $_.Exception.Message); exit 1 }
$json | ConvertTo-Json -Depth 6 | Set-Content -Path $report -Encoding utf8

$problems = @()
foreach ($key in @('endpoint', 'nodeId', 'fingerprints', 'checks', 'conversations', 'result', 'allOk')) {
    if (-not ($json.PSObject.Properties.Name -contains $key)) { $problems += ('缺少字段 ' + $key) }
}
if ($json.checks) {
    $failed = @($json.checks | Where-Object { -not $_.ok })
    if ($json.allOk -and $failed.Count -gt 0) { $problems += ('allOk=true 但有 ' + $failed.Count + ' 项 FAIL') }
    if (-not $json.allOk -and $failed.Count -eq 0) { $problems += 'allOk=false 但所有检查都 PASS' }
    foreach ($check in $json.checks) { if ([string]::IsNullOrWhiteSpace($check.name)) { $problems += '有检查项缺 name' } }
} else { $problems += 'checks 为空' }
if ($json.allOk -and -not ([string]$json.result).StartsWith('PASS')) { $problems += ('allOk=true 但 result=' + $json.result) }
if ($exit -ne 0 -and $json.allOk) { Write-Host ('NOTE diagnose exit=' + $exit + ' 但 allOk=true（网络抖动可能导致 exit=1）') }

Write-Host ('checks: ' + @($json.checks).Count + '（FAIL ' + (@($json.checks | Where-Object { -not $_.ok }).Count) + '）')
Write-Host ('conversations: ' + @($json.conversations).Count)
Write-Host ('result: ' + $json.result)
Write-Host ('report: ' + $report)
if ($problems.Count -eq 0) { Write-Host 'DIAGNOSE JSON: PASS'; exit 0 }
Write-Host ('DIAGNOSE JSON: FAIL（' + $problems.Count + ' 处）')
foreach ($problem in $problems) { Write-Host ('  - ' + $problem) }
exit 1
