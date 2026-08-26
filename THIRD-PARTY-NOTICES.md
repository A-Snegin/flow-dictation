# Third-party notices

Flow links and ships work by others. Their licence terms are reproduced here,
as those licences require.

## Moonshine

Speech recognition. The streaming English models and the runtime library are
used unmodified, linked into `flow-core.exe` and, in the bundled installer,
redistributed as `.ort` model files.

<https://github.com/moonshine-ai/moonshine>

Moonshine releases all streaming speech-to-text models and all English-language
models under the MIT licence, which covers `small-streaming-en` and
`tiny-streaming-en` as shipped here. Their non-English legacy non-streaming
models are under a separate non-commercial licence and are **not** distributed
with Flow.

```
MIT License

Copyright (c) 2025 Useful Sensors, Inc. (dba Moonshine AI)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## ONNX Runtime

Inference engine, redistributed as `onnxruntime.dll`.

<https://github.com/microsoft/onnxruntime>

```
MIT License

Copyright (c) Microsoft Corporation

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Rust crates

Build-time and runtime dependencies, all permissively licensed:
`windows` (MIT), `serde` (MIT or Apache-2.0), `toml` (MIT or Apache-2.0),
`winresource` (MIT).

## Test audio

`bench/corpus` uses public-domain recordings from the Moonshine repository for
benchmarking only. They are not redistributed in any release.
