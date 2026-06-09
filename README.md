# Zord

**Private meeting transcription that never leaves your machine.**

Zord records your microphone *and* your computer's audio — Teams, Zoom, a
browser call, anything playing — and turns it into an accurate, timestamped,
searchable transcript that knows who said what. It runs entirely on your
device: no cloud, no account, no subscription, nothing uploaded. Your
conversations stay yours.

---

## Why Zord

- **Genuinely private.** Capture, transcription, summaries, and storage all
  happen locally. There is no server to send audio to and no telemetry. For
  sensitive calls — legal, medical, HR, strategy — that isn't a setting you
  toggle; it's the architecture.
- **Hears both sides.** Most tools capture only your microphone. Zord records
  the other participants too (the system-audio loopback) and keeps the two on
  one timeline, labeled **Me** and **Others** — so a remote call transcribes as
  cleanly as an in-person one.
- **Accurate, and yours to tune.** Local Whisper (or NVIDIA Parakeet) with
  selectable model sizes; GPU-accelerated on Apple Silicon, CPU everywhere else.
  Don't like a line? Re-transcribe it later with a bigger model.
- **More than a transcript.** Optional on-device AI turns meetings into
  summaries, action items, a cross-meeting project overview, and a chat you can
  ask "what did we decide about X?" — grounded only in what was actually said.

> **Platforms.** macOS 13+ (Apple Silicon) is the primary, fully-tested target.
> Windows 10/11 (x64) is supported in code (WASAPI loopback) and built in CI;
> runtime testing on Windows is ongoing.

---

## What it does

### Capture and transcribe

- **Dual-channel capture** — your mic and the system loopback are transcribed
  separately and merged onto a single Me/Others timeline, with live level
  meters per channel.
- **Local transcription** — Whisper (whisper.cpp), with optional NVIDIA
  Parakeet (sherpa-onnx) for fast, accurate English. Models download on first
  use and then run fully offline.
- **Per-channel audio levels** — boost a quiet mic or even out a level that
  varies meeting to meeting, with automatic leveling or a manual gain per
  channel. A soft limiter keeps it from clipping.
- **Deferred transcription for light hardware** — turn live transcription off
  and recording becomes capture-only (no CPU spikes or model memory mid-call);
  the transcript is generated when you stop. Or keep a small model live and
  re-transcribe with a larger one afterward.

### Make sense of your meetings

- **AI summaries** *(optional)* — turn a session into clean Markdown notes:
  TL;DR, key points, action items. Choose a style preset or write your own
  prompt.
- **Cross-meeting overview** *(optional)* — a standing, project-grouped rollup
  across recent meetings: what's in progress, what's done, who owns what, and
  your open action items.
- **Chat with your meetings** *(optional)* — ask questions about one meeting or
  across all of them. Answers come only from your transcripts, and it says when
  something wasn't discussed rather than inventing an answer.
- **Per-speaker labels** *(optional)* — split the Others channel into individual
  speakers, rename them, and see them color-coded throughout. Each meeting
  remembers its own expected headcount.
- **Searchable history** — every session is stored locally with full-text
  search across everything you've ever recorded.
- **Per-line replay** — hover any transcript line to play back exactly that
  moment of audio, handy for fixing a misheard word by ear.

### Your AI, your choice

The AI features run on a built-in local model out of the box. Prefer your own
setup? Point Zord at any OpenAI-compatible server — LM Studio, Ollama,
llama-server, vLLM — and every AI feature uses it instead. Either way, nothing
goes to a third party you didn't choose.

### Private by design

- Keep or auto-delete recordings on a schedule you set (30-day default).
- Relocate where everything is stored.
- Optional at-rest database encryption (SQLCipher), with the passphrase in your
  OS keychain.
- Export to Markdown, SRT, or JSON whenever you want your data elsewhere — on
  your terms, not as the only way to read it.

### Two ways in

A native desktop app, and a read-only `localhost` web dashboard for reviewing
transcripts in a browser. Both are local-only.

---

## Getting started (building from source)

> Zord is currently distributed as source. These steps build the desktop app
> and the command-line tool.

### 1. Prerequisites

| Tool | Why | Install |
|---|---|---|
| **Rust** (stable, ≥1.80) | builds everything | <https://rustup.rs> |
| **CMake** | compiles the bundled whisper.cpp | macOS: `brew install cmake` · Windows: ships with Visual Studio / `winget install Kitware.CMake` |
| **C/C++ toolchain** | whisper.cpp | macOS: Xcode Command Line Tools (`xcode-select --install`) · Windows: Visual Studio Build Tools (MSVC) |
| **dioxus-cli** *(optional)* | hot-reload dev loop + bundling | `cargo install dioxus-cli --version 0.7.9 --locked` |

