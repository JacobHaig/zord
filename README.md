# Zord

**Private meeting transcription that never leaves your machine.**

Zord records your microphone *and* the other side of the call — Teams, Zoom, a
browser tab, a Discord voice channel — and turns it into an accurate,
timestamped, searchable transcript that knows who said what. It runs entirely
on your device: no cloud, no account, no subscription, nothing uploaded. Your
conversations stay yours.

---

## Why Zord

- **Genuinely private.** Capture, transcription, summaries, and storage all
  happen locally. There is no server to send audio to and no telemetry. For
  sensitive calls — legal, medical, HR, strategy — that isn't a setting you
  toggle; it's the architecture.
- **Hears both sides.** Most tools capture only your microphone. Zord records
  the other participants too — the whole system mix, or *just one app* — and
  keeps everything on one timeline, labeled **Me** and **Others**, so a remote
  call transcribes as cleanly as an in-person one.
- **Knows who's speaking.** On a Discord call, Zord receives **one separated
  audio track per participant** through your own bot — real names on every
  line, by construction, no guesswork. Everywhere else, optional on-device
  diarization splits the Others channel into labeled speakers.
- **Accurate, and yours to tune.** Local Whisper (or NVIDIA Parakeet) with
  selectable model sizes; GPU-accelerated on Apple Silicon, CPU everywhere
  else. Don't like a transcript? Re-run it later with a bigger model.
- **More than a transcript.** Optional on-device AI turns meetings into
  summaries, action items, a cross-meeting project overview, and a chat you
  can ask "what did we decide about X?" — grounded only in what was actually
  said.

> **Platforms.** macOS 13+ (Apple Silicon) is the primary, fully-tested
> target. Windows 10/11 (x64) is supported in code (WASAPI loopback,
> per-app capture, self-updating portable EXE) and built in CI; runtime
> testing on Windows is ongoing.

---

## What it does

### Capture and transcribe

- **Dual-channel capture** — your mic and the system loopback are transcribed
  separately and merged onto a single Me/Others timeline, with live level
  meters and per-channel mute.
- **Record one app, not the whole desktop** — scope the Others channel to a
  single application (just the meeting, not your music or notifications).
- **Record Discord calls properly** — your own bot joins the voice channel as
  a visible participant and captures **every speaker on their own track**,
  decrypted end-to-end (DAVE), with their real Discord names. One blurple
  button starts it.
- **Local transcription** — Whisper (whisper.cpp), with optional NVIDIA
  Parakeet (sherpa-onnx) for fast, accurate multilingual ASR. Models download
  on first use and then run fully offline.
- **Per-channel audio levels** — boost a quiet mic or even out a wildly-mixed
  call, with automatic leveling or manual gain and a soft limiter.
- **Deferred transcription for light hardware** — turn live transcription off
  and recording becomes capture-only (no CPU spikes mid-call); the transcript
  is generated the moment you stop.

### Make sense of your meetings

- **AI summaries** *(optional)* — turn a session into clean Markdown notes:
  TL;DR, key points, action items. Choose a style preset or write your own
  prompt.
- **Cross-meeting overview** *(optional)* — a standing, project-grouped rollup
  across recent meetings: what's in progress, what's done, who owns what, and
  your open action items.
- **Chat with your meetings** *(optional)* — ask questions about one meeting
  or across all of them. Answers come only from your transcripts, and it says
  when something wasn't discussed rather than inventing an answer.
- **Per-speaker labels** *(optional)* — split the Others channel into
  individual speakers, rename them, and see them color-coded throughout.
- **Searchable history** — every session is stored locally with full-text
  search across everything you've ever recorded, plus per-session notes.
- **Per-line replay** — hover any transcript line to play back exactly that
  moment of audio; export a session's tracks as one merged WAV.

### Your AI, your choice

The AI features run on a built-in local model out of the box. Prefer your own
setup? Point Zord at any OpenAI-compatible server — LM Studio, Ollama,
llama-server, vLLM — and every AI feature uses it instead. Either way, nothing
goes to a third party you didn't choose.

### Private by design

- Keep or auto-delete recordings on a schedule you set (30-day default).
- Relocate where everything is stored.
- Optional at-rest database encryption (SQLCipher), with the passphrase in
  your OS keychain.
