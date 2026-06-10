# Audio Compression (Opus) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Age-based compression of kept recordings (WAV → Opus-in-Ogg, ~96% smaller) with every consumer — replay, re-transcribe, diarize, merged export — working on compressed sessions, plus a "compress now" action for existing libraries.

**Architecture:** zord-audio gains a `compress` module (streaming Opus encode/decode + extension-dispatching `read_audio_*` readers); `resolve_track` learns `.opus`; the engine gains a `DbCmd::CompressAudio` sweep worker (job-registered, verify-then-delete) scheduled ~90 s after startup and every 6 h; settings/UI add `compress_after_days` (default 7) + `compress_quality` (default 32 kbps).

**Tech Stack:** `opus2 0.4` (same libopus songbird uses — one shared native build), `ogg 0.9` (pure Rust container), hound, existing `MonoResampler::to_rate`.

Spec: `docs/superpowers/specs/2026-06-10-audio-compression-design.md`.
Commits to `develop` per task; full gate before each.

---

### Task 1: zord-audio `compress` module (encode, pull-decoder, dispatch readers) + tests

**Files:**
- Modify: `crates/zord-audio/Cargo.toml` — add `opus2 = "0.4"`, `ogg = "0.9"`
- Create: `crates/zord-audio/src/compress.rs`
- Modify: `crates/zord-audio/src/lib.rs` — `mod compress;` + re-exports

