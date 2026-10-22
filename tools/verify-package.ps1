# verify-package.ps1 - package-level regression for the self-contained app image (2026-10-20).
#
# Purpose: prove that the SHIPPED artifact still works - not the build tree, not the
# gradle classpath, but build/dist/app-image/Chat with its own bundled runtime.
#
# Stages:
#   1. build     : gradle :apps:desktop:appImageWithNative (skip with -SkipBuild)
#   2. structure : Chat.exe / app/app.jar / app/native/chat_ffi.dll / runtime/bin/java.exe
#   3. smokes    : run 4 smoke main classes with the PACKAGED java.exe and the packaged jar,
#                  each in its own temp working dir, -Djna.library.path=<app>/app/native, 180s timeout.
#
# Notes:
#   * Pure ASCII output (Windows PowerShell 5.1 mangles UTF-8 console text).
#   * A failing stage never stops the run; the exit code is 1 if any stage failed.
#   * Logs: build/verify-package-logs/<ts>/<stage>.log ; report: build/verify-package-<ts>.md
#
# Usage:
#   powershell -File tools/verify-package.ps1
#   powershell -File tools/verify-package.ps1 -SkipBuild
#   powershell -File tools/verify-package.ps1 -SkipBuild -Only PlatformSmoke,P0Smoke

[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [switch]$SkipGui,
    [string]$Only = ""
)

Set-Location (Split-Path -Parent $PSScriptRoot)
$root = (Get-Location).Path

# Toolchain environment (gradle/java). Dot-sourced so child processes inherit PATH/JAVA_HOME.
$activate = Join-Path $root "tools/activate.ps1"
if (Test-Path $activate) { . $activate }

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$logDir = Join-Path $root ("build/verify-package-logs/" + $timestamp)
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$reportPath = Join-Path $root ("build/verify-package-" + $timestamp + ".md")
$appDir = Join-Path $root "build/dist/app-image/Chat"

$selected = @()
if ($Only -ne "") {
    foreach ($name in $Only.Split(",")) {
        $trimmed = $name.Trim()
        if ($trimmed -ne "") { $selected += $trimmed }
    }
}

function Should-Run {
    param([string]$Name)
    if ($selected.Count -eq 0) { return $true }
    foreach ($item in $selected) {
        if ($item -ieq $Name) { return $true }
    }
    return $false
}

$script:stages = @()

function Add-Stage {
    param([string]$Name, [string]$Status, [double]$Seconds, [string]$Detail, [string]$Log)
    $script:stages += [pscustomobject]@{ Name = $Name; Status = $Status; Seconds = $Seconds; Detail = $Detail; Log = $Log }
    $suffix = ""
    if ($Detail -ne "") { $suffix = " - " + $Detail }
    Write-Host ("   " + $Status + " (" + $Seconds + "s)" + $suffix)
}

function Skip-Stage {
    param([string]$Name, [string]$Reason)
    Write-Host ""
    Write-Host ("== " + $Name + " == SKIP (" + $Reason + ")")
    Add-Stage -Name $Name -Status "SKIP" -Seconds 0 -Detail $Reason -Log ""
}

function Get-LogTail {
    param([string]$Path, [int]$Count = 3)
    if (-not (Test-Path $Path)) { return "" }
    $lines = @(Get-Content -Path $Path | Where-Object { $_.Trim() -ne "" })
    if ($lines.Count -eq 0) { return "" }
    $take = [math]::Min($Count, $lines.Count)
    return (($lines[($lines.Count - $take)..($lines.Count - 1)]) -join " | ")
}

