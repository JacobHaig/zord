# Kickstart — build and run Zord locally

Everything you need to go from a fresh machine to a running, locally-built
Zord: prerequisites per platform, build commands, optional features, first-run
steps, the CLI, and where your data lives.

> Just want to **use** Zord? Download a build from the
> [releases page](https://github.com/JacobHaig/zord/releases) instead — see
> ["Installing a release"](README.md#installing-a-release) in the README.
> This guide is for building from source.

---

## 1. Prerequisites

The default build needs only **Rust + CMake + a C/C++ compiler**. Everything
else is optional.

### macOS (13+, Apple Silicon — the primary platform)

```bash
xcode-select --install                  # C/C++ toolchain + git
brew install cmake                      # builds the bundled whisper.cpp
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust (stable)
```

### Windows (10/11, x64)

| Tool | How |
|---|---|
| **Visual Studio Build Tools 2022** | install with the **"Desktop development with C++"** workload (MSVC + Windows SDK) |
| **CMake** | included in the VS workload, or `winget install Kitware.CMake` |
| **Rust** (stable, MSVC toolchain) | <https://rustup.rs> |

The GUI uses the OS **WebView2** runtime, preinstalled on up-to-date Windows.

### Linux

The CLI and web dashboard build and run; **system-audio capture is not
implemented on Linux yet** (microphone capture works), and the desktop GUI
needs the usual Dioxus/WebKitGTK system packages. Linux is currently
build-supported but not a tested target.

### Optional tools

| Tool | Needed for | Install |
|---|---|---|
| **dioxus-cli 0.7.9** | hot-reload dev loop, `.app`/`.dmg`/installer bundling | `cargo install dioxus-cli --version 0.7.9 --locked` |
| **perl** | only the `encryption` feature (vendored OpenSSL) | preinstalled on macOS; Windows: Strawberry Perl |

---

## 2. Build

```bash
git clone https://github.com/JacobHaig/zord.git && cd zord
cargo build            # debug (fast iteration)
cargo build --release  # optimized — recommended for real use
```

That produces two binaries under `target/{debug,release}/`:

- **`zord-gui`** — the desktop app
- **`zord`** — the command-line tool

### Optional features

The default build is lean (Whisper transcription only). Capabilities are
opt-in Cargo features so you compile only what you use — combine freely with
`--features a,b,c` on either binary:

| Feature | Adds | Extra requirements | First build |
|---|---|---|---|
| `parakeet` | NVIDIA Parakeet ASR (fast, 25 languages, CPU-friendly) | none (prebuilt ONNX libs are fetched) | ~30 s |
| `llm-local` | AI features on a built-in local LLM (compiles llama.cpp; Metal on Apple Silicon) | none | minutes |
| `llm-remote` | AI features against your own OpenAI-compatible server (LM Studio, Ollama, vLLM, …) | none (pure HTTP) | fast |
| `diarization` | per-speaker labels within the Others channel (sherpa-onnx) | none | ~1 min |
| `voiceprints` | opt-in speaker memory: name someone once, Zord auto-names them in future sessions; per-person deletable; requires `diarization` | none | negligible |
| `discord` | record Discord calls with one separated track per speaker | none (compiles libopus + the DAVE E2EE stack — slow first build) | minutes |
| `encryption` | SQLCipher at-rest database encryption | **perl** at build time | ~1 min |
| `self-update` | in-app update check + Windows in-place install | none (GitHub-channel release builds only) | fast |

```bash
# What official releases ship (plus self-update on the GitHub channel):
cargo build --release -p zord-gui --features parakeet,diarization,voiceprints,llm-local,llm-remote,discord
cargo build --release -p zord-app --features parakeet,diarization,llm-local,llm-remote
```

AI models are **never** compiled in — they download on first use (with
progress UI) and are cached locally, after which everything runs offline.

---

## 3. Run the desktop app

```bash
cargo run -p zord-gui                      # default features
cargo run -p zord-gui --features discord   # with extras

# with dioxus-cli installed:
dx serve --package zord-gui --platform desktop   # hot-reload dev loop
```

First session:

1. Press **Record** (the first run downloads the transcription model).
2. Talk, or play a call/video — the transcript streams in, labeled **Me** and
   **Others**.
3. Press **Stop** — the session lands in the sidebar: searchable, exportable.
4. Use the **Generate ▾** menu on a saved session to summarize, compress,
   identify speakers, or re-transcribe.
5. **Settings** (gear, left rail) holds models, microphone choice, audio
   levels, retention, integrations, and updates.

### First-run permissions

- **Microphone** — macOS and Windows prompt on the first recording.
- **Screen Recording** (macOS only; powers the Others / system-audio channel) —
  enable Zord under *System Settings → Privacy & Security → Screen Recording*
  and relaunch. Until then Zord records mic-only and shows a banner. Windows
  loopback needs no permission.

### Recording a Discord call

Build with `--features discord`, then in **Settings → Integrations** paste a
bot token + your user ID, invite the bot to your server, and press the blurple
**Record Discord** button. One audio track per participant, real names, no
diarization. Full guide: [`docs/discord-integration.md`](docs/discord-integration.md).

---

## 4. Use the command line

`cargo run -p zord-app -- <CMD>`, or `./target/release/zord <CMD>`:

```bash
# Record mic + system audio until you press Enter (or --seconds N)
zord record
zord record --seconds 30 --model large-v3-turbo --keep-audio ~/calls/standup.wav

# Transcribe an existing WAV (any rate/channels)
zord file /path/to/audio.wav

# Read a transcript / search across all sessions
zord show <session-id>
zord search "quarterly numbers"

# Export a session (md | srt | json)
zord export <session-id> --format srt --out talk.srt

# Review everything in your browser (read-only, localhost only)
zord serve            # then open http://127.0.0.1:7777

# Re-transcribe a kept-audio session with a different/better model
zord retranscribe <session-id> --model large-v3-turbo

# Identify speakers (needs --features diarization + retained audio)
zord diarize <session-id>

# Summarize / compress (needs --features llm-local and/or llm-remote)
zord summarize <session-id>
zord compress  <session-id>

# Print the living overview document / fold new sessions into it
zord overview
zord overview --update
```

**Models** (`--model`): `large-v3-turbo-q5_0` (default — best size/speed),
`large-v3-turbo`, `large-v3`, `medium.en`, `small.en`, `base.en`, `tiny.en`.
A `--features parakeet` build adds `parakeet-tdt-0.6b-v3`.

**`--keep-audio <file.wav>`** saves the raw audio as `<file>.me.wav` and
`<file>.others.wav`, which `retranscribe` can reuse later.

---

## 5. Where your data lives

Everything stays under one local app-data folder:

- **macOS:** `~/Library/Application Support/Zord/`
- **Windows:** `%APPDATA%\Zord\data\`
- **Linux:** `~/.local/share/Zord/`

```
Zord/
├── config.json     # settings (model, retention, device, storage dir, …)
├── zord.db         # SQLite: sessions + transcript segments + full-text index
├── models/         # downloaded models (Whisper, Parakeet, summary, speaker)
├── audio/          # kept recordings (one date-named folder per session;
│                   #   WAV when fresh, .opus once aged — see Settings → Recording)
├── logs/           # zord.log + crash.log
└── exports/        # files written by the export buttons
```

`storage_dir` in Settings relocates the db/audio/exports (models, config, and
logs stay in the app-data dir).

---

## 6. Troubleshooting

- **`cmake` / build-script errors** — install CMake and a C/C++ toolchain
  (section 1).
- **No Others text on macOS** — grant Screen Recording permission and relaunch.
  The mic (Me) channel works without it.
- **First record is slow / "Downloading model…"** — the model downloads once,
  then is cached under `models/`.
- **Slow transcription on a CPU-only machine** — pick a smaller model
  (`small.en`), or turn off live transcription and let it run after you stop.
- **Behind a corporate proxy / HuggingFace blocked** — in-app downloads use
  your OS certificate store and proxy environment variables; if a download
  still fails, Zord shows the direct URL and a ModelScope mirror so you can
  fetch it in a browser and drop it in the models folder.
- **"app isn't running" when recording one app** — per-app capture resolves
  the chosen app at record time; launch the target app first (on Windows it
  must have played audio at least once to appear in the picker).
- **Record Discord does nothing** — the bot must be invited to the server
  you're calling in (Settings → Integrations → "Invite bot to a server") and
  you must be in a voice channel when you press it. "Test connection" verifies
  the token.
- **macOS warns "unidentified developer"** — release builds are unsigned for
  now: right-click → **Open** (needed once).

---

## 7. Packaging a distributable build

```bash
dx bundle --release --package zord-gui --platform desktop
```

Produces `ZordGui.app` + a `.dmg` (macOS) or an installer (Windows) under
`target/dx/zord-gui/bundle/`. Signing, notarization, distribution channels,
and the CI release workflow: [`docs/RELEASE.md`](docs/RELEASE.md).
