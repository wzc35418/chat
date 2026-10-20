# verify-all.ps1 - one-shot full regression (added 2026-10-19).
#
# Design notes:
#   * Pure ASCII output on purpose: Windows PowerShell 5.1 mangles UTF-8
#     console text, so every message here is ASCII only.
#   * Stage 1 is the disk check (added after the 2026-10-19 disk-full
#     incident); it aborts the whole run when free space is too low.
#   * A failing stage never stops the run: later stages still execute so one
#     report shows the real state of everything. The exit code is 1 when at
#     least one stage failed (2 when the disk check aborts).
#   * Native commands run through cmd.exe with plain file redirection:
#     PowerShell 5.1 otherwise turns native stderr into ErrorRecords and the
#     log file ends up empty on failure.
#   * Logs: build/verify-logs/<timestamp>/<stage>.log
#     Report: build/verify-all-<timestamp>.md
#
# Usage:
#   powershell -File tools/verify-all.ps1
#   powershell -File tools/verify-all.ps1 -SkipAndroid -SkipGradle
#   powershell -File tools/verify-all.ps1 -TargetDir C:\chat-target

[CmdletBinding()]
param(
    [switch]$SkipTwoDevice,
    [switch]$SkipLan,
    [switch]$SkipTls,
    [switch]$SkipOffline,
    [switch]$SkipAndroid,
    [switch]$SkipGradle,
    [string]$TargetDir = ""
)

Set-Location (Split-Path -Parent $PSScriptRoot)
$root = (Get-Location).Path

# Toolchain environment (cargo, JDK, Android SDK, MSVC). Dot-sourced so the
# child cmd.exe processes inherit PATH/JAVA_HOME/etc.
$activate = Join-Path $root "tools/activate.ps1"
if (Test-Path $activate) {
    . $activate
}

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logDir = Join-Path $root ("build/verify-logs/" + $timestamp)
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$reportPath = Join-Path $root ("build/verify-all-" + $timestamp + ".md")

$script:stages = @()

# Runs one native command line through cmd.exe, capturing stdout+stderr to a log.
function Invoke-Native {
    param(
        [string]$Name,
        [string]$CommandLine
    )
    $log = Join-Path $logDir ($Name + ".log")
        $fullCommand = $CommandLine + ' > "' + $log + '" 2>&1'
    cmd /c $fullCommand | Out-Null
    return [pscustomobject]@{ Exit = $LASTEXITCODE; Log = $log }
}

# Last few non-empty log lines, used as a short failure reason.
function Get-LogTail {
    param([string]$Path)
    if (-not (Test-Path $Path)) { return "" }
    $lines = @(Get-Content -Path $Path | Where-Object { $_.Trim() -ne "" })
    if ($lines.Count -eq 0) { return "" }
    $take = [math]::Min(3, $lines.Count)
    return (($lines[($lines.Count - $take)..($lines.Count - 1)]) -join " | ")
}

function Add-Stage {
    param(
        [string]$Name,
        [string]$Status,
        [double]$Seconds,
        [string]$Detail,
        [string]$Log
    )
    $script:stages += [pscustomobject]@{
        Name = $Name
        Status = $Status
        Seconds = $Seconds
        Detail = $Detail
        Log = $Log
    }
    $suffix = ""
    if ($Detail -ne "") { $suffix = " - " + $Detail }
    Write-Host ("   " + $Status + " (" + $Seconds + "s)" + $suffix)
}

function Invoke-Stage {
    param(
        [string]$Name,
        [string]$CommandLine
    )
    Write-Host ""
    Write-Host ("== " + $Name + " ==")
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $result = Invoke-Native -Name $Name -CommandLine $CommandLine
    $stopwatch.Stop()
    $seconds = [math]::Round($stopwatch.Elapsed.TotalSeconds, 1)
    if ($result.Exit -eq 0) {
        Add-Stage -Name $Name -Status "PASS" -Seconds $seconds -Detail "" -Log $result.Log
    } else {
        $tail = Get-LogTail -Path $result.Log
        Add-Stage -Name $Name -Status "FAIL" -Seconds $seconds -Detail ("exit " + $result.Exit + "; " + $tail) -Log $result.Log
    }
}

