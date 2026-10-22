# Check that the packaged artifact is NEWER than the sources it was built from.
#
# Why: four separate failures in this session came from running verification against a
# stale jar ("messages sent: unknown", "REMOTE=1", ...). This script turns that into a
# deterministic check instead of something to remember.
#
# Exit codes: 0 = fresh, 1 = stale (rebuild), 3 = artifact missing (nothing to compare).
param(
    [string]$Artifact = '',
    [string]$Root = '',
    [switch]$Quiet
)
$ErrorActionPreference = 'Continue'
if ([string]::IsNullOrWhiteSpace($Root)) { $Root = Split-Path -Parent $PSScriptRoot }
if ([string]::IsNullOrWhiteSpace($Artifact)) { $Artifact = Join-Path $Root 'build\dist\app-image\Chat\app\app.jar' }

if (-not (Test-Path $Artifact)) {
    if (-not $Quiet) { Write-Host ('SKIP freshness: artifact missing (' + $Artifact + ')') }
    exit 3
}
$artifactTime = (Get-Item $Artifact).LastWriteTimeUtc

# Sources that end up inside app.jar (Kotlin) or its native libs (Rust FFI).
$roots = @(
    (Join-Path $Root 'apps\desktop\src'),
    (Join-Path $Root 'shared'),
    (Join-Path $Root 'crates\ffi\src'),
    (Join-Path $Root 'crates\core\src')
)
$extensions = @('.kt', '.kts', '.rs')
$newest = $null
$newestPath = ''
foreach ($dir in $roots) {
    if (-not (Test-Path $dir)) { continue }
    $files = Get-ChildItem -Path $dir -Recurse -File -ErrorAction SilentlyContinue | Where-Object {
        $extensions -contains $_.Extension.ToLowerInvariant()
    }
    foreach ($file in $files) {
        if ($null -eq $newest -or $file.LastWriteTimeUtc -gt $newest) {
            $newest = $file.LastWriteTimeUtc
            $newestPath = $file.FullName
        }
    }
}

if ($null -eq $newest) {
    if (-not $Quiet) { Write-Host 'SKIP freshness: no source files found' }
    exit 3
}

$artifactAge = [math]::Round(($newest - $artifactTime).TotalMinutes, 1)
if ($Quiet) {
    if ($newest -gt $artifactTime) { exit 1 }
    exit 0
}
if ($newest -gt $artifactTime) {
    Write-Host 'FAIL freshness: packaged artifact is STALE'
    Write-Host ('  artifact : ' + $Artifact)
    Write-Host ('             ' + $artifactTime.ToString('yyyy-MM-ddTHH:mm:ssZ'))
    Write-Host ('  newest   : ' + $newestPath)
    Write-Host ('             ' + $newest.ToString('yyyy-MM-ddTHH:mm:ssZ') + '  (' + $artifactAge + ' min newer)')
    Write-Host '  fix      : powershell -ExecutionPolicy Bypass -File tools\verify-package.ps1'
    exit 1
}
Write-Host ('PASS freshness: artifact is newer than all sources (' + $artifactTime.ToString('yyyy-MM-ddTHH:mm:ssZ') + ')')
exit 0

