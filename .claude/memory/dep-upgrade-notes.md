---
name: dep-upgrade-notes
description: Dependency upgrade cycle (June 2026) — per-crate breaking changes + the keyring deferral; reference for the next bump
metadata:
  node_type: memory
  type: reference
---

Workspace-wide dependency upgrade done June 2026 (one crate at a time, build+test+commit
each). Breaking changes encountered, for next time:

- **rusqlite 0.32 → 0.40**: dropped `ToSql`/`FromSql` for `u64`. Our timestamps/counts are
  u64 → cast losslessly at the SQL boundary (SQLite is i64 anyway). Helpers in zord-store:
  `i64v`/`opt_i64v` (write), `get_u64`/`get_opt_u64` (read). On-disk format unchanged.
- **ureq 2 → 3**: full rewrite. `Agent::config_builder()...build().into()`; timeouts are
  `Option<Duration>` config fields; native-tls via `TlsConfig::builder().provider(NativeTls)`
  (no direct native-tls dep); set `http_status_as_error(false)` to still read non-2xx bodies
  (then check `resp.status()`); body via `body_mut().read_to_string()` /
  `with_config().limit(N).reader()` / `into_body().into_reader()`; `http` crate re-exported as
  `ureq::http`. All of this is isolated in zord-net (public API unchanged).
- **rodio 0.20 → 0.22**: `Sink`→`Player` (`Player::connect_new(&Mixer)`, infallible);
  `(OutputStream,OutputStreamHandle)`→`MixerDeviceSink` via `DeviceSinkBuilder::open_default_sink()`;
  `SamplesBuffer::new` takes NonZero rate/channels; cpal output now behind the `playback` feature.
- **cpal 0.15 → 0.18**: `DeviceTrait::name()` removed → `device.description()?.name()`;
  `StreamConfig.sample_rate` is now plain `u32` (no `.0`); `build_input_stream` takes
  `StreamConfig` by value (clone).
- **rubato 0.16 → 3.0**: `SincFixedIn`→`Async::new_sinc(ratio, max, &params, chunk, ch,
  FixedAsync::Input)` (params by ref); `process_into_buffer` now takes audioadapter buffers +
  `Option<&Indexing>` — mono = a 1-channel `rubato::audioadapter_buffers::direct::InterleavedSlice`.
- **axum 0.7 → 0.8**: path params `/:id` → `/{id}`.
- **bzip2 0.4 → 0.6**, **directories 5 → 6**, **screencapturekit 6.1 → 7.0**: drop-in (the SCK
  major was internal FFI hardening + additive video; the audio builder API was unchanged).

**DEFERRED — keyring 3 (NOT upgraded):** keyring 4 is an ecosystem split — `keyring` became a
sample/CLI crate over `keyring-core` 1.0; OS stores moved to separate provider crates needing
store registration at startup. Not a drop-in; keychain behavior isn't headless-testable and the
Windows store crate isn't buildable here. Staying on keyring 3. Revisit only if the
keyring-core migration is explicitly wanted.

**Test gotcha (ureq 3):** a mock HTTP server doing a single `stream.read()` can capture only
the request headers — ureq 3 may flush the body in a separate TCP segment. Read until
headers + Content-Length body are complete (see remote.rs mock servers).

**Coexisting versions:** rodio 0.22 pulls cpal 0.17 for playback while zord-capture uses cpal
0.18 — two cpal versions in the tree, which is fine (independent). Verification ceiling: audio
capture/playback + keychain are runtime-only (not headless-testable) — see [[verification-limits]].
Related: [[feature-build-deps]], [[summary-model-catalog]].
