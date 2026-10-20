# check-gui-launch.ps1 - GUI launch aliveness check for the packaged app image (2026-10-20).
#
# What it proves: the shipped Chat.exe starts and is STILL RUNNING after 8 seconds
# (a launcher that dies immediately - missing DLL, bad runtime image, wrong JVM args -
# is caught here, which the jar smokes cannot catch).
#
# Exit codes:
#   0 = the process survived the observation window (PASS)
#   1 = the process exited early or could not be started (FAIL)
#   3 = no interactive desktop session -> SKIP (never reported as PASS/FAIL)
#
# Pure ASCII output. Idempotent: kills leftover Chat processes before starting.

[CmdletBinding()]
param(
    [int]$WaitSeconds = 8,
    [string]$Executable = ""
)

Set-Location (Split-Path -Parent $PSScriptRoot)
$root = (Get-Location).Path
if ($Executable -eq "") {
    $Executable = Join-Path $root "build/dist/app-image/Chat/Chat.exe"
}

Write-Host ("exe: " + $Executable)
if (-not (Test-Path $Executable)) {
    Write-Host "FAIL: Chat.exe is missing"
    exit 1
}

# Desktop/session gate: without an interactive window station a GUI app cannot be
# judged - report SKIP instead of inventing a verdict.
$interactive = $false
try { $interactive = [System.Environment]::UserInteractive } catch { $interactive = $false }
$session = $env:SESSIONNAME
if (-not $interactive -or $session -eq "Services" -or $session -eq "" -or $session -eq $null) {
    Write-Host ("SKIP: no interactive desktop session (UserInteractive=" + $interactive + ", SESSIONNAME=" + $session + ")")
    Write-Host "SKIP: launch-aliveness cannot be judged from a non-interactive session."
    exit 3
}

# Idempotent: clear leftovers from earlier runs (a stale Chat.exe would fake a PASS).
$leftovers = @(Get-Process -Name Chat -ErrorAction SilentlyContinue)
if ($leftovers.Count -gt 0) {
    Write-Host ("cleanup: stopping " + $leftovers.Count + " leftover Chat process(es)")
    foreach ($process in $leftovers) {
        try { $process.Kill() } catch { }
    }
    Start-Sleep -Milliseconds 500
}

$started = $null
try {
    $started = Start-Process -FilePath $Executable -PassThru -ErrorAction Stop
} catch {
    Write-Host ("FAIL: could not start Chat.exe: " + $_.Exception.Message)
    exit 1
}
Write-Host ("started: pid=" + $started.Id + ", waiting " + $WaitSeconds + "s")
Start-Sleep -Seconds $WaitSeconds

$survived = $false
$exitCode = $null
try {
    $survived = -not $started.HasExited
    if (-not $survived) { $exitCode = $started.ExitCode }
} catch {
    $survived = $false
}

if ($survived) {
    Write-Host ("PASS: Chat.exe is still running after " + $WaitSeconds + "s (pid=" + $started.Id + ")")
    try {
        $started.Kill()
        Write-Host "cleanup: process stopped"
    } catch {
        Write-Host ("cleanup: could not stop the process: " + $_.Exception.Message)
    }
    exit 0
}

if ($null -ne $exitCode) {
    Write-Host ("FAIL: Chat.exe exited during the " + $WaitSeconds + "s window (exit code " + $exitCode + ")")
} else {
    Write-Host ("FAIL: Chat.exe exited during the " + $WaitSeconds + "s window")
}
exit 1