> The first build compiles whisper.cpp and the Dioxus stack — expect a few
> minutes. Later builds are fast.

### 2. Build

```bash
git clone <your-fork-url> zord && cd zord
cargo build            # debug
cargo build --release  # optimized (recommended for real use)
```

That produces two binaries:

- `zord` — the command-line tool (crate `zord-app`)
- `zord-gui` — the desktop app (crate `zord-gui`)

The lean default build is Whisper-only. The capabilities below are opt-in build
features so you only compile (and ship) what you use.

#### NVIDIA Parakeet models

Fast, 25-language transcription that runs well on CPU, via sherpa-onnx:

```bash
cargo run -p zord-gui --features parakeet
```

The build script fetches prebuilt sherpa-onnx libraries; the settings panel
then lists Parakeet alongside the Whisper models.

#### AI features (summaries, compression, overview, chat)

Two composable features choose the LLM that powers them — build either or both:

```bash
# Built-in local model (compiles llama.cpp):
cargo run -p zord-gui --features llm-local
# Your own OpenAI-compatible server (LM Studio, Ollama, …) — no llama.cpp toolchain:
cargo run -p zord-gui --features llm-remote
# Both (what releases ship) — pick the backend in Settings → AI:
cargo run -p zord-gui --features llm-local,llm-remote
cargo build -p zord-app --features llm-local,llm-remote
```

`llm-local` compiles llama.cpp (Metal on Apple Silicon) and, on first use,
downloads a ~2 GB instruct model (Qwen2.5-3B-Instruct). The context window for
compression and overview is configurable in Settings → AI (default 16K, up to
128K).

#### Per-speaker diarization

Label individual speakers within the Others channel:

```bash
cargo run -p zord-gui --features diarization
cargo build -p zord-app --features diarization
```

