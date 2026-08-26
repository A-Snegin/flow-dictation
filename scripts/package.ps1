# Builds a folder someone else can unzip and run.
#
# Two shapes, because they answer different questions:
#
#   .\package.ps1                 the app only, about 25 MB. The model is
#                                 fetched on first run, which keeps the thing
#                                 you send small enough to email.
#   .\package.ps1 -WithModel      everything, about 170 MB. No download, works
#                                 on a machine with no internet at all.
#
# Neither needs Rust, Visual Studio, or an installer. Unzip and run.

param(
    [switch]$WithModel,
    [string]$Model = 'small',
    [string]$OutDir = "$env:USERPROFILE\Desktop"
)

$ErrorActionPreference = 'Stop'

$Release = Join-Path $env:LOCALAPPDATA 'Flow\target\release'
if (-not (Test-Path (Join-Path $Release 'flow-core.exe'))) {
    throw "No build at $Release. Run: cargo build --release"
}

$Stage = Join-Path $env:TEMP ("Flow-package-" + [Guid]::NewGuid().ToString('N').Substring(0, 8))
$App = Join-Path $Stage 'Flow'
New-Item -ItemType Directory -Force -Path $App | Out-Null

Write-Host "Collecting the application ..."
foreach ($f in @('flow-core.exe', 'onnxruntime.dll')) {
    Copy-Item (Join-Path $Release $f) (Join-Path $App $f)
    Write-Host ("  {0,-20} {1,8:N1} MB" -f $f, ((Get-Item (Join-Path $App $f)).Length / 1MB))
}

# The diagnostics travel too: they are how anyone answers "is it working"
# without needing me.
foreach ($f in @('bench-e2e.exe', 'wer.exe')) {
    $src = Join-Path $Release $f
    if (Test-Path $src) { Copy-Item $src (Join-Path $App $f) }
}

if ($WithModel) {
    $ModelDir = Join-Path $env:LOCALAPPDATA "Flow\models\$Model-streaming-en"
    if (-not (Test-Path $ModelDir)) {
        throw "No model at $ModelDir. Run: .\fetch-model.ps1 -Model $Model"
    }
    $dest = Join-Path $App "models\$Model-streaming-en"
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Copy-Item (Join-Path $ModelDir '*') $dest
    $mb = (Get-ChildItem $dest | Measure-Object Length -Sum).Sum / 1MB
    Write-Host ("  {0,-20} {1,8:N1} MB" -f "$Model model", $mb)
}

# First run: put the model where the app looks, then start it.
$setup = @'
@echo off
setlocal
set FLOW_HOME=%LOCALAPPDATA%\Flow
set MODELS=%FLOW_HOME%\models\small-streaming-en

if exist "%MODELS%\streaming_config.json" goto run

if exist "%~dp0models\small-streaming-en\streaming_config.json" (
  echo Installing the speech model ...
  mkdir "%MODELS%" 2>nul
  copy /y "%~dp0models\small-streaming-en\*" "%MODELS%" >nul
  goto run
)

echo Downloading the speech model, about 142 MB, once ...
mkdir "%MODELS%" 2>nul
set BASE=https://download.moonshine.ai/model/small-streaming-en/quantized_26_08_21
for %%F in (frontend.model.ort frontend.weights.ort encoder.ort adapter.ort cross_kv.ort decoder_kv.ort streaming_config.json tokenizer.bin) do (
  echo   %%F
  curl -sfL -o "%MODELS%\%%F" "%BASE%/%%F" || goto failed
)

:run
echo Starting Flow. Hold Right Ctrl and talk.
start "" "%~dp0flow-core.exe"
exit /b 0

:failed
echo.
echo Could not download the speech model. Check your connection and try again.
pause
exit /b 1
'@
Set-Content -Path (Join-Path $App 'Start Flow.bat') -Value $setup -Encoding ASCII

$readme = @'
Flow: hold a key, talk, let go, the text is there.

  1. Unzip anywhere.
  2. Run "Start Flow.bat". The first run fetches the speech model if it is
     not already bundled, about 142 MB, once.
  3. Hold Right Ctrl, speak, let go. The text appears wherever your cursor is.

Flow lives in the notification area. Right-click it for settings.

Everything runs on your computer. Your voice is never sent anywhere, and no
audio is written to disk.

What it stores, all local, all yours:
  %APPDATA%\Flow\settings.toml        your settings and dictionary
  %LOCALAPPDATA%\Flow\traces.jsonl    timings only, no transcript text
  %LOCALAPPDATA%\Flow\models          the speech model

To remove it: quit from the tray, delete this folder and those two folders.

Needs Windows 10 or 11, 64-bit, and a microphone.
'@
Set-Content -Path (Join-Path $App 'README.txt') -Value $readme -Encoding UTF8

$suffix = if ($WithModel) { "with-$Model-model" } else { 'app-only' }
$Zip = Join-Path $OutDir "Flow-$suffix.zip"
if (Test-Path $Zip) { Remove-Item $Zip }
Compress-Archive -Path $App -DestinationPath $Zip -CompressionLevel Optimal
Remove-Item -Recurse -Force $Stage

$size = (Get-Item $Zip).Length / 1MB
Write-Host ""
Write-Host ("Package: {0}" -f $Zip)
Write-Host ("Size:    {0:N1} MB" -f $size)
Write-Host "Send that. The other person unzips it and runs Start Flow.bat."
