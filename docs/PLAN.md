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

### Phase 20 — Auto meeting title ✅ DONE
Implemented: `auto_title` setting (default on), `title_prompt()` + `clean_title()`,
auto-titling in the GUI summarize worker and `zord summarize` (never overwrites a
manual title; falls back to the timestamp id without `summaries`).

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

### Phase 24 — External LLM endpoints (OpenAI-compatible) ✅ (24a–d done)
Let the user point Zord at their **own inference server** — LM Studio, Ollama
(`ollama serve`), llama-server, vLLM, Jan, KoboldCpp — and use it instead of the
built-in llama.cpp for every LLM feature (Summarize, Compress, Overview, Chat,
auto-title). Connection details (base URL, optional API key, model) live in
Settings. One protocol covers all of those platforms: the OpenAI-compatible
`POST /v1/chat/completions` (+ `GET /v1/models` for the picker).

**Why it's one seam, not five features:** every LLM call already funnels through
`Summarizer::generate`/`chat` (chat-style messages → string) — exactly the
chat-completions shape. The work is one backend abstraction + an HTTP client +
settings UI.

**DECIDED (June 2026):**
- **Failure mode:** clear error, **no silent fallback** to the local model
  ("Couldn't reach http://… — is the server running?").
- **API key:** optional, **plaintext in config.json** (LAN servers rarely need
  one); keychain only if hosted-endpoint demand appears.
- **Scope:** **one global backend switch** — local GGUF or external endpoint —
  drives all LLM features; no per-feature routing.

Sub-phases:
- **24a** — ✅ **done** — **backend seam** (no behavior change). `LlmBackend` in
  `zord-summarize` (`backend.rs`): `Local(Summarizer)` exposing the existing
  `summarize/compress/generate/chat/count_tokens` surface (`Remote` lands in
  24b). Engine `summarize_loop`/chat cache, `zord-overview` (7 params + load),
  and the CLI all ported; nothing outside `zord-summarize` touches `Summarizer`
  directly anymore. `count_tokens` → chars/4 estimate on the remote path
  (Overview budgeting only; the server owns its real context).
- **24b** — ✅ **done** — **OpenAI-compatible client.** `zord-net` grew
  `post_json`/`get_json` + a typed `ApiError` (Connect/Status/BadJson) on the
  Phase 18 OS-cert-store + proxy agent. `zord-summarize::remote`: `RemoteLlm`
  (non-streaming `/v1/chat/completions`, `temperature: 0` to mirror the local
  greedy decode), `list_models` (`/v1/models`, doubles as test-connection),
  `RemoteConfig {base_url, api_key, model, timeout_secs}` with base-URL
  normalization (tolerates trailing `/` and `/v1`), and friendly error mapping
  (refused → "is the server running?", 401/403 → key, 404 → wrong endpoint/
  model). `LlmBackend::Remote` wired; `count_tokens` estimates chars/4.
  Tested: unit tests + an end-to-end in-process mock-server test.
- **24c** — ✅ **done** — **settings + wiring.** `zord-config`: `llm_backend`
  ("local"|"external", default local), `llm_base_url` (default LM Studio's
  `http://localhost:1234`), `llm_api_key`, `llm_model`, `llm_timeout_secs`
  (300). Settings → Summaries: backend selector; External swaps the GGUF model
  list for URL/key fields, a model dropdown fed by `/v1/models`
  (`ModelCmd::ListRemoteLlm` → `Event::RemoteModels`; auto-picks the first
  model when none chosen), a **Test connection** button, and the privacy note.
  Engine routes via one `build_llm_backend` (summarize/compress/overview/chat/
  auto-title); the resident chat cache keys on `ChatLlmKey` (GGUF path | remote
  config) so editing the connection rebuilds it. `zord_overview::synthesize`
  now takes the prebuilt backend. CLI shares a `build_llm_backend` helper
  (deduplicated the old per-command model resolution). Not verified against a
  real LM Studio yet — the mock-server test covers the wire format.
- **24d** — **polish / later.** ✅ **Chat streaming** (done): replies render
  as they generate on both backends — `LlmBackend::chat_stream(…, on_delta)`
  (local: per-token pieces from the decode loop; remote: `stream: true` + SSE
  via `zord_net::post_sse`, `[DONE]`/role/finish chunks filtered),
  `Event::ChatDelta` appends to the in-progress bubble, terminal
  `Event::ChatReply` replaces it with the full text. Errors now also land as a
  ChatReply ("⚠️ Chat failed: …"), fixing the pre-existing stuck-busy spinner
  on chat errors. Summarize/compress/overview stay single-shot by design.
  ✅ "via Ollama" download entries relabeled ("GGUF download from the Ollama
  registry"). ✅ **Backend feature split** (decided + done): `summaries` is
  replaced (clean break, no alias) by two composable flags — **`llm-local`**
  (llama.cpp, crate feature `llama`) and **`llm-remote`** (OpenAI-compatible
  client, new crate feature `remote` in zord-summarize/zord-overview — pure
  HTTP, no llama.cpp toolchain). Shared types moved to `opts.rs`;
  `LlmBackend`'s variants compile independently. Rules: with both flags the
  setting picks; with one, it's used regardless (notice only when the settings
  explicitly ask for a missing backend); with neither, the old "not built in"
  message. Settings section renamed **"AI"** (it long outgrew "Summaries") and
  is capability-aware. Releases ship both flags
  (`diarization,llm-local,llm-remote,parakeet`). All four build configs +
  clippy + tests verified.

Known gaps: `compress_ctx`/`overview_ctx` become input-budget knobs only for
remote (server-side context is the server's business — UI wording to match);
chunked-prefill (the v0.2.9 crash fix) is llama-only and N/A for remote;
auto-title rides the same backend switch.

### Phase 25 — Deferred & re-transcription ✅
**Post-ship polish (June 2026):** the Transcription settings became one
holistic panel — a single model list with **Live / Re role chips** per row
(two radio groups; Delete blocked while a model holds a role) replacing the
old separate list + dropdown; plus a **"Transcribe automatically after
recording"** toggle (default off), independent of Live: live+auto = auto-
upgrade the live transcript at stop with the Re model; off+off = fully
deferred (WAVs kept regardless of keep-audio until first transcription; the
first 🔁 honors diarize-auto).
For low-power machines (Windows + Teams): live transcription bursts the CPU
60–80% per VAD chunk (webcam stutter) and pins ~1 GB of model RAM for the whole
meeting. Fix: make live transcription **optional**, and make post-hoc
(re)transcription a first-class GUI action with its **own model choice** —
record with nothing (or a small model), transcribe with a big one after.
The CLI already proves the pipeline (`zord retranscribe` / `run_retranscribe`).

**Design decisions (June 2026):**
- Two independent knobs, both can be on: **Live transcription** toggle
  (default on; model picked as today) and a **Re-transcription model**
  (its own dropdown, all models listed — low-power users may want a small one
  even post-hoc; default `large-v3-turbo-q5_0`). The Re-transcribe action
  *always* uses the re-transcription model from settings.
- **Timestamps:** safe by construction — kept WAVs are wall-clock aligned
  (silence-padded), so re-derived segment times live on the same session
  timeline; both channels are re-transcribed from their own WAVs, preserving
  Me/Others alignment, per-line replay, and diarization span mapping.
- Re-transcribing **replaces** segments → confirm dialog (manual line edits
  are lost), then **auto re-run diarization** when the session had speaker
  labels (and audio is still present). Summary/compression go stale — left in
  place; the user regenerates if they care.
- Capture-only recordings always write the per-channel WAVs (transcription
  input!) regardless of keep-audio; if keep-audio is off they're deleted after
  the post-pass, mirroring the diarize temp-WAV behavior.

Sub-phases:
- **25a** — ✅ **done** — **settings + capture-only recording.** `zord-config`:
  `live_transcription: bool` (default true), `retranscribe_model: String`
  (default `large-v3-turbo-q5_0`). Settings → Transcription: the toggle + the
  re-transcription model dropdown. Recorder: when live is off, skip model
  load + transcribe jobs entirely (meters/VAD/WAV writing only — ~1–2% CPU,
  no model RAM); Live view shows "Recording — transcription runs when you
  stop (live transcription is off)".
- **25b** — ✅ **done** — **engine post-pass.** Extract the CLI's WAV→VAD→transcribe→insert
  pipeline into shared code; new engine command (dedicated worker thread, like
  on-demand diarize) with progress notices + a busy state; on Stop of a
  capture-only recording, auto-run it (downloading the post model if needed),
  then the existing diarize-auto chain. Emits refreshed transcript + badges.
- **25c** — ✅ **done** — **GUI Re-transcribe.** 🔁 button in the session toolbar next to
  Summarize/Compress/Identify speakers — enabled when the session's kept WAVs
  exist; confirm dialog ("replaces the transcript; manual edits are lost");
  busy state with a rough ETA (like diarize); auto re-diarize after when
  speaker labels existed.
- **25d** — ✅ **done** — **single full-quality audio track** (REVISED June 2026 —
  supersedes the earlier two-stage-retention idea). Store ONE WAV per channel
  at the **device's native rate** (mono, 16-bit, wall-clock silence-padded at
  that rate — padding moves to *before* the resampler in `spawn_proc`); the
  16 kHz stream the models need is **derived on the fly** and never stored.
  (Honest note: device-rate audio improves *playback* only — models consume
  16 kHz either way — but deriving 16 kHz from the original is lossless, so
  one original-rate track strictly dominates storing the downsample.)
  - **Re-transcription:** already rate-agnostic (the pipeline reads the WAV
    header and resamples) — no change.
  - **Diarization:** gains an on-the-fly downsample step when loading the
    Others WAV; stream/chunk it — a 1 h 48 kHz file is ~690 MB as f32 if
    slurped whole.
  - **Per-line replay:** reads the rate from the WAV header (today it assumes
    16 kHz) and plays at native rate — listening quality improves for free.
    Timestamp math stays exact: `sample = ms × rate/1000` at the file's rate.
  - **Back-compat:** every reader stays rate-agnostic so existing 16 kHz
    session WAVs keep working untouched.
  - **Defaults:** `keep_audio` → **on**, `auto_delete_days` → **30** (was
    never). ⚠ Existing users' audio older than 30 days gets purged on first
    launch after upgrade — call out in release notes. `diarize_keep_audio`
    becomes redundant (the one kept track serves re-diarization) — fold it
    away. Safety rule kept: never auto-purge a capture-only recording that
    hasn't been transcribed yet.
  - **Disk math:** 48 kHz mono 16-bit ≈ 5.8 MB/min/channel (~345 MB per
    1 h meeting both channels) vs ~1.9 MB/min at 16 kHz — 3×, bounded by the
    30-day default.

