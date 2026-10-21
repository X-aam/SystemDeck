# Builds SystemDeck and assembles two separate, ready-to-copy folders:
#
#   release\SystemDeck-Server\   systemdeck-server.exe, start-server.bat, README.md, data\ (created on first start),
#                               downloads\SystemDeck-Client.zip (offered on the dashboard's "Download app" page)
#   release\SystemDeck-Client\   systemdeck.exe, README.md
#   release\SystemDeck-Setup-<version>.exe   client installer (when Inno Setup 6 is installed;
#                               also published as downloads\SystemDeck-Setup.exe)
#
# Existing data\ folders are never touched. Needs the windows-gnu Rust toolchain
# plus MinGW binutils (winget install BrechtSanders.WinLibs.POSIX.UCRT).
param(
    [switch]$Debug,
    [ValidateSet("all", "server", "client")] [string]$Only = "all"
)
$ErrorActionPreference = "Stop"

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
    $buildProfile = if ($Debug) { "debug" } else { "release" }
    $cargoArgs = @("build")
    if (-not $Debug) { $cargoArgs += "--release" }
    switch ($Only) {
        "server" { $cargoArgs += @("-p", "systemdeck-server") }
        "client" { $cargoArgs += @("-p", "systemdeck-client") }
        default  { $cargoArgs += "--workspace" }
    }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    if ($Only -ne "client") {
        $dir = "release\SystemDeck-Server"
        New-Item -ItemType Directory -Force $dir | Out-Null
        Copy-Item "target\$buildProfile\systemdeck-server.exe" $dir -Force
        Copy-Item "server\dist\*" $dir -Force
        Write-Host "Server -> $dir"
    }
    if ($Only -ne "server") {
        $dir = "release\SystemDeck-Client"
        New-Item -ItemType Directory -Force $dir | Out-Null
        Copy-Item "target\$buildProfile\systemdeck.exe" $dir -Force
        Copy-Item "client\dist\*" $dir -Force
        # The embedded browser (WebView2) needs this loader DLL next to the exe.
        $loader = Get-ChildItem "target\$buildProfile\build" -Recurse -Filter "WebView2Loader.dll" -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "\\x64\\" } | Select-Object -First 1
        if ($loader) {
            Copy-Item $loader.FullName $dir -Force
            Copy-Item $loader.FullName "target\$buildProfile" -Force
            Write-Host "WebView2Loader.dll -> $dir"
        } else {
            Write-Warning "WebView2Loader.dll not found; the embedded browser will not start."
        }
        Write-Host "Client -> $dir"
    }
    # Publish the client as a download on the server's dashboard ("Download app").
    if (Test-Path "release\SystemDeck-Client\systemdeck.exe") {
        $downloads = "release\SystemDeck-Server\downloads"
        New-Item -ItemType Directory -Force $downloads | Out-Null
        # Leave out the browser cache folder (systemdeck.exe.WebView2) a local run may create.
        $files = Get-ChildItem "release\SystemDeck-Client" -File
        Compress-Archive -Path $files.FullName -DestinationPath "$downloads\SystemDeck-Client.zip" -Force
        Write-Host "Client download -> $downloads\SystemDeck-Client.zip"
    }

    # Single-file portable client (no install): a launcher that embeds the packaged client.
    if ($Only -ne "server" -and (Test-Path "release\SystemDeck-Client\WebView2Loader.dll")) {
        $env:SYSTEMDECK_CLIENT_DIR = (Resolve-Path "release\SystemDeck-Client").Path -replace '\\', '/'
        $portableArgs = @("build", "--manifest-path", "portable\Cargo.toml", "--target-dir", "target")
        if (-not $Debug) { $portableArgs += "--release" }
        & cargo @portableArgs
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        Copy-Item "target\$buildProfile\systemdeck-portable.exe" "release\SystemDeck-Portable.exe" -Force
        Copy-Item "release\SystemDeck-Portable.exe" "release\SystemDeck-Server\downloads\SystemDeck-Portable.exe" -Force
        Write-Host "Portable download -> release\SystemDeck-Server\downloads\SystemDeck-Portable.exe"
    }

    # Windows installer (Inno Setup 6: winget install JRSoftware.InnoSetup).
    if ($Only -ne "server" -and -not $Debug) {
        $iscc = @(
            "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
            "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
            "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
        ) | Where-Object { Test-Path $_ } | Select-Object -First 1
        if ($iscc) {
            $version = (Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
            $source = (Resolve-Path "release\SystemDeck-Client").Path
            $out = (Resolve-Path "release").Path
            & $iscc /Q "/DAppVersion=$version" "/DSourceDir=$source" "/DOutputDir=$out" "client\installer\systemdeck.iss"
            if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
            $setup = "release\SystemDeck-Setup-$version.exe"
            Write-Host "Installer -> $setup"
            # Stable name on the dashboard so old versions don't pile up.
            Copy-Item $setup "release\SystemDeck-Server\downloads\SystemDeck-Setup.exe" -Force
            Write-Host "Installer download -> release\SystemDeck-Server\downloads\SystemDeck-Setup.exe"
        } else {
            Write-Warning "Inno Setup not found; skipping the installer. Install with: winget install JRSoftware.InnoSetup"
        }
    }
} finally {
    Pop-Location
}
