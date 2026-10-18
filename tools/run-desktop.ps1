$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')
Set-Location $chatRoot
& .\gradlew.bat :apps:desktop:run --console=plain --offline --no-daemon
if ($LASTEXITCODE -ne 0) { throw "Desktop application failed ($LASTEXITCODE)" }