### Phase 26 — Rolling project ledger (stateful Overview) ✅ DONE — major, direction change

Replace the stateless from-scratch Overview with a **durable, incrementally
maintained per-project ledger**. Today `synthesize` recompresses recent
meetings and re-derives one Markdown blob every refresh (`collect_digests →
fit_to_budget → one pass`, stored in `app_meta["overview"]`); the token
ceiling is the whole reason for compression. The new model keeps a persistent
set of **projects**, each with a running record (status, active action items,
completed items, open questions, decisions, history), and folds each new
meeting in as a **delta**: route it to the right project(s), mark resolved
items done, add new ones, transition statuses.

**Why it also fixes the token problem:** each update reasons over only
*(one project's current state) + (one meeting's delta)* — bounded regardless
of how many meetings accumulate. The ledger is the memory; the LLM never sees
the whole corpus at once.

```
TODAY:  [all compressions] → fit to budget → one blob   (recomputed each refresh)
NEW:    meeting → extract delta → route to project(s) → merge into ledger
                                                            │
                          persistent projects ◄────────────┘
                          (name · status · active items · done items · history)
```

**Decisions (June 2026):**
- **Fold lazily, on Overview open/refresh** — apply not-yet-folded sessions in
  chronological order, with progress; no surprise LLM work mid-recording.
- **Auto-assign project routing** — LLM best guess (match existing / create
  new); **low-confidence → an "Unfiled" bucket** for the user to assign/name.
- **Full manual editing** — rename / merge / split / archive projects; add /
  edit / complete / reopen items by hand. The ledger is the user's; the LLM
  maintains it but never has the last word.
- **Opt-in "Build from history"** replays all past sessions in order to seed
  the initial ledger. ⚠ **Rebuild is DESTRUCTIVE to manual edits** — it
  regenerates from the transcripts and discards hand corrections, so it warns +
  confirms. Normal incremental folding **preserves** manual edits; only the
  explicit full rebuild wipes.
- **Provenance, no hallucinated completion** — an item is only marked done when
  the transcript says so, and each status change records the session that
  caused it (auditable "why is this done?").

Sub-phases (all shipped):
- ✅ **26a — schema + store API.** New tables: `projects` (id, name, status,
  description, created/updated, last-activity), `project_items` (id,
  project_id, kind action|question|decision, text, owner, status
  open|blocked|waiting|done, created/updated/completed-session, `manual` flag
  so folding doesn't clobber hand-edited rows), `session_overview_state`
  (session_id → applied_at + stored extract JSON, for idempotency + staleness
  when a session is later re-transcribed/edited), and a `project_history`
  audit log (item/status change → session, at). Migrations; no LLM yet.
- ✅ **26b — per-meeting structured extract.** An LLM pass turns a session
  (transcript, or its compression when long) into a schema'd delta: projects
  touched + action items (with which prior items they resolve) + decisions +
  open questions. Supersedes the free-prose compress for the ledger (compress
  may stay as a chat-context fallback).
- ✅ **26c — routing + merge engine** (in `zord-overview`). Split into
  `plan_fold` (LLM) + `apply_plan` (backend-free, id-validated):
  extract → route each project (match-or-create against the existing
  project-name list, with a confidence threshold → Unfiled) → merge the delta
  into the matched project's state (mark done, add new, transition; never
  delete history; stamp provenance). Idempotent + chronological. `fold_pending`
  (apply unapplied sessions) and `rebuild_from_history` (destructive replay).
- ✅ **26d — ledger Overview UI.** The Overview view becomes a project list
  (active first), each expandable to status · active items · "show completed /
  history" · open questions · decisions · source sessions. Refresh (fold
  pending, with progress) + Build-from-history (with the destructive-rebuild
  confirm). Unfiled bucket → assign to a project.
- ✅ **26e — full editing.** Rename / archive / delete projects; item
  add / edit / complete / reopen; the `manual` flag protects edited rows from
  being overwritten by later folds.
- ✅ **26f — chat + CLI.** Cross-meeting chat grounds on the structured ledger
  (falling back to the old compressions until first folded). CLI:
  `zord overview` prints the ledger; `--refresh` folds pending, `--rebuild`
  for the destructive replay.

**Shipped notes / deviations from the sketch:**
- Project routing uses match-by-id or null→create, with a normalized-name
  merge guard; an explicit confidence *threshold* wasn't needed — the
  reconcile model picks, and `apply_plan` validates every id (a bad/invented
  id drops that op, so no phantom completions). Unroutable items → `Unfiled`.
- 26e shipped rename/describe/archive/delete + item add/edit/complete/reopen/
  move/delete. Project **merge/split** deferred (move-item covers the common
  case; full merge/split is a later nicety).
- Legacy `app_meta["overview"]` is still shown as a read-only fallback until
  the ledger is first folded (graceful upgrade), then superseded.

**Gaps / risks to watch:**
- Entity resolution (project routing + item matching) is the error-prone core;
  a small local model will misroute/duplicate. Mitigations: confidence →
  Unfiled, easy correction, provenance, and the external-LLM option for users
  who want a stronger model.
- Idempotency + staleness: re-transcribing or editing an already-folded session
  must mark it stale and offer a re-fold; never double-count.
- Merge drift over many sessions → "Build from history" is the reset button
  (destructive, by design).
- Migration cost: replay is many LLM calls — progress + cancellable + opt-in.
- The legacy `app_meta["overview"]` blob becomes vestigial; keep reading it for
  one release so an upgrade isn't jarring, then drop.

---

## 8. Platform integrations (Phases 27–31) — major initiative

> 📐 ASCII reference diagrams for this initiative live in
> [`docs/diagrams/integrations.md`](diagrams/integrations.md). A user + service
> flow walkthrough is in [`docs/discord-integration.md`](discord-integration.md).

Today every voice the app hears arrives as one **mixed** stream: the system
loopback ("Others"), which blends all remote participants together. Per-speaker
diarization (Phase 16) recovers identity from that mix by *clustering* — error-
prone (a 10-person call over-split into ~80 "speakers"; Phase 21) and label-less
("Speaker 1", not "Alex").

**The insight.** Some platforms can hand us **separate, already-identified audio
feeds — one per participant**. When we have that, diarization is unnecessary: we
*know* who is speaking, with their real name, by construction. Discord is the
first and best fit (its voice gateway sends each participant as a distinct RTP
stream). Desktop/system capture stays the universal fallback for everything that
*can't* give us separated feeds.

### Approaches researched (June 2026)

| # | Approach | Per-participant? | Real names? | Bot/SDK? | Verdict |
|---|---|---|---|---|---|
| **A** | **Discord bot voice receive** (`songbird` `receive` feature) | ✅ per-SSRC PCM | ✅ via gateway speaking events → REST | bot joins VC as a visible participant | **Headline. Phases 27, 30.** |
| **B** | **Per-process OS audio tap** (macOS 14.4+ Core Audio taps; Windows process-loopback) | ❌ still a per-*app* mix | ❌ | none | Universal fallback. **Phase 31.** Still needs diarization. |
| **C** | **Meeting-platform media bots / SDKs** (Zoom Meeting SDK raw audio, Teams real-time media bot) | ✅ | ✅ | bot joins + credentials + (Teams) tenant admin + server infra | Heavyweight; **backlog**, not near-term. |
| **D** | **Post-hoc transcript enrichment** (Teams Graph `callTranscript` names) | n/a (text) | ✅ | Azure AD app + tenant | **Declined** (no tenant access — see `teams-audio-options` memory). |

