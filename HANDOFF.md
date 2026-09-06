# Flow handoff

Local dictation for Windows and Linux. Hold Right Ctrl, talk, let go, text
lands at the cursor. Built 26 Aug 2026, shipped public the same day. Linux port
on branch `linux`, 6 Sep 2026.

## Linux port (branch `linux`, 6 Sep 2026)

One crate, one binary, platform code split under `src/platform/{windows,linux}`
with the same module surface on both. Shared code moved out of the Windows
files: `audio_convert.rs` (resampler), `overlay_model.rs` (pill state machine,
geometry, animation), `control.rs` (HotkeyEvent), `platform/*/sys.rs` (waker,
priority, clock). `main.rs` is platform neutral: the Windows message pump and
tray stay under `cfg(windows)`; Linux blocks on an eventfd.

- Audio: PipeWire stream connected at startup, inactive; `arm()` is a graph
  state change. Measured: arm 0.01 ms, first sample 15 ms after arm, 16 kHz
  mono negotiated so the resampler is bypassed.
- Hotkey: Hyprland binds bare `CONTROL_R` (press) and `CTRL + CONTROL_R` with
  release to `flow-ctl down|up`, a std-only client that talks to
  `$XDG_RUNTIME_DIR/flow/control.sock`. `flow-ctl toggle|cancel|reload|report|quit`
  replaces the tray. No evdev, no input group.
- Overlay: wlr-layer-shell surface via smithay-client-toolkit, drawn with
  tiny-skia and ab_glyph, no toolkit. Pixel geometry matches the Windows GDI
  code; `docs/images/overlay.png` predates the current code on both platforms.
- Insertion: `wtype -d 0 -` for type mode (60 chars in 2 ms), wl-copy/wl-paste
  for paste mode. Type is the Linux default; terminals detected by Hyprland
  window class.
- Runtime: Moonshine Linux bundle is one `libmoonshine.so` plus
  `libonnxruntime.so.1`, linked with an rpath, at `~/.local/share/flow/moonshine`.
  Scripts: `scripts/fetch-runtime.sh`, `scripts/fetch-model.sh`,
  `scripts/install-linux.sh` (systemd user unit `flow.service`).
- Measured on the Ryzen 7 7735U under Omarchy: ready in 150 ms, 220 MB
  resident, key-up to text 582 ms on the small model via a WAV through a null
  sink (no human spoke during the autonomous build).

Open on Linux: regenerate `docs/images/overlay.png`; verify the Windows build
still compiles after the split (only reviewed, not built); consider an
in-process virtual keyboard only if `insertion_ms` in traces warrants it;
merge `linux` into `main` once Anton has used it for a few days.

## Done

**v1.0.0 is public and installable.** <https://github.com/A-Snegin/flow-dictation>,
MIT, `main`, 28 tests passing.

- Rust core linking the prebuilt Moonshine v2 streaming C library through
  hand-written FFI (`src/ffi.rs`, `src/asr.rs`). No ONNX export work needed:
  Moonshine ships a Windows bundle and quantised `.ort` models on a CDN.
- WASAPI capture opened at startup, started on key-down (`src/audio.rs`).
- Single ASR worker that refuses to start a partial once release is pending
  (`src/asr_service.rs`).
- Overlay: layered window, coral dot, 11-bar live waveform, tail-following text,
  `Hold Ctrl` key cap (`src/overlay.rs`).
- Tray menu, settings window in plain Win32, autostart, live settings reload
  (`src/tray.rs`, `src/settings_ui.rs`, `src/autostart.rs`).
- Terminal-aware insertion: keystrokes in shells, line breaks stripped so a
  dictation cannot press Enter (`src/target_app.rs`, `src/inject.rs`).
- Diagnostics and benchmarks: `--mic-test`, `--dictate`, `--which-app`,
  `--dictionary`, `--overlay-demo`, `--settings`, plus `bench-e2e` and `wer`.
- Installers via Inno Setup, per user, no admin (`installer/flow.iss`,
  `scripts/build-installer.ps1`). One-line web install
  (`scripts/web-install.ps1`), verified from a clean machine state.
- README with two Mermaid diagrams, overlay screenshot, measured numbers.
  Language pass done through the `humanizer` skill.

## Current state

Running from `%LOCALAPPDATA%\Flow\app\flow-core.exe`, installed by the web
installer. Working tree clean, everything pushed.

Measured on the Ryzen 7 7735U while OneDrive held most of a core:

| | Small | Tiny |
|---|---:|---:|
| Key-up to text, p50 | 493 ms | 221 ms |
| p95 | 1070 ms | 380 ms |
| Real-time factor | 0.62 | 0.28 |
| Word error rate | 5.0% | 3.4% |

Idle CPU 0.0%, 220 MB resident, 2 to 4 ms hotkey to microphone.

## Decisions

- **Moonshine prebuilt library over exporting ONNX ourselves.** Their Windows
  bundle has the exact streaming lifecycle needed and is MIT.
- **No GPU path.** Their `.ort` graphs fuse into `com.microsoft` CPU operators
  that no compiling execution provider recognises; the model would fragment.
- **One ONNX thread.** Same real-time factor, same word error rate (measured),
  better tail, one core instead of eight.
- **Microphone opened at startup, started on key-down.** Opening it per
  utterance cost 321 ms and clipped the first word.
- **Overlay paints after the microphone arms.** Painting first cost 231 ms of
  speech per dictation.
- **Model stays `balanced` (small).** Tiny is twice as quick and scored better
  on one clean passage, but the published multi-dataset gap runs the other way
  and Anton's speech has client names in it.
- **Repo public, installers unsigned.** SmartScreen warning accepted for now.
- **`includeCoAuthoredBy: false`** set in `~/.claude/settings.json`; history was
  rewritten to strip the trailer, so Anton is the only contributor.

## Open items

1. **Latency gate unverified on a quiet machine.** Every figure above was taken
   while OneDrive used ~80% of a core. Next action: pause OneDrive syncing, run
   `bench-e2e --holds 24 --model %LOCALAPPDATA%\Flow\models\small-streaming-en`,
   update the README table.
2. **`IAudioClient::Start` intermittently takes ~247 ms**, visible as `start_us`
   in `%LOCALAPPDATA%\Flow\traces.jsonl`, roughly one dictation in five. Fix is
   a grace period keeping the stream running after release, which leaves the
   Windows microphone indicator lit between dictations. Anton declined this for
   v1 on 26 Aug. Raise again only if he reports missing first words.
3. **No accuracy corpus.** Deciding tiny against small properly needs Anton's
   own voice with a written reference, scored by `wer`.
4. **No demo GIF.** The strongest missing thing for the repo. Ten seconds of
   dictating into Notepad.
5. **Code signing.** Needed before this goes to clients. OV certificate builds
   SmartScreen reputation slowly, EV clears it immediately.

## Next task

Record the accuracy corpus and settle the model choice. Every other open item
changes what the README claims. This one changes what users get.

Anton reads a prepared passage of his own writing, 2 to 3 minutes, including
client names and consulting vocabulary. Save the audio as 16 kHz mono WAV and
the text verbatim, then:

```powershell
wer --wav <recording>.wav --ref <reference>.txt --model %LOCALAPPDATA%\Flow\models\small-streaming-en --arch 4
wer --wav <recording>.wav --ref <reference>.txt --model %LOCALAPPDATA%\Flow\models\tiny-streaming-en  --arch 2
```

Files: `bench/corpus/` for the recording and reference, `src/bin/wer.rs` if the
scoring needs proper-noun breakdown, `README.md` speed table for the result.
