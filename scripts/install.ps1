# Puts the built binaries somewhere stable, adds them to PATH, and drops a
# Desktop launcher.
#
# The build output lives under %LOCALAPPDATA%\Flow\target and moves whenever
# cargo rebuilds, so it is not somewhere to point a shortcut or a Run entry.
# This copies the finished binaries to a fixed bin directory instead.

$ErrorActionPreference = 'Stop'

$Release = Join-Path $env:LOCALAPPDATA 'Flow\target\release'
$Bin     = Join-Path $env:LOCALAPPDATA 'Flow\bin'

if (-not (Test-Path (Join-Path $Release 'flow-core.exe'))) {
    throw "No build found at $Release. Run: cargo build --release"
}

New-Item -ItemType Directory -Force -Path $Bin | Out-Null

# onnxruntime.dll has to sit beside the executables.
$files = @('flow-core.exe', 'bench-e2e.exe', 'bench.exe', 'insert-test.exe', 'onnxruntime.dll')
foreach ($f in $files) {
    $src = Join-Path $Release $f
    if (Test-Path $src) {
        try {
            Copy-Item $src (Join-Path $Bin $f) -Force
            Write-Host "  copied $f"
        } catch {
            # flow-core.exe cannot be overwritten while it is running.
            Write-Warning "  $f is in use, skipped. Quit Flow from the tray and re-run."
        }
    }
}

# User PATH, not machine PATH: no elevation, and it only affects Anton.
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$Bin*") {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$Bin", 'User')
    Write-Host "Added $Bin to your user PATH. New terminals will pick it up."
} else {
    Write-Host "$Bin is already on your PATH."
}

# Desktop launcher, matching the Start CRM.bat convention.
$Launcher = Join-Path ([Environment]::GetFolderPath('Desktop')) 'Start Flow.bat'
@"
@echo off
rem Starts Flow in the background. It lives in the notification area.
start "" "$Bin\flow-core.exe"
"@ | Set-Content -Path $Launcher -Encoding ASCII
Write-Host "Launcher written to $Launcher"

Write-Host ""
Write-Host "Installed. From a new terminal:"
Write-Host "  flow-core --which-app"
Write-Host "  flow-core --mic-test 3"
Write-Host "  bench-e2e --holds 24"
