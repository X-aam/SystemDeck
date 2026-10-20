@echo off
rem Double-click to build and run both the SystemDeck server and client from source.
rem Pass args through, e.g.  start.bat -Only server   or   start.bat -Release
cd /d "%~dp0"
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start.ps1" %*
if errorlevel 1 pause