**Approach A specifics (researched):**
- Discord's voice gateway sends every participant's audio as a separate RTP
  stream keyed by **SSRC**. [`songbird`](https://github.com/serenity-rs/songbird)
  (serenity ecosystem) exposes decoded per-user PCM via its **`receive`** feature:
  a sink's `write()` gets `VoiceData { user, audio }`. SSRC→user comes from
  `SpeakingStateUpdate` events; user→display-name from REST.
- ⚠ **DAVE is the feasibility gate.** Since March 2026 Discord mandates
  end-to-end encryption ([DAVE](https://discord.com/blog/meet-dave-e2ee-for-audio-video),
  MLS + WebRTC encoded transforms) on all voice. Bots that don't implement it
  **cannot decrypt received audio** (cf. open `discord.js` issues:
  `DecryptionFailed(UnencryptedWhenPassthroughDisabled)`). **songbird v0.6.0
  (April 2026) added DAVE incl. in-place decryption** — so the Rust path is
  viable in principle, but **receive-under-DAVE must be live-verified before any
  UI work** (Phase 28 exists solely to retire this risk).
- **Setup model (decided):** the user **brings their own bot** — creates a
  Discord application, pastes the bot token into Zord settings, invites it to
  their server. No Zord-operated infrastructure (keeps the all-local ethos); the
  bot joins the VC as a *visible participant*, which also aids consent.
- **Consent/ToS:** Discord's Developer Policy requires explicit per-instance
  recording consent and minimal retention — baked into the connect UX.

### Architecture (decided)

**Reuse the diarization identity surface — do not generalize `Source`.** Phase 16
already gave segments a `speaker` index within `Others` plus a `speaker_names`
table (rename "Speaker 1" → "Alex"), wired through transcript colors, search, and
exports. An integration is just **a capture source that pre-assigns the speaker
label from ground truth instead of inferring it** — diarization with the
clustering replaced by known identity.

```
                 today                          with an integration
   mic ──► Me                          mic ──► Me   (unchanged)
   system-loopback ──► Others ─┐       Discord ─┬─► Others + speaker=0 ("Alex")
                               │                ├─► Others + speaker=1 ("Sam")
                  diarization ─┘                └─► Others + speaker=2 ("Jo")
                  (cluster → Speaker N)         name map written directly,
                                                NO diarization pass
```

Each participant stream runs the **same** `spawn_proc` resample→VAD→transcribe
path (tagged `Others` + a stable speaker index); the integration writes real
names into `speaker_names`. FTS / exports / transcript UI therefore need almost
no change — the work is the integration seam, the Discord connection, the
auth/consent UX, and an **audio-storage rework** (below). **"Me" is the followed
user's own Discord stream** (`is_me` → `Source::Me`), not a local mic — everyone
is captured through the platform, so its noise suppression applies uniformly and
there's no dedupe or mic-vs-Discord drift (decided Phase 30; superseded the
earlier local-mic idea).

**Diarization parity (decided).** Diarized desktop audio and integration audio
must be *structurally identical* once stored — same `source=Others` + `speaker`
index + `speaker_names` entry. The only difference is provenance: diarization
*infers* the speaker index by clustering one mixed `others` track; an integration
*knows* it from the source. Consequences:
- An integration session is **never diarized** — it already has ground-truth
  speakers. The "Identify speakers" button is hidden/disabled when speakers are
  pre-assigned (just as "Me" mic audio is kept as plain transcription, integration
  per-speaker audio is kept as plain transcription — no clustering pass ever).
- Desktop-only sessions behave exactly as today: plain `Others` until the user
  clicks Identify speakers, which clusters the mix into speaker indices.
- Re-transcription and per-line replay resolve a segment to its audio by
  `(source, speaker)` uniformly, regardless of how the speaker was assigned.

**Sparse audio → explicit silence (decided, critical).** Integration sources are
*sparse*: a participant's stream delivers packets only while they speak (a user
silent for minutes sends nothing). Absence **must be counted as silence**, or
timestamps collapse and transcription mis-segments. This is the same hazard the
WASAPI loopback already hit (no samples during silence) and is solved the same
way: each per-speaker stream's `spawn_proc` pads silence to wall-clock
(`produced` vs session-clock; see `capture-design` memory). ⚠ The existing
**5-min silence-pad cap** must be revisited — a participant idle longer than that
would drift; for integration sources, drive padding from the bot-connection
session clock (which we know exactly) rather than capping. Wall-clock alignment
keeps every speaker on one timeline and keeps the saved per-speaker WAVs exact for
replay / re-transcription.

**Audio storage → folder-per-session (decided).** Today audio is flat files keyed
by a prefix: `audio/<id>.me.wav`, `audio/<id>.others.wav` (`sessions.audio_path`
holds the prefix; replay / re-transcribe / diarize / retention all resolve by
`{prefix}.{role}.wav`). A fixed two-file scheme can't hold N per-speaker tracks.
Move to **one folder per session, named with the session start date-time** —
`audio/2026-06-09_18-15-47/` (local, sortable; **all** session types, Discord or
desktop) — containing `me.wav`, `others.wav` (when desktop capture is used), and
per-speaker integration tracks `spk-0.wav`, `spk-1.wav`, … — with a small **track
manifest** mapping each file to its role + speaker index + the speaker's real name
(so a reader knows whether speaker N has a dedicated file (integration) or maps
into the single `others.wav` (diarized mix)). `sessions.audio_path` now holds the
folder path. Migration: resolvers accept the **old flat layout** for existing
sessions while new sessions use the folder; retention deletes whole session
folders by age.

