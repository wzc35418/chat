[CmdletBinding()]
param([switch]$RefreshLocks)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'activate.ps1')
Set-Location $chatRoot
New-Item -ItemType Directory -Force build\p0-evidence | Out-Null
& cargo test --workspace --all-features --locked --offline *> build\p0-evidence\rust-tests.log
if ($LASTEXITCODE -ne 0) { Get-Content build\p0-evidence\rust-tests.log -Tail 60; throw 'Rust P0 tests failed.' }
& cargo test --locked --offline -p chat-crypto --test p0_persistence -- --nocapture *> build\p0-evidence\persistence-crash.log
if ($LASTEXITCODE -ne 0) { Get-Content build\p0-evidence\persistence-crash.log -Tail 80; throw 'P0 SQLite/OpenMLS crash tests failed.' }
& cargo test --locked --offline -p chat-node-services --test p0_accept -- --nocapture *> build\p0-evidence\node-acceptance.log
if ($LASTEXITCODE -ne 0) { Get-Content build\p0-evidence\node-acceptance.log -Tail 80; throw 'P0 node acceptance tests failed.' }
Write-Host 'PASS Rust workspace and P0 protocol/MLS/iroh/SQLite/node crash tests.'
& cargo run --locked --offline -p chat-core --bin chat-p0 1> build\p0-evidence\rust-roundtrip.json 2> build\p0-evidence\rust-roundtrip-build.log
if ($LASTEXITCODE -ne 0) { throw 'Rust P0 CLI roundtrip failed.' }
$gradleArgs = @(
    ':apps:desktop:p0Smoke', ':apps:desktop:p0Snapshot',
    ':apps:desktop:presentationChecks', ':apps:desktop:previewSnapshot',
    ':apps:android:assembleDebug',
    ':desktop:plugin-host:assemble', ':desktop:platform:assemble',
    ':shared:platform-api:assemble',
    ':plugins:examples:hello-command:assemble', ':plugins:examples:message-formatter:assemble',
    '--console=plain', '--offline'
)
if ($RefreshLocks) { $gradleArgs += '--write-locks' }
& .\gradlew.bat @gradleArgs *> build\p0-evidence\gradle-p0.log
if ($LASTEXITCODE -ne 0) { Get-Content build\p0-evidence\gradle-p0.log -Tail 80; throw 'Kotlin/Compose/Android P0 verification failed.' }
$artifacts = @(
    'build/native/host/chat_ffi.dll',
    'build/native/android/arm64-v8a/libchat_ffi.so',
    'apps/android/build/outputs/apk/debug/android-debug.apk'
)
$hashes = foreach ($artifact in $artifacts) {
    $hash = Get-FileHash -LiteralPath $artifact -Algorithm SHA256
    [ordered]@{ path = $artifact; sha256 = $hash.Hash.ToLowerInvariant(); bytes = (Get-Item -LiteralPath $artifact).Length }
}
$report = [ordered]@{
    date = [DateTimeOffset]::Now.ToString('o')
    status = 'PASS'
    scope = 'Windows x64; ephemeral local MLS/iroh chain plus isolated disposable SQLite/OpenMLS and signed-node crash experiments; Kotlin/Compose; Android arm64 build'
    checks = @('Rust workspace tests', 'Fourteen process-abort checkpoints covering T1 intent, T1-to-T2 preparation, SQLite/OpenMLS send/receive, and isolated node accept; rollback and fixed-cipher retry', 'Node same-submission replay, pinned-key Ed25519 receipt verification, tamper rejection, contiguous hash-chain sequence and durable delivery job', 'Rust CLI encrypted roundtrip', 'Kotlin/native bidirectional roundtrip and error lifecycle', 'Compose P0 desktop/compact rendering and preview presentation checks/snapshots', 'Android debug APK with Rust arm64 library', 'JVM skeleton and example plugin builds')
    artifacts = @($hashes)
    evidence = @('build/p0-evidence/rust-tests.log', 'build/p0-evidence/persistence-crash.log', 'build/p0-evidence/node-acceptance.log', 'build/p0-evidence/rust-roundtrip.json', 'build/p0-evidence/gradle-p0.log', 'build/p0-snapshots/desktop.png', 'build/p0-snapshots/compact.png', 'build/preview-snapshots/preview-desktop-accepted.png', 'build/preview-snapshots/preview-compact-chat.png')
    notRun = @('Android device runtime', 'Linux runtime', 'Cross-device interoperability', 'Formal production T1/C01, authenticated HTTP and signed node receipt for C05, C07-C10, full C06 mailbox ACK, power-loss recovery', 'Encrypted database and platform key protection', 'Self-hosted relay/NAT matrix')
}
$report | ConvertTo-Json -Depth 7 | Set-Content -Encoding utf8 docs\p0-validation-latest.json
Write-Host 'PASS P0 minimal chain. Evidence: docs/p0-validation-latest.json'