# ---------------------------------------------------------------- stage 1: build
if (-not (Should-Run "build")) {
    Skip-Stage -Name "build" -Reason "not selected by -Only"
} elseif ($SkipBuild) {
    Skip-Stage -Name "build" -Reason "-SkipBuild"
} else {
    Write-Host ""
    Write-Host "== build =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "build.log"
    cmd /c ("gradlew.bat :apps:desktop:appImageWithNative --offline --max-workers=1 > `"" + $log + "`" 2>&1") | Out-Null
    $code = $LASTEXITCODE
    $sw.Stop()
    $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    if ($code -eq 0) {
        Add-Stage -Name "build" -Status "PASS" -Seconds $seconds -Detail "" -Log $log
    } else {
        Add-Stage -Name "build" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + (Get-LogTail -Path $log)) -Log $log
    }
}

# ---------------------------------------------------------------- stage 2: structure
$required = @(
    "Chat.exe",
    "app/app.jar",
    "app/native/chat_ffi.dll",
    "app/Chat.cfg",
    "runtime/bin/java.exe"
)
if (-not (Should-Run "structure")) {
    Skip-Stage -Name "structure" -Reason "not selected by -Only"
} else {
    Write-Host ""
    Write-Host "== structure =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "structure.log"
    $missing = @()
    $report = @()
    foreach ($relative in $required) {
        $full = Join-Path $appDir $relative
        if (Test-Path $full) {
            $size = (Get-Item $full).Length
            $report += ("OK   " + $relative + " (" + $size + " bytes)")
        } else {
            $missing += $relative
            $report += ("MISS " + $relative)
        }
    }
    Set-Content -Path $log -Value $report -Encoding ASCII
    $sw.Stop()
    $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    if ($missing.Count -eq 0) {
        Add-Stage -Name "structure" -Status "PASS" -Seconds $seconds -Detail "" -Log $log
    } else {
        Add-Stage -Name "structure" -Status "FAIL" -Seconds $seconds -Detail ("missing: " + ($missing -join ", ")) -Log $log
    }
}

# ---------------------------------------------------------------- stage 3: smokes
$smokes = @(
    @{ Name = "PlatformSmoke"; Class = "chat.desktop.PlatformSmokeKt" },
    @{ Name = "P0Smoke"; Class = "chat.desktop.P0SmokeKt" },
    @{ Name = "LocalStoreSmoke"; Class = "chat.desktop.LocalStoreSmokeKt" },
    @{ Name = "IdentitySmoke"; Class = "chat.desktop.IdentitySmokeKt" }
)

$smokeNames = @("PlatformSmoke", "P0Smoke", "LocalStoreSmoke", "IdentitySmoke");
$runSmokes = ($selected.Count -eq 0) -or ($selected -contains "smokes") -or ((@($selected | Where-Object { $smokeNames -contains $_ })).Count -gt 0);
if (-not $runSmokes) {
    Skip-Stage -Name "smokes" -Reason "not selected by -Only"
} else {
    $java = Join-Path $appDir "runtime/bin/java.exe"
    $jar = Join-Path $appDir "app/app.jar"
    $nativeDir = Join-Path $appDir "app/native"
    foreach ($smoke in $smokes) {
        $name = "smoke-" + $smoke.Name
        if (-not (Should-Run $smoke.Name) -and $selected.Count -gt 0) {
            Skip-Stage -Name $name -Reason "not selected by -Only"
            continue
        }
        Write-Host ""
        Write-Host ("== " + $name + " ==")
        if (-not ((Test-Path $java) -and (Test-Path $jar))) {
            Add-Stage -Name $name -Status "FAIL" -Seconds 0 -Detail "packaged java.exe or app.jar is missing" -Log ""
            continue
        }
        $workDir = Join-Path $env:TEMP ("chat-package-" + $smoke.Name + "-" + $timestamp)
        New-Item -ItemType Directory -Force -Path $workDir | Out-Null
        $log = Join-Path $logDir ($name + ".log")
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $psi = New-Object System.Diagnostics.ProcessStartInfo
        $psi.FileName = $java
        $psi.Arguments = "-Djna.library.path=`"" + $nativeDir + "`" -cp `"" + $jar + "`" " + $smoke.Class
        $psi.WorkingDirectory = $workDir
        $psi.UseShellExecute = $false
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $timedOut = $false
        $exitCode = -1
        try {
            $proc = [System.Diagnostics.Process]::Start($psi)
            $outTask = $proc.StandardOutput.ReadToEndAsync()
            $errTask = $proc.StandardError.ReadToEndAsync()
            if (-not $proc.WaitForExit(180000)) {
                $timedOut = $true
                try { $proc.Kill() } catch { }
            }
            $out = $outTask.Result
            $err = $errTask.Result
            if (-not $timedOut) { $exitCode = $proc.ExitCode }
            $text = @($out, $err) -join [Environment]::NewLine
            Set-Content -Path $log -Value $text -Encoding ASCII
        } catch {
            Set-Content -Path $log -Value ("launch failed: " + $_.Exception.Message) -Encoding ASCII
        }
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        if ($timedOut) {
            Add-Stage -Name $name -Status "FAIL" -Seconds $seconds -Detail "timed out after 180s" -Log $log
        } elseif ($exitCode -eq 0) {
            Add-Stage -Name $name -Status "PASS" -Seconds $seconds -Detail "" -Log $log
        } else {
            Add-Stage -Name $name -Status "FAIL" -Seconds $seconds -Detail ("exit " + $exitCode + "; " + (Get-LogTail -Path $log)) -Log $log
        }
        try { Remove-Item -Recurse -Force $workDir -ErrorAction SilentlyContinue } catch { }
    }
}