**Sparse-speaker model → full session-aligned tracks (decided).** Every track —
`me`, `others`, and each `spk-N` — is **anchored at session start and spans the
whole recording**, wall-clock silence-padded (exactly how Me/Others already work
per `capture-design`). A participant who joins 5 min in gets 5 min of leading
silence; one who leaves early gets trailing silence to the stop. **No per-track
offset** — `sample N` is the same real instant on every track, so a segment's
`t_start_ms` maps 1:1 to a sample on any track and replay / re-transcribe /
diarization-overlap / timeline-merge need **zero new logic**. (Rejected
alternatives: presence-window tracks + offset — saves storage but adds an offset
concept to every reader; per-utterance clips — smallest storage but fragments a
speaker's intermittent speech and wrecks ASR quality.) Transcription quality is
unaffected by the leading/trailing silence (VAD skips it). **Storage cost** of
session-length silence for partial-attendance speakers is accepted, bounded by
the 30-day retention; **trailing-silence trimming** is a noted future
optimization, not part of this phase.

### Phase 27 — Discord receive spike (de-risk DAVE) ✅ VERIFIED LIVE (June 2026)
A minimal `songbird` (+`serenity`) receive bench behind the `discord` feature:
join a real voice channel with a user-supplied bot token and **prove per-user PCM
decrypts under DAVE** (write per-SSRC WAVs, mapped to user ids). This is Phase
0-style risk-killing and gates everything below. **Exit criteria:** clean
per-user audio from a live DAVE-encrypted channel. If it fails, the bot path is
blocked and we pivot to Approach B (Phase 31) as the primary — *learn it now, not
after building storage + UI.*

**Done (build):** new `crates/zord-integrations` crate; `discord` feature pulls
`songbird = "0.6"` (default feats + `receive`; DAVE/`davey` + `opus2` come with
the driver) + `serenity = "0.12"` + `tokio`. The `discord-spike` bin
(`required-features = ["discord"]`, so a bare `cargo build` never pulls the heavy
tree) joins a fixed VC by id, subscribes `CoreEvent::{VoiceTick, SpeakingStateUpdate,
ClientDisconnect}`, downmixes each speaker's decoded 48 kHz stereo to mono, writes
one `spk-<ssrc>.wav` per user **silence-padded to wall-clock via `tick.silent`**
(prototyping the Phase 28 sparse→silence model), maps SSRC→user, leaves after N s.
Verified: `--features discord` compiles + links (davey/opus2/songbird all build);
default workspace build stays green; clippy clean on the crate.
**✅ Verified live (June 2026):** ran against a real DAVE-encrypted channel. Crypto
negotiated `Aes256Gcm`, the DAVE/MLS handshake completed (opcode-25 binary
frames), and the bot received **527 decoded audio frames** over 30 s →
`spk-6529.wav` (48 kHz mono) measured peak 16992/32767, ~15% non-silent windows =
**clean intelligible speech**. So **DAVE receive works via songbird 0.6** — the
bot path is unblocked. **End-to-end confirmed:** `zord file spk-6529.wav` ran the
captured audio through the real pipeline (resample→VAD→Whisper Metal) → an
accurate timestamped transcript (7 segments, speech correctly placed across the
30 s — proving the sparse→silence wall-clock padding too). The spike also grew the real **follow-the-user** mechanic
(guild-agnostic: scans every shared server's voice states + `voice_state_update`
to join whichever channel the configured user is in — no guild/channel config),
de-risking Phase 30 early.

**⚠ Gap found — SSRC→user mapping:** the run got audio but `mapped_users=0` — no
`SpeakingStateUpdate` mapped the speaking SSRC to a Discord user id (likely the
speaker was already talking before the bot joined, so no fresh speaking-state was
sent). Audio attribution worked by *stream* but not by *identity*. **Phase 30 must
make SSRC→user mapping robust** (e.g. seed from voice states / client-connect on
join, backfill on first speaking event, fall back to "Speaker N"). Not a DAVE
blocker — the decryption/decode path is proven.

### Phase 28 — Session audio storage rework (folder-per-session) 🟢 28a–d DONE
Prerequisite for N per-speaker tracks (see "Audio storage" + "Sparse-speaker
model" above). Move from the flat `audio/<id>.{me,others}.wav` prefix scheme to a
**date-time-named folder per session** holding `me.wav`, `others.wav`, and (later)
`spk-N.wav`, with full session-aligned tracks. **Pure storage/plumbing refactor —
no integration code yet, fully verifiable on the existing desktop/diarization
paths** before anything depends on it.

Sub-steps:
- **28a — paths + back-compat resolver (`zord-config`).** ✅ **DONE.**
  `Settings::session_audio_dir(started_at) → audio/<YYYY-MM-DD_HH-MM-SS>/`
  (unique, created), `session_dir_name()`, `track_path(dir, role)`, and
  `resolve_track(audio_path, role)` — which returns the existing track whether
  it's in the **new folder** (`<dir>/<role>.wav`) or the **old flat**
  (`<prefix>.<role>.wav`) layout. 3 unit tests (both layouts + name format).
  Added `chrono` (clock) to `zord-config` for local-time naming.
- **28b — engine writes to the folder.** ✅ **DONE.** `run_session` builds a
  `session_dir` via `session_audio_dir`; `wav_path`/`others_wav` write
  `track_path(&session_dir, …)`; `sessions.audio_path` stores the folder. Existing
  wall-clock silence-padding already yields full session-aligned tracks.
- **28c — update readers.** ✅ **DONE.** `session_audio_files` (replay), diarize's
  `others` lookup, and `post_transcribe_inner` (GUI) + `run_retranscribe` /
  `cmd_diarize` (CLI) all resolve via `resolve_track` (folder + flat back-compat).
  No timeline-offset logic (session-aligned). **Migration-free:** existing flat
  sessions keep working; new recordings use the folder.
- **28d — retention.** ✅ **DONE.** `apply_retention` now removes whole session
  **folders** (`remove_dir_all`) *and* legacy flat files by age.
- **28e — per-speaker (`spk-N`) plumbing + track manifest.** **→ folded into
  Phase 30.** The foundation is ready (`resolve_track`/`track_path` already accept
  arbitrary roles like `spk-0`), but a manifest (role+speaker idx+name→file) and
  manifest-driven multi-track read (resolve a segment to its track by
  `(source, speaker)`) can't be tested without a `spk-N` producer — so it lands
  with the Discord source in Phase 30.
- **Deferred refinement:** revisit the 5-min silence-pad cap (drive padding from
  the session clock) when integration sources arrive in Phase 30 — not exercised
  by today's mic/desktop paths.

### Phase 29 — Integration framework (the seam) 🟢 29a DONE
Define the seam in `zord-integrations`, then wire the engine. **No network code**
— a built-in fake provider validates the engine/store/GUI paths before any heavy
dep lands. Designed so a **local vs hosted backend swap** is feasible later.

- **29a — trait + fake provider.** ✅ **DONE.** Dependency-free seam in the
  default build: `Integration` trait (`name`/`start`/`stop`) emitting
  `IntegrationEvent::{ParticipantJoined { participant, sample_rate, audio },
  ParticipantRenamed { key, name }, Ended { reason }}`; `Participant { key,
  name }`; `AudioStream = Receiver<Vec<f32>>` (mono f32, same shape as the
  capture `FrameSink`, sparse by nature). `FakeProvider` emits N participants
  with real-time-paced sparse tone bursts + silent gaps, then `Ended`. Unit-test
  passes; clippy clean; stays out of the `discord` feature (light seam).
- **29b — engine wiring.** ✅ **DONE (build-verified).** `drive_session` (in
  `zord-integrations`, unit-tested) pumps an `Integration`'s events and assigns a
  stable 0-based speaker index per participant. The engine's new
  `run_integration_session` (a *separate* fn, so it can't destabilize the proven
  `run_session`) runs it: per `ParticipantJoined` it registers the name
  (`set_speaker_name`) and spawns a per-speaker proc (`Others` + ground-truth
  speaker index → `spk-N.wav`, wall-clock aligned via the shared `session_start`);
  `Job` gained a `speaker: Option<i32>` so segments carry the index;
  `ParticipantRenamed` updates `speaker_names`; the session ends on the provider's
  `Ended` *or* a user Stop; the local mic still drives "Me". No diarization pass
  (ground-truth speakers). Triggered by `ZORD_FAKE_INTEGRATION=1` (hidden dev
  trigger reusing the Record button). **Runtime check is a GUI launch** (like all
  engine work — `verification-limits`): `ZORD_FAKE_INTEGRATION=1 cargo run -p
  zord-gui`, press Record → expect `spk-0/1.wav` in the session folder +
  "Tester 1/2" in `speaker_names`. Builds + clippy + integration unit tests green.
- **29c — GUI surface → folded into Phase 30.** The env-var trigger reuses the
  Record button, so no separate minimal UI is needed now; the proper start/stop +
  per-speaker live state lands with the Settings → Integrations tab in Phase 30.

### Phase 30 — Discord integration (full) 🟡 30a–c DONE (30c build-verified)
The real `discord` `Integration` on the Phase 29 seam, using the Phase 27 receive
code, plus the Settings UI.

**Decisions (June 2026):**
- **Feature flag = `discord`** (per-platform, not an umbrella) — `zord-gui`/
  `zord-app` passthrough to `zord-integrations/discord`; releases adopt it when
  mature. Future Teams/Zoom get their own flags.
- **Trigger = a `capture_mode` value "Discord"** alongside mic/system/both; the
  normal Record button runs an integration session. **Mutually exclusive with
  desktop loopback** — recording Discord *and* system audio would double-capture
  the call, so "Discord" mode captures neither mic nor system locally.
- **"Me" = the followed user's own Discord stream (decided), NOT a local mic.**
  Everyone — including the operator — is captured through Discord, so its noise
  suppression / echo-cancel / AGC apply uniformly (and Phase 27 already proved a
  user's own Discord stream transcribes cleanly). The followed user's stream →
  `Source::Me`; every other participant → `Others` + speaker index. No local mic,
  no mic permission, no self-dedupe, no mic-vs-Discord clock drift.
- **Consent = optional in-channel announcement** — when the bot joins, it posts a
  "recording started" message in the channel's text chat (needs Send-Messages),
  so participants see it live. (No per-session dialog; the visible bot + the
  message are the transparency signal.)
- **Optional merged single audio file** — on request, mix all session-aligned
  tracks (`me` + every `spk-N`) into one WAV for download. Cheap *because* tracks
  are session-aligned (Phase 28): sum sample-wise + soft-limit; derived on demand,
  not stored.

**Sub-steps:**
- **30a — feature flag + config.** ✅ **DONE.** `discord` feature on `zord-gui`
  (→ `zord-integrations/discord`); `discord_bot_token` + `discord_user_id`
  settings (plaintext, mirroring `llm_api_key`). Default + feature builds green.
- **30b — "Me from platform" seam + engine.** ✅ **DONE** (reworked June 2026:
  unified tracks). `Participant.is_me` marks the followed user; `drive_session`
  assigns **every** participant — the user included — the next 0-based speaker
  index, and `run_integration_session` records them all as uniform
  `Others`/`spk-N.wav` tracks named from the platform, **with no local mic**.
  "Me" is a session tag (`sessions.me_speaker`, from `is_me`) driving styling
  and perspective only — not a separate channel, so replay, voiceprints, and
  re-transcription treat the user like any participant. `FakeProvider` marks
  one participant `is_me` for testing; unit tests updated + green.
- **30c — the real `DiscordProvider`.** ✅ **DONE (build-verified).**
  `crates/zord-integrations/src/discord.rs` (behind `discord`): a serenity client +
  songbird voice receiver on a dedicated tokio runtime thread, bridging into the
  std `mpsc` event channel. Follows the configured user (`cache_ready` scan +
  `voice_state_update`), joins their VC, and on each `SpeakingStateUpdate` maps
  SSRC→user, resolves a name (server nick → global → username, cached, via REST),
  and emits `ParticipantJoined` (followed id → `is_me`); `VoiceTick` decoded PCM
  is downmixed to mono and routed to that participant's stream; leaving voice →
  `Ended`. Engine selects it via `build_integration_provider` when
  `capture_mode == "discord"` / `ZORD_DISCORD` (+ feature + token; settings or
  `DISCORD_TOKEN`/`DISCORD_USER_ID` env fallback); else `FakeProvider`. Builds
  (default + `--features discord`) + clippy + tests green. **Runtime = user step**
  (live DAVE call with their bot — not headless-testable).
  - **Known v1 trade-offs / follow-ups:** mapping is announce-on-`SpeakingStateUpdate`
    (the reliable carrier) — a participant already mid-sentence when the bot joins
    isn't captured until their next speaking transition (the Phase 27 gap); seeding
    from voice states + `ParticipantRenamed` backfill is the planned hardening.
    The **5-min silence-pad cap** still needs revisiting for very-late joiners
    (drive padding from the session clock).
- **30d — Settings → Integrations tab.** ✅ **DONE (June 2026).** New `stab`
  "integrations"; Discord section: token field (masked) + user-id field +
  "how to find your user id" help + announce toggle; **"Invite bot to a
  server"** (REST `GET /oauth2/applications/@me` via new `zord_net::discord_bot_app`
  → `oauth2/authorize?client_id=<id>&scope=bot&permissions=1051648` (View
  Channel + Send Messages + Connect) → system browser via `open`);
  **"Test connection"** (validates the token, shows the bot name).
  Capability-aware ("install a release build / build with `--features discord`"
  note when not built). "Discord" added to the capture-mode selector (discord
  builds only) with an explainer. Guards: discord capture mode in a featureless
  build → clear error (no silent fake session); missing credentials → error
  *before* the session row is created (`build_integration_provider` → `Result`,
  provider resolved up front).
- **30e — announcement + merged-file.** ✅ **DONE (June 2026).** In-channel
  "recording started" post on join (`DiscordProvider::with_announce`,
  best-effort `channel.say`, default ON via `discord_announce` setting);
  **Export ▾ → "Merged audio (.wav)"** mixes the session-aligned tracks via
  `zord_audio::mix_wavs` (streamed 1 s blocks, highest input rate wins,
  lower-rate tracks resampled up via `MonoResampler::to_rate`, overlap
  clamped) → `exports/<id>.merged.wav`, off the db thread with a job spinner.
- **30f ✅ DONE (June 2026)** Dedicated **Record Discord** button (sidebar
  foot, shown when the build + credentials + an Integrations toggle allow it);
  `RecorderCmd::Start` carries an explicit `integration` flag instead of the
  engine re-reading `capture_mode`; the `"discord"` capture mode was removed
  from the dropdown and old configs migrate to `"both"`. Mute buttons no
  longer render during integration sessions (nothing local to mute). Spec:
  `docs/superpowers/specs/2026-06-10-discord-record-button-design.md`.
- **30g ✅ — live-test hardening (June 2026).** First real GUI tests surfaced
  three bugs, all fixed:
  1. **songbird scheduler lifetime** — its default scheduler is a process
     global whose core task spawns on the first tokio runtime; our
     runtime-per-session design killed it after session #1 and every later
     voice join panicked (empty sessions). Each session now passes its own
     `Scheduler` via songbird `Config`.
  2. **SSRC-mapping race** — Discord delivers the Speaking events (SSRC→user)
     immediately on join; handlers were registered *after* `join()` returned
     and missed them → no `ParticipantJoined`, nothing recorded. Handlers now
     register **before** the join, and any SSRC producing audio unmapped for
     ~1 s is announced unnamed ("Speaker N", upgraded on the late mapping) —
     audio can no longer be lost silently. Joins are bounded by a 20 s
     timeout, and the bot now **leaves the channel** before gateway shutdown
     (a lingering voice state timed out the next join).
  3. **No post-stop transcription** — integration sessions lacked
     `run_session`'s Phase 25 post pass *and* `post_transcribe_inner` ignored
     `spk-N` tracks (the folded 28e gap). Both fixed: the post pass runs for
     integration sessions and every per-speaker track transcribes with its
     ground-truth index.
  A clean end-to-end re-verification on a live call is the remaining step.
- Heavy deps (`serenity`/`songbird`/`opus`/`davey`) stay behind the `discord`
  feature; releases ship it (✅ in the release feature set since Phase 34/35).

### Phase 31 — Per-app capture (Approach B, bot-free universal fallback)
✅ **DONE (June 2026; macOS build-verified + Windows cross-compile-verified —
live Windows run still untested).** `SystemAudio` can now tap a **single chosen
app** instead of the whole-system mix; one app's audio (just Zoom, just a
browser) — excludes music/notifications, works for *any* meeting app with no
bot/SDK. Still a per-app **mix**, so diarization remains the identity path here
(no real names). The fallback for every platform that can't hand us separated
feeds.
- **macOS:** the ScreenCaptureKit content filter scoped via
  `with_including_applications` (simpler than the originally-planned Core Audio
  process taps — SCK's filter applies to audio, needs only macOS 13 + the same
  Screen Recording permission). Picker = `SCShareableContent` applications.
- **Windows:** WASAPI **process-loopback**
  (`AudioClient::new_application_loopback_client(pid, include_tree=true)`,
  Windows 10 2004+; child processes included so multi-process apps are captured
  whole; fixed 20 ms period — `get_device_period` is unsupported in this mode).
  Picker = audio sessions on the default render device → PID → exe name
  (`QueryFullProcessImageNameW`).
- **Shared surface:** `CapturableApp { id, name, pid }` —
  `id` is the *stable* identity settings persist (bundle id / exe name), PID is
  resolved fresh at record time. `zord_capture::list_capturable_apps()` +
  `SystemAudio::start_app(sink, id)`; missing app → actionable error.
- **UI:** capture mode **"Microphone + one app's audio"** + an app picker
  (Refresh button; enumeration is never eager — it triggers the macOS Screen
  Recording prompt; saved choice stays listed as "(not running)").
  Settings: `capture_app_id` / `capture_app_name`. CLI stays whole-mix (v1).
- **CI:** new `windows-check` job (windows-latest `cargo check` on
  zord-capture/config/net) keeps the cfg(windows) code compiling.

### Integration backlog (post-30)
- **⭐ Centralized / hosted bot (the long-term direction — keep accessible).**
  Instead of the local machine running everything, a Zord-operated bot (named
  after the app) lives in the cloud. A user supplies their **Discord user ID /
  identity**; the bot finds the voice session that user is currently in, joins,
  records, and delivers the transcript **back to the requester** (e.g. DM). The
  *only* server-side requirement is the bot having been added to the server where
  the call happens — no per-user token, no local capture. This is why Phase 30's
  local flow is built as **follow-by-identity → find live session → join**: the
  exact same primitive the hosted bot needs, so the local implementation rolls
  forward into the centralized one. Deliberately **back-burnered** for now (local
  is the right call today); design the Phase 29 seam and the Discord
  connect/resolve code so a "local vs hosted" backend swap is feasible later.
- **Zoom Meeting SDK / Teams media bot** (Approach C) — per-participant + names,
  but bot-joins-as-participant + credentials + (Teams) tenant admin + server
  infra. The Integrations tab is where they'd surface. Revisit only on demand.
- Generalizing `Source` into a first-class participant model — considered and
  **deferred**; the diarization-surface reuse covers the need with far less churn.

### Gaps / risks to watch
- **DAVE receive** — verified in principle (songbird 0.6), unverified live →
  Phase 27 retires it first.
- **Async-runtime bridge** — songbird needs a *long-lived tokio task* holding the
  gateway + voice connection, vs. today's thread-per-capture model. The Discord
  integration runs that task and bridges each received per-user PCM stream into a
  sync `FrameSink` (mpsc) → `spawn_proc`. New shape; the engine already has a
  tokio event channel to build on.
- **Discord audio format** — voice is **Opus 48 kHz stereo**; songbird decodes to
  48 kHz PCM. Downmix to mono + the usual resample to 16 kHz; the native-rate
  stored `spk-N.wav` is 48 kHz (rate-agnostic readers already handle this).
- **Identity by user ID (decided)** — following by **user ID** needs only
  `GUILDS` + `GUILD_VOICE_STATES` (non-privileged). User ID is the primary path;
  username→ID resolution (would need the *privileged* `GUILD_MEMBERS` intent /
  REST member search) is deferred / best-effort only.
- **Dynamic speaker set** — Discord participants join/leave **mid-call**, so
  speaker indices, `spk-N.wav` tracks, and `speaker_names` rows are created
  *during* the session (diarization assumed a fixed set discovered at the end).
  The store/UI must handle speakers appearing mid-session.
- **"Me" = followed user's Discord stream** (decided) — the configured identity
  marks which received stream is `is_me` → `Source::Me` (captured via the
  platform, noise-suppressed). No local mic, no self-dedupe. Depends on SSRC→user
  mapping resolving the followed user (reliable — their id is known up front). In
  the hosted future the requester isn't at the machine, but this still holds (Me
  is always *their* platform stream).
