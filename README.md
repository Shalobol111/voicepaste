# VoicePaste

**English** · [Русский](README.ru.md)

Voice typing for Windows that runs **entirely on your computer**. Press a hotkey, speak, stop the recording, and the recognized text is pasted into whatever field has the cursor (browser, messenger, Word, Notepad, a code editor). The text also stays on the clipboard.

- Russian and English, including both mixed in one sentence.
- Recognition by [Whisper](https://github.com/ggml-org/whisper.cpp), locally. No cloud, no account, no subscription.
- Two hotkeys at the same time: "toggle" and "push to talk".
- Tray icon, a recording indicator on top of other windows, a settings window.

## Download

Get the latest build from the **[Releases](../../releases)** page. Each release has two archives and a `SHA256SUMS.txt`:

| Package | For | Speed |
|---|---|---|
| `VoicePaste-…-nvidia` | **NVIDIA RTX 40-series** (4050–4090) with a recent driver | very fast: a few seconds of speech are recognized in well under a second |
| `VoicePaste-…-cpu` | Any PC whose CPU supports AVX2 (almost everything made since 2013–2015); no graphics card needed | much slower: a few seconds on the `Small` model, about 15 s on `Large turbo`. `Small` is the default |

The NVIDIA build is compiled for RTX 40 only. It will not start on other GPUs (GTX, RTX 20/30/50, AMD): use the `cpu` package, or build from source for your card (see below). If the CPU is too old (no AVX2), the `cpu` package says so in a window and exits. Some low-end Celeron and Pentium chips lack AVX2.

Requirements: Windows 10 or 11 (64-bit) and a microphone. No administrator rights needed.

## Install

1. Unzip the archive anywhere (for example `C:\Programs\VoicePaste`). Keep all files together.
2. Run `voicepaste.exe`. A tray icon appears near the clock: **yellow** while the model loads, **grey** when ready, **red** while recording, **orange** on a problem (hover it to see why).
3. The Whisper model is not in the archive because it is 190 MB to 1.6 GB. Get it either way:
   - right-click the tray icon → **Settings…** → "Recognition" → **Download** next to "Large v3 turbo (q5_0)" (575 MB, recommended). It downloads from huggingface.co and only after you press the button;
   - or put a `.bin` file into the models folder (tray icon → "Open models folder") and pick it in the settings.
4. Press **Save** in the settings, and start dictating.

## Use

| Default hotkey | What it does |
|---|---|
| `Ctrl+Alt+D` — toggle | press → recording starts; press again → it stops and is recognized |
| `Ctrl+Alt+S` — push to talk | hold → recording; release → it stops and is recognized |

A recording is stopped by the same key that started it. Recordings shorter than 0.3 s, or without speech, paste nothing and play an error beep. Both hotkeys can be changed in the settings (press "Set", then the combination you want) and either one can be turned off by clearing its field. A hotkey needs Ctrl, Alt, Shift or Win, or must be an F1–F24 key.

### Settings (tray icon → "Settings…")

- **Hotkeys**: both keys, with a "taken by another app" check.
- **Microphone**: device choice and a live level meter ("Test").
- **Recognition**: model (with a Download button), language, accuracy, GPU use, a hint for the model (names and terms it often gets wrong).
- **Paste**: paste into the field or only copy; paste method; trailing space; strip the final period; **start with a lowercase letter**; **press Enter after pasting** (handy to send chat messages right away).
- **Interface**: recording indicator, sounds and their volume, start with Windows.

Changes apply immediately, no restart needed. The bottom of the window shows the app's log.

### Files

Everything lives in `%APPDATA%\VoicePaste\`:

- `config.toml` — settings (commented; you can edit it by hand and the app picks up the changes);
- `models\` — Whisper models;
- `logs\` — the log (what you dictate is **not** written to it).

## Privacy

- Recognition runs on your computer. Audio is never sent anywhere and is not saved to disk.
- The only network access is downloading a model from huggingface.co, and only when you press "Download". No telemetry, no updates, no accounts.

## Known limitations

- **Windows run as administrator** (Task Manager, programs started "as administrator"): Windows does not let a normal program type into them. The text still lands on the clipboard, so you can paste it yourself with `Ctrl+V`.
- Some programs block pasting. Choose the "type character by character" paste method in the settings (slower). For consoles where `Ctrl+V` does not work there is `Shift+Insert`.
- `Ctrl+Alt+Space` is taken by the Claude app, so the defaults are `Ctrl+Alt+D` and `Ctrl+Alt+S`. If a chosen combination is used by another app, the tray icon turns orange and the settings window warns you in advance.
- The model sometimes mishears names and rare terms. The "hint for the model" field in the settings helps.
- Speech translation to English is not implemented yet (planned as a separate build).
- Tested on Windows 11 with an RTX 4060 Laptop GPU only.

## Troubleshooting

| What you see | What to do |
|---|---|
| Orange tray icon | Hover it: the tooltip gives the reason (hotkey taken, model file missing, error in `config.toml`) |
| "model file not found" | Download a model in the settings or put a `.bin` into the models folder |
| Nothing is recognized | Pick the right microphone in the settings and check the level with "Test" |
| The NVIDIA build does not start | It needs an RTX 40-series card and a current NVIDIA driver; otherwise use the `cpu` package |
| Want to see what is going on | Tray icon → "Open logs folder" |

## Build from source

You need Rust (stable, MSVC toolchain), Visual Studio Build Tools with the C++ workload, CMake and LLVM (clang); for the NVIDIA build also CUDA Toolkit 13. The architecture and development plan are in [AGENTS.md](AGENTS.md) (written in Russian, for AI coding agents, but readable by people).

```powershell
. .\env.ps1                                       # CUDA/LLVM environment variables (edit the paths for your machine)
cargo test --release                              # tests
cargo run --release                               # console development build (logs on screen)

.\scripts\build-release.ps1 -Variant nvidia       # tray-only exe, CUDA for RTX 40 (sm_89)
.\scripts\build-release.ps1 -Variant cpu          # tray-only exe, no graphics card needed
.\scripts\package.ps1                             # folders and zip files in dist\release\
```

For another NVIDIA card, change `CMAKE_CUDA_ARCHITECTURES` in `scripts\build-release.ps1` (for example `86` for RTX 30, `75` for GTX 16 / RTX 20, `120a` for RTX 50) and rebuild whisper.cpp with `-Clean` (the first CUDA build takes about ten minutes). The scripts run at below-normal priority with a limited number of jobs, so the PC stays usable. See [docs/RELEASING.md](docs/RELEASING.md) for publishing a release.

Handy command-line options for debugging: `--list-devices`, `--device <part of name>`, `--model <path>`, `--dump-audio <folder>`, `--transcribe-file <wav>`, `--test-mic`, `--demo-overlay`.

## Third-party components

- [whisper.cpp](https://github.com/ggml-org/whisper.cpp) and the [Whisper](https://github.com/openai/whisper) models — MIT.
- Rust libraries (egui, winit, cpal, tray-icon and others) — MIT and/or Apache-2.0.
- NVIDIA CUDA libraries (`cublas64_13.dll`, `cublasLt64_13.dll`, `cudart64_13.dll`) are included **only in the NVIDIA release archive**, never in this repository. They are redistributed under the [NVIDIA CUDA Toolkit license](https://docs.nvidia.com/cuda/eula/).
- The Visual C++ runtime (`msvcp140.dll`, `vcruntime140*.dll`) in the release archives is a redistributable Microsoft component.

## License

[MIT](LICENSE)
