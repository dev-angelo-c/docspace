@echo off
rem Double-click to install everything DocSpace needs (safe to run again).
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
pause