- **Integration replaces system-loopback** — a Discord session captures neither
  mic nor desktop locally: Me + per-speaker tracks all come from Discord; **no
  mixed `others.wav`** (avoids double-capturing the call). Capture mode gains a
  "Discord" option distinct from
  mic/system/both.
- **Clock/latency** — Discord PCM arrives ~tens of ms after the local mic; fine
  locally (same machine clock, wall-clock padding absorbs it), but cross-machine
  clock sync becomes real in the hosted future.
- **SSRC→user gaps** — mapping needs a `SpeakingStateUpdate`/client-connect event;
  a participant silent the whole call (or who joined before the bot) may be
  unlabeled until they speak — backfill names, fall back to "Speaker N" if never
  resolved.
- **Bot token is a secret in plaintext `config.json`** — like the remote-LLM key;
  acceptable precedent but a real credential → note in `docs/SECURITY.md` and
  consider keychain if demand appears.
- **Many-speaker UI/CPU** — enough distinct transcript colors for N speakers;
  live transcription of N streams is heavy → deferred (post-stop) transcription
  is the default for integration sessions (reuse Phase 25).
- **Consent + retention** — per-instance consent gate; honor minimal-retention;
  optional in-channel "recording started" message for transparency.
- **Heavy deps** — `serenity`/`songbird`/`opus` behind `discord`, out of the
  default build; confirm they coexist with the whisper/sherpa/llama toolchains.
- **Verification limit** — live Discord + DAVE needs a real bot + a live call;
  not headlessly testable (add to `verification-limits`).

### Cross-cutting / smaller
- macOS code-sign + notarize automation (needs Apple Developer account).
- ~~Multilingual UX~~ / ~~CUDA release builds~~ — **declined** (not wanted).
- Windows code-signing (Authenticode) so SmartScreen/managed machines don't
  block the binaries (CI step ready to wire once a cert/signing service exists).

---

## 9. Productionization & official release (Phases 32–35) — major initiative

Goal (June 2026): stabilize the app and prepare an **official public release**.
The stability audit (June 2026) found the app solid for the happy path but with
concrete crash/data-loss/hang gaps; this initiative closes them, adds CI gates so
they stay closed, and builds the release/distribution machinery.

**Decisions locked (June 2026):**
- **Versioning stays 0.2.x** — no 1.0 declaration; the release is "latest".
- **Multi-channel distribution**: GitHub Releases now; **Steam, Microsoft Store,
  maybe Mac App Store, possibly an own store** later. **Stores own updates on
  their channels** (they forbid self-updating binaries); only the GitHub /
  own-store channel self-updates.
- **Update mechanism = distribution-channel build seam.** A build-time channel
  id (github | steam | msstore | macappstore) + a **`self-update` Cargo
  feature** compiled only into GitHub/own-store builds: check the GitHub
  releases API (opt-out toggle), notify in-app, and on Windows swap the
  portable EXE via rename (running EXEs can be *renamed* but not overwritten:
  download new → rename running to `.old` → write new at original path →
  relaunch → clean up; `self-replace` crate). macOS stays **notify + link**
  until signing exists (Gatekeeper re-quarantines unsigned downloads).
- **Ship unsigned for now** (no Apple/Windows certs yet) — document the
  Gatekeeper right-click-open and SmartScreen "More info → Run anyway" paths;
  store channels mitigate later (MS Store signs for us, Steam's client is
  trusted). Wire signing into CI when certs exist (steps already gated).
- **Discord 30d/30e land BEFORE the release** (headline feature).
- **Order: 32 → 33 → 30d/30e → 34 → release; 35 (stores) can trail.**

### Phase 32 — Crash & data-integrity hardening ✅ DONE (June 2026)
Findings from the audit, impact-ordered; each lands with a test where testable.
All six sub-phases landed in one pass (32a–f below), plus clearing the four
pre-existing clippy warnings ahead of the `-D warnings` CI gate.
- **32a — SQLite robustness**: set `busy_timeout` (none today → concurrent
  db_loop + transcription writes can fail instantly with `SQLITE_BUSY`); make
  multi-statement write paths transactional; surface a corrupt/locked DB at
  startup as a visible error instead of a dead thread.
- **32b — WAV integrity**: stop swallowing finalize errors
  (`engine.rs` `let _ = w.finalize()`); finalize-on-drop guard so a panicking
  proc still writes the header; repair truncated WAVs on open (recompute data
  length from file size) so a crash mid-recording doesn't lose the audio.
