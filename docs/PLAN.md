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

### Inter-phase UX increments (shipped between numbered phases)
- ✅ Dioxus signal best-practices pass (pass signals to children; fewer re-renders).
- ✅ Export **Reveal in Finder/Explorer** + **Open in editor** buttons (`osutil`).
- ✅ **dB-scale level meters** with time-based attack/release (consistent mic vs
  system behaviour).

---

## 7. Backlog — planned future phases

Done **one at a time**, each a sizable, self-contained phase with its own
verification. Order is a suggestion, not fixed.

### Phase 11 — SQLCipher at-rest encryption  ✅ DONE (feature-gated, verified)
- [x] `encryption` feature (`rusqlite/bundled-sqlcipher-vendored-openssl`),
  feature-gated so the default build + CI stay lean.
- [x] Process-wide key (`set_db_key`) applied as `PRAGMA key` on every
  `Store::open`; wrong/missing key fails clearly. `encrypt_existing` /
  `decrypt_existing` migrate via `sqlcipher_export` (with backups); `is_encrypted`
  detection. (11a — roundtrip test passes.)
- [x] CLI: `resolve_db` unlocks via keychain → `ZORD_PASSPHRASE` → hidden prompt;
  `zord encrypt [--remember]` / `zord decrypt`. (11b — full encrypt/read/decrypt
  cycle verified at runtime.)
- [x] Config: `encrypted` + `encrypt_pending`/`decrypt_pending`; optional
  `keychain` module (keyring). GUI: unlock screen at launch (keychain
  auto-unlock or passphrase prompt + remember); Enable/Disable in settings that
  migrate **on next launch** (safe — no live-DB migration). (11c — builds + launches.)
- **Passphrase UX:** set-once + optional OS keychain (chosen). Runtime: store
  roundtrip + CLI cycle verified here; GUI unlock/enable exercised by build+launch
  (full click-through is a user step).

### Phase 12 — App icon & brand polish  ✅ DONE
- [x] Icon rendered via `tools/make_icon.swift` (CoreGraphics) — brand meter
  bars (blue/orange) on a dark rounded square. Assets in `crates/zord-gui/icons/`:
  `icon.icns` (macOS), `icon.ico` (Windows, PNG-in-ICO), `icon.png` (1024) +
  `icon-256.png` (runtime).
- [x] Wired: `Dioxus.toml [bundle] icon`; bundle embeds `ZordGui.icns` with
  `CFBundleIconFile` set in the (custom) Info.plist; runtime window/dock icon via
  `dioxus::desktop::icon_from_memory`.
- [x] Fixed the release CI `.app` glob (dx emits `ZordGui.app`, not `Zord.app`).
- Note: the bundle **displays** as "Zord" (CFBundleName/DisplayName); the folder
  is still `ZordGui.app` (dx derives it from the package name). Cosmetic only.

### Phase 13 — Local AI summaries / action items  ✅ DONE (feature build verified)
- [x] `zord-summarize` crate: `llama` feature pulls `llama-cpp-2` (Metal on
  macOS). `Summarizer` runs one chat completion (apply_chat_template + greedy
  decode) → Markdown notes (TL;DR / key points / action items).
  `ensure_summary_model` downloads Qwen2.5-3B-Instruct Q4_K_M on demand. (13a)
- [x] `zord-store`: `summary` column + `set_summary`/`get_summary`. CLI
  `zord summarize <id>`. GUI: ✨ Summarize button in the session toolbar →
  engine summarize-worker thread → persisted + shown in a Summary panel; loading
  a session restores its saved summary. (13b)
- [x] Passthrough `summaries` feature on `zord-app` + `zord-gui`; default build
  leaves llama.cpp out and stays lean.
- **Verified:** default green; `--features summaries` compiles + links + launches
  (CLI & GUI). Runtime summarization needs the ~2 GB model + is slow (user step).

### Phase 14 — UX polish pass  ✅ DONE
- [x] Session management (14a): human titles (relative time) + meta
  (model · duration); inline **rename** (Enter/Esc); per-row **delete** with a
  confirm dialog (returns to Live if the open session is deleted).
  zord-store `set_session_title`/`delete_session`; engine `Rename`/`DeleteSession`.