function Add-SkippedStage {
    param([string]$Name, [string]$Reason)
    Write-Host ""
    Write-Host ("== " + $Name + " == SKIP (" + $Reason + ")")
    Add-Stage -Name $Name -Status "SKIP" -Seconds 0 -Detail $Reason -Log ""
}

# ---------------------------------------------------------------- stage 1: disk
$disk = Invoke-Native -Name "disk" -CommandLine "powershell -NoProfile -ExecutionPolicy Bypass -File tools\check-disk.ps1"
Write-Host ""
Write-Host "== disk =="
if ($disk.Exit -ne 0) {
    Add-Stage -Name "disk" -Status "FAIL" -Seconds 0 -Detail ("exit " + $disk.Exit + "; " + (Get-LogTail -Path $disk.Log)) -Log $disk.Log
    Write-Host ""
    Write-Host "ABORT: disk check failed; refusing to build. See build/verify-logs."
    exit 2
}
Add-Stage -Name "disk" -Status "PASS" -Seconds 0 -Detail "" -Log $disk.Log

# ---------------------------------------------------------------- stage 2: rust
if ($TargetDir -ne "") {
    $env:CARGO_TARGET_DIR = $TargetDir
    Write-Host ("   CARGO_TARGET_DIR=" + $TargetDir)
}
$env:CARGO_INCREMENTAL = "0"
$rustCommand = "cargo check --offline --locked --workspace --all-targets"
# Quoted set form: without it the space before && becomes part of the value.
if ($TargetDir -ne "") { $rustCommand = 'set "CARGO_TARGET_DIR=' + $TargetDir + '" && ' + $rustCommand }
Invoke-Stage -Name "rust-check" -CommandLine $rustCommand

# ---------------------------------------------------------------- stage 3: project
Invoke-Stage -Name "verify-project" -CommandLine "node tools\verify-project.cjs"

# ---------------------------------------------------------------- stage 4: gradle
if ($SkipGradle) {
    Add-SkippedStage -Name "gradle" -Reason "-SkipGradle"
} else {
    $tasks = @(
        ":apps:desktop:presentationChecks",
        ":apps:desktop:p0Smoke",
        ":apps:desktop:p0Snapshot",
        ":apps:desktop:previewSnapshot",
        ":apps:desktop:localStoreSmoke",
        ":apps:desktop:identitySmoke",
        ":apps:desktop:groupSmoke",
        ":apps:desktop:attachmentSmoke",
        ":apps:desktop:pluginSmoke",
        ":apps:desktop:pluginJvmSmoke",
        ":apps:desktop:platformSmoke",
        ":apps:desktop:credentialManagerSmoke",
        ":apps:desktop:keySlotSelectorSmoke"
    )
    Write-Host ""
    Write-Host ("== gradle == " + $tasks.Count + " tasks, max-workers=1")
    Invoke-Stage -Name "gradle" -CommandLine ("gradlew.bat " + ($tasks -join " ") + " --offline --max-workers=1")
}

# ---------------------------------------------------------------- stage 5: android
if ($SkipAndroid) {
    Add-SkippedStage -Name "android" -Reason "-SkipAndroid"
} else {
    Invoke-Stage -Name "android" -CommandLine "gradlew.bat :apps:android:assembleDebug --offline --max-workers=1"
}

# ---------------------------------------------------------------- stage 6: delivery package
if ($SkipPackage) {
    Add-SkippedStage -Name "package" -Reason "-SkipPackage"
} else {
    Write-Host ""
    Write-Host "== package =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "package.log"
    $script = Join-Path $root "tools/verify-package.ps1"
    if (-not (Test-Path $script)) {
        $sw.Stop()
        Add-Stage -Name "package" -Status "FAIL" -Seconds ([math]::Round($sw.Elapsed.TotalSeconds, 1)) -Detail "tools/verify-package.ps1 is missing" -Log ""
    } else {
        cmd /c ("powershell -NoProfile -ExecutionPolicy Bypass -File `"" + $script + "`" -SkipBuild > `"" + $log + "`" 2>&1") | Out-Null
        $code = $LASTEXITCODE
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        $reportLine = (Get-Content -Path $log | Where-Object { $_ -match "^report:" } | Select-Object -Last 1)
        $detail = ""
        if ($reportLine) { $detail = ($reportLine -replace "^report:\s*", "").Trim() }
        if ($code -eq 0) {
            Add-Stage -Name "package" -Status "PASS" -Seconds $seconds -Detail $detail -Log $log
        } else {
            $tail = Get-LogTail -Path $log
            Add-Stage -Name "package" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + $tail + "; " + $detail) -Log $log
        }
    }
}

