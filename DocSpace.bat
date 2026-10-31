@echo off
rem DocSpace: double-click to set up, clean up old copies, build and run.
rem   DocSpace.bat -Release   builds the Windows installer instead.
cd /d "%TEMP%"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0docspace.ps1" %*
if errorlevel 1 pause