- [x] Transcript niceties (14b): **Copy** transcript + **Copy** summary
  (arboard); **recording timer** in the topbar; **auto-scroll** to latest while
  recording; **auto-dismissing** notices (+ manual ✕).
- Built + launches; full workspace compiles.
- Deferred from the original list (fine to revisit later): global keyboard
  shortcuts; first-run onboarding hint.

### Phase 15 — Configuration & use-case polish  ✅ DONE
Closed gaps from the post-14 review (verified: default + feature builds, GUI launches):
- [x] Summary model selection (Qwen2.5 1.5B/3B/7B) + preset styles
  (balanced/bullets/exec/actions) **and** editable prompt with reset — in
  settings, used by CLI + GUI.
- [x] Capture mode (mic/system/both) — settings dropdown + CLI `--capture`;
  engine + pipeline start only the chosen sources.
- [x] Inline transcript editing (double-click a line) → `update_segment_text`
  (FTS-synced); `Segment.id` exposed.
- [x] "Open data folder" button; summary section gated under `summaries`.

Original scope notes:
- **Summary model selection** — a small catalog of summary LLMs (e.g.
  Qwen2.5 1.5B / 3B / 7B Instruct, Q4_K_M); pick + download/select in settings.
  `Summarizer` + `ensure_summary_model` become model-parameterized.
- **Summary prompt customization** — preset styles (bullets / exec brief /
  action-items / balanced) **and** a freeform editable prompt with reset.
  `Summarizer::summarize(transcript, system_prompt)`; config stores
  `summary_model`, `summary_preset`, optional `summary_prompt` override.
- **Capture mode** — record mic-only / system-only / both, in settings; engine
  honors it (skip starting a source).
- **Transcript editing** — inline-edit a transcript line in the GUI; persists via
  `Store::update_segment_text` (FTS stays in sync via the existing UPDATE
  trigger). Requires exposing a segment `id` on `Segment` (serde-skipped when
  absent).
- Freebies if cheap: an **"Open data folder"** button; show summary/Parakeet
  models in the managed list.
Done in sub-steps (config+store → summarize params → GUI), feature-aware
(summary bits under `summaries`). Not started.

### Phase 16 — Per-speaker diarization (within "Others") ✅
Distinguish individual speakers inside the system channel, turning **Others →
Speaker 1/2/3**. Channel separation already covers Me-vs-Others; this layers
identity *within* the Others track. Feature-gated (`diarization`) so the default
build stays lean; reuses the already-resolved `sherpa-onnx` crate (no new heavy
dep).

**Architecture — offline-first.** Diarization = embed each speech chunk +
**cluster** embeddings into speakers. Clustering is inherently *global* (you must
see every speaker, and their count is unknown until the end), so the accurate,
source-of-truth pass is **offline**, run after recording. It also avoids
competing with ASR for CPU/Metal during the call.
- `zord-diarize` crate: pyannote segmentation + speaker-embedding models
  (TitaNet small/large, WeSpeaker CAM++) downloaded/selected/deleted via the same
  model-management UI as Whisper/summary models. `Diarizer` wraps
  `OfflineSpeakerDiarization`; `LiveLabeler` wraps `SpeakerEmbeddingManager`.
- The "Others" 16 kHz mono track is written to a WAV during recording (a temp
  file when audio retention is off, deleted after the pass), then diarized and
  mapped onto stored segments by **max temporal overlap**.
- **Triggers:** auto at stop *and* an on-demand "Identify speakers" button /
  `zord diarize <session>` CLI (on-demand needs retained audio).
- **Live mode (optional, off by default):** `diarize_live` shows rough
  provisional labels during recording via incremental embedding-match; these are
  always replaced by the offline pass at stop. Gated by a settings toggle to
  spare constrained hardware.
- Storage: nullable `speaker` column on segments + a per-session `speaker_names`
  table (rename "Speaker 1" → "Alex"). Labels flow into the transcript view
  (per-speaker colors), search, and MD/SRT/JSON exports.

Done in sub-steps: 16a config/core/store foundations → 16b `zord-diarize` crate →
16c engine offline pass + on-demand worker → 16d live labeling → 16e GUI → 16f
exports + CLI + docs.

> **Runtime note:** the sherpa-onnx model download URLs and GPU/ONNX inference
> are wired but not exercised headlessly — first-run download + accuracy need a
> manual check on-device (see `verification-limits`).

