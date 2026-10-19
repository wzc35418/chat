<#
Pre-build disk check (added after the 2026-10-19 disk-full incident).

Background: when drive D: was full, the write tool (truncate-then-write) lost the
tail of files (e.g. crates/node-services/src/group.rs lost ~200 lines), and the
build failed with link.exe LNK1318 / os error 112.

Usage:
  powershell -File tools/check-disk.ps1                  # check only (exit 2 if below threshold)
  powershell -File tools/check-disk.ps1 -Clean           # check and delete rebuildable caches
  powershell -File tools/check-disk.ps1 -MinFreeGb 10    # custom threshold
#>
param(
    [switch]$Clean,
    [double]$MinFreeGb = 6
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$driveLetter = (Get-Item $root).PSDrive.Name
$drive = Get-PSDrive $driveLetter
$freeGb = [math]::Round($drive.Free / 1GB, 2)
Write-Host ("Drive {0}: free {1} GB (threshold {2} GB)" -f $driveLetter, $freeGb, $MinFreeGb)

$candidates = @(
    (Join-Path $root 'target\debug\incremental'),
    (Join-Path $root 'target\debug\deps\.fingerprint')
)
$cleanedGb = 0

if ($Clean) {
    foreach ($path in $candidates) {
        if (Test-Path $path) {
            $sum = (Get-ChildItem -Recurse -File $path -ErrorAction SilentlyContinue |
                Measure-Object -Property Length -Sum).Sum
            if (-not $sum) { $sum = 0 }
            Remove-Item -Recurse -Force $path
            $cleanedGb += ($sum / 1GB)
            Write-Host ("Removed {0} ({1} GB)" -f $path, [math]::Round($sum / 1GB, 2))
        }
    }
    $drive = Get-PSDrive $driveLetter
    $freeGb = [math]::Round($drive.Free / 1GB, 2)
    Write-Host ("After clean: free {0} GB (freed about {1} GB)" -f $freeGb, [math]::Round($cleanedGb, 2))
}

if ($freeGb -lt $MinFreeGb) {
    Write-Host ("NOT ENOUGH SPACE: free {0} GB < threshold {1} GB. Run with -Clean or free space first." -f $freeGb, $MinFreeGb)
    exit 2
}

Write-Host 'OK: enough space to build.'
exit 0
