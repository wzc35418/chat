# Verify the DELIVERED zip end-to-end: unzip it somewhere clean and run the packaged app there.
#
# Why: verify-package.ps1 checks build/dist/app-image, but what users download is the zip.
# This proves the zip itself is complete and runnable.
#
# Exit codes: 0 = PASS, 1 = FAIL, 3 = SKIP (zip missing).
param(
    [string]$Zip = '',
    [string]$WorkDir = '',
    [switch]$Keep
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($Zip)) { $Zip = Join-Path $root 'build/dist/Chat-app-0.1.0.zip' }
if (-not (Test-Path $Zip)) { Write-Host ('SKIP zip check: not found (' + $Zip + ')'); exit 3 }
if ([string]::IsNullOrWhiteSpace($WorkDir)) {
    $WorkDir = Join-Path ([System.IO.Path]::GetTempPath()) ('chat-zip-' + [guid]::NewGuid().ToString('N'))
}
$fail = 0
function Line([string]$name, [string]$value) { Write-Host ($name.PadRight(28) + $value) }
function Bad([string]$name, [string]$value) { $script:fail = 1; Write-Host ($name.PadRight(28) + 'FAIL: ' + $value) }

Write-Host '== delivered zip =='
Line 'zip' $Zip
Line 'size' (([math]::Round((Get-Item $Zip).Length / 1MB, 2)).ToString() + ' MB')
Line 'sha256' (Get-FileHash $Zip -Algorithm SHA256).Hash

if (Test-Path $WorkDir) { Remove-Item -Recurse -Force $WorkDir }
New-Item -ItemType Directory -Path $WorkDir -Force | Out-Null
try {
    Expand-Archive -Path $Zip -DestinationPath $WorkDir -Force -ErrorAction Stop
    Line 'extracted to' $WorkDir
} catch {
    Bad 'extract' $_.Exception.Message
    Write-Host ''; Write-Host 'ZIP CHECK: FAIL'; exit 1
}

$exe = Get-ChildItem -Path $WorkDir -Recurse -Filter 'Chat.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($exe) { Line 'Chat.exe' $exe.FullName } else { Bad 'Chat.exe' 'missing' }
$appRoot = $WorkDir
if ($exe) { $appRoot = Split-Path -Parent $exe.FullName }
foreach ($rel in @('app/app.jar', 'app/native/chat_ffi.dll', 'runtime/bin/java.exe')) {
    $p = Join-Path $appRoot $rel
    if (Test-Path $p) { Line $rel ('present (' + [math]::Round((Get-Item $p).Length / 1KB, 1) + ' KB)') } else { Bad $rel 'missing' }
}

$referenceExe = Join-Path $root 'build/dist/app-image/Chat/Chat.exe'
if ($exe -and (Test-Path $referenceExe)) {
    $a = (Get-FileHash $exe.FullName -Algorithm SHA256).Hash
    $b = (Get-FileHash $referenceExe -Algorithm SHA256).Hash
    if ($a -eq $b) { Line 'exe matches app-image' ('yes (' + $a.Substring(0, 12) + '...)') } else { Bad 'exe matches app-image' ('no: ' + $a.Substring(0,12) + ' vs ' + $b.Substring(0,12)) }
}

$java = Join-Path $appRoot 'runtime/bin/java.exe'
$jar = Join-Path $appRoot 'app/app.jar'
$native = Join-Path $appRoot 'app/native'
if ((Test-Path $java) -and (Test-Path $jar)) {
    $log = Join-Path $WorkDir 'smoke.log'
    & $java ('-Djna.library.path=' + $native) '-cp' $jar 'chat.desktop.P0SmokeKt' *> $log
    $code = $LASTEXITCODE
    if ($code -eq 0) { Line 'packaged smoke (from zip)' 'PASS' } else { Bad 'packaged smoke (from zip)' ('exit ' + $code) }
} else {
    Bad 'packaged smoke' 'java or jar missing'
}

if ($exe) {
    $version = (Get-Item $exe.FullName).VersionInfo.FileVersion
    if ($version) { Line 'exe file version' $version }
}

if (-not $Keep) { Remove-Item -Recurse -Force $WorkDir -ErrorAction SilentlyContinue } else { Line 'kept' $WorkDir }

Write-Host ''
if ($fail -eq 0) { Write-Host 'ZIP CHECK: PASS (delivered zip is complete and runs)'; exit 0 }
Write-Host 'ZIP CHECK: FAIL'
exit 1