### Phase 17 — Diagnostics, on-disk shortcuts & manual-download fallback ✅
Make the app's on-disk locations discoverable, make errors easy to grab, and
make the **manual model-download workaround first-class** — because dropping a
file into the `models/` folder works on *any* network (proxy, HTTPS-inspection,
air-gapped), unlike the automatic downloader. Prioritized **above** Phase 18:
this is the network-agnostic safety net, validated in practice (a user behind a
corporate proxy fetched the model in a browser and dropped it in — seamless).

- **Settings "Open…" shortcuts:** reveal each of — **models** folder, **data**
  folder (config/db/audio/exports; already has an "Open data folder" button to
  build on), **logs** folder, the **config.json** file, and the **database**
  file. Reuse the existing `osutil::open_folder` / `reveal_in_file_manager` /
  `open_in_editor` helpers.
- **Graceful download-failure fallback:** when an in-app model download fails,
  don't just show an error — surface the **exact download URL** (one-click copy)
  and an **"Open models folder"** button, so the user can grab it in a browser
  (which uses the proxy) and drop it in. Document the expected folder/layout per
  model. This is the highest-value bit and works regardless of network policy.
- **File logging (prerequisite):** today `tracing` only writes to stderr, so a
  bundled GUI app leaves no log behind. Add a rotating file sink (e.g.
  `tracing-appender`) writing to `<data>/logs/zord.log` alongside stderr, so
  errors (failed model downloads, capture/transcribe/diarize failures, etc.)
  persist. Respect the same `storage_dir` relocation as the rest of the data.
- **Copy affordance:** a "Open log" (in editor) and/or "Copy last error" button
  so users can paste diagnostics into a bug report without hunting for the file.
- Keep it lean: no new runtime deps beyond a small log-rotation crate; pure UI +
  logging plumbing, no feature gate.

**Done.** Settings → "Files & folders" reveals models / data / logs / config /
database; "Open log" + "Copy recent log" for diagnostics; file logging to
`<app-data>/logs/zord.log` (via `tracing-appender`, alongside stderr); and on a
failed model download the settings panel shows the direct URL(s) (copy / open in
browser) + "Open models folder". Model `urls` are carried in the catalog
(`ModelInfo.urls`); engine emits `Event::DownloadFailed`.

### Phase 18 — Proxy-aware / resilient downloads ✅
The automatic counterpart to Phase 17's manual fallback. All model downloads now
go through a shared **`zord-net`** crate (`download_to_file`) that:
- uses the **OS certificate store** via **native-tls** (Windows schannel / macOS
  Secure Transport) instead of ureq's bundled Mozilla roots — so corporate
  **HTTPS-inspection** root CAs are trusted, like the browser (the most likely
  cause of in-app downloads failing while the browser works);
- honors an explicit **proxy** from `HTTPS_PROXY`/`HTTP_PROXY`/`ALL_PROXY` env
  vars; and
- retries transient failures (3×) and streams atomically (`.partial` + rename).
`zord-transcribe` / `zord-summarize` / `zord-diarize` dropped their own `ureq`
and call `zord_net::download_to_file`. Verified with an (ignored) native-tls
download test.

> Not covered: a **PAC/WPAD or Windows-registry (WinINET) system proxy** with no
> env var set isn't auto-detected — the Phase 17 manual browser-download fallback
> still covers that. (Possible follow-up: read the WinINET system proxy on
> Windows.)

### Phase 19 — Flexible model sourcing (no-HuggingFace) ✅
For users who can't reach HuggingFace (Whisper ggml + Qwen GGUFs live there) but
*can* reach GitHub (Parakeet + diarization models do):
- **Custom summary GGUF:** any `.gguf` dropped into the models folder is scanned
  and appears in Settings → Summaries as a selectable "Custom GGUF" model
  (`zord_summarize::list_custom_models` / `custom_model_path` /
  `delete_custom_model`). The summarizer + CLI resolve a selected id as either a
  built-in catalog model (download) or a local custom file — fully source-
  agnostic, so a model obtained through any channel works. No download needed.
- **More GitHub diarization models:** added 3D-Speaker CAM++ and WeSpeaker
  ResNet34 embedding models (sherpa-onnx GitHub release) to the catalog.
