[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$chatRoot = Split-Path $PSScriptRoot -Parent
$chatTools = Join-Path $chatRoot '.tools'
$downloads = Join-Path $chatTools 'downloads'
New-Item -ItemType Directory -Force $downloads | Out-Null

function Get-Artifact([string]$Url, [string]$Path, [string]$Hash = '', [string]$Algorithm = 'SHA256') {
    if (!(Test-Path $Path)) {
        Write-Host "Downloading $(Split-Path $Path -Leaf)"
        Invoke-WebRequest -Uri $Url -OutFile "$Path.partial" -TimeoutSec 1200
        Move-Item -LiteralPath "$Path.partial" -Destination $Path -Force
    }
    if ($Hash -and (Get-FileHash -LiteralPath $Path -Algorithm $Algorithm).Hash -ne $Hash) {
        throw "Checksum mismatch: $Path. Remove the corrupt download before retrying."
    }
}
function Invoke-Checked([string]$Program, [string[]]$Arguments) {
    & $Program @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Program failed with exit code $LASTEXITCODE" }
}

$lockPath = Join-Path $PSScriptRoot 'toolchain-lock.json'
if (Test-Path $lockPath) {
    $versions = Get-Content -Raw $lockPath | ConvertFrom-Json
} else {
    $jdkAssets = Invoke-RestMethod 'https://api.adoptium.net/v3/assets/latest/21/hotspot?architecture=x64&image_type=jdk&os=windows&vendor=eclipse'
    $jdk = $jdkAssets[0].binary.package
    $rustManifest = Join-Path $downloads 'channel-rust-stable.toml'
    Get-Artifact 'https://static.rust-lang.org/dist/channel-rust-stable.toml' $rustManifest
    $rustText = Get-Content -Raw $rustManifest
    if ($rustText -notmatch '(?ms)\[pkg\.rust\]\s*version = "(\d+\.\d+\.\d+)') { throw 'Cannot read Rust stable version.' }
    $rustVersion = $matches[1]
    $gradleHash = (Invoke-RestMethod 'https://services.gradle.org/distributions/gradle-8.14.3-bin.zip.sha256').Trim()
    $repository = Join-Path $downloads 'repository2-1.xml'
    Get-Artifact 'https://dl.google.com/android/repository/repository2-1.xml' $repository
    [xml]$sdkRepo = Get-Content -Raw $repository
    $sdkPackage = $sdkRepo.SelectSingleNode("//*[local-name()='remotePackage' and @path='cmdline-tools;19.0']")
    $sdkArchive = $sdkPackage.SelectSingleNode(".//*[local-name()='archive'][*[local-name()='host-os']='windows']/*[local-name()='complete']")
    if (!$sdkArchive) { throw 'Cannot locate Windows Android command-line tools.' }
    $versions = [ordered]@{
        jdk = [ordered]@{ url = $jdk.link; sha256 = $jdk.checksum; file = $jdk.name }
        rust = $rustVersion
        gradle = [ordered]@{ version = '8.14.3'; sha256 = $gradleHash }
        androidCommandLine = [ordered]@{
            url = 'https://dl.google.com/android/repository/' + $sdkArchive.url
            checksum = [string]$sdkArchive.checksum
            checksumAlgorithm = if ($sdkArchive.checksum.type -eq 'sha256') { 'SHA256' } else { 'SHA1' }
        }
        androidPackages = @('platform-tools', 'platforms;android-36', 'build-tools;36.0.0', 'ndk;28.2.13676358')
    }
    $versions | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $lockPath
    $versions = Get-Content -Raw $lockPath | ConvertFrom-Json
}

$jdkZip = Join-Path $downloads $versions.jdk.file
Get-Artifact $versions.jdk.url $jdkZip $versions.jdk.sha256
if (!(Get-ChildItem (Join-Path $chatTools 'jdk') -Filter java.exe -Recurse -ErrorAction SilentlyContinue)) {
    New-Item -ItemType Directory -Force (Join-Path $chatTools 'jdk') | Out-Null
    Invoke-Checked 'tar.exe' @('-xf', $jdkZip, '-C', (Join-Path $chatTools 'jdk'))
}
$gradleZip = Join-Path $downloads 'gradle-8.14.3-bin.zip'
Get-Artifact 'https://services.gradle.org/distributions/gradle-8.14.3-bin.zip' $gradleZip $versions.gradle.sha256
if (!(Test-Path (Join-Path $chatTools 'gradle\gradle-8.14.3\bin\gradle.bat'))) {
    New-Item -ItemType Directory -Force (Join-Path $chatTools 'gradle') | Out-Null
    Invoke-Checked 'tar.exe' @('-xf', $gradleZip, '-C', (Join-Path $chatTools 'gradle'))
}
$sdkZip = Join-Path $downloads 'android-commandlinetools-19.zip'
Get-Artifact $versions.androidCommandLine.url $sdkZip $versions.androidCommandLine.checksum $versions.androidCommandLine.checksumAlgorithm
$sdkTools = Join-Path $chatTools 'android-sdk\cmdline-tools'
if (!(Test-Path (Join-Path $sdkTools '19.0\bin\sdkmanager.bat'))) {
    New-Item -ItemType Directory -Force $sdkTools | Out-Null
    Invoke-Checked 'tar.exe' @('-xf', $sdkZip, '-C', $sdkTools)
    Move-Item -LiteralPath (Join-Path $sdkTools 'cmdline-tools') -Destination (Join-Path $sdkTools '19.0')
}

. (Join-Path $PSScriptRoot 'activate.ps1')
$rustInstaller = Join-Path $downloads 'rustup-init.exe'
if (!(Test-Path (Join-Path $env:CARGO_HOME 'bin\rustup.exe'))) {
    Get-Artifact 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe' $rustInstaller
    # Rustup is published without Authenticode; verify the vendor's SHA-256 instead.
    $rustHashFile = Join-Path $downloads 'rustup-init.exe.sha256'
    Get-Artifact 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe.sha256' $rustHashFile
    $rustHash = ((Get-Content -Raw $rustHashFile) -split '\s+')[0]
    if ((Get-FileHash $rustInstaller).Hash -ne $rustHash) { throw 'Rustup checksum mismatch.' }
    Invoke-Checked $rustInstaller @('-y', '--no-modify-path', '--default-host', 'x86_64-pc-windows-msvc', '--default-toolchain', $versions.rust, '--profile', 'minimal')
}
Invoke-Checked 'rustup' @('toolchain', 'install', $versions.rust, '--profile', 'minimal', '--component', 'rustfmt,clippy')
Invoke-Checked 'rustup' @('target', 'add', 'aarch64-linux-android', '--toolchain', $versions.rust)

Write-Host 'Installing Android SDK packages (accepting the required SDK/NDK licenses).'
$sdkManager = Join-Path $env:ANDROID_HOME 'cmdline-tools\19.0\bin\sdkmanager.bat'
$licenseAnswers = 1..100 | ForEach-Object { 'y' }
$sdkArgs = @("--sdk_root=$env:ANDROID_HOME") + @($versions.androidPackages)
$sdkLog = Join-Path $chatTools 'android-install.log'
$licenseAnswers | & $sdkManager @sdkArgs *> $sdkLog
if ($LASTEXITCODE -ne 0) { throw "Android SDK installation failed. See $sdkLog" }
foreach ($package in $versions.androidPackages) {
    $packagePath = Join-Path $env:ANDROID_HOME ($package.Replace(';', '\'))
    if (!(Test-Path (Join-Path $packagePath 'source.properties'))) { throw "SDK package missing after installation: $package" }
}
$sdkPath = $env:ANDROID_HOME.Replace('\','/')
"sdk.dir=$sdkPath" | Set-Content -Encoding ascii (Join-Path $chatRoot 'local.properties')
Write-Host 'Windows toolchain installation finished.'
