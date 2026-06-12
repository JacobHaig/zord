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
event, never on the UI. Retained WAVs (ONE per channel, at the capture
device's native rate since Phase 25d; models derive 16 kHz on the fly) are
wall-clock aligned (silence-padded at that rate), so a segment's `t_start_ms`
maps 1:1 to sample offset (`ms × rate/1000`) — per-line replay and
re-transcription stay exact at any rate.

**Timeline subsystem (Phase 42, done):** `crates/zord-gui/src/timeline.rs` —
collapsible bottom panel; `TimelineLane { peaks, speech, … }` computed by
`zord_audio::compute_track_peaks` (streaming, 1500-bucket peaks + per-bucket
RMS speech flags); `MixReader` streams N-track 48 kHz mix for scrub/play;
diagnostics: `untranscribed_buckets` + `clipping_buckets` pure fns;
speed (`PlayCmd::TimelineSpeed`), silence-skip (GUI-driven `use_effect`),
range selection with export-clip + re-transcribe (`DbCmd::ExportClip` /
`DbCmd::RetranscribeRange`); `store.delete_segments_in_range` for range rewrite.
Related: [[capture-design]], [[dx-bundling-gotchas]].
