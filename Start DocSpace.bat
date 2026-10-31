@echo off
rem Double-click to start DocSpace (checks Downloads for an update first).
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0run.ps1" %*
if errorlevel 1 pause
