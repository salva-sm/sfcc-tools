@echo off
rem Double-clickable entry point for install.ps1.
rem
rem Windows refuses to run a downloaded .ps1 under the default execution policy,
rem and a .cmd is not subject to it, so this is the way in that works on a
rem machine nobody has configured. Bypass applies to this one call only; nothing
rem on the system is changed.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"
echo.
pause
