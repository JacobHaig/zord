# Zord

A fast, **fully-local** desktop app that records your **microphone** *and* your
**desktop/system audio** (Teams, Zoom, browser — anything playing) and produces
an accurate, timestamped, searchable transcript that labels who said what
(**Me** vs **Others**). No cloud, no server — all capture, transcription, and
storage happen on your machine.

- 🎙️ **Dual-channel capture** — your mic + system loopback, transcribed
  separately and merged onto one timeline.
- 🧠 **Local Whisper** (whisper.cpp) — GPU-accelerated on Apple Silicon (Metal),
  CPU on Windows. The model downloads once on first run, then works offline.
- 🔎 **Searchable history** — every session stored in local SQLite with
  full-text search.
- 📤 **Export** — Markdown, SRT, or JSON.
- 🖥️ **Two front-ends** — a native desktop GUI (Dioxus) and a `localhost` web
  dashboard for reviewing transcripts in a browser.

> Platforms: **macOS 13+ (Apple Silicon)** is the primary, fully-tested target.
> **Windows 10/11 (x64)** is supported in code (WASAPI loopback) and built in
> CI; runtime testing on Windows is still pending.

---

## 1. Prerequisites

| Tool | Why | Install |
|---|---|---|
| **Rust** (stable, ≥1.80) | builds everything | <https://rustup.rs> |
| **CMake** | compiles the bundled whisper.cpp | macOS: `brew install cmake` · Windows: ships with Visual Studio / `winget install Kitware.CMake` |
| **C/C++ toolchain** | whisper.cpp | macOS: Xcode Command Line Tools (`xcode-select --install`) · Windows: Visual Studio Build Tools (MSVC) |
| **dioxus-cli** *(only for packaging)* | `dx bundle` builds the `.app`/`.msi` | `cargo install dioxus-cli --version 0.7.9 --locked` |

> First build compiles whisper.cpp **and** the Dioxus stack — expect a few
> minutes. Subsequent builds are fast.

---

## 2. Build

```bash
git clone <your-fork-url> zord && cd zord
cargo build            # debug
cargo build --release  # optimized (recommended for real use)
```

That builds both binaries:

- `zord` — the command-line tool (crate `zord-app`)
- `zord-gui` — the desktop app (crate `zord-gui`)

---

## 3. Run the desktop app

```bash
cargo run -p zord-gui
# or, after a release build:
./target/release/zord-gui
```

In the window:

1. Click **● Record**. (First run downloads the Whisper model — you'll see a
   progress %.)
2. Talk, and/or play some audio (a video, a call). Watch the transcript stream
   in, color-coded **Me** / **Others**.
3. Click **■ Stop**. The session appears in the left sidebar.
4. Use the **search box** to find text across every session; click the **⚙**
   gear for settings; open a saved session to **export** it.

### First-run permissions

- **Microphone** — macOS/Windows prompt automatically on first record.
- **Screen Recording** *(the "Others" / system-audio channel)* — **macOS only**:
  the first record shows 0 system audio until you enable Zord under **System
  Settings → Privacy & Security → Screen Recording**, then **relaunch** the app.
  Until then, Zord degrades gracefully to mic-only and shows a banner. (Windows
  loopback needs no special permission.)

---

## 4. Use the command line

The CLI mirrors the engine and is handy for scripting and testing. The binary is
`zord` (`cargo run -p zord-app -- <CMD>`, or `./target/release/zord <CMD>`).

```bash
# Record mic + system audio until you press Enter (or --seconds N)
zord record
zord record --seconds 30 --model large-v3-turbo --keep-audio ~/calls/standup.wav

# Transcribe an existing WAV (any rate/channels) — great for a quick test
zord file /path/to/audio.wav

# List a session's transcript / search across all sessions
zord show <session-id>
zord search "quarterly numbers"

# Export a session
zord export <session-id> --format srt --out talk.srt   # md | srt | json

# Review everything in your browser (read-only, localhost only)
zord serve            # then open http://127.0.0.1:7777
zord serve --port 8080

# Re-transcribe a kept-audio session with a different/better model
zord retranscribe <session-id> --model large-v3-turbo
```

**Models** (`--model`): `large-v3-turbo-q5_0` (default — best size/speed),
`large-v3-turbo` (highest accuracy), `small.en` (light, good on CPU).

**`--keep-audio <file.wav>`** saves the raw audio as `<file>.me.wav` and
`<file>.others.wav`, which `retranscribe` can later reuse.

---

## 5. Where your data lives

Everything stays under one local app-data folder:

- **macOS:** `~/Library/Application Support/io.zord.zord/`
- **Windows:** `%APPDATA%\zord\zord\data\`
- **Linux:** `~/.local/share/zord/`

```
io.zord.zord/
├── config.json     # settings (model, retention, device, storage dir)
├── zord.db         # SQLite: sessions + transcript segments + FTS index
├── models/         # downloaded Whisper ggml models
├── audio/          # kept recordings (only if "keep audio" is on)
└── exports/        # files written by the GUI export buttons
```

Settings (GUI ⚙ or `config.json`) let you pick the model, choose a microphone,
toggle keep-audio, auto-delete old audio after N days, and relocate the storage
folder.

---

## 6. Package a distributable build

```bash
# from the repo root — -p/--package selects the workspace member
dx bundle --release --package zord-gui --platform desktop
```

Produces `ZordGui.app` + a `.dmg` (macOS) under
`target/dx/zord-gui/bundle/`. To distribute it without Gatekeeper warnings you
must **code-sign + notarize** with your Apple Developer account — see
[`docs/RELEASE.md`](docs/RELEASE.md) for the exact steps and the GitHub Actions
release workflow (tag `v*` → build + attach to a Release).

---

## 7. Project layout

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `zord-core` | shared types (`Source`, `Segment`, `Session`) |
| `zord-audio` | resample → 16 kHz mono, voice-activity segmentation, WAV writer |
| `zord-capture` | audio sources: `Microphone` (cpal) + `SystemAudio` (ScreenCaptureKit / WASAPI loopback) |
| `zord-transcribe` | Whisper (whisper-rs) + first-run model download |
| `zord-store` | SQLite storage + FTS5 search |
| `zord-config` | persisted settings + paths + retention |
| `zord-export` | Markdown / SRT / JSON renderers |
| `zord-web` | axum `localhost` review dashboard |
| `zord-app` | the `zord` CLI |
| `zord-gui` | the Dioxus desktop app |

The full design, decisions, and phase history live in
[`docs/PLAN.md`](docs/PLAN.md).

---

## 8. Troubleshooting

- **`cmake` / build-script errors** — install CMake and a C/C++ toolchain (§1).
- **No "Others" text on macOS** — grant **Screen Recording** permission and
  relaunch (§3). The mic ("Me") works without it.
- **First record is slow / "Downloading model…"** — the model (~0.5–1.5 GB
  depending on choice) downloads once, then is cached in `models/`.
- **Slow transcription on a CPU-only machine** — pick a smaller model
  (`small.en`) in settings or with `--model`.
- **GUI won't start from a terminal with odd permission attribution** — that's
  expected for the bare binary in dev; a bundled, signed `.app` (§6) gets its
  own identity and clean permission prompts.