Key constants: 48 kHz mono; 20 ms frames (960 samples); slice seek-back margin
`2 × 48_000 + 3_840` (page resync + the spec's 80 ms pre-roll).

Ogg granule scheme (RFC 7845): per audio packet `granule += 960`; the final
packet's granule is set to `pre_skip + total_input_samples` (end-trim) and
`pre_skip` = encoder lookahead, written in OpusHead; the decoder skips
`pre_skip` samples and stops at `final_granule − pre_skip`.

Public surface (consumed by later tasks):

```rust
pub fn opus_bitrate(quality: &str) -> i32; // "space"→24_000, "high"→48_000, else 32_000
pub fn compress_wav_to_opus(src: &Path, dst: &Path, bitrate: i32) -> Result<()>;
pub struct OpusBlocks { /* pull-based 48k mono block reader */ }
impl OpusBlocks {
    pub fn open(path: &Path) -> Result<Self>;
    pub fn sample_rate(&self) -> u32; // always 48_000
    pub fn next_block(&mut self) -> Result<Option<Vec<f32>>>;
    pub fn total_samples(&self) -> Option<u64>; // from the final granule, minus pre-skip
}
pub fn read_audio_mono_f32(path) -> Result<(Vec<f32>, u32)>;   // (samples, rate)
pub fn read_audio_mono_16k(path) -> Result<Vec<f32>>;
pub fn read_audio_slice_ms(path, start_ms, end_ms) -> Result<(Vec<f32>, u32)>;
```

Dispatch rule: extension `opus` → decode path; anything else → the existing
`read_wav_*` (which keep crash-repair). NOTE: the existing
`read_wav_mono_f32` returns `Vec<f32>` without a rate — `read_audio_mono_f32`
returns the rate too (callers need it for opus's fixed 48 k).

Tests (same file, `#[cfg(test)]`):
- `roundtrip_preserves_duration_and_energy` — 3 s synthetic (1 s silence,
  1 s 440 Hz tone, 1 s silence) at 44_100 Hz WAV → compress → `OpusBlocks`
  decode: duration within one frame of 3 s × 48 k; RMS of the middle second
  > 10× the RMS of the outer seconds.
- `slice_matches_wav_slice` — slice [1200 ms, 1400 ms) from both files;
  decoded slice has the expected length at its native rate and tone-level
  energy.
- `opus_bitrate_presets` — mapping + unknown → 32_000.

- [ ] Step 1: deps + module skeleton + tests written (red)
- [ ] Step 2: implement encode (stream WAV blocks → `MonoResampler::to_rate(.., 48_000)` when needed → 960-sample frames, zero-padded final frame → `Encoder::new(48000, Channels::Mono, Application::Voip)` + `set_bitrate(Bitrate::Bits(bitrate))` + `get_lookahead()` for pre-skip → `ogg::PacketWriter` (OpusHead page, OpusTags page, audio packets, end-trimmed final granule) → write to `dst`)
- [ ] Step 3: implement `OpusBlocks` (PacketReader; parse OpusHead for pre-skip; `Decoder::decode_float` per packet; skip pre-skip; clamp at `total_samples`) and the three `read_audio_*` (slice path: `seek_absgp` to `target − margin`, resync on a `last_in_page` packet granule, decode-discard to the window)
- [ ] Step 4: `cargo test -p zord-audio` green
- [ ] Step 5: commit `feat(audio): Opus compression — streaming encode/decode + format-dispatching readers`

---

### Task 2: zord-config — `.opus` track resolution + settings

**Files:** `crates/zord-config/src/lib.rs`

- [ ] Step 1 (red): extend the existing `resolve_track` test: write only
  `me.opus` in a session folder → `resolve_track(dir, "me")` returns it;
  when both exist, `.wav` wins. Add settings test: defaults
  `compress_after_days == Some(7)`, `compress_quality == "standard"`.
- [ ] Step 2: implement — `resolve_track` tries `role.wav` (both layouts)
  then `role.opus` (both layouts). Settings:

```rust
    /// Compress kept audio (WAV → Opus) once a session is older than this
    /// many days (Phase 37). `Some(0)` = as soon as it has ended; `None` =
    /// never. Default 7 — recent sessions stay bit-exact WAV.
    #[serde(default = "default_compress_after_days")]
    pub compress_after_days: Option<u32>,
    /// Opus quality preset: "space" (24 kbps) | "standard" (32) | "high" (48).
    #[serde(default = "default_compress_quality")]
    pub compress_quality: String,
```

with `fn default_compress_after_days() -> Option<u32> { Some(7) }`,
`fn default_compress_quality() -> String { "standard".into() }` + Default-impl
entries.
- [ ] Step 3: tests green; commit `feat(config): opus track resolution + compression settings`

---

### Task 3: consumers — replay/transcribe/diarize/merge read both formats

**Files:**
- Modify: `crates/zord-audio/src/wav.rs` — `mix_wavs` → generalized `mix_tracks`
- Modify: `crates/zord-gui/src/engine.rs` — replay + post-transcribe call sites
- Modify: `crates/zord-transcribe/src/offline.rs` + `crates/zord-diarize` read path (whichever read fn they use: swap `read_wav_mono_16k` → `read_audio_mono_16k`, `read_wav_slice_ms` → `read_audio_slice_ms`)
- Modify: `crates/zord-app/src/pipeline.rs` / `main.rs` if they read tracks directly

- [ ] Step 1: `mix_tracks(paths, out)`: per-track reader becomes an enum
  { Wav(existing struct), Opus(OpusBlocks) } with `rate()` + `read_native(frames)`;
  rest of the mixing loop unchanged; keep `mix_wavs` name re-exported as alias
  or update the single engine call site (`export_merged_audio`) — prefer the
  rename + call-site update. Folder enumeration in `export_merged_audio`
  accepts `.opus` too.
- [ ] Step 2: grep `read_wav_` across the workspace; swap consumers (NOT the
  internal wav helpers) to `read_audio_*`. Adjust for the new
  `(Vec<f32>, u32)` return where applicable.
- [ ] Step 3: existing tests + gate green; commit
  `feat(audio): all consumers read wav+opus (replay, transcribe, diarize, merge)`

---

### Task 4: engine sweep — `DbCmd::CompressAudio`, verify-then-delete, scheduler

**Files:** `crates/zord-gui/src/engine.rs`

- [ ] Step 1: pure helper + unit test (engine `mod tests`):

```rust
/// One session-track compression: encode WAV → `<track>.opus.partial`,
/// verify (decoded total duration within 1% of the WAV's), promote to
/// `.opus`, delete the WAV. Returns bytes reclaimed.
fn compress_track(wav: &Path, bitrate: i32) -> anyhow::Result<u64>
```

  Test with a temp dir + tiny synthetic WAV: after the call, `.wav` is gone,
  `.opus` exists, no `.partial` remains, reclaimed > 0; a pre-existing stale
  `.partial` is overwritten.
- [ ] Step 2: `DbCmd::CompressAudio { ignore_age: bool }` arm in db_loop —
  spawn supervised worker (`jobs.begin(.., "compress", "Compressing kept audio")`):
  1. list sessions from the store with `audio_path` AND `ended_at` set,
  2. age check: `started_at` older than `compress_after_days` (skip arm when
     `ignore_age`); `None` setting → only run when `ignore_age`,
  3. per session: folder layout → every `*.wav` in the folder; legacy flat →
     resolve me/others; call `compress_track`; honor the cancel token
     between tracks; clean stale `*.partial` first,
  4. final `Event::Notice` with sessions touched + MB reclaimed; refresh
     `AudioFiles` if the open session was compressed (send `Sessions` refresh).
- [ ] Step 3: scheduler in `Engine::spawn`: thread — sleep 90 s, send
  `CompressAudio { ignore_age: false }`, then every 6 h. Reads no state (the
  worker re-reads settings each run).
- [ ] Step 4: gate green; commit
  `feat(engine): age-based opus compression sweep (verify-then-delete, scheduled + on demand)`

---

### Task 5: UI — retention controls + "compress now"

**Files:** `crates/zord-gui/src/main.rs` (`RetentionSettings`, `FilesSettings`)

- [ ] Step 1: RetentionSettings gains, mirroring the auto-delete row style:
  a "Compress kept audio after N days" number+Never control writing
  `compress_after_days`, and a quality `select` (Space-saver 24 / Standard 32 /
  High 48 → "space"/"standard"/"high") with a note ("~14 MB per hour per
  track at Standard; replay, re-transcribe and export all keep working").
- [ ] Step 2: FilesSettings gains **"Compress all kept recordings now"**
  (`engine.db_tx.send(DbCmd::CompressAudio { ignore_age: true })` + notice
  "Compressing in the background — watch the jobs panel."). FilesSettings
  needs the `engine` prop added (call site passes `engine.clone()`).
- [ ] Step 3: gate green; commit `feat(gui): compression retention controls + compress-now action`

---

### Task 6: docs + close-out

- [ ] PLAN.md: Phase 37 entry (done); KICKSTART data-layout note (`audio/` may
  hold `.opus` after aging); update `.claude/memory/data-locations.md`.
- [ ] Full gate incl. `--features discord,parakeet` clippy; push develop;
  rebuild + relaunch app for the user's manual pass (set N=0 → record →
  watch shrink → replay → re-transcribe → merged export).

---

## Self-review

- **Spec coverage:** codec/container + presets (T1), readers/dispatch + slices
  (T1), resolve/settings (T2), four consumers (T3), sweep + verify + schedule +
  compress-now (T4/T5), docs (T6). Edge cases: live session excluded via
  `ended_at` (T4), `.partial` hygiene (T1 caller/T4), legacy flat layout (T4),
  WAV-repair untouched (dispatch only routes `.opus`).
- **Placeholders:** none — signatures + behaviors pinned; codec details
  (granule scheme, pre-skip, seek margin) specified above.
- **Type consistency:** `read_audio_mono_f32 -> (Vec<f32>, u32)` flagged at
  both definition (T1) and call-site swap (T3); `mix_wavs → mix_tracks`
  rename includes its one call site.