- **32c — Engine thread panic safety**: only diarization is `catch_unwind`-
  wrapped today; a panic in `control_loop`/`db_loop`/`model_loop`/`play_loop`/
  `spawn_proc` workers dies silently and hangs the UI. Wrap them: log to
  `crash.log`, emit `Status::Error`, finalize the session.
- **32d — Atomic config writes**: `config.json` is written in place; crash
  mid-write corrupts it and load silently resets all settings. Write-temp +
  rename.
- **32e — WASAPI drain guard**: unchecked `pop_front().unwrap()`s in the
  loopback frame drain (`zord-capture/src/system.rs`) — a queue-underflow race
  is a crash on Windows.
- **32f — Runtime unwrap sweep**: reachable `unwrap()/expect()` in runtime
  paths across `zord-store`/`zord-overview`/`zord-summarize`/`zord-net`/GUI
  (incl. `SystemTime` unwraps, the LLM-cache `.expect` in engine.rs).

### Phase 33 — CI & quality gates ✅ DONE (June 2026)
- **33a ✅ — PR workflow** (`.github/workflows/ci.yml`): `cargo fmt --check`,
  `clippy --all-targets -D warnings`, `cargo test` (default features) +
  `cargo check --features discord` on every PR/push to develop+main, on
  macos-15 (Xcode 26, mirrors release.yml). The heavy native engines
  (`diarization`, `llm-local`, `llm-remote`, `parakeet`) are a weekly +
  manual-dispatch check matrix instead of blocking every PR (they're also
  fully built on every release tag; `encryption` skipped, as in release.yml).
- **33b ✅ — Coverage for untested crates**: added unit tests for the
  headless-testable logic — zord-core (speaker labels, enum parse
  round-trips, segment serde shape), zord-transcribe (model catalog/parse/
  URLs), zord-capture (byte→f32 PCM), zord-gui engine (sanitize_fts,
  pad_to_wallclock, smooth_level). Live audio/ASR stays manual per
  `verification-limits`.

### Phase 34 — Release readiness ✅ DONE (June 2026)
- **34a ✅ — Channel seam + update check**: `zord_core::DIST_CHANNEL` baked at
  compile time from `ZORD_CHANNEL` (github | steam | msstore | dev);
  `is_newer_version` (unit-tested); `zord_net::latest_github_release`;
  zord-gui **`self-update` feature** (github-channel builds only): launch
  check (opt-out `check_updates` setting), toast on hit, Settings → About
  shows version + channel + manual check + **Windows one-click
  download-and-install** (portable-EXE rename-swap via `self-replace`;
  Windows path compile-verified only). macOS = notify + open download page
  (unsigned downloads get re-quarantined, so no silent swap until signing).
- **34b ✅ — Docs pass**: README "Installing a release" (unsigned Gatekeeper /
  SmartScreen bypasses, update behavior per channel), Discord + per-app
  troubleshooting, release-feature line fixed; RELEASE.md channel table +
  asset-names-are-an-API warning + stale notes cleaned.
- **34c ✅ — Error-state polish**: mic-permission denial now carries an
  actionable hint per OS; model-download / no-device / DB failures verified
  to surface via Status::Error (32a/32c made the remaining silent paths
  visible).

### Phase 35 — Store distribution (scaffolded; publishing may trail)
- **Scaffold ✅ (June 2026)**: release.yml gained a `channel` dispatch input
  (github | steam | msstore) — store builds bake their channel id, OMIT
  `self-update`, carry the channel in artifact names, and upload as workflow
  artifacts for manual store submission. `discord` joined the release
  feature set.
- **35a — Steam**: steamworks depot config + upload pipeline (needs a Steam
  partner account).
- **35b — Microsoft Store**: MSIX packaging (store-signed — solves
  SmartScreen on that channel; needs a Partner Center account).
- **35c — Mac App Store / own store**: needs Apple Developer account; audit
  sandbox constraints (ScreenCaptureKit loopback under sandbox) before
  committing.

### Phase 37 — Audio compression for kept recordings ✅ DONE (June 2026)
Kept WAVs (~350 MB/hour/track) now age into **Opus-in-Ogg** (~14 MB/hour at
the default 32 kbps): `record → WAV (exact, crash-repairable) → after
`compress_after_days` (default 7; 0 = immediately; blank = never) → .opus →
deleted at the retention limit`. Every consumer keeps working — replay
(page-granule seek + 80 ms pre-roll), re-transcribe (streaming opus branch in
`transcribe_wav_file`), diarize, merged export (`mix_tracks`) — via the
extension-dispatching `read_audio_*` readers and an opus-aware
`resolve_track`. The engine sweep (`DbCmd::CompressAudio`, visible/cancellable
job, 90 s after startup then 6-hourly) encodes to `.partial`, **verifies the
decoded length against the WAV header, promotes, and only then deletes**;
"Compress all kept recordings now" (Settings → Files) handles existing
libraries. Encoder detail caught by the verify test: resampling encodes flush
the resampler's latency tail with silence and end-trim via the final granule,
so durations match exactly. Deps: `opus2` (libopus — shared with songbird) +
pure-Rust `ogg`, in the default build. Quality presets: 24/32/48 kbps.
Spec: `docs/superpowers/specs/2026-06-10-audio-compression-design.md`.

### Phase 36 — Premium UX pass
- **36a ✅ DONE (June 2026) — UI polish + theming.** Token layer in
  `style.css` (spacing/radius/elevation/motion/focus + color roles split:
  `--accent` interactive (themable, defaults to the old cyan), `--danger`
  fixed record/destructive red, `--me`/`--others` channels (themable, with
  computed `-fg` pairs), `--discord` fixed); shared button-state primitives
  via selector groups (hover/press/focus-visible/disabled — no markup churn);
  pop-in entrances on menus/dialogs/toasts, elevation tokens, themed
  scrollbars, session-action hover fades, gradient+glow Record buttons; fixed
  latent undefined `var(--fg)`/`var(--rec)` bugs. **Theme panel**: 6 accent
  presets + custom hex for accent/Me/Others, luminance-picked readable
  foregrounds, live apply via root custom properties, reset. Default palette
  pixel-identical. Spec:
  `docs/superpowers/specs/2026-06-10-ui-polish-theming-design.md`.
- **36b ✅ DONE (June 2026) — First-run guided setup wizard.** Fully-skippable
  overlay shown until completed/skipped (`setup_complete` setting),
  re-runnable from Settings → About. Steps adapt to intent: welcome → intent
  cards (meetings / Discord / voice + low-power, tuning real defaults via the
  unit-tested `apply_intents`) → mic device + **live level test** (new
  `RecorderCmd::MicTestStart/Stop`: meter events, no session; the OS mic
  prompt fires here) → macOS Screen Recording walkthrough (System Settings
  deep-link) → recommended model + in-wizard download → embedded Discord
  setup (reuses `IntegrationsSettings`) → ready summary with the right CTA.
  Lives in `crates/zord-gui/src/wizard.rs`; styled from the 36a tokens.
  Spec: `docs/superpowers/specs/2026-06-10-setup-wizard-design.md`.

### Phase 38 — Voiceprints: cross-session speaker identity ✅ DONE (June 2026)
Zord now remembers voices across sessions. Per-cluster speaker embeddings
(the sherpa-onnx extractor we already ship — a few-KB vector, never audio)
are persisted in a local **voiceprint library** and matched by cosine
similarity: standard threshold 0.72, 0.05 runner-up margin, ≥3 s speech
floor to avoid enrolling on fragments, rolling 8 samples per person to stay
current. Presets: strict 0.78 / standard 0.72 / relaxed 0.66.
Enrollment is **implicit**: renaming a speaker in any session enrolls them;
Discord sessions auto-enroll from their ground-truth per-participant tracks
post-stop. After every diarization pass the engine matches clusters against
the library and renames any match automatically ("Recognized Alex." notice).
New **Speakers** rail view (under Overview/Search, `voiceprints` builds
only) shows all known people as person cards with inline rename, per-session
appearance chips, and **Forget this voice** (per-person removal). Settings →
Speakers adds a "Voice identification" block: opt-in toggle (fires the
one-time consent dialog; `voiceprints_consented_at` timestamp), match
strictness picker, and **Forget all voices**. The runtime toggle
(`voiceprints_enabled`, default off) guards matching and enrollment; cluster
embeddings are persisted per-session regardless so the store is ready once
the user opts in. The entire capability sits behind the **`voiceprints`**
Cargo feature (requires `diarization`) as a build-time kill-switch.
Implemented and gate-verified; live end-to-end testing pending.
Legal posture: `docs/voiceprints-legal.md`.
Spec: `docs/superpowers/specs/2026-06-10-voiceprints-design.md` ·
Plan: `docs/superpowers/plans/2026-06-10-voiceprints.md`.

### Phase 39 — Faithful compression + the living Overview document ✅ DONE (June 2026)
Replaces the Phase 26 extract→reconcile ledger (which minted random projects
and dumped item piles) with two honest layers. **Compression** is now pure
line-by-line condensation: the rewritten `compress_prompt()` keeps speaker
labels and utterance order, rewrites each line to its shortest faithful form,
may drop pure-filler lines, and is forbidden from adding structure, action
items, or summaries — the condensed text *is* the conversation, just dense.
A **"Re-compress all sessions"** action (Settings → AI) redoes history with
the new prompt. **The Overview** is now ONE living markdown document
(`app_meta.overview_doc`, organized by `##` project sections) that the AI
edits via `zord_overview::update_document` — folding each meeting's condensed
transcript in, tracking `- [ ]`/`- [x]` items with owners, moving stale
content to a dated `## Archive`, pruning archive entries older than 30 days,
and preserving the user's own edits (the doc is fully user-editable). Folds
are tracked per session (`sessions.overview_folded_ms` stamp — a newer fold
can never hide an older unfolded session); a 20 % sanity floor rejects
destructive LLM rewrites; `overview_doc_prev` gives one-step "Revert last AI
update"; an optimistic re-read retries once if the user edited mid-fold. The
auto chain (toggle `overview_auto`, default on) runs compress→fold after each
session's transcript is final; "Update now" folds anything unstamped. UI:
rendered markdown (pulldown-cmark, raw HTML escaped — never executed) ⟷ raw
editor toggle. Chat's cross-meeting scope grounds on the document (fallback:
compressed digests). The old ledger pipeline (extract/reconcile/synthesize,
ledger UI, its prompts) is deleted; `projects`/`project_items` tables remain
inert. Implemented and gate-verified; live LLM pass pending.
Spec: `docs/superpowers/specs/2026-06-11-living-overview-design.md` ·
Plan: `docs/superpowers/plans/2026-06-11-living-overview.md`.

