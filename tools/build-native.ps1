[CmdletBinding()]
param([switch]$AndroidOnly)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')
Set-Location $chatRoot
function Run-Cargo([string[]]$Arguments) {
    & cargo @Arguments
    if ($LASTEXITCODE -ne 0) { throw "Rust build failed ($LASTEXITCODE)" }
}
function Clear-RedundantIrohLibraries([string]$Directory, [string]$Pattern) {
    # Only remove this build's redundant library artifacts, never source files or entire directories.
    $allowedRoot = [IO.Path]::GetFullPath((Join-Path $chatRoot 'build\native')) + [IO.Path]::DirectorySeparatorChar
    $targetDirectory = [IO.Path]::GetFullPath((Join-Path $chatRoot $Directory))
    if (!$targetDirectory.StartsWith($allowedRoot, [StringComparison]::OrdinalIgnoreCase)) { throw 'Invalid native artifact directory.' }
    if (Test-Path -LiteralPath $targetDirectory) {
        Get-ChildItem -LiteralPath $targetDirectory -File -Filter $Pattern | ForEach-Object {
            Remove-Item -LiteralPath $_.FullName -Force
        }
    }
}
if ($AndroidOnly) {
    Run-Cargo @('ndk', '-t', 'arm64-v8a', '--platform', '26', 'build', '--locked', '--offline', '-p', 'chat-ffi', '--lib')
    New-Item -ItemType Directory -Force build\native\android\arm64-v8a | Out-Null
    Clear-RedundantIrohLibraries 'build\native\android\arm64-v8a' 'libiroh*.so'
    Copy-Item -LiteralPath target\aarch64-linux-android\debug\libchat_ffi.so -Destination build\native\android\arm64-v8a\libchat_ffi.so -Force
} else {
    Run-Cargo @('build', '--locked', '--offline', '-p', 'chat-ffi', '--lib')
    New-Item -ItemType Directory -Force build\native\host,build\generated\chat-ffi | Out-Null
    # iroh's Rust code is linked into chat_ffi; its incidental cdylibs are not runtime dependencies.
    Clear-RedundantIrohLibraries 'build\native\host' 'iroh*.dll'
    Copy-Item -LiteralPath target\debug\chat_ffi.dll -Destination build\native\host\chat_ffi.dll -Force
    Run-Cargo @('run', '--locked', '--offline', '-p', 'chat-ffi', '--features', 'bindgen', '--bin', 'chat-bindgen', '--', 'generate', '--library', 'target/debug/chat_ffi.dll', '--language', 'kotlin', '--no-format', '--out-dir', 'build/generated/chat-ffi')
}
