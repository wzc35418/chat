@echo off
setlocal
pushd "%~dp0" || exit /b 1
echo Starting Chat desktop app. This window stays open while the app is running.
echo If Gradle stops at Task :apps:desktop:run, look for "Chat" on the taskbar or use Alt+Tab.
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0tools\run-desktop.ps1"
set "chatExitCode=%ERRORLEVEL%"
popd
if not "%chatExitCode%"=="0" (
    echo.
    echo Chat failed to start. Review the error above.
    pause
)
exit /b %chatExitCode%
