# Flow

Hold a key, talk, let go, the text is there. Local dictation for Windows, built
so the wait after you stop speaking is as short as the machine allows.

Everything runs on this computer. No network call sits anywhere near the path
from your voice to the text, and nothing is uploaded, ever.

## What it is

| | |
|---|---|
| Recogniser | Moonshine v2 streaming, English, on ONNX Runtime CPU |
| Default model | `small-streaming-en`, 142 MB quantised |
| Faster option | `tiny-streaming-en`, 43 MB, roughly twice as quick |
| Audio | WASAPI shared mode, event driven, 16 kHz mono |
| Interaction | Hold Right Ctrl. Release ends the utterance immediately |
| Insertion | Clipboard paste, or synthesised keystrokes in terminals |
| Process | One native executable, no browser engine, no background service |

## Setting it up

```powershell
# One-off: the inference runtime and a model, both outside the source tree
.\scripts\fetch-runtime.ps1
.\scripts\fetch-model.ps1                 # small, the default
.\scripts\fetch-model.ps1 -Model tiny     # optional, the fast profile

cargo build --release
.\scripts\install.ps1                   # copies the binaries to a fixed path, adds them
                                      # to your PATH, writes a Desktop launcher
```

Then run `flow-core`, or use the Desktop launcher. It loads the model, warms it, installs the hotkey and
sits in the notification area. Hold Right Ctrl, speak, let go.

Build outputs and models live in `%LOCALAPPDATA%\Flow`. Settings live in
`%APPDATA%\Flow\settings.toml`.

## Diagnostics

```powershell
flow-core.exe --mic-test 3     # audio path: open cost, arm cost, rate, level
flow-core.exe --dictate 5      # one dictation, printed rather than inserted
flow-core.exe --which-app      # what has focus and how text would go into it
insert-test.exe                # clipboard round trip, unicode, save and restore
bench-e2e.exe --holds 24       # key-up to transcript, percentiles
```

## The pill

The only thing Flow draws. A fixed-size rounded bar, low on the screen, click
through, and never focusable: if it took focus the caret would leave your
document and the text would land in the wrong place.

Left to right: a recording dot, a live eleven-bar waveform, your words as they
are recognised with a caret at the end, and a reminder of which key you are
holding. The waveform moves with your voice, so you can see the microphone is
hearing you rather than guess. The text follows the tail of what you are
saying, with the left edge fading into the background, so you always read your
most recent words instead of your first ones. When the text lands it says so
for a second, then disappears.

It never resizes. A shape that grows and shrinks as words arrive is movement at
the edge of vision, which is the opposite of unobtrusive.

Costs: fonts, the memory DC and the bitmap are built once and reused, the whole
thing repaints at 25 fps and only while it is on screen, and idle CPU with Flow
resident measures 0.000 percent.

## Privacy, precisely

Your voice never leaves the machine, and no audio is ever written to disk. It
lives in memory for as long as it takes to recognise, and that is all.

What Flow does write, all of it local and all of it yours:

| | |
|---|---|
| `%APPDATA%\Flow\settings.toml` | settings and your dictionary |
| `%LOCALAPPDATA%\Flow	races.jsonl` | timings and a character count, never the text |
| `%LOCALAPPDATA%\Flow\models` | the speech model |

Two things worth knowing rather than assuming.

Pasting means the text passes through the Windows clipboard for a moment.
Windows Clipboard History (Win+V) would ordinarily keep a copy, and sync it to
your Microsoft account if that is switched on, so every paste is marked with
the formats that tell Windows not to: `CanIncludeInClipboardHistory`,
`CanUploadToCloudClipboard` and `ExcludeClipboardContentFromMonitorProcessing`.
Your previous clipboard contents are restored afterwards. Choosing `type`
insertion avoids the clipboard entirely.

Flow does not print what you dictated. It reports a length. Anything that
redirects the process would otherwise write your dictation to a file, which is
not a promise worth making and then quietly breaking. `FLOW_ECHO=1` turns the
text on when you are debugging.

To remove Flow completely: quit from the tray, then delete the two folders
above and the folder you unzipped.

## Sending it to someone else

```powershell
.\scriptsuild-installer.ps1              # 19 MB, fetches the model on first run
.\scriptsuild-installer.ps1 -WithModel   # 134 MB, works with no internet
```

Produces `dist\Flow-Setup.exe`. It installs per user, so there is no
administrator prompt, and it adds a Start Menu entry, an uninstaller and an
optional start-with-Windows. Uninstalling asks separately about the speech
model and the dictionary, because someone reinstalling should not have to
download 136 MB again and someone leaving should not be left with it.

