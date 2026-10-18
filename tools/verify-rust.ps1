[CmdletBinding()]
param([switch]$Android)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')
Set-Location $chatRoot
function Run-Cargo([string[]]$Arguments) {
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw "cargo $Arguments failed ($LASTEXITCODE)" }
}
Run-Cargo @('fetch')
Run-Cargo @('check', '--workspace', '--all-features', '--locked')
Run-Cargo @('run', '--locked', '-p', 'chat-dependency-check', '--bin', 'dependency-smoke')
Run-Cargo @('build', '--locked', '-p', 'chat-dependency-check', '--lib')
Run-Cargo @('run', '--locked', '-p', 'chat-dependency-check', '--features', 'bindgen', '--bin', 'uniffi-bindgen', '--', 'generate', '--library', 'target/debug/chat_dependency_check.dll', '--language', 'kotlin', '--no-format', '--out-dir', 'tools/kotlin-dependency-check/build/generated/uniffi')
if (!(Test-Path (Join-Path $env:CARGO_HOME 'bin\cargo-ndk.exe'))) {
    Run-Cargo @('install', 'cargo-ndk', '--version', '4.1.2', '--locked')
}
if ($Android) {
    Run-Cargo @('ndk', '-t', 'arm64-v8a', '--platform', '26', '-o', 'tools/kotlin-dependency-check/build/generated/jniLibs', 'build', '--locked', '-p', 'chat-dependency-check', '--lib')
}
