# Dot-source this script: . .\tools\activate.ps1
$ErrorActionPreference = 'Stop'
$chatRoot = Split-Path $PSScriptRoot -Parent
$chatTools = Join-Path $chatRoot '.tools'
$env:CARGO_HOME = Join-Path $chatTools 'cargo'
$env:RUSTUP_HOME = Join-Path $chatTools 'rustup'
$env:GRADLE_USER_HOME = Join-Path $chatTools 'gradle-home'
$env:ANDROID_HOME = Join-Path $chatTools 'android-sdk'
$env:ANDROID_SDK_ROOT = $env:ANDROID_HOME
$env:ANDROID_USER_HOME = Join-Path $chatTools 'android-user'
New-Item -ItemType Directory -Force $env:ANDROID_USER_HOME | Out-Null
$env:ANDROID_NDK_HOME = Join-Path $env:ANDROID_HOME 'ndk\28.2.13676358'
$chatJdk = Get-ChildItem (Join-Path $chatTools 'jdk') -Directory -ErrorAction SilentlyContinue | Select-Object -First 1
if ($chatJdk) { $env:JAVA_HOME = $chatJdk.FullName }
$chatVswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
if (Test-Path $chatVswhere) {
    $chatVs = & $chatVswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($chatVs) {
        # Import the existing compiler environment without changing the user's global PATH.
        $chatDevCmd = Join-Path $chatVs 'Common7\Tools\VsDevCmd.bat'
        $chatEnvLines = & $env:ComSpec /d /c "call `"$chatDevCmd`" -arch=x64 -host_arch=x64 >nul && set"
        if ($LASTEXITCODE -ne 0) { throw 'Cannot initialize the Visual Studio compiler environment.' }
        foreach ($chatLine in $chatEnvLines) {
            if ($chatLine -match '^([^=]+)=(.*)$') {
                [Environment]::SetEnvironmentVariable($matches[1], $matches[2], 'Process')
            }
        }
        $chatCmake = Join-Path $chatVs 'Common7\IDE\CommonExtensions\Microsoft\CMake'
        $env:PATH = "$chatCmake\CMake\bin;$chatCmake\Ninja;$env:PATH"
    }
}
$env:PATH = "$env:CARGO_HOME\bin;$env:JAVA_HOME\bin;$chatTools\gradle\gradle-8.14.3\bin;$env:ANDROID_HOME\cmdline-tools\19.0\bin;$env:ANDROID_HOME\platform-tools;$env:PATH"
$env:CARGO_BUILD_JOBS = '4'