- Export to Markdown, SRT, or JSON whenever you want your data elsewhere — on
  your terms, not as the only way to read it.

### Two ways in

A native desktop app, and a read-only `localhost` web dashboard for reviewing
transcripts in a browser. Both are local-only.

---

## Installing a release

Grab the latest build from the
[releases page](https://github.com/JacobHaig/zord/releases): a `.dmg`
(macOS, Apple Silicon) or a `-setup.exe` / portable `-gui.exe` / `.zip`
(Windows x64). Releases ship every optional engine (Parakeet, diarization,
local + remote AI, Discord).

**The builds are currently unsigned**, so the OS warns on first launch:

- **macOS** — right-click the app → **Open** → **Open** (needed once), or
  `xattr -dr com.apple.quarantine /Applications/ZordGui.app`.
- **Windows** — SmartScreen: **More info → Run anyway**. The `.zip` artifact
  usually skips the prompt entirely (extract, then run).

**Updates.** GitHub builds check the releases page at launch (turn it off in
Settings → About) and show a notice when a newer version exists. On Windows
the portable EXE can **download & install the update in place** from
Settings → About; on macOS the notice links to the download page. Store builds
(Steam / Microsoft Store, when they exist) update through their store and
never self-update.

---

## Building from source

```bash
git clone https://github.com/JacobHaig/zord.git && cd zord
cargo run -p zord-gui --release
```

Prerequisites are just **Rust + CMake + a C/C++ toolchain**. The full guide —
per-platform setup, every optional feature (`parakeet`, `llm-local`,
`llm-remote`, `diarization`, `discord`, `encryption`), the CLI, first-run
permissions, data locations, and troubleshooting — lives in
**[`KICKSTART.md`](KICKSTART.md)**.

---

## Project layout

A Cargo workspace of focused crates:

| Crate | Responsibility |
|---|---|
| `zord-core` | shared types (`Source`, `Segment`, `Session`), version/channel helpers |
| `zord-audio` | resample, voice-activity segmentation, level control, WAV read/write/repair/mix |
| `zord-capture` | audio sources: `Microphone` (cpal) + `SystemAudio` (ScreenCaptureKit / WASAPI, whole-mix or per-app) |
| `zord-transcribe` | `TranscribeBackend` trait; Whisper always, Parakeet under `parakeet`; offline re-transcription; model catalog + downloads |
| `zord-store` | SQLite storage + full-text search; optional SQLCipher encryption |
| `zord-config` | persisted settings, paths, retention, session-folder layout |
| `zord-export` | Markdown / SRT / JSON renderers |
| `zord-summarize` | LLM backend (local llama.cpp under `llama`, OpenAI-compatible client under `remote`) for summaries, compression, and chat |
| `zord-overview` | cross-meeting overview synthesis (compress → group → roll up) |
| `zord-diarize` | per-speaker diarization (sherpa-onnx) under `diarization`; speaker-model catalog |
| `zord-net` | shared, proxy/cert-store-aware HTTP: model downloads, remote LLM, Discord REST, release checks |
| `zord-integrations` | platform integrations (per-participant capture); Discord via `songbird`/`serenity` under `discord` |
| `zord-web` | axum `localhost` review dashboard |
| `zord-app` | the `zord` CLI |
| `zord-gui` | the Dioxus desktop app |

---

## Documentation

| Doc | What's in it |
|---|---|
| [`KICKSTART.md`](KICKSTART.md) | prerequisites, building, running, the CLI, troubleshooting |
| [`docs/PLAN.md`](docs/PLAN.md) | the design, every decision, and the full phase-by-phase history |
| [`docs/discord-integration.md`](docs/discord-integration.md) | the Discord integration: user flow + how it works under the hood |
| [`docs/RELEASE.md`](docs/RELEASE.md) | cutting releases, signing/notarization, distribution channels |
| [`docs/SECURITY.md`](docs/SECURITY.md) | threat model, security review, and posture |
| [`docs/model-licensing.md`](docs/model-licensing.md) | the commercial-licensing audit of every model offered in-app |
| [`docs/diagrams/integrations.md`](docs/diagrams/integrations.md) | ASCII architecture diagrams for the integrations work |
