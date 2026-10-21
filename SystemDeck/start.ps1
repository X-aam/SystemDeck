# Dev launcher: builds and runs the SystemDeck server and client from source.
#
#   .\start.ps1                 build + run both (debug), server in its own window
#   .\start.ps1 -Only server    just the server (foreground)
#   .\start.ps1 -Only client    just the client
#   .\start.ps1 -Release         use the optimized release build
#   .\start.ps1 -- --bind 0.0.0.0:9000   pass extra args through to the server
#
# Sets up the WinLibs MinGW toolchain on PATH (same as build.ps1) and makes sure
# WebView2Loader.dll sits next to the client exe so the embedded browser starts.
param(
    [ValidateSet("all", "server", "client")] [string]$Only = "all",
    [switch]$Release,
    [Parameter(ValueFromRemainingArguments = $true)] [string[]]$ServerArgs
)
$ErrorActionPreference = "Stop"

# --- Toolchain on PATH (cargo + MinGW binutils for the windows-gnu target) ---
$winlibs = Get-ChildItem "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Directory -Filter "BrechtSanders.WinLibs*" -ErrorAction SilentlyContinue |
    ForEach-Object { Join-Path $_.FullName "mingw64\bin" } |
    Where-Object { Test-Path (Join-Path $_ "dlltool.exe") } |
    Select-Object -First 1
if (-not $winlibs) {
    Write-Error "MinGW binutils not found. Install with: winget install BrechtSanders.WinLibs.POSIX.UCRT"
}
$env:PATH = "$env:USERPROFILE\.cargo\bin;$winlibs;$env:PATH"

Push-Location $PSScriptRoot
try {
    $buildProfile = if ($Release) { "release" } else { "debug" }
    $targetDir = Join-Path $PSScriptRoot "target\$buildProfile"
    $dataDir   = Join-Path $PSScriptRoot "systemdeck-data"

    # Which crates to build.
    $pkgs = switch ($Only) {
        "server" { @("-p", "systemdeck-server") }
        "client" { @("-p", "systemdeck-client") }
        default  { @("--workspace") }
    }
    $buildArgs = @("build") + $pkgs
    if ($Release) { $buildArgs += "--release" }
    Write-Host "Building ($buildProfile)..." -ForegroundColor Cyan
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # The embedded browser (WebView2) needs this loader DLL next to the client exe.
    if ($Only -ne "server") {
        $loaderDst = Join-Path $targetDir "WebView2Loader.dll"
        if (-not (Test-Path $loaderDst)) {
            $loader = Get-ChildItem (Join-Path $targetDir "build") -Recurse -Filter "WebView2Loader.dll" -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -match "\\x64\\" } | Select-Object -First 1
            if ($loader) { Copy-Item $loader.FullName $loaderDst -Force }
            else { Write-Warning "WebView2Loader.dll not found; the embedded browser will not start." }
        }
    }

    $serverExe = Join-Path $targetDir "systemdeck-server.exe"
    $clientExe = Join-Path $targetDir "systemdeck.exe"

    # --- Launch ---
    if ($Only -eq "server") {
        Write-Host "Starting server (data: $dataDir)..." -ForegroundColor Green
        & $serverExe --data-dir $dataDir @ServerArgs
    }
    elseif ($Only -eq "client") {
        Write-Host "Starting client..." -ForegroundColor Green
        & $clientExe
    }
    else {
        # Server in its own window so its log stays visible; client in this one.
        Write-Host "Starting server in a new window (data: $dataDir)..." -ForegroundColor Green
        $serverCmd = "& '$serverExe' --data-dir '$dataDir' $($ServerArgs -join ' '); Write-Host ''; Read-Host 'Server stopped - press Enter to close'"
        Start-Process powershell -ArgumentList "-NoExit", "-Command", $serverCmd
        Start-Sleep -Milliseconds 800
        Write-Host "Starting client..." -ForegroundColor Green
        & $clientExe
    }
} finally {
    Pop-Location
}
