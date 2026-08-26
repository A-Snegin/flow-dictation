# Installs Flow from the latest GitHub release.
#
#   irm https://raw.githubusercontent.com/A-Snegin/flow-dictation/main/scripts/web-install.ps1 | iex
#
# Downloads the installer, runs it, starts Flow. Per user, so no administrator
# prompt. Pass -WithModel to fetch the larger installer that carries the speech
# model, for a machine that will be offline afterwards.

param(
    [switch]$WithModel,
    [switch]$Silent
)

$ErrorActionPreference = 'Stop'
$repo = 'A-Snegin/flow-dictation'
$asset = if ($WithModel) { 'Flow-Setup-with-model.exe' } else { 'Flow-Setup.exe' }

Write-Host "Flow: local dictation for Windows"
Write-Host ""

if ([Environment]::Is64BitOperatingSystem -eq $false) {
    throw "Flow needs 64-bit Windows."
}
if ([Environment]::OSVersion.Version.Major -lt 10) {
    throw "Flow needs Windows 10 or later."
}

Write-Host "Finding the latest release ..."
$release = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{
    'User-Agent' = 'flow-web-install'
}
$download = ($release.assets | Where-Object { $_.name -eq $asset }).browser_download_url
if (-not $download) { throw "The latest release has no $asset" }

$target = Join-Path $env:TEMP $asset
$mb = [math]::Round((($release.assets | Where-Object { $_.name -eq $asset }).size) / 1MB, 1)
Write-Host ("Downloading {0} {1} ({2} MB) ..." -f $release.tag_name, $asset, $mb)

# Faster than Invoke-WebRequest for a file this size, and it shows progress.
$ProgressPreference = 'SilentlyContinue'
Invoke-WebRequest -Uri $download -OutFile $target -UseBasicParsing

Write-Host "Installing ..."
$flags = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
if ($Silent) { $flags += '/TASKS=' }
Start-Process -FilePath $target -ArgumentList $flags -Wait
Remove-Item $target -ErrorAction SilentlyContinue

$exe = Join-Path $env:LOCALAPPDATA 'Flow\app\flow-core.exe'
if (-not (Test-Path $exe)) { throw "Install finished but $exe is missing." }

if (-not $Silent) {
    Start-Process -FilePath $exe
    Write-Host ""
    Write-Host "Installed. Hold Right Ctrl, speak, let go."
    Write-Host "Flow is in the notification area; right-click it for settings."
} else {
    Write-Host "Installed to $exe"
}
