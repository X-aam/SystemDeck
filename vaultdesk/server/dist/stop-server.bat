@echo off
rem Stops a SystemDeck server started from this folder (console or background).
taskkill /IM systemdeck-server.exe /F >nul 2>&1 && echo SystemDeck server stopped. || echo SystemDeck server was not running.
timeout /t 2 >nul
