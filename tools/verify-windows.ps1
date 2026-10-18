[CmdletBinding()]
param([switch]$RefreshLocks)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')
Set-Location $chatRoot
# Keep native library generation and JVM loading sequential on Windows.
& (Join-Path $PSScriptRoot 'verify-rust.ps1') -Android
$gradleArgs = @(
    ':tools:kotlin-dependency-check:resolveDependencies',
    ':tools:kotlin-dependency-check:compileKotlinDesktop',
    ':tools:kotlin-dependency-check:assembleDebug',
    ':tools:kotlin-dependency-check:verifyFfi',
    '--console=plain'
)
if ($RefreshLocks) { $gradleArgs += '--write-locks' }
& (Join-Path $chatRoot 'gradlew.bat') @gradleArgs
if ($LASTEXITCODE -ne 0) { throw "Kotlin/Android/FFI verification failed ($LASTEXITCODE)" }
Write-Host 'All Windows-host dependency checks passed.'
