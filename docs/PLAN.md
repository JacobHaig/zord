# Zord — Local Audio Capture & Transcription

> A fast, self-contained, **fully-local** desktop application that records your
> microphone **and** background/desktop audio (Teams, Zoom, browser, anything),
> then produces an accurate, timestamped, searchable transcript labeled by
> source ("Me" vs "Others"). No cloud, no server. All capture, processing, and
> transcription happen on-device.

---

## 1. Decisions locked in

| Decision | Choice | Rationale |
|---|---|---|
| **Platforms** | macOS (Apple Silicon) **+** Windows | Teams runs on both. macOS shipped first, Windows second. |
| **Distribution** | Native desktop app (Dioxus Desktop) + optional **localhost** web dashboard | A browser sandbox *cannot* capture system audio. Native is the only way to meet the core requirement. Local web UI is a review surface only. |
| **UI framework** | **Dioxus 0.7.x** (Rust) | Current stable (0.7.9, May 2026). Cross-platform desktop via WebView, RSX, hot-reload. |
| **Language** | Rust (entire stack) | Per requirement. One workspace, multiple crates. |
| **Source separation** | Two independent channels: **mic** + **system loopback**, transcribed separately, labeled "Me" / "Others" | Far more reliable than ML speaker diarization. No diarization model needed. |
| **Transcription** | `whisper-rs` 0.16 (whisper.cpp) | Mature, actively maintained, GPU-accelerated (Metal/CUDA/Vulkan). Runs fully local. |
| **Model** | `large-v3-turbo` (quantized) default; configurable | ~95%+ of large-v3 accuracy at 2–5× the speed. English-only build can also use `distil-large-v3` / `*.en` models. |
| **Hardware** | Auto-detect acceleration; model size is a setting | User hardware "varies" — detect Metal/CUDA at runtime, fall back to CPU, recommend a model accordingly. |
| **Mode** | **Batch / near-real-time** (not strictly live) | Accuracy > latency. Transcribe in chunks behind a queue. |
| **Trigger** | **Manual start/stop** for v1 | Predictable and private. Auto-detect meetings is a later phase. |
| **Language scope** | **English** | Use English-tuned models for best speed/accuracy. |
| **Audio retention** | **Setting** — keep audio + transcript by default; toggle + auto-delete-after-N-days | Lets you re-transcribe later with better models; respects disk/privacy. |
| **Post-processing** | Timestamps + full-text search + export (Markdown / SRT / JSON) | AI summaries and custom vocabulary are explicitly **out of v1 scope** (future phase). |

---

## 2. High-level architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                         Dioxus Desktop App (UI)                       │
│   Record button · live level meters · transcript view · search        │
└───────────────┬───────────────────────────────────┬─────────────────┘
                │ (in-process channels / state)       │
        ┌───────▼────────┐                    ┌───────▼────────────┐
        │  Capture layer  │                    │  Local web server   │
        │  (per-OS)       │                    │  axum @ 127.0.0.1   │
        └───────┬────────┘                    │  (review dashboard) │
                │                              └─────────────────────┘
   ┌────────────┴────────────┐
   │ mic stream   sys stream │   each: f32 PCM @ native rate
   └─────┬───────────┬───────┘
         │           │
   ┌─────▼───────────▼─────┐
   │  Resample → 16 kHz mono│  (rubato)
   │  + VAD segmentation    │  (silero/webrtc-vad)
   └───────────┬────────────┘
               │  segments (with wall-clock timestamps)
        ┌──────▼───────┐
        │  Transcribe   │  whisper-rs worker pool (GPU/CPU)
        │  queue/pool   │
        └──────┬───────┘
               │  segment text + word timestamps + source tag
        ┌──────▼───────────────────────┐
        │  Storage  (SQLite + FTS5)     │  transcripts, sessions, segments
        │  + optional WAV on disk       │
        └───────────────────────────────┘