- **Re-diarize with a different model:** on-demand diarization re-reads the
  session's "Others" WAV, so it only worked when audio was retained. Added a
  `diarize_keep_audio` opt-in (Settings → Speakers) that keeps just the Others
  track (even with Keep-audio off) so "Identify speakers" can be re-run later
  with a bigger/different model. Without it, the on-demand notice now explains
  how to enable it. Re-diarization re-reads the original Others WAV and
  re-clusters from scratch (`clear_speakers` + reassign) — never builds on a
  prior pass.
- **Expected-speaker-count control:** `diarize_num_speakers` (0 = auto) forces a
  fixed speaker count. The auto-clustering can over-split a noisy meeting *mix*
  (the Others channel is the call's compressed/echo-cancelled output) into far
  too many "speakers" (e.g. 80 for a 10-person call); pinning the headcount fixes
  it deterministically. Wired into GUI + engine + `zord diarize`.
- Transcription is already GitHub-sourced via **Parakeet** (Whisper is the
  HF one to skip on HF-blocked networks).

Note: GGUF LLMs are HF-centric, so there's no good *catalog* of GitHub-hosted
summary models — the custom-GGUF drop-in is the intended path there.

### Phase 20 — Auto meeting title (pending) ⭐ next
After a recording is summarized (or at stop), make one small LLM call to generate
a concise title from the transcript/summary and set it as the session title —
today sessions default to `sess-<timestamp>` until manually renamed, like how
Claude/ChatGPT auto-title a conversation so it's findable later.
- Reuse the loaded summary model (`summaries` feature); a dedicated short "title"
  prompt (≤8 words, no quotes/punctuation). Falls back gracefully (keeps the
  timestamp id) when summaries aren't built/available.
- Only auto-set when the user hasn't already named the session; never overwrite a
  manual title. Wire into the summarize worker (GUI) + `zord summarize` (print/set
  title) and re-run path.
- Cheap: a single short generation; no new deps, no feature beyond `summaries`.

### Phase 21 — Diarization tuning (Sortformer found infeasible) 🟡
Goal was to fix over-splitting (a 10-person call → ~80 speakers) with a stronger
model. **Sortformer was investigated and ruled out** (June 2026):
- ONNX **export is broken** (NVIDIA-NeMo issue #15077, unresolved — dynamic
  slicing incompatible with ONNX), so there's no ONNX model to run via sherpa /
  onnxruntime;
- the models are PyTorch/NeMo on **HuggingFace** (which HF-blocked users can't
  reach anyway), and embedding a Torch runtime in the desktop app is a non-starter.
So sherpa-onnx stays the engine (pyannote-seg + embedding + fast clustering).

Shipped the tractable levers instead — full manual control over the clustering:
- `diarize_num_speakers` (Phase 19) — pin the exact headcount (deterministic fix).
- `diarize_threshold` (0.1–0.95, default 0.5) — clustering granularity when count
  is auto: lower splits into more speakers, higher merges into fewer. Settings →
  Speakers, wired into engine + `zord diarize`.
Future option if ever needed: speech-separation-guided diarization, or revisit
Sortformer if/when a working ONNX export lands.

> **Researched June 2026 — decisions:**
> - **Teams real speaker names (Graph `callTranscript`)** — **DECLINED**: no
>   tenant access/authorization available to the user. (Per-participant audio
>   would need a Graph media **bot** joining the call — also rejected.) Kept in
>   the `teams-audio-options` memory in case access changes.
> - **Audio playback + click-to-seek transcript** — nice-to-have; **kept as a
>   note, not a planned phase** for now.
> - Smarter notes + chat-with-meeting → promoted to Phase 23 below.

### Phase 22 — Non-HuggingFace model sources ✅ (ModelScope mirror + Ollama in-app)
For networks that block HuggingFace (where the Whisper ggml + Qwen GGUFs live).
Two reliable non-HF sources verified June 2026:
- **ModelScope** (`modelscope.cn`) ✅ — mirrors the Qwen GGUFs at
  `…/resolve/master/<same-filename>` (browser-pasteable). Because the filename
  matches the built-in model, a manual browser-download dropped into the models
  folder is recognized as that built-in model. Wired: `SummaryModel::mirror_url`
  is included in `ModelInfo.urls`, so the download-failure fallback now shows a
  `modelscope.cn` link alongside the HF one — the user fetches it in the browser
  (which uses their proxy) and drops it in. This is the path for proxy/browser-
  only networks.
- **Ollama registry** (`registry.ollama.ai`) ✅ — one-click in-app download,
  using Ollama purely as a model **CDN** (no Ollama install/daemon/engine). For a
  curated model we GET `/v2/library/<repo>/manifests/<tag>`, take the
  `application/vnd.ollama.image.model` layer digest, then GET `/blobs/<digest>`
  (a standard GGUF) and run it via the same llama.cpp path. `zord-net::
  download_ollama_model` (manifest parse + blob fetch); `zord-summarize` exposes a
  small catalog (qwen2.5 3b/1.5b, llama3.2 3b, phi3.5) shown in the Summaries
  list. Reaches the registry through the Phase 18 OS-cert-store + proxy agent, so
  it works on direct-allowed networks; proxy-only-via-browser users still use the
  ModelScope link.

### Phase 23 — Cross-meeting synthesis ("Overview") ⭐ next — major
The headline uplift: a standing, holistic picture across the **last ~30–50
meetings** — per-project state, what's pending, what's accomplished, who owns
what, open questions — oriented around the user ("Me"). So when asked "where's
project X?", the user reads off a current, faithful rollup.

**Architecture — compress, then synthesize (NOT one giant raw context).**
50 raw meetings ≈ 400–650K tokens — far beyond any practical local/CPU context.
So compress first:
1. **Compress (per meeting, once, cached):** the LLM condenses a meeting into a
   token-minimal, **free-form dense prose** representation that preserves the
   facts — projects + current state, action items (owner → what → status), what
   was completed, decisions, open questions — terse, low/no formatting, written
   **model-to-model** (not for display). ~300–800 tokens vs 8–13K raw. Stored on
   the session; exposed via a **"Compress"** button and **"Copy compressed"**
   (lazily generated if it doesn't exist). The compression is *working memory*,
   not the record — the full transcript stays for drill-down + citations.
2. **Synthesize (Overview):** feed the stored compressions (lazily compressing any
   missing, in the background) into the overview model in **one pass** → a
   holistic, project-grouped rollup. The context window is **configurable**
   (default ~32K; can raise to 64–128K). RAM is the limit (KV cache), and on a
   16 GB / CPU laptop the **3B model** is the sweet spot: ~6 GB at 64K, ~9 GB at
   128K (vs 7B which is tight at 64K, risky at 128K). The model is loaded only for
   the background pass then unloaded, so context costs RAM only during the run.
   The real cost is **CPU prefill time** — tens of minutes at 64–100K — which is
   fine for background churn. Future lever: KV-cache quantization (q8) ~halves KV.
   **Fallback at scale** (exceeding the chosen context): hierarchical — group by
   project and compress-the-compressions before the final pass.
3. **Overview output** = per-project rollups (state / pending / done / owners /
   unknowns) + a pinned **"My open action items"**.

**Decisions (locked):**
- **Compression format:** **free-form dense prose** (max compression, LLM-to-LLM).
- **UI:** a dedicated full **Overview view** (third top-level mode beside
  live/session), opened via a "📊 Overview" button above the session list;
  project list → expand for state/pending/done/owners/open-questions; pinned "My
  action items"; refresh + "last updated"; items cite their source meeting.
- **Projects:** **LLM auto-detects + names** topics within the synthesis pass,
  with normalization to merge fuzzy/duplicate names.

**Gaps to handle:** **context window** — the summarizer hard-caps `N_CTX = 8192`
and truncates input. Make context **configurable** for both compress (≥16K to
ingest a full ~1 hr meeting) and synthesis (default 32K, up to 64–128K). Pick a
default that's safe on 16 GB and warn that 64K+ wants the 3B model; model must
support the context (Qwen2.5 does, to 128K). Loaded only during the background
run, then unloaded. Compression is **lossy** → keep full transcript + cite
sources. Faithful, non-editorializing compress prompt. Topic normalization.
Owner attribution leans on diarization+names ("Me" always known). First-run
compute over the backlog (background, incremental, progress). Recency weighting +
drop closed items.

**Sub-steps:**
- **23a** — ✅ **done.** Per-meeting **compress** (free-form dense prose) +
  storage + the Compress / Copy-compressed buttons; on-demand generation.
  - `zord-summarize`: `GenOpts` (n_ctx / max_new_tokens / char budget) +
    `generate()`; `summarize()` is now a thin wrapper (8K ctx) and `compress(n_ctx)`
    runs the dense-prose pass at a **configurable** context (clamped 8K–128K).
  - `zord-config`: `compress_prompt()` (faithful, machine-oriented, no formatting)
    + `compress_ctx` setting (default 16K, editable in Settings → Summaries).
  - `zord-store`: `compressed TEXT` column (parallel to `summary`) +
    `set_compressed` / `get_compressed` (ALTER migration).
  - GUI: 🗜 **Compress** button in the session toolbar, a collapsible
    **Compressed (dense)** panel with Show/Hide + Copy; `Event::Compressed` is
    emitted on session load. CLI: `zord compress <id>`.
- **23b** — ✅ **done.** Cross-meeting **Overview synthesis** in the new
  `zord-overview` crate (feature `llama`), shared by CLI + (soon) GUI.
  - `synthesize(db, settings, progress)`: loads the summary model once; gathers
    the most recent `overview_max_meetings` sessions newest-first, reusing each
    stored compression and **lazily generating + persisting** any missing;
    assembles them (each headed by `YYYY-MM-DD · title`); one-pass synthesis at
    `overview_ctx` (default 32K). **Hierarchical fallback** when they overflow:
    greedily pack into groups, compress-the-compressions, then a recency trim
    (logged, not silent) if still over budget.
  - `zord-config`: `overview_prompt()` (project-grouped, "My open action items"
    first, faithful + cites source meetings) + `overview_ctx` (32K) /
    `overview_max_meetings` (50) settings.
  - `zord-store`: generic `app_meta(key,value,updated_at)` table +
    `set_meta`/`get_meta`; the rollup is stored under `overview` (+ meeting count).
  - `zord-summarize`: `count_tokens()` for budgeting + `GenOpts::overview()`;
    `generate()` now takes the user message verbatim (framing moved into
    `summarize`/`compress`). CLI: `zord overview [--max N]`.
- **23c** — ✅ **done.** The GUI **Overview view**.
  - Engine: `SummCmd::Overview` (runs `zord_overview::synthesize` on the summarize
    worker, relays progress as notices), `DbCmd::LoadOverview` (reads stored meta),
    `Event::Overview(Option<OverviewData>)` (feature-independent mirror struct).
  - GUI: a 📊 **Overview** button above the session list opens a third top-level
    view; **Generate / Refresh** + "N meetings · updated …" + Copy; the rollup is
    rendered as collapsible `## `-headed sections (My open action items open first).
    Summary/compressed panels are now gated to Session/Live views so they don't
    bleed into Overview.
- **23d** — **chat** ✅ (done): grounded Q&A, both **per-meeting** (in a session)
  and **cross-meeting** (in the Overview).
  - `zord-summarize`: `chat(system, turns, n_ctx)` + `ChatRole`; `generate`/`chat`
    share a `complete(messages, opts)` core; `GenOpts::chat`.
  - `zord-config`: `chat_system_prompt()` (answer ONLY from the provided context;
    say when unknown; cite meetings).
  - `zord-overview`: `cross_meeting_context()` reuses the gather + budget-fit
    (collect_digests / fit_to_budget refactor) to build grounding context.
  - engine: `SummCmd::Chat { scope, turns }`, `ChatScope`, `Event::ChatReply`,
    `chat_one` with a **resident model** kept across turns (freed before one-shot
    jobs to bound RAM); per-meeting context = transcript (or its compression when
    too long, generated on the fly).
  - GUI: a `ChatPanel` (scrolling Q&A + input) under a session and under the
    Overview; one conversation signal reset when the scope changes.
  - Remaining (optional polish): recency cadence / staleness nudge, mark-done &
    edit of overview items.

Reuses the existing llama.cpp summary model (with a larger configurable context
for compress/synthesis); no new heavy deps. Optional much later: a **live rolling
summary** during recording (same mid-meeting hardware caveat as live diarization).

### Cross-cutting / smaller
- macOS code-sign + notarize automation (needs Apple Developer account).
- ~~Multilingual UX~~ / ~~CUDA release builds~~ — **declined** (not wanted).
- Windows code-signing (Authenticode) so SmartScreen/managed machines don't
  block the binaries (CI step ready to wire once a cert/signing service exists).

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
