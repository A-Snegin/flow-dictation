# Builds the Flow installer.
#
#   .\build-installer.ps1              22 MB, fetches the model on first run
#   .\build-installer.ps1 -WithModel   138 MB, works with no internet at all
#
# Produces a per-user installer: no administrator prompt, Start Menu entry,
# proper uninstaller, and an offer to start with Windows.

param(
    [switch]$WithModel,
    [string]$Model = 'small',
    [switch]$Sign,
    [string]$CertThumbprint
)

$ErrorActionPreference = 'Stop'

$Root = Split-Path $PSScriptRoot -Parent
$Release = Join-Path $env:LOCALAPPDATA 'Flow\target\release'
# winget installs it per-user or per-machine depending on the day, so look in
# both rather than hard-coding the one that happened to be right once.
$Iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $Iscc) { throw "Inno Setup not found. winget install JRSoftware.InnoSetup" }
if (-not (Test-Path (Join-Path $Release 'flow-core.exe'))) {
    throw "No build at $Release. Run: cargo build --release"
}

$ModelDir = Join-Path $env:LOCALAPPDATA "Flow\models\$Model-streaming-en"
if ($WithModel -and -not (Test-Path (Join-Path $ModelDir 'streaming_config.json'))) {
    throw "No model at $ModelDir. Run: .\fetch-model.ps1 -Model $Model"
}

New-Item -ItemType Directory -Force -Path (Join-Path $Root 'dist') | Out-Null

$args = @(
    "/DStageDir=$Release",
    "/DModelDir=$ModelDir"
)
if ($WithModel) { $args += "/DIncludeModel=1" }
$args += (Join-Path $Root 'installer\flow.iss')

Write-Host ("Building the {0} installer ..." -f $(if ($WithModel) { 'full' } else { 'app-only' }))
& $Iscc @args | Select-String -Pattern 'Successful|Error|error' | ForEach-Object { Write-Host ("  " + $_.Line) }

$name = if ($WithModel) { 'Flow-Setup-with-model.exe' } else { 'Flow-Setup.exe' }
$out = Join-Path $Root "dist\$name"
if (-not (Test-Path $out)) { throw "Inno Setup did not produce $out" }

# Signing is the difference between "Windows protected your PC" and a normal
# install. Without a certificate the installer still works, but the person you
# send it to has to click through a warning that tells them not to run it.
if ($Sign) {
    if (-not $CertThumbprint) { throw "-Sign needs -CertThumbprint" }
    $signtool = Get-ChildItem 'C:\Program Files (x86)\Windows Kits\10\bin' -Recurse -Filter signtool.exe |
        Where-Object { $_.FullName -like '*x64*' } | Select-Object -Last 1
    if (-not $signtool) { throw "signtool.exe not found; install the Windows SDK" }
    & $signtool.FullName sign /sha1 $CertThumbprint /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $out
    if ($LASTEXITCODE -ne 0) { throw "signing failed" }
    Write-Host "  signed"
}

$size = (Get-Item $out).Length / 1MB
Write-Host ""
Write-Host ("Installer: {0}" -f $out)
Write-Host ("Size:      {0:N1} MB" -f $size)
if (-not $Sign) {
    Write-Host ""
    Write-Host "Not code signed. Whoever you send this to will see a SmartScreen"
    Write-Host "warning and has to choose More info, then Run anyway. Tell them"
    Write-Host "to expect it, or sign the installer with a certificate."
}