On first use this downloads two small ONNX models (a segmentation model plus a
speaker-embedding model). Diarization runs offline after recording and on
demand; an optional live mode shows rough labels while recording (recomputed
accurately at stop). The segmentation model is selectable in Settings →
Speakers (stock pyannote, or Rev's more-accurate Reverb fine-tunes).

#### Database encryption (SQLCipher)

Encrypt the local database at rest:

```bash
cargo run -p zord-gui --features encryption
cargo build -p zord-app --features encryption
```

Encryption is opt-in. Enable it in settings (applied on next launch) or with
`zord encrypt`; the passphrase can live in your OS keychain or be prompted each
launch (`ZORD_PASSPHRASE` is honored for scripting). **Keep your passphrase
safe — losing it means unrecoverable data.** This feature vendors SQLCipher +
OpenSSL, so the build also needs perl.

#### Discord integration (experimental, in progress)

Record a Discord voice call with each participant on their own track — no
diarization needed, since Discord gives us a separate, identified audio stream
per person (real names instead of "Speaker 1"). A bot you control joins the
voice channel you're in and receives per-user audio.

```bash
cargo build -p zord-integrations --features discord
```

Build requirements: **no extra system dependencies** beyond the CMake + C/C++
toolchain already listed above. The `discord` feature pulls
[`songbird`](https://github.com/serenity-rs/songbird) + `serenity` and compiles
`libopus` (Opus audio codec) and the DAVE end-to-end-encryption stack from
source — that's what the C toolchain/CMake are for. Discord enforces
[DAVE](https://discord.com/blog/meet-dave-e2ee-for-audio-video) (E2EE) on all
voice; songbird 0.6 implements it, so the bot can still receive and decrypt
audio. The first build of this feature is slow (it compiles songbird + serenity).

Status: the receive/decrypt path is **verified working** via a de-risking spike
(`cargo run -p zord-integrations --features discord --bin discord-spike`, driven
by `DISCORD_TOKEN` / `DISCORD_USER_ID` env vars — it follows you into voice and
writes a per-user WAV). The full in-app integration (Settings → Integrations, a
one-click bot invite, follow-the-user auto-join, live transcription) is being
built out — see `docs/PLAN.md` → "Platform integrations (Phases 27–31)". You
bring your own bot token; nothing is hosted by Zord.

### 3. Run the desktop app

```bash
cargo run -p zord-gui
# or, after a release build:
./target/release/zord-gui
```

With `dioxus-cli` installed you can use the hot-reload dev loop instead:

```bash
dx serve --package zord-gui --platform desktop   # build + run with hot-reload
dx run   --package zord-gui --platform desktop   # build + run, no hot-reload
```

In the app:

1. Press **Record**. (The first run downloads the transcription model, with a
   progress indicator.)
2. Talk, or play a call or video. The transcript streams in, labeled Me and
   Others.
3. Press **Stop** — the session appears in the sidebar.
4. Search across every session, open one to read or export it, and use the
   **Generate** menu on a session to summarize, compress, identify speakers, or
   re-transcribe.
5. Open **Settings** (the gear in the left rail) to manage models, pick a
   microphone, set audio levels, and control retention.

#### First-run permissions

- **Microphone** — macOS and Windows prompt on the first recording.
- **Screen Recording** (the Others / system-audio channel) — **macOS only**: the
  first record captures no system audio until you enable Zord under *System
  Settings → Privacy & Security → Screen Recording*, then relaunch. Until then
  Zord degrades gracefully to mic-only and shows a banner. (Windows loopback
  needs no special permission.)

### 4. Use the command line

The `zord` CLI mirrors the engine — handy for scripting and quick tests
(`cargo run -p zord-app -- <CMD>`, or `./target/release/zord <CMD>`).

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

# Cross-meeting overview across recent sessions
zord overview --max 50
```

**Models** (`--model`): `large-v3-turbo-q5_0` (default — best size/speed),
`large-v3-turbo`, `large-v3`, `medium.en`, `small.en`, `base.en`, `tiny.en`.
A `--features parakeet` build adds `parakeet-tdt-0.6b-v3`.

**`--keep-audio <file.wav>`** saves the raw audio as `<file>.me.wav` and
`<file>.others.wav`, which `retranscribe` can reuse later.

### 5. Where your data lives

Everything stays under one local app-data folder:

- **macOS:** `~/Library/Application Support/Zord/`
- **Windows:** `%APPDATA%\Zord\data\`
- **Linux:** `~/.local/share/Zord/`

```
Zord/
├── config.json     # settings (model, retention, device, storage dir, …)
├── zord.db         # SQLite: sessions + transcript segments + full-text index
├── models/         # downloaded models (Whisper, Parakeet, summary, speaker)
├── audio/          # kept recordings (native rate, per channel)
├── logs/           # zord.log
└── exports/        # files written by the export buttons
```

Settings let you pick the model, choose a microphone, set per-channel audio
levels, control keep/auto-delete retention, and relocate the storage folder.

### 6. Package a distributable build

```bash
dx bundle --release --package zord-gui --platform desktop
```

Produces `ZordGui.app` and a `.dmg` (macOS) under `target/dx/zord-gui/bundle/`.
To distribute without Gatekeeper warnings, code-sign and notarize with an Apple
Developer account — see [`docs/RELEASE.md`](docs/RELEASE.md) for the exact steps
and the GitHub Actions release workflow (tag `v*` → build → attach to a Release).

---

## Project layout

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `zord-core` | shared types (`Source`, `Segment`, `Session`) |
| `zord-audio` | resample to 16 kHz mono, voice-activity segmentation, level control, WAV read/write |
| `zord-capture` | audio sources: `Microphone` (cpal) + `SystemAudio` (ScreenCaptureKit / WASAPI loopback) |
| `zord-transcribe` | `TranscribeBackend` trait; Whisper always, Parakeet under `parakeet`; offline re-transcription; model catalog + downloads |
| `zord-store` | SQLite storage + full-text search; optional SQLCipher encryption |
| `zord-config` | persisted settings, paths, retention |
| `zord-export` | Markdown / SRT / JSON renderers |
| `zord-summarize` | LLM backend (local llama.cpp under `llama`, OpenAI-compatible client under `remote`) for summaries, compression, and chat |
| `zord-overview` | cross-meeting overview synthesis (compress → group → roll up) |
| `zord-diarize` | per-speaker diarization (sherpa-onnx) under `diarization`; speaker-model catalog |
| `zord-net` | shared, proxy/cert-store-aware HTTP for model downloads + the remote LLM |
| `zord-integrations` | platform integrations (per-participant capture); Discord via `songbird`/`serenity` under `discord` |
| `zord-web` | axum `localhost` review dashboard |
| `zord-app` | the `zord` CLI |
| `zord-gui` | the Dioxus desktop app |

Security posture and review notes: [`docs/SECURITY.md`](docs/SECURITY.md).
Full design, decisions, and phase history: [`docs/PLAN.md`](docs/PLAN.md).

---

## Troubleshooting

- **`cmake` / build-script errors** — install CMake and a C/C++ toolchain.
- **No Others text on macOS** — grant Screen Recording permission and relaunch.
  The mic (Me) channel works without it.
- **First record is slow / "Downloading model…"** — the model downloads once,
  then is cached under `models/`.
- **Slow transcription on a CPU-only machine** — pick a smaller model
  (`small.en`), or turn off live transcription and let it run after you stop.
- **Behind a corporate proxy / HuggingFace blocked** — in-app downloads use your
  OS certificate store and proxy environment variables; if a download still
  fails, Zord shows the direct URL and a ModelScope mirror so you can fetch it in
  a browser and drop it in the models folder.