# ---------------------------------------------------------------- stage 7: two-device exchange
if ($SkipTwoDevice) {
    Add-SkippedStage -Name "two-device" -Reason "-SkipTwoDevice"
} else {
    Write-Host ""
    Write-Host "== two-device =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "two-device.log"
    $script = Join-Path $root "tools/demo-two-devices.ps1"
    if (-not (Test-Path $script)) {
        $sw.Stop()
        Add-Stage -Name "two-device" -Status "FAIL" -Seconds ([math]::Round($sw.Elapsed.TotalSeconds, 1)) -Detail "tools/demo-two-devices.ps1 is missing" -Log ""
    } else {
        # app-image 由 package 阶段构建；这里只跑演示本身，避免两条构建路径抢 build 目录。
        cmd /c ("powershell -NoProfile -ExecutionPolicy Bypass -File `"" + $script + "`" -SkipBuild > `"" + $log + "`" 2>&1") | Out-Null
        $code = $LASTEXITCODE
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        if ($code -eq 0) {
            Add-Stage -Name "two-device" -Status "PASS" -Seconds $seconds -Detail "A/B exchange (send accepted + pull applied)" -Log $log
        } else {
            Add-Stage -Name "two-device" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + (Get-LogTail -Path $log)) -Log $log
        }
    }
}

# ---------------------------------------------------------------- stage 8: LAN (non-loopback)
if ($SkipLan) {
    Add-SkippedStage -Name "lan" -Reason "-SkipLan"
} else {
    Write-Host ""
    Write-Host "== lan =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "lan.log"
    $script = Join-Path $root "tools/verify-lan.ps1"
    if (-not (Test-Path $script)) {
        $sw.Stop()
        Add-Stage -Name "lan" -Status "FAIL" -Seconds ([math]::Round($sw.Elapsed.TotalSeconds, 1)) -Detail "tools/verify-lan.ps1 is missing" -Log ""
    } else {
        cmd /c ("powershell -NoProfile -ExecutionPolicy Bypass -File `"" + $script + "`" > `"" + $log + "`" 2>&1") | Out-Null
        $code = $LASTEXITCODE
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        $lanLine = (Get-Content -Path $log | Where-Object { $_ -match "^lan address:" } | Select-Object -Last 1)
        $detail = ""
        if ($lanLine) { $detail = ($lanLine -replace "^lan address:\\s*", "").Trim() }
        if ($code -eq 0) {
            Add-Stage -Name "lan" -Status "PASS" -Seconds $seconds -Detail ("non-loopback e2e; " + $detail) -Log $log
        } elseif ($code -eq 3) {
            Add-Stage -Name "lan" -Status "SKIP" -Seconds $seconds -Detail "no non-loopback IPv4" -Log $log
        } else {
            Add-Stage -Name "lan" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + (Get-LogTail -Path $log)) -Log $log
        }
    }
}

# ---------------------------------------------------------------- stage 9: node-side TLS
if ($SkipTls) {
    Add-SkippedStage -Name "tls" -Reason "-SkipTls"
} else {
    Write-Host ""
    Write-Host "== tls =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "tls.log"
    $script = Join-Path $root "tools/verify-tls.ps1"
    if (-not (Test-Path $script)) {
        $sw.Stop()
        Add-Stage -Name "tls" -Status "FAIL" -Seconds ([math]::Round($sw.Elapsed.TotalSeconds, 1)) -Detail "tools/verify-tls.ps1 is missing" -Log ""
    } else {
        cmd /c ("powershell -NoProfile -ExecutionPolicy Bypass -File `"" + $script + "`" > `"" + $log + "`" 2>&1") | Out-Null
        $code = $LASTEXITCODE
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        if ($code -eq 0) {
            Add-Stage -Name "tls" -Status "PASS" -Seconds $seconds -Detail "https health 200; plaintext refused; untrusted refused" -Log $log
        } elseif ($code -eq 3) {
            Add-Stage -Name "tls" -Status "SKIP" -Seconds $seconds -Detail "openssl not installed" -Log $log
        } else {
            Add-Stage -Name "tls" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + (Get-LogTail -Path $log)) -Log $log
        }
    }
}

