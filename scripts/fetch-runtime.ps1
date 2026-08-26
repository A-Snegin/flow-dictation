# Downloads the prebuilt Moonshine Windows runtime that Flow links against.
#
# It goes to %LOCALAPPDATA%\Flow\moonshine rather than into the source tree:
# it is 130 MB of static libraries, nothing version control should carry, and
# on a machine where the project lives in OneDrive it is nothing a sync client
# should churn on either.
#
# Pinned to a release tag on purpose. Upstream is at v0.1.x and moves weekly;
# the header we compile against must match the binary we link.

$ErrorActionPreference = 'Stop'

$Tag  = 'v0.1.5'
$Dest = Join-Path $env:LOCALAPPDATA 'Flow'
$Url  = "https://github.com/moonshine-ai/moonshine/releases/download/$Tag/moonshine-voice-windows-x86_64.tar.gz"

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
$Archive = Join-Path $env:TEMP 'moonshine-voice-windows-x86_64.tar.gz'

Write-Host "Downloading Moonshine runtime $Tag ..."
Invoke-WebRequest -Uri $Url -OutFile $Archive -UseBasicParsing

$Extracted = Join-Path $Dest 'moonshine-voice-windows-x86_64'
$Final     = Join-Path $Dest 'moonshine'
if (Test-Path $Extracted) { Remove-Item -Recurse -Force $Extracted }
if (Test-Path $Final)     { Remove-Item -Recurse -Force $Final }

Write-Host "Extracting to $Final ..."
tar -xzf $Archive -C $Dest
Rename-Item -Path $Extracted -NewName 'moonshine'
Remove-Item $Archive

$lib = Join-Path $Final 'lib\moonshine.lib'
if (-not (Test-Path $lib)) { throw "extraction did not produce $lib" }

Write-Host "Runtime ready at $Final"
Write-Host "Contents:"
Get-ChildItem (Join-Path $Final 'lib') | Format-Table Name, @{n='MB';e={[math]::Round($_.Length/1MB,1)}} -AutoSize
