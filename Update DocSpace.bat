@echo off
rem Double-click to install the newest DocSpace release from your Downloads folder.
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0update.ps1" %*
pause