```

### Workspace crate layout

```
zord/
├─ Cargo.toml                 # workspace
├─ crates/
│  ├─ zord-app/               # Dioxus desktop binary (entry point)
│  ├─ zord-ui/                # Dioxus components (shared desktop + web)
│  ├─ zord-capture/           # trait + per-OS backends (mic + system)
│  │   ├─ src/macos.rs        #   screencapturekit
│  │   ├─ src/windows.rs      #   wasapi (loopback) + cpal (mic)
│  │   └─ src/lib.rs          #   AudioSource trait, device enumeration
│  ├─ zord-audio/             # resample, VAD, ring buffers, WAV writer
│  ├─ zord-transcribe/        # whisper-rs wrapper, model mgmt, worker pool
│  ├─ zord-store/             # SQLite schema, FTS5 search, retention policy
│  ├─ zord-web/               # axum localhost dashboard (read-only review)
│  └─ zord-core/              # shared types: Session, Segment, Source, config
└─ docs/
   └─ PLAN.md                 # this file
```

A single `AudioSource` trait abstracts capture so the rest of the app is
OS-agnostic:

```rust
pub enum Source { Microphone, System }

pub trait AudioSource: Send {
    /// Native sample format/rate of this source.
    fn config(&self) -> AudioConfig;
    /// Start delivering f32 PCM frames to `sink` until stopped.
    fn start(&mut self, sink: FrameSink) -> Result<()>;
    fn stop(&mut self);
}
```

---

## 3. The hard parts (gaps) and how we close them

These are the things that sink projects like this. Each is addressed by a
specific phase and mitigation.

### G1 — System ("desktop") audio capture is OS-specific and permissioned
- **macOS:** Use the `screencapturekit` crate (v1.5.0). Captures system audio
  (and mic) via Apple's ScreenCaptureKit on macOS 13+. **Requires the user to
  grant Screen Recording permission** (TCC prompt) the first time, plus
  Microphone permission. App must handle the "permission not yet granted" state
  gracefully and link to System Settings.
- **Windows:** Use the `wasapi` crate for **loopback** capture of the default
  render device, and `cpal`/`wasapi` for the mic. *We deliberately avoid relying
  on `cpal`'s built-in loopback* — it has a history of being removed/flaky
  (RustAudio/cpal issues #251, #476, #516). The `wasapi` crate exposes
  `AUDCLNT_STREAMFLAGS_LOOPBACK` directly and reliably.
- **Mitigation:** Phase 0 is a *capture spike* on each OS before any UI work —
  prove we can write 30s of clean mic + system WAV on both platforms.

### G2 — Two devices = two clocks (drift & alignment)
Mic and system streams run on independent clocks at possibly different sample
rates. Over a long call they drift.
- **Mitigation:** Stamp every captured buffer with a monotonic wall-clock time
  at arrival. Resample both to 16 kHz mono (`rubato`). Align transcript segments
  by their wall-clock timestamps, not by sample count. Interleave the two
  channels' segments into one timeline for the UI.

### G3 — Whisper input requirements
whisper.cpp expects **16 kHz, mono, f32**. Capture is often 44.1/48 kHz stereo.
- **Mitigation:** A fixed resample stage (`rubato`, high-quality sinc) +
  downmix in `zord-audio`. Validate with a known sample.

### G4 — Long recordings: memory & latency
A 1-hour call is huge if buffered in RAM, and you don't want to wait until the
end to transcribe.
- **Mitigation:** Stream PCM to a ring buffer; **VAD-segment** on silence into
  utterance chunks (target 5–30 s). Push chunks to a bounded transcription queue
  consumed by a worker pool. Optionally append raw audio to a WAV on disk as we
  go (if retention is on). This gives near-real-time results without blocking.

### G5 — GPU detection & model selection
Hardware "varies."
- **Mitigation:** At startup detect Metal (macOS) / CUDA (Windows+NVIDIA);
  fall back to CPU. Recommend a default model per detected capability
  (e.g. large-v3-turbo on GPU, small/distil on CPU). Expose model choice in
  Settings. First-run **downloads** the chosen ggml model from Hugging Face to a
  local cache (this is a *model* download, not a server dependency — fully
  offline thereafter).

### G6 — Distribution & signing (the boring blocker)
Unsigned native apps that ask for mic + screen-recording permission are a
terrible UX (Gatekeeper / SmartScreen warnings).
- **macOS:** Bundle via `dx bundle` / `cargo-bundle`; declare
  `NSMicrophoneUsageDescription` and screen-recording entitlements in
  `Info.plist`; **codesign + notarize** for distribution outside the App Store.
- **Windows:** Build an installer (e.g. MSI via `cargo-wix` or NSIS);
  **Authenticode sign** to avoid SmartScreen.
- **Mitigation:** Phase 6 owns this; document the signing steps and provide a
  GitHub Actions release workflow that builds, signs, and attaches artifacts.

### G7 — Bundling the native whisper library
`whisper-rs` compiles whisper.cpp (and GPU kernels) via its build script.
- **Mitigation:** Pin `whisper-rs`; build with `metal` feature on macOS and
  `cuda` feature (optional, behind a build flag) on Windows. Provide a CPU-only
  fallback binary so users without CUDA still get a working release.

### G8 — Privacy & data at rest
Everything is local, but transcripts/audio are sensitive.
- **Mitigation:** Store under the OS app-data dir. Offer optional
  encryption-at-rest (SQLCipher) and a clear retention policy (auto-delete audio
  after N days; transcripts kept). A visible "all-local, nothing leaves this
  machine" statement + a one-click "delete this session."

### G9 — Permission UX & failure states
Capture can fail: permission denied, device unplugged, no loopback device.
- **Mitigation:** Explicit app states (`NeedsPermission`, `NoSystemDevice`,
  `Recording`, `Transcribing`, `Idle`) surfaced in the UI with actionable copy.

---

## 4. Core data model (`zord-store`)

```sql
CREATE TABLE sessions (
  id          TEXT PRIMARY KEY,        -- uuid
  started_at  INTEGER NOT NULL,        -- unix ms
  ended_at    INTEGER,
  title       TEXT,
  audio_path  TEXT,                    -- nullable if discarded
  model       TEXT NOT NULL            -- which whisper model produced it
);

