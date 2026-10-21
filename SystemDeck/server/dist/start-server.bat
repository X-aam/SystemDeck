@echo off
rem Starts the SystemDeck team server. Data (database + encryption key) lives in .\data
rem Change the port with:  start-server.bat --bind 0.0.0.0:9000
rem Full paths: some PCs don't run programs from the current directory.
cd /d "%~dp0"
title SystemDeck Server
"%~dp0systemdeck-server.exe" --data-dir "%~dp0data" %*
if errorlevel 1 pause
