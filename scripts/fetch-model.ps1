# Downloads a Moonshine streaming model into %LOCALAPPDATA%\Flow\models.
#
#   .\fetch-model.ps1                 small-streaming-en, the balanced profile
#   .\fetch-model.ps1 -Model tiny     tiny-streaming-en, the fast profile
#
# File sizes are checked against the published catalogue. A truncated download
# fails the model load with an unhelpful ONNX error hours later, so it is worth
# catching here.

param(
    [ValidateSet('small', 'tiny', 'medium')]
    [string]$Model = 'small',
    [string]$Version = 'quantized_26_08_21'
)

$ErrorActionPreference = 'Stop'

$Name = "$Model-streaming-en"
$Base = "https://download.moonshine.ai/model/$Name/$Version"
$Dest = Join-Path $env:LOCALAPPDATA "Flow\models\$Name"

# name -> expected bytes, from core/moonshine-model-file-metadata.generated.cpp
$Expected = @{
    'small' = @{
        'frontend.model.ort'    = 26944
        'frontend.weights.ort'  = 7769464
        'encoder.ort'           = 44148576
        'adapter.ort'           = 2870368
        'cross_kv.ort'          = 5356536
        'decoder_kv.ort'        = 81878600
        'streaming_config.json' = 512
        'tokenizer.bin'         = 249974
    }
    'tiny' = @{
        'frontend.model.ort'    = 23344
        'frontend.weights.ort'  = 2093464
        'encoder.ort'           = 7675440
        'adapter.ort'           = 1319664
        'cross_kv.ort'          = 1287544
        'decoder_kv.ort'        = 32583720
        'streaming_config.json' = 509
        'tokenizer.bin'         = 249974
    }
}

New-Item -ItemType Directory -Force -Path $Dest | Out-Null
Write-Host "Downloading $Name into $Dest"

$files = if ($Expected.ContainsKey($Model)) { $Expected[$Model].Keys } else {
    @('frontend.model.ort','frontend.weights.ort','encoder.ort','adapter.ort',
      'cross_kv.ort','decoder_kv.ort','streaming_config.json','tokenizer.bin')
}

$total = 0
foreach ($f in $files) {
    $out = Join-Path $Dest $f
    Invoke-WebRequest -Uri "$Base/$f" -OutFile $out -UseBasicParsing
    $size = (Get-Item $out).Length
    $total += $size

    if ($Expected.ContainsKey($Model) -and $Expected[$Model][$f] -ne $size) {
        throw "$f is $size bytes, expected $($Expected[$Model][$f]). Download incomplete."
    }
    Write-Host ("  {0,-24} {1,10:N0} bytes" -f $f, $size)
}

Write-Host ("Done. {0:N1} MB total." -f ($total / 1MB))
Write-Host "Set model.profile in the settings file to use it:"
Write-Host ("  {0}" -f (Join-Path $env:APPDATA 'Flow\settings.toml'))
