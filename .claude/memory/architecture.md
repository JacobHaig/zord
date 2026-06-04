---
name: architecture
description: Zord workspace crate layout, core traits, and the GUI engine threading model
metadata:
  node_type: memory
  type: project
---

Zord is a Cargo workspace of focused crates; the app is fully local (no server).

Crates: `zord-core` (shared types: `Source` Me/Others, `Segment`, `Session`),
`zord-audio` (resample→16kHz mono + VAD + WAV), `zord-capture` (`Microphone`
cpal + `SystemAudio` ScreenCaptureKit/WASAPI), `zord-transcribe`
(`TranscribeBackend` trait → Whisper always, Parakeet under feature),
`zord-store` (SQLite + FTS5), `zord-config` (settings + paths), `zord-export`
(MD/SRT/JSON), `zord-summarize` (llama.cpp, feature-gated), `zord-web` (axum
localhost dashboard), `zord-app` (`zord` CLI), `zord-gui` (Dioxus 0.7 desktop).

**Why:** clean separation lets heavy/optional engines (Parakeet, llama, SQLCipher)
be feature-gated without bloating the default build. See [[feature-flags]].

**How to apply:** add transcription engines behind `TranscribeBackend`; the
GUI's `engine.rs` runs the recorder on dedicated threads because cpal/SCStream
are `!Send` — a control thread owns the streams, plus db / model / summarize /
playback (rodio) worker threads, all emitting `Event`s over a tokio channel
drained into Dioxus signals. New long-running work = a new worker thread +
event, never on the UI. Retained WAVs are wall-clock aligned (silence-padded),
so a segment's `t_start_ms` maps 1:1 to sample offset (`ms × 16` at 16 kHz) —
this is what makes per-line replay exact.
Related: [[capture-design]], [[dx-bundling-gotchas]].