### Phase 40 — Find in session ✅ DONE (June 2026)
`FindBar` component toggled by a "Find" button in the session toolbar (and a
matching button on the Live-view header). Esc or × closes it and clears
highlights. Client-side over the loaded transcript: `find_hits(segments,
query)` — case-insensitive substring, skips id-less segments, unit-tested (6
tests). Hit count badge ("N of M"), ▲/▼ prev/next buttons + Enter/Shift-Enter
cycling; active hit scrolled into view by reusing the existing
`highlight_seg`/`scrollIntoView` mechanism; all other hits get a soft
`find-hit` background class; closing reverts all highlights. Works on both
saved-session and Live views. Cmd/Ctrl-F was not added — no existing global
keydown pattern exists in the Dioxus desktop app; the button is the entry
point, consistent with every other floating panel in the app.

### Phase 41 — Parallel post-stop transcription ✅ DONE (June 2026)
New `Settings::transcribe_workers: u32` (default 1, clamped 1..=4). Effective
parallelism = `min(transcribe_workers, tracks_present)` — a 2-track session
never spins more than 2 workers. `workers == 1` keeps the existing sequential
path byte-for-byte unchanged (zero-risk default). `workers > 1` uses
`std::thread::scope` + a shared `Mutex<VecDeque>` work queue; workers send
`(speaker, Segment)` over an mpsc channel to the calling thread, which
performs all store inserts and throttled GUI pushes (store stays
single-threaded). Cancel/keep-partial semantics preserved: segments already
received by the drain loop are committed even if the token fires mid-run. Both
backends (`WhisperBackend` via `WhisperInnerContext`; `ParakeetBackend` via
`OfflineRecognizer`) already carry `unsafe impl Send + Sync` in their upstream
crates, so no additional unsafe code was needed. Settings UI in
Transcription → "Parallel transcription workers" select (1–4).

### Phase 42 — Session timeline: multi-track audio reconstructor ✅ DONE (June 2026)
Shipped a collapsible **timeline panel at the bottom of the session view**
(toggled from the toolbar) that reconstructs a session's audio and makes it
scrubbable — the diagnostic tool that lets you see every dropped word without
listening.

What shipped (Phases 42a–42d):
- **One lane per track** (`me`, `others`, Discord `spk-N`) with checkboxes,
  speaker-name labels, and per-lane colors (`--me`/`--others` + spk palette).
- **Amplitude graph per lane**: 1 500-bucket peaks computed streaming via
  `zord_audio::compute_track_peaks` (WAV + Opus); results cached per session.
  Others lane colored by diarized speaker spans (`bucket_speakers`).
- **Stacked and merged/overlay modes** — talk-over regions visible in merged.
- **Scrub + play**: click/drag playhead; `MixReader` streams the N-track 48 kHz
  mix from any offset; pause/resume; transcript auto-highlights under playhead.
- **Diagnostics**:
  - *Speech flags*: per-bucket RMS ≥ relative floor (`speech: Vec<bool>` in
    `TimelineLane`; computed alongside peaks in the same streaming pass).
  - *Untranscribed-speech markers*: `untranscribed_buckets()` pure fn —
    speech-active runs of ≥ 2 buckets (~5 s) not covered by a transcript
    segment draw thin red ticks at the lane top (`tl-gap`). Source-aware:
    `me` lane checks `Source::Me`, `others` any `Source::Others`, `spk-N`
    checks the matching speaker index.
  - *Clipping indicators*: buckets with peak ≥ 0.985 draw a red triangle at
    the lane bottom (`tl-clip`). Both marker types trigger a header legend line.
- **Speed**: `PlayCmd::TimelineSpeed(f32)` → `sink.set_speed(speed)`; 1×/1.5×/2×
  cycle button. Position ticks scale elapsed wall-time by speed; each speed
  change or pause flushes the accumulator so the scrubber stays accurate.
  NOTE: rodio `set_speed` affects pitch — accepted for 1.5×/2× preview use.
- **Silence skip** (GUI-driven): toggle button; `use_effect` on `timeline_pos`
  calls `silence_skip_target()` and fires `TimelineSeek` when the playhead is
  in a silent run > 2 s. Loop guard: only fires when the new target is > 500 ms
  ahead and hasn't been fired yet; clears on playback stop.
- **Range selection**: Shift-drag on the graph creates a selection (start/end
  ms signals; translucent overlay rect). Action chip row: **Export clip** /
  **Re-transcribe** / dismiss.
  - *Export clip*: `DbCmd::ExportClip` — `MixReader` streams [start, end),
    writes a 16-bit 48 kHz mono WAV to exports as `<id>-clip-<s>-<e>.wav`.
  - *Re-transcribe selection*: `DbCmd::RetranscribeRange` — for each track:
    `read_audio_slice_ms`, resample to 16 kHz, `transcriber.transcribe(&samples,
    source, start_ms)` (raw slice without extra VAD pre-pass — a few minutes of
    audio is fine; timestamps are session-absolute via `base_offset_ms`).
    `store.delete_segments_in_range(session_id, start, end)` deletes segments
    whose `t_start_ms` ∈ [start, end); new segments inserted, transcript refreshed.

Deferred stretch items: per-lane solo, per-lane gain in the mix, loop-a-selection.
Live verification pending (headless test environment cannot exercise audio I/O).

### Phase 43 — Production readiness + workflow depth ✅ DONE (June 2026)
The pre-release punch list (user-confirmed June 2026), in sub-phases:
- ✅ **43a — release hygiene.** Proprietary `LICENSE` shipped; `Cargo.toml`
  `license-file` set; GitHub Actions bumped (checkout/upload/download v4→v5,
  deadline June 16 met); `encryption` feature added to FEATURES + CLI_FEATURES
  (Windows runner: Perl+NASM for vendored OpenSSL). Private-repo updater
  decision documented: public releases-only repo recommended; decision still
  pending final call.
- ✅ **43b — signing, wired + dormant (no certs yet).** Windows Authenticode CI
  step gated on `HAS_WIN_SIGNING` secret (mirrors macOS pattern); `docs/SIGNING.md`
  runbook added (cert purchase, secret names, activation steps). Signing is
  wired but dormant — certs not yet purchased.
- ✅ **43c — "Report a problem" bundle.** Settings → About: exports diagnostic
  zip (logs, config with bot-token/API-key/passphrase fields REDACTED, system
  info, recent session metadata sans transcripts). `DbCmd::ExportDiagnostics`
  wired end-to-end.
- ✅ **43d — voiceprint management depth.** Speakers view: **Merge into…**
  (rename-merge), per-session **"wrong person" unlink** on appearance chips,
  match-confidence score in "Recognized" notice + chip tooltip.
- ✅ **43e — Overview checkboxes clickable in rendered view.** `toggle_nth_task`
  pure helper (TDD, 10 tests — fenced-code skip, case-insensitive, nth indexing,
  all-bullets). Eval bridge wired: JS click → `dioxus.send(idx)` → Rust recv
  loop → toggle + `DbCmd::SaveOverviewDoc`. Generation counter guards re-render
  loop. Bridge proved reliable — degrade clause not invoked. Live verification
  pending (headless test env).
- ✅ **43f — batch session selection.** Cmd/Ctrl-click toggles, Shift-click
  range-selects; selected rows styled; action bar ("N selected — Delete / Clear")
  with confirm; `DbCmd::DeleteSessions(Vec<String>)` batch delete.
- ✅ **43g — per-app capture surfaced.** App-picker surfaced in the recording
  flow (app-select near Record control when capture mode includes system audio;
  "All system audio" default); Settings entry retained for advanced use.
  Live verification pending (requires live audio hardware + running apps).

### ✅ Phase 44 — Knowledge-base export (DONE)
Mirror Zord's knowledge into a user-chosen folder as plain markdown, so
Obsidian/Logseq/any-PKM picks it up for free. Setting `kb_export_dir`
(blank = off); the engine mirrors on change, debounced:
`Overview.md` (the living document, on every save/fold/revert) and
`sessions/<YYYY-MM-DD>-<sanitized-title>-<short-id>.md` (metadata YAML
front-matter + summary + condensed transcript or transcript, on
summarize/compress/re-transcribe/rename). One-way export (Zord remains
the source of truth). Filenames stable per trailing-8-char short-id;
renames move the file; deletions remove it (single + batch delete).
Settings → Files, Knowledge-base export block with path input and
"Export everything now" button. All write sites inline (no db_tx available
at summarize/compress/fold/retranscribe workers); `DbCmd::KbMirror` kept
for future routing; `DbCmd::KbExportAll` for the button. Tests: sanitizer,
short-id, session markdown renderer (golden), rename-moves-file,
delete-removes-file. Gate: fmt + clippy + tests all pass. Live
verification pending (requires a running app).