# ---------------------------------------------------------------- stage 4: gui launch aliveness
if (-not (Should-Run "gui")) {
    Skip-Stage -Name "gui" -Reason "not selected by -Only"
} elseif ($SkipGui) {
    Skip-Stage -Name "gui" -Reason "-SkipGui"
} else {
    Write-Host ""
    Write-Host "== gui =="
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $log = Join-Path $logDir "gui.log"
    $checker = Join-Path $root "tools/check-gui-launch.ps1"
    if (-not (Test-Path $checker)) {
        $sw.Stop()
        Add-Stage -Name "gui" -Status "FAIL" -Seconds ([math]::Round($sw.Elapsed.TotalSeconds, 1)) -Detail "tools/check-gui-launch.ps1 is missing" -Log ""
    } else {
        cmd /c ("powershell -NoProfile -ExecutionPolicy Bypass -File `"" + $checker + "`" > `"" + $log + "`" 2>&1") | Out-Null
        $code = $LASTEXITCODE
        $sw.Stop()
        $seconds = [math]::Round($sw.Elapsed.TotalSeconds, 1)
        if ($code -eq 0) {
            Add-Stage -Name "gui" -Status "PASS" -Seconds $seconds -Detail "" -Log $log
        } elseif ($code -eq 3) {
            # 无交互桌面：既不判 PASS 也不判 FAIL，明确标 SKIP 并说明原因。
            Add-Stage -Name "gui" -Status "SKIP" -Seconds $seconds -Detail "no interactive desktop session" -Log $log
        } else {
            Add-Stage -Name "gui" -Status "FAIL" -Seconds $seconds -Detail ("exit " + $code + "; " + (Get-LogTail -Path $log)) -Log $log
        }
    }
}

# ---------------------------------------------------------------- summary
$failed = @($script:stages | Where-Object { $_.Status -eq "FAIL" })
$overall = "PASS"
if ($failed.Count -gt 0) { $overall = "FAIL" }

Write-Host ""
Write-Host "===== verify-package summary ====="
foreach ($stage in $script:stages) {
    $line = "{0,-22} {1,-6} {2,8}s" -f $stage.Name, $stage.Status, $stage.Seconds
    if ($stage.Detail -ne "") { $line = $line + "  " + $stage.Detail }
    Write-Host $line
}
Write-Host ("overall: " + $overall)
Write-Host ("report:  " + $reportPath)

$report = New-Object System.Collections.Generic.List[string]
$report.Add("# verify-package report (" + $timestamp + ")")
$report.Add("")
$report.Add("- overall: **" + $overall + "**")
$report.Add("- artifact: build/dist/app-image/Chat")
$report.Add("- generated: " + (Get-Date -Format "yyyy-MM-dd HH:mm:ss"))
$report.Add("")
$report.Add("| stage | status | seconds | detail | log |")
$report.Add("| --- | --- | --- | --- | --- |")
foreach ($stage in $script:stages) {
    $logText = "-"
    if ($stage.Log -ne "") { $logText = "build/verify-package-logs/" + $timestamp + "/" + [System.IO.Path]::GetFileName($stage.Log) }
    $detail = $stage.Detail
    if ($report -ne $null -and $detail -eq "") { $detail = "-" }
    $report.Add("| " + $stage.Name + " | " + $stage.Status + " | " + $stage.Seconds + " | " + $detail + " | " + $logText + " |")
}
$report.Add("")
$report.Add("## notes")
$report.Add("")
$report.Add("- The four smokes run with the PACKAGED runtime/bin/java.exe and app/app.jar, not with the build tree.")
$report.Add("- Each smoke runs in its own temp working directory; -Djna.library.path points at app/native.")
$report.Add("- A failing stage never stops later stages; the exit code is 1 when any stage failed.")
Set-Content -Path $reportPath -Value $report -Encoding ASCII

# 打包成功后把 app.jar 的时间戳刷成“刚刚”：
# Gradle 按**内容**判断是否重建，源码只改 mtime 时 jar 不会重写，单看 mtime 会误判成 stale。
# 这里之所以可以刷，是因为上面四个冒烟**确实是用这个 jar 跑过的**（不是空口说“已打包”）。
if ($overall -eq "PASS") {
    $jarPath = Join-Path $appDir "app/app.jar"
    if (Test-Path $jarPath) {
        (Get-Item $jarPath).LastWriteTimeUtc = [datetime]::UtcNow
        Write-Host ("stamped: " + $jarPath)
    }
}

if ($overall -eq "PASS") { exit 0 }
exit 1
