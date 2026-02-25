@echo off
setlocal

set "SCRIPT_DIR=%~dp0"
set "PS_SCRIPT=%SCRIPT_DIR%dev.ps1"

where pwsh >nul 2>nul
if %ERRORLEVEL% EQU 0 (
  pwsh -NoProfile -ExecutionPolicy Bypass -File "%PS_SCRIPT%" %*
) else (
  powershell -NoProfile -ExecutionPolicy Bypass -File "%PS_SCRIPT%" %*
)

exit /b %ERRORLEVEL%