There is also `scripts\package.ps1`, which makes a plain zip with a batch file
instead. Use it when an installer would be unwelcome, on a locked-down machine
or where an unsigned setup is a harder sell than a folder.

**The installer is not code signed.** Whoever you send it to will see
"Windows protected your PC" and has to choose More info, then Run anyway. Warn
them, or sign it:

```powershell
.\scriptsuild-installer.ps1 -WithModel -Sign -CertThumbprint <thumbprint>
```

That needs an Authenticode certificate. An OV certificate is a few hundred
pounds a year and still builds reputation slowly; an EV certificate clears
SmartScreen immediately and costs more. Worth it if this goes to clients,
overkill for a handful of colleagues who can be told to expect the warning.

Installed size on their machine is about 170 MB, nearly all of it the speech
model:

| | |
|---|---:|
| `flow-core.exe` | 10 MB |
| `onnxruntime.dll` | 14 MB |
| small model | 136 MB |
| tiny model, if they add it | 43 MB |

## Terminals

Dictating into PowerShell, cmd or Windows Terminal is handled separately from
dictating into a document, for two reasons.

Paste keybindings vary between consoles and are sometimes off, so terminals get
synthesised Unicode keystrokes instead, which work anywhere a console reads
input.

More importantly, a line break at a shell prompt is the Enter key. Saying "new
line" in a document should break the line; saying it at a prompt would run the
command. Inside a terminal, line breaks are replaced with a space. Add your own
terminal executables under `insertion.terminal_apps` if you use one that is not
in the built-in list.

## Your own words

The `[dictionary]` section does two jobs from one list. Each entry biases the
decoder while it is listening, so the recogniser is more likely to produce the
term in the first place, and corrects the output afterwards if it did not.

Two forms, because people reach for both. A bare term is biased and written as
you typed it; a pair rewrites what was heard into what you want written.

```toml
[dictionary]
"lift off consulting" = "Lift-Off Consulting"   # typed as just: Lift-Off Consulting
"lift off" = "Lift-Off"
"dddm" = "DDDM"
```

A bare term is keyed on how it sounds rather than how it is written, so
`Lift-Off Consulting` matches a transcript that reads "lift off consulting".
Check what any entry does to a phrase without dictating it:

```powershell
flow-core --dictionary "we met the lift off consulting team about dddm"
```

Edit it in the settings window (tray, Open settings) or in the file directly.
Either way it applies straight away: the window saves and the app reloads, and
a file edited in an editor is noticed within a couple of seconds. Nothing needs
a restart, including the hotkey and the model.

The bias strength (`model.keyterm_boost`) defaults to 2.0. Upstream measured
that as the point where terms come out most accurately; raising it starts
putting them where they were not said.

## How it is put together

```
Right Ctrl down
   |
   +-- WASAPI stream starts        device opened at startup, so this is ~0 ms
   |      |
   |      +-- capture thread: copy, downmix, resample to 16 kHz, hand over
   |             |
   |             v
   |      ASR thread: add_audio, then transcribe_stream on a cadence
   |             |
   |             +--> live text to the overlay
   v
Right Ctrl up
   |
   +-- refuse to start another partial, collect the 15 ms release tail
   +-- stop_stream, drain, final transcript
   +-- deterministic formatting: punctuation, capitals, dictionary
   +-- insert at the caret
```

Three decisions carry most of the speed.

The device is opened once at startup and only started on key-down. Opening it
per utterance measured 321 ms on this machine and clipped the first word every
time; arming an already-initialised client measures 0.0 ms with the first packet
4.2 ms later. An initialised but stopped client is not capturing, so there is no
permanent microphone indicator and no audio to read.

The ASR worker never starts a partial once release is pending. The library
cannot cancel a call in flight, so refusing to begin one is the only bound
available on how long key-up has to wait.

Nothing on the critical path allocates a model, touches the disk, opens a
socket, or joins a thread. Capture teardown, clipboard restore and trace writing
all happen after the text is already on screen.

## What is deliberately not here

No GPU path. The Radeon 680M looks like free performance and is not: upstream
converts these models to ORT format at full graph optimisation, which fuses
whole regions into `com.microsoft` CPU operators. No compiling execution
provider recognises those, and because they sit mid-graph the model shatters
into dozens of fragments. Their published measurements show fewer than seven
nodes per partition on every model they tested. CPU is the fast path here.

No LLM in the critical path. Formatting is deterministic string work measured in
microseconds. A polish pass is a reasonable later feature, but it belongs after
the text lands, not before.

No Electron and no WebView. The settings window is plain Win32 controls,
created when it opens and destroyed when it closes, so it costs nothing while
it is shut. The settings file stays the source of truth and stays
hand-editable; the window is a friendlier way to reach the same values.
