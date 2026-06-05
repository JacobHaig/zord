# Zord

A fast, **fully-local** desktop app that records your **microphone** *and* your
**desktop/system audio** (Teams, Zoom, browser — anything playing) and produces
an accurate, timestamped, searchable transcript that labels who said what
(**Me** vs **Others**). No cloud, no server — all capture, transcription, and
storage happen on your machine.

- 🎙️ **Dual-channel capture** — your mic + system loopback, transcribed
  separately and merged onto one **Me / Others** timeline, with live dB level
  meters per channel.
- 🧠 **Local transcription** — **Whisper** (whisper.cpp), GPU-accelerated on
  Apple Silicon (Metal), CPU elsewhere. Optionally **NVIDIA Parakeet**
  (sherpa-onnx) behind a build feature. Models download on demand and run offline.
- ⚙️ **Model management** — pick from several Whisper sizes (and Parakeet) in a
  settings panel; download / select / delete locally; re-transcribe old sessions
  with a better model. No HuggingFace access? Parakeet + diarization models come
  from GitHub, and any **custom `.gguf`** dropped in the models folder shows up
  as a selectable summary model — no download required. In-app downloads use your
  **OS certificate store + proxy env vars**, so they work behind corporate
  HTTPS-inspection/proxies (and there's always the manual drop-in fallback).
- 🔎 **Searchable history** — every session stored in local SQLite with
  full-text search.
- ✨ **Local AI summaries** *(optional)* — a local LLM (llama.cpp) turns a
  session into Markdown notes, fully offline. Pick the model (Qwen2.5 1.5B/3B/7B),
  a style preset, or write your own prompt — all in settings. Or point Zord at
  your **own inference server** (LM Studio, Ollama, llama-server, vLLM — any
  OpenAI-compatible endpoint) in Settings → AI, and every AI feature
  (summaries, compression, Overview, chat, auto-titles) uses it instead.
- 🗜 **Dense compression** *(optional)* — condense a whole meeting into
  token-minimal dense prose (projects + state, action items, decisions, open
  questions) with one click. Stored per session and copyable; the context window
  is configurable (16K fits ~an hour) so an entire meeting compresses without
  truncation.
- 📊 **Cross-meeting Overview** *(optional)* — a holistic, project-grouped rollup
  across your recent meetings: per-project state / pending / done / owners / open
  questions, plus a pinned **"My open action items"**, oriented around you. Built
  by compressing each meeting then synthesizing them in one pass (with a
  hierarchical fallback at scale) — all local. Generate/refresh from the 📊
  Overview view; in the CLI, `zord overview`.
- 💬 **Chat with your meetings** *(optional)* — ask questions grounded in your
  data: per-meeting Q&A (in a session) and cross-meeting Q&A (in the Overview).
  Answers come only from the local transcripts/compressions — it says when
  something wasn't discussed rather than inventing it.
- 🗣 **Per-speaker diarization** *(optional)* — split the "Others" channel into
  **Speaker 1/2/3** (sherpa-onnx), rename them, and see them colored in the
  transcript + exports. Runs offline after recording (and on demand); an optional
  live mode shows provisional labels while recording. Each meeting remembers its
  own expected speaker count (set it next to **Identify speakers**; blank = auto).
  The segmentation model is selectable too: stock pyannote 3.0, or Rev's
  **Reverb v1/v2** fine-tunes (more accurate; non-commercial license).
- 🔊 **Per-line audio replay** — hover a transcript line and press ▶ to hear
  exactly that span of the retained audio (handy for fixing a mis-transcribed
  line by ear). The button appears only when that channel's audio file exists
  on disk. Kept audio stores at the device's native rate, so replay is full
  capture quality; models derive their 16 kHz from it on the fly.
- 🔁 **Deferred & re-transcription** — on low-power machines, turn **live
  transcription off** (Settings → Transcription): recording becomes capture-only
  (meters + audio, no CPU spikes or model RAM mid-meeting) and the transcript
  generates when you stop, with a separately chosen **re-transcription model**.
  Or keep live on with a small model and hit **🔁 Re-transcribe** on any session
  later to regenerate it with a bigger one — speaker labels are re-derived
  automatically. Kept audio defaults to a 30-day retention window.
- 🎚️ **Configurable** — capture mic-only / system-only / both, **mute your mic
  mid-recording** with a toggle next to Record, auto-generate a meeting **title**,
  edit transcript lines inline, and tune summary model + prompt from the settings
  panel. HuggingFace blocked? Summary models have a **ModelScope** mirror link in
  the download-failure panel (browser-download, drop in the models folder).
- 📤 **Export** — Markdown, SRT, or JSON, with one-click **Reveal in
  Finder/Explorer** and **Open in editor**.
- 🧰 **Diagnostics** — Settings → *Files & folders* jumps straight to the
  models / data / logs folders (and reveals the config + database). Errors are
  written to a `zord.log` you can open or copy. And if a model download is
  blocked (e.g. a corporate proxy), Zord shows the direct URL + *Open models
  folder* so you can fetch it in a browser and drop it in.
- 🖥️ **Two front-ends** — a native desktop GUI (Dioxus) and a `localhost` web
  dashboard for reviewing transcripts in a browser.
- 🔒 **Private by design** — retention controls (keep/auto-delete audio), a
  configurable storage location, and optional **at-rest database encryption**
  (SQLCipher, behind a build feature). Nothing leaves the device.

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
| **dioxus-cli** *(optional)* | `dx serve/run` dev loop + `dx bundle` packaging | `cargo install dioxus-cli --version 0.7.9 --locked` |

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

### Optional: NVIDIA Parakeet models

By default Zord transcribes with Whisper. To also enable **NVIDIA Parakeet**
(via sherpa-onnx — fast, 25-language, runs well on CPU), build with the
`parakeet` feature:

```bash
cargo run -p zord-gui --features parakeet   # or: cargo build --release -p zord-gui --features parakeet
```

This pulls in an ONNX runtime (the build script downloads prebuilt sherpa-onnx
libs). With it enabled, the settings panel lists Parakeet alongside the Whisper
models to download and select. Without the feature, the default build stays
lean and Whisper-only.

### Optional: AI features (summaries, compression, Overview, chat)

Two composable features pick the LLM that powers them — build with either or
both:

```bash
# Built-in local LLM (compiles llama.cpp):
cargo run -p zord-gui --features llm-local
# External OpenAI-compatible server (LM Studio, Ollama, …) — no llama.cpp toolchain:
cargo run -p zord-gui --features llm-remote
# Both (what releases ship) — pick the backend in Settings → AI:
cargo run -p zord-gui --features llm-local,llm-remote
cargo build -p zord-app --features llm-local,llm-remote   # CLI: `zord summarize` / `compress` / `overview`
```

`llm-local` compiles llama.cpp (Metal on Apple Silicon) and, on first use, downloads a
~2 GB instruct model (Qwen2.5-3B-Instruct, Q4_K_M) to the models folder. The same
model also powers 🗜 **Compress**, which condenses a meeting into token-minimal
dense prose; its context window is set in Settings → AI (default 16K, up to
128K — a 3B model handles 64K comfortably on a 16 GB machine).

### Optional: per-speaker diarization

Label individual speakers within the "Others" channel, build with the
`diarization` feature:

```bash
cargo run -p zord-gui --features diarization     # GUI: 🗣 Identify speakers on a saved session
cargo build -p zord-app --features diarization   # CLI: `zord diarize <session-id>`
```

On first use this downloads two small ONNX models (a pyannote segmentation model
+ a speaker-embedding model) to the models folder; manage them in Settings →
Speakers. Diarization runs **offline after recording** (accurate) and on demand;
the on-demand path needs retained audio. An optional **live** toggle shows rough
labels while recording (off by default — it's recomputed accurately at stop).

### Optional: database encryption (SQLCipher)

To encrypt the local database at rest, build with the `encryption` feature:

```bash
cargo run -p zord-gui --features encryption    # GUI: unlock screen + Settings → Encryption
cargo build -p zord-app --features encryption  # CLI: `zord encrypt [--remember]` / `zord decrypt`
```

Encryption is opt-in. Enable it from the GUI settings (applied on next launch) or
with `zord encrypt`; the passphrase can be remembered in your OS keychain or
prompted each launch (`ZORD_PASSPHRASE` is honored for scripting). **Keep your
passphrase safe — a lost passphrase means unrecoverable data.** This feature
vendors SQLCipher + OpenSSL, so the build needs perl in addition to the C
toolchain.

---

## 3. Run the desktop app

```bash
cargo run -p zord-gui
# or, after a release build:
./target/release/zord-gui
```

### Or run it with `dx` (the Dioxus CLI)

If you've installed `dioxus-cli` (§1), you can build/run through `dx` instead of
cargo. `dx` selects the workspace member with `-p/--package`, so run these from
the **repo root**:

```bash
dx serve --package zord-gui --platform desktop   # build + run with hot-reload (dev)
dx run   --package zord-gui --platform desktop    # build + run, no hot-reload
dx build --release --package zord-gui --platform desktop   # compile only (no bundle)
```

- **`dx serve`** is the nicest dev loop: edit the RSX/Rust and the running app
  hot-patches without a full restart.
- **`dx build`** just compiles (artifacts under `target/dx/…`); use
  **`dx bundle`** (§6) when you want a shippable `.app`/`.dmg`/`.msi`.
- Plain `cargo run -p zord-gui` works too and needs no `dx` — use whichever you
  prefer.

In the window:

1. Click **● Record**. (First run downloads the Whisper model — you'll see a
   progress %.)
2. Talk, and/or play some audio (a video, a call). Watch the transcript stream
   in, color-coded **Me** / **Others**.
3. Click **■ Stop**. The session appears in the left sidebar.
4. Use the **search box** to find text across every session. Open a saved
   session to **export** it (Markdown/SRT/JSON) and **Reveal**/**Open** the file.
5. Click the **⚙ gear** for the full settings panel: download/select/delete
   transcription models, choose your microphone, toggle keep-audio +
   auto-delete-after-N-days.

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

# Label individual speakers in the "Others" channel (needs --features diarization
# + retained audio for that session)
zord diarize <session-id>

# Summarize / compress a session (needs --features llm-local and/or llm-remote)
zord summarize <session-id>                            # Markdown notes
zord compress  <session-id>                            # token-minimal dense prose

# Synthesize a cross-meeting Overview across recent sessions (compresses any
# meetings that aren't yet, then rolls them up by project). Same features as above.
zord overview --max 50
```

**Models** (`--model`): `large-v3-turbo-q5_0` (default — best size/speed),
`large-v3-turbo`, `large-v3`, `medium.en`, `small.en`, `base.en`, `tiny.en`.
With a `--features parakeet` build, `parakeet-tdt-0.6b-v3` is also available.

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
├── models/         # downloaded models (Whisper .bin + any Parakeet dirs)
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
| `zord-audio` | resample → 16 kHz mono, voice-activity segmentation, WAV read/write |
| `zord-capture` | audio sources: `Microphone` (cpal) + `SystemAudio` (ScreenCaptureKit / WASAPI loopback) |
| `zord-transcribe` | `TranscribeBackend` trait + Whisper (whisper-rs) always, Parakeet (sherpa-onnx) under `parakeet`; model catalog + downloads |
| `zord-store` | SQLite storage + FTS5 search |
| `zord-config` | persisted settings + paths + retention |
| `zord-export` | Markdown / SRT / JSON renderers |
| `zord-summarize` | local-LLM summaries + compression (llama.cpp) under `summaries`; summary-model catalog |
| `zord-overview` | cross-meeting Overview synthesis (compress → group → rollup) under `summaries` |
| `zord-diarize` | per-speaker diarization (sherpa-onnx) under `diarization`; speaker-model catalog |
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