# ---------------------------------------------------------------- stage 10: offline (store-and-forward)
if ($SkipOffline) {
    Add-SkippedStage -Name "offline" -Reason "-SkipOffline"
} else {
    Write-Host ""
    Write-Host "== offline =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "offline.log"
    $script = Join-Path $root "tools/verify-offline-delivery.ps1"
    if (-not (Test-Path $script)) {
        $sw.Stop()
        Add-Stage -Name "offline" -Status "FAIL" -Seconds ([math]::Round($sw.Elapsed.TotalSeconds, 1)) -Detail "tools/verify-offline-delivery.ps1 is missing" -Log ""
    } else {
        cmd /c ("powershell -NoProfile -ExecutionPolicy Bypass -File `"" + $script + "`" > `"" + $log + "`" 2>&1") | Out-Null
        $code = $LASTEXITCODE
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        if ($code -eq 0) {
            Add-Stage -Name "offline" -Status "PASS" -Seconds $seconds -Detail "sent with receiver offline; delivered on next sync" -Log $log
        } else {
            Add-Stage -Name "offline" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + (Get-LogTail -Path $log)) -Log $log
        }
    }
}

# ---------------------------------------------------------------- summary
$failed = @($script:stages | Where-Object { $_.Status -eq "FAIL" })
$overall = "PASS"
if ($failed.Count -gt 0) { $overall = "FAIL" }

Write-Host ""
Write-Host "===== verify-all summary ====="
foreach ($stage in $script:stages) {
    $line = "{0,-16} {1,-6} {2,8}s" -f $stage.Name, $stage.Status, $stage.Seconds
    if ($stage.Detail -ne "") { $line = $line + "  " + $stage.Detail }
    Write-Host $line
}
Write-Host ("overall: " + $overall)
Write-Host ("report:  " + $reportPath)

$report = New-Object System.Collections.Generic.List[string]
$report.Add("# verify-all report (" + $timestamp + ")")
$report.Add("")
$report.Add("- overall: **" + $overall + "**")
$report.Add("- generated: " + (Get-Date -Format "yyyy-MM-dd HH:mm:ss"))
$report.Add("- root: " + $root)
if ($TargetDir -ne "") { $report.Add("- CARGO_TARGET_DIR: " + $TargetDir) }
$report.Add("")
$report.Add("| stage | status | seconds | detail | log |")
$report.Add("| --- | --- | --- | --- | --- |")
foreach ($stage in $script:stages) {
    $logText = "-"
    if ($stage.Log -ne "") { $logText = "build/verify-logs/" + $timestamp + "/" + $stage.Name + ".log" }
    $detail = $stage.Detail
    if ($detail -eq "") { $detail = "-" }
    $report.Add("| " + $stage.Name + " | " + $stage.Status + " | " + $stage.Seconds + " | " + $detail + " | " + $logText + " |")
}
$report.Add("")
$report.Add("## notes")
$report.Add("")
$report.Add("- Stage 1 aborts the run (exit 2) when free disk space is below the tools/check-disk.ps1 threshold.")
$report.Add("- A failing stage never stops later stages; the exit code is 1 when any stage failed.")
$report.Add("- Gradle stage runs 11 desktop regression tasks with --offline --max-workers=1.")
$report.Add("- Android stage runs :apps:android:assembleDebug --offline --max-workers=1 unless -SkipAndroid.")
$report.Add("- Rust stage runs cargo check --offline --locked --workspace --all-targets.")
Set-Content -Path $reportPath -Value $report -Encoding ASCII

if ($overall -eq "PASS") { exit 0 }
exit 1