### Phase 45 — Semantic search ✅ DONE
Meaning-based search beside FTS: BGE-small-en-v1.5 (384-dim, MIT, ~24 MB
ONNX) embeds ~250-word transcript chunks into a `chunk_embeddings` table
(f32 LE blob, CASCADE delete, same pattern as voiceprints); brute-force
cosine (top-20, floor 0.35) is plenty at this scale. Search view gains a
Keyword/Semantic mode toggle (only in `semantic` builds); empty-index hint
+ "Build semantic index" button in Settings → AI. Model files downloaded
via zord-net proxy-aware agent from Xenova/bge-small-en-v1.5 on first use.
Auto-chains after post-stop transcription and re-transcription. Behind
`semantic` Cargo feature (fastembed 5.x + ort ONNX Runtime).
Note: brute-force scale ceiling (~tens of thousands of chunks); live
end-to-end embedding verification pending first model download.

### Business split — strategic direction (recorded June 2026)
The app will eventually fork into a **premium / business-user tier**
(proprietary license already in place). Premium candidates: platform
integrations beyond Discord (Teams/Zoom identity), semantic search,
knowledge-base export, voiceprints at team scale, policy/admin controls,
store distribution. Design new features with a clean free/premium seam in
mind (Cargo features + license gating later); no gating implemented yet.

### Phase 46 — Conversation analytics: "Meeting DNA" ✅ DONE
**Hard constraint (user, June 2026): NO LLM anywhere in this phase — every
metric is pure-fn computation over data Zord already has (fast, exact,
unit-tested). Any future metric that would need an LLM gets discussed
first, not built.** Real numbers from data only Zord has (per-speaker
audio + cross-session identity, all local):
- **Per-session stats**, computed post-transcription into a cached
  `session_stats` store (recomputed on load / re-transcribe / re-diarize):
  talk-time share per speaker (diarized spans / integration track speech
  flags), interruption & talk-over counts (the timeline's overlap data),
  longest monologue, words-per-minute per speaker, question density
  (`?`-terminated lines), silence ratio, meeting length vs speech length.
  `compute_stats` lives in `zord-core` — pure fn, 13 unit tests, zero
  panics on degenerate input.
- **Store cache:** `session_stats (session_id PK, json, computed_at)`
  table in zord-store; 4 store tests (absent/roundtrip/upsert/cascade).
- **Engine:** `DbCmd::LoadStats` + `Event::Stats`; hooks after
  `DbCmd::Load`, `post_transcribe_inner`, and `apply_diarization`.
- **Surfaces:** "Stats" toolbar toggle button (closes on session switch);
  `StatsPanel` component with meeting-length header, speech% ratio,
  standout-metric one-liner (pure-fn heuristic), per-speaker rows with
  talk-share progress bar + "32% · 4:20 · 142 wpm · 9 Q · longest 3:40
  · 4 interruptions". Speaker identity colors from existing palette.
  Empty state: "Transcribe first." Un-diarized sessions show
  me/others rows honestly.
- **Cross-session trends** (row is there for Phase 48 queries): the
  `session_stats.json` blob feeds Phase 48 person profiles unchanged.
- Live verification pending (headless-only build environment).
  Commit: `feat(gui): conversation analytics — pure-fn Meeting DNA stats card (Phase 46)`.

### Phase 47 — Voice bookmarks: "mark that" ✅ DONE
Say a trigger phrase while recording → a bookmark drops at that moment.
- **v1 trigger = live-transcript matching** (no new model): while live
  transcription runs, watch finalized segments for any configured phrase
  (case/punct-insensitive containment). Honest constraint: requires live
  transcription on; documented in the settings panel. KWS (sherpa-onnx
  keyword spotting, works without live transcription) recorded as stretch.
- **Configurable, MULTIPLE phrases**: Settings → Recording gains a
  "Bookmark phrases" editor (list add/remove; defaults "mark that",
  "bookmark this"); stored as `bookmark_phrases: Vec<String>`.
- **Storage/UI**: `bookmarks (session_id, t_ms, phrase)` table; a marker
  lane in the timeline panel (click → seek) + a bookmark chip row above
  the transcript (click → scroll transcript); a manual "Bookmark" button
  while recording (ghost style, sidebar foot near mute buttons). The
  trigger segment is bookmarked at `t_start_ms - back_offset_ms`
  (default 10 s, configurable in Settings → Recording).
- ✅ `matches_bookmark_phrase` pure fn + 2 tests (zord-config)
- ✅ `bookmarks` table + `add_bookmark` / `bookmarks(session_id)` + 4
  tests including ON DELETE CASCADE (zord-store)
- ✅ `Event::Bookmarks`, `RecorderCmd::DropBookmark`, phrase trigger in
  both `run_session` and `run_integration_session` (engine.rs)
- ✅ `BookmarkBar` chip row + `BookmarkSettings` section in Settings →
  Recording; timeline bookmark tick lane (diamond markers, click → seek)
- Gate: fmt + clippy (workspace + features) + all tests green

### Phase 48 — Person profiles ✅ DONE
The Speakers view grows person pages: clicking a voiceprint's name opens a
profile detail pane (back button returns to the card grid).

- ✅ `ProfileMeeting` + `ProfileData` structs in `zord-gui/src/profile.rs`
- ✅ `DbCmd::LoadProfile(i64)` + `Event::Profile(ProfileData)` wired into the
  engine db thread (`load_profile_and_emit`)
- ✅ **Stats-key mapping**: `speaker_names WHERE voiceprint_id = ?` returns the
  0-based speaker index; the matching `SpeakerStats` row has key `"spk-N"`.
  This holds for both standard and integration sessions (the `is_me` flag
  is set on the row when `speaker == me_speaker` but the key is still
  `"spk-N"`).  Missing-stats rows fall through to zeroes (honest cheap
  choice: avoids a full segment reload per session for infrequently-viewed
  profiles; the meeting is still listed with its title).
- ✅ `Store::speaker_idx_for_voiceprint(session_id, voiceprint_id)` added to
  `zord-store` (replaces earlier direct-conn approach)
- ✅ `overview_items_for(doc, name)` pure fn — unchecked `- [ ]` lines,
  case-insensitive name match, caps at 20; 7 unit tests
- ✅ `tfidf_topics(person_lines, other_lines, k)` pure fn — lowercase
  alphanumeric tokenize ≥3 chars, 50-word stoplist, TF × ln(1 + N/df) IDF,
  top-6 terms; 6 unit tests (13 new tests total in profile module)
- ✅ UI (speakers.rs): name → clickable button → `DbCmd::LoadProfile` →
  loading spinner → `ProfilePane` (header, meetings list with session links,
  open items, topics chips); back button clears the signal; leaving the
  Speakers view clears the profile signal
- ✅ CSS: token-based, mirrors the speaker-card styling family; new classes:
  `.profile-pane`, `.profile-header`, `.profile-meetings`, `.profile-meeting-row`,
  `.profile-section`, `.profile-topic-chip`, `.profile-loading`,
  `.speaker-name-btn`
- ✅ Gate: fmt + clippy (workspace + features) + all tests green
- Caveat: live verification pending (headless build only); sessions with no
  cached `session_stats` row show zero talk_share/interruptions — they will
  populate next time the user opens the session (which triggers
  `compute_and_cache_stats`)

### Phase 49 — Sentiment "moments" (planned — audio-first, in the Timeline)
APPROVED (June 2026) with the design pivoted to **audio prosody, no LLM**,
and the markers **built into the Timeline panel** as a moments lane:
- **Model: SenseVoiceSmall via sherpa-onnx** (the runtime we already ship
  for diarization/voiceprints/Parakeet — a model download, not a new
  dependency). One small model emits per-utterance **emotion labels** and
  **audio events** (laughter, applause, crying, coughing) from tone of
  voice — signals text analysis fundamentally can't see.
- **Conservative rendering (the accuracy concern, addressed):**
  - *Event markers always*: laughter/applause are near-unambiguous —
    rendered as their own tick style on the Timeline (click → seek). "The
    room laughed here" is the most trustworthy sentiment signal there is.
  - *Emotion ticks only on persistence*: an emotion mark renders only when
    a strong non-neutral label holds across N consecutive utterances
    (const, commented) — no continuous arc, nothing speculative.
  - Stretch: cross-check against a small ONNX text classifier (ort is
    already in-tree via Phase 45); render emotion ticks only when audio
    and text agree.
- **Pipeline:** post-transcription job (cancellable) runs SenseVoice over
  the per-speaker tracks → `moments (session_id, t_ms, kind, speaker)`
  table → Timeline moments lane + a small list in the session view.
  Speaker attribution comes free from per-track processing.
- **Gate task: verify the SenseVoiceSmall weights' license** for
  commercial use per the model-licensing policy BEFORE wiring downloads
  (code is Apache-2.0; weights' terms must be confirmed — if they fail
  the policy, fall back to a commercially-licensed SER model or hold).

**Declined (June 2026): pre-meeting briefing** — composing context for a
10-person meeting whose attendees each carry separate meeting histories
exceeds practical context windows; the digest would lose the thread.
Revisit only if a per-person retrieval design (Phase 48 + semantic
retrieval) proves itself first.

---

## 10. Open questions to revisit during build
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

## 11. Sources (research, May 2026)
- whisper-rs (bindings, GPU features): https://github.com/tazz4843/whisper-rs · https://crates.io/crates/whisper-rs
- screencapturekit crate (macOS system+mic audio): https://crates.io/crates/screencapturekit · https://github.com/svtlabs/screencapturekit-rs
- cpal & WASAPI loopback caveats: https://github.com/RustAudio/cpal · issues #251/#476/#516
- ruhear (evaluated, not adopted): https://github.com/aizcutei/ruhear
- Dioxus releases (0.7.x current): https://github.com/dioxuslabs/dioxus/releases · https://docs.rs/crate/dioxus/latest
- Whisper large-v3-turbo accuracy/speed: https://huggingface.co/openai/whisper-large-v3-turbo · https://whispernotes.app/blog/introducing-whisper-large-v3-turbo
