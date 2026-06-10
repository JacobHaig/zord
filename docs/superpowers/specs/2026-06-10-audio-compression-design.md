# Audio compression for kept recordings — design

**Date:** 2026-06-10 · **Status:** approved (defaults confirmed: 7 days, 32 kbps)

## Problem

Kept audio is mono 16-bit WAV per track (~346 MB/hour/track, 2+ tracks per
session) — multi-gigabyte libraries in days. Compress aged recordings to a
high-quality, low-storage format while keeping **every** consumer working:
per-line replay, re-transcribe, diarize, merged export.

## Decisions (user-confirmed)

- **Codec: Opus** (in standard Ogg, `.opus` extension) — the speech codec;
  ~14 MB/hour/track at the default quality (~96% smaller).
- **Timing: age-based**, extending retention:
  `record → WAV (exact, crash-repairable) → after N days compress → at
  retention limit delete`. Defaults: **N = 7** days; `0` = compress as soon
  as a session has ended; off = never.
- Quality presets: Space-saver 24 / **Standard 32 (default)** / High 48 kbps.
- **"Compress all kept recordings now"** button (Settings → Files) for
  existing libraries — same sweep, ignoring age; reports bytes reclaimed.
- In the **default build** (no feature flag): storage-format support must not
  fragment. Deps: `opus` (libopus, compiled locally — same C toolchain we
  already require; reuse the same sys crate songbird pulls when possible) +
  the pure-Rust `ogg` container crate.

## Architecture

### zord-audio: `compress` module (encode + decode, streaming)

- **Encode** `compress_wav_to_opus(src, dst, bitrate) -> Result<()>`:
  streams the WAV in blocks, resamples to 48 kHz mono when the device rate
  differs (`MonoResampler::to_rate`), encodes 20 ms frames (960 samples),
  muxes Opus-in-Ogg (OpusHead with encoder pre-skip, OpusTags, granule
  positions per spec). Writes `dst` with a `.partial` suffix; caller renames.
- **Decode core** `decode_opus_blocks(path, on_block)` — streaming (an hour
  must never be slurped whole); pre-skip honored.
- **Readers**: `read_audio_mono_f32 / read_audio_slice_ms / read_audio_mono_16k`
  dispatch on extension (`wav` → existing readers, `opus` → decode path).
  Slices use page-granule seeking with the standard 80 ms pre-roll so replay
  stays snappy on hour-long files. Existing `read_wav_*` stay (crash-repair
  applies to WAV only).
- **Merged export**: `mix_wavs` generalizes its per-track reader to wav/opus
  sources (rename to `mix_tracks`; alias kept if needed).

### zord-config

- `resolve_track(prefix, role)` learns `.opus`: prefer `role.wav`, else
  `role.opus` (both layouts). `apply_retention` already deletes whole
  folders — unchanged.
- Settings: `compress_after_days: Option<u32>` (default `Some(7)`),
  `compress_quality: String` ("space" 24k / "standard" 32k / "high" 48k,
  default "standard").

### Engine (zord-gui)

- `DbCmd::CompressAudio { ignore_age: bool }` → db_loop spawns a supervised,
  job-registered worker ("compress" in the jobs panel, cancellable between
  tracks). The worker:
  1. cleans up stale `*.partial`,
  2. lists **ended** sessions with kept audio from the store (never the
     live session), old enough unless `ignore_age`,
  3. per track (me/others/spk-N; folder layout and legacy flat): encode →
     **verify** (decoded duration from the final granule vs WAV duration,
     ±1%) → rename `.partial` → `.opus` → delete the WAV,
  4. notices the total bytes reclaimed; emits `Event::Sessions` refresh.
- **Scheduling**: a thread in `Engine::spawn` sends
  `CompressAudio { ignore_age: false }` ~90 s after startup and every 6 h.
- Transcription/diarization read through the dispatching readers, so
  deferred transcription and re-runs work on compressed sessions (input is
  very-slightly-lossy — documented, not blocked).

### UI

- Settings → Recording (retention section): "Compress kept audio after N
  days" (number + Never, like retention) + quality select with size hints.
- Settings → Files: **"Compress all kept recordings now"** button.

## Edge cases

- Active recording's folder: excluded (only ended sessions).
- Crash mid-encode: `.partial` ignored by all readers, cleaned next sweep;
  WAV untouched until verify+rename complete.
- Mixed sessions (some tracks already opus): per-track skip if `.opus`
  exists.
- The diarizer's temp `<id>.others.wav` flow: unchanged (operates pre-sweep
  or decodes via dispatch when re-run later).
- WAV header repair: WAV-only path, untouched.

## Testing

- Roundtrip: encode a synthetic WAV (tone + silence) → decode → duration
  within one frame, energy where expected.
- Slice: `read_audio_slice_ms` on opus matches the wav slice within
  tolerance (start-aligned, correct length).
- Sweep unit test with temp dirs: partial cleanup, verify-then-delete order,
  skip-live, legacy flat layout.
- `resolve_track` opus fallback test.
- Full gate + manual pass: set N=0, record, watch it shrink, replay a line,
  re-transcribe, merged export.