CREATE TABLE segments (
  id          INTEGER PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  source      TEXT NOT NULL,           -- 'me' | 'others'
  t_start_ms  INTEGER NOT NULL,        -- relative to session start
  t_end_ms    INTEGER NOT NULL,
  text        TEXT NOT NULL,
  words_json  TEXT                     -- optional word-level timestamps
);

-- Full-text search over segment text
CREATE VIRTUAL TABLE segments_fts USING fts5(
  text, content='segments', content_rowid='id'
);
```

Export renders from these tables: **Markdown** (readable transcript), **SRT**
(subtitles, from timestamps), **JSON** (full fidelity incl. word timings).

---

## 5. Recommended crate stack

| Concern | Crate(s) | Notes |
|---|---|---|
| UI | `dioxus` 0.7.x (`desktop` feature) | WebView-based desktop. |
| Local web dashboard | `axum`, `tower-http` | Bind `127.0.0.1` only. |
| Mic capture | `cpal` | Cross-platform input. |
| System capture (macOS) | `screencapturekit` 1.5 | System + mic, macOS 13+. |
| System capture (Windows) | `wasapi` | Reliable loopback flag. |
| Resampling | `rubato` | High-quality sinc → 16 kHz mono. |
| VAD | `voice_activity_detector` (silero) or `webrtc-vad` | Silence-based segmentation. |
| Transcription | `whisper-rs` 0.16 + ggml model | Features: `metal` / `cuda`. |
| Storage | `rusqlite` (bundled, FTS5) or `sqlx` + SQLite; optional `sqlcipher` | Local DB + search. |
| WAV I/O | `hound` | Write/read raw audio. |
| Async runtime | `tokio` | Queues, web server, workers. |
| Errors/logging | `thiserror`, `tracing` | |
| Packaging | `dx bundle` / `cargo-bundle`, `cargo-wix` | macOS .app / Windows MSI. |

> Validate exact versions with `cargo add` at implementation time; the build
> script for `whisper-rs` needs a C/C++ toolchain + CMake on the build machine.

---

## 6. Phased delivery

Each phase ends with a **demoable, testable** result. macOS is the lead
platform; Windows-specific capture lands in Phase 2b.

### Phase 0 — De-risking spikes (1–2 days)  ⚠️ do this first
- [ ] Workspace skeleton + CI (build on macOS & Windows).
- [ ] **macOS capture spike:** record 30 s of mic + system audio to two WAVs via
      `screencapturekit`; confirm permission prompts work.
- [ ] **Windows capture spike:** same, via `wasapi` loopback + mic.
- [ ] **whisper spike:** transcribe a known WAV with `whisper-rs`, GPU + CPU.
- **Exit criteria:** clean WAVs on both OSes + a correct transcript of a test clip.
  *If a capture path is blocked, we learn it now, not in month two.*

### Phase 1 — Single-channel end-to-end (mic only)  ✅ DONE
- [x] `zord-audio`: resample to 16 kHz mono (rubato) + energy/VAD segmentation.
- [x] `zord-transcribe`: whisper-rs (Metal), first-run model download/cache.
- [x] `zord-store`: SQLite schema + insert + FTS5 search.
- [x] CLI trigger (`zord record` live mic; `zord file` for deterministic test).
- **Exit criteria MET:** verified against canonical `jfk.wav` — accurate
  transcript, correct timestamps, stored in SQLite, Metal GPU confirmed, FTS5
  search returns correct segments. Live mic path (`zord record`) uses the
  identical pipeline; needs an interactive run (macOS mic-permission prompt).

### Phase 2 — Dual-channel capture + sync  🟡 macOS impl done; live-verify pending
- **2a (macOS):** ✅ `zord-capture` crate — `Microphone` (cpal) + `SystemAudio`
  (ScreenCaptureKit 6.1). Both emit mono f32; system audio via `SCStream` with
  `captures_audio`. Graceful degradation if Screen Recording permission absent.
- [x] Dual-channel pipeline: per-channel resample+VAD, fan-in to one transcribe
  stage, per-channel first-frame base offset → single interleaved timeline.
- [x] Builds + runs; mic-only fallback path verified (clean degradation message).
- [ ] **Live verification (user step):** grant Screen Recording permission, play
  audio while speaking, confirm Me/Others attribution. (Requires TCC grant +
  real audio — can't be automated.)
- **2b (Windows):** ✅ implemented. Mic via `cpal` (already cross-platform);
  system audio via the `wasapi` crate's render-device loopback
  (`AUDCLNT_STREAMFLAGS_LOOPBACK`) on a dedicated COM thread, emitting mono f32
  like macOS. Whisper runs CPU-only on Windows (no Metal). **Verified by
  `cargo check --target x86_64-pc-windows-msvc` (type-checks clean)**; a
  `windows-latest` CI job does the real compile/link/bundle (`.msi`). Runtime
  verification needs a Windows host (no host in this build env).
- **Build note:** macOS 13 deployment target + a Swift-lib search path are set in
  `.cargo/config.toml` for the ScreenCaptureKit Swift bridge (CLT-only setups).

### Phase 3 — Dioxus desktop UI  ✅ DONE (built + launches)
- [x] `zord-gui` crate (Dioxus 0.7 desktop). Threaded `Engine`: a control thread
  owns the `!Send` capture streams; a db thread answers queries; both push
  events to the UI over a tokio channel, drained into signals by a `spawn`ed task.
- [x] Record/Stop control, status indicator (idle/preparing/downloading/recording),
  live Me/Others level meters.
- [x] Session sidebar + transcript view (Me/Others colour + timestamps); click a
  session to load it.
- [x] **FTS5 search** box across all sessions (sanitized MATCH query).
- [x] Permission/error states (G9): degradation notice banner, error status.
- **Exit criteria MET (build/launch):** compiles, launches a window, event loop
  runs, no panic. Live recording behaviour shares Phase 1/2 verified pipeline;
  full click-through with real audio is the same interactive step as Phase 2.
- CLI (`zord`) retained alongside the GUI.

### Phase 4 — Export + local web dashboard  ✅ DONE (verified)
- [x] `zord-export` crate: Markdown / SRT / JSON renderers (pure functions).
- [x] CLI `zord export <id> --format md|srt|json [--out]`.
- [x] `zord-web` crate: axum dashboard bound to `127.0.0.1`; routes `/`,
      `/api/sessions`, `/api/session/:id`, `/api/search?q=`; DB reads via
      `spawn_blocking`. CLI `zord serve [--port]`.
- [x] GUI export buttons (MD/SRT/JSON) when viewing a session → writes to the
      app data `exports/` dir, shows a notice.
- **Exit criteria MET:** exported jfk session to all three formats (valid SRT
  timestamps, clean MD, full JSON); launched `zord serve` and curled every
  endpoint successfully; GUI builds with export bar.

### Phase 5 — Settings, retention & polish  ✅ DONE (encryption deferred)
- [x] `zord-config` crate: persisted `Settings` (JSON in app data dir) + path
      helpers (storage_dir / db / exports / audio); `apply_retention()`.
- [x] Settings: model choice, audio-retention toggle, auto-delete-after-N-days,
      input-device selection, storage location override.
- [x] GUI settings panel (gear button): model + mic dropdowns, keep-audio toggle,
      auto-delete days; persists on change.
- [x] Audio retention: per-channel WAVs written when keep-audio is on; old audio
      auto-deleted on startup per `auto_delete_days`.
- [x] Re-transcribe a kept session with a different model — `zord retranscribe
      <id> --model X` (verified: regenerated the jfk transcript, bumped the
      stored model).
- [~] **Encryption-at-rest (SQLCipher): DEFERRED** to its own pass. Rationale:
      requires the `bundled-sqlcipher` feature (touches every DB open across
      CLI/GUI/web), a passphrase-entry UX + key PRAGMA per connection, migration
      of the existing plaintext DB, and carries irreversible data-loss risk on a
      lost passphrase. Not a safe tail-end add.
- **Exit criteria MET** (minus encryption): configurable, retention works,
  robust to missing config/audio.

### Phase 6 — Packaging & distribution  🟡 macOS bundle done; signing = user step
- [x] `dx bundle` produces `ZordGui.app` + a `.dmg` (Apple Silicon, macOS 13+).
- [x] Complete `Info.plist` (id `io.zord.zord`, mic usage string, exec/version);
      `entitlements.plist` (audio-input + JIT for the webview); hardened runtime.
      Verified: bundle launches and registers as `io.zord.zord` (so TCC grants
      attach correctly); `plutil` lint OK.
- [x] `build.rs`: links `libclang_rt.osx` (resolved via `clang
      -print-resource-dir`) so the explicit-`--target` release link finds
      `___isPlatformVersionAtLeast` (used by ggml-metal's `@available`).
- [x] GitHub Actions `release.yml`: on `v*` tag, builds the macOS bundle and
      attaches it to a Release; codesign + notarize steps run only if signing
      secrets are set. `docs/RELEASE.md` documents the Apple-account steps.
- [ ] **Codesign + notarize (user step):** needs your Apple Developer ID cert +
      credentials (can't be done in this environment). Steps + CI secrets are
      documented in `docs/RELEASE.md`.
- [ ] Windows MSI / Authenticode — tied to Phase 2b (no Windows host yet).
- [ ] App icon — add an icon set + reference in `Dioxus.toml` before public release.
- **Exit criteria (build) MET:** a runnable, correctly-identified `.app`/`.dmg`
  is produced locally and in CI. Signing is a documented user step.

### Phase 9 — Settings overhaul + full model management  ✅ DONE (built + launches)
- [x] Replaced the small top dropdown with a **full-screen settings overlay**
  (gear opens, ✕ closes): Models, Audio input, Recording & retention, About.
- [x] Expanded Whisper catalog to 7 models (tiny.en → large-v3) with size +
  description; `is_downloaded` / `delete_model` helpers.
- [x] **Model management** in the overlay: every model is listed; not-downloaded
  ones show **Download** (with a live progress bar), downloaded ones show
  **Select** / **Delete** (can't delete the active one). Driven by a dedicated
  engine **model worker thread** (List/Download/Delete + `ModelProgress` events).
- [x] Mic device dropdown, keep-audio toggle, auto-delete-days — all in the
  overlay, persisted to config.
- **Next (Phase 10):** Parakeet via `sherpa-rs` behind a transcription-backend
  trait (lets the catalog include non-Whisper engines).

### Phase 10 — Parakeet / multi-backend transcription  ✅ DONE (feature build verified)
- [x] `TranscribeBackend` trait; Whisper moved to `WhisperBackend`; `Transcriber`
  dispatches by `ModelId::engine()`. (Phase 10a)
- [x] `ParakeetBackend` via the `sherpa-onnx` crate (offline `nemo_transducer`),
  behind the **`parakeet` cargo feature** so the default build stays lean/green.
- [x] Catalog entry `parakeet-tdt-0.6b-v3` (25 languages); `ensure_model`
  downloads + extracts the sherpa-onnx `.tar.bz2`; `is_downloaded`/`delete_model`
  are directory-aware for Parakeet. Listed in the settings UI only with the feature.
- [x] Passthrough `parakeet` feature on `zord-app` + `zord-gui`.
- **Verified:** default build green + jfk works through the trait; `--features
  parakeet` **compiles & links** for zord-transcribe, the CLI, and the GUI
  (sherpa-onnx build script fetches prebuilt libs). Runtime Parakeet inference
  (download the ~650 MB model + real audio) is a user step — can't be exercised
  in this build env.
- Build it: `cargo run -p zord-gui --features parakeet` → the settings overlay
  lists Parakeet to download/select.

### Phase 7/Future — backlog (explicitly out of v1 scope)
- Auto-detect active meeting apps (Teams/Zoom) → prompt/auto-start.
- Local AI summaries / action items via a local LLM (llama.cpp).
- Custom vocabulary (whisper `initial_prompt` / hotword biasing).
- Per-speaker diarization within the "Others" channel.
- SQLCipher at-rest encryption; app icon.

---

## 7. Open questions to revisit during build
1. ~~**macOS minimum version**~~ — **DECIDED:** target whatever runs on Apple
   Silicon M1–M5. We'll set the deployment target to macOS 13 (the first version
   with ScreenCaptureKit system-audio support that all M-series machines run),
   and use 14/15 APIs only behind availability checks if ever needed.
2. **Windows mic + loopback device pairing** — handle multiple output devices
   (which one is "the call"?). Default render device for v1.
3. ~~**Model download UX**~~ — **DECIDED:** always **download on first run**
   (with progress UI); never embed the model in the application binary/installer.
   Cached locally thereafter → fully offline.
4. **CUDA in releases** — ship CUDA builds, or CPU-only + "build from source for
   GPU"? CUDA build matrix adds CI complexity.

---

## 8. Sources (research, May 2026)
- whisper-rs (bindings, GPU features): https://github.com/tazz4843/whisper-rs · https://crates.io/crates/whisper-rs
- screencapturekit crate (macOS system+mic audio): https://crates.io/crates/screencapturekit · https://github.com/svtlabs/screencapturekit-rs
- cpal & WASAPI loopback caveats: https://github.com/RustAudio/cpal · issues #251/#476/#516
- ruhear (evaluated, not adopted): https://github.com/aizcutei/ruhear
- Dioxus releases (0.7.x current): https://github.com/dioxuslabs/dioxus/releases · https://docs.rs/crate/dioxus/latest
- Whisper large-v3-turbo accuracy/speed: https://huggingface.co/openai/whisper-large-v3-turbo · https://whispernotes.app/blog/introducing-whisper-large-v3-turbo
