---
name: meter-event-throttle
description: Why the live level meter lagged ~30s on macOS only, and the throttle fix
metadata:
  node_type: memory
  type: feedback
---

The live level meter ("Me"/"Others" bars) lagged ~30s on **macOS only** — speaking
slowly built the bar, stopping slowly drained it; Windows tracked instantly.

**Why:** the smoothing math in `spawn_proc` (crates/zord-gui/src/engine.rs) is
correct and platform-independent (time-based exponential `alpha = 1 - exp(-dt/tau)`,
attack 0.08s / release 0.35s). The bug was emitting **one `Event::Level` per
capture buffer** over the **unbounded** tokio event channel, with one Dioxus
`signal.set` (→ re-render) per event. macOS CoreAudio delivers the mic in many
tiny buffers (~180–375/sec; `BufferSize::Default`, no fixed size set in
microphone.rs) vs Windows WASAPI (~100/sec or fewer). At ~300 events/sec the UI
can't drain fast enough → an unbounded backlog of stale Level events replays
slowly → the meter runs tens of seconds behind. Mic-specific because the mic fires
far more buffers/sec than ScreenCaptureKit ("Others", ~50/sec).

Confirmed by 3 parallel investigation agents (capture cadence, full data-path,
smoothing math) — all converged on per-buffer emission + unbounded channel +
macOS's high buffer cadence; the `dt`/multi-channel theories were ruled out (mic is
downmixed to mono before `spawn_proc`).

**How to apply:** throttle level emission to a fixed cadence decoupled from buffer
rate. Fix shipped: still integrate `level` every buffer, but only `ev.send` when
≥33ms (~30Hz) elapsed since the last send (`level_send_interval` / `last_level_send`
in spawn_proc). Keep this decoupling for any future meter work — never emit a UI
event per capture buffer. If lag ever returns in a **debug** build, the next
suspect is the heavy sinc resampler (SincFixedIn sinc_len 256 / oversampling 256 in
crates/zord-audio/src/resample.rs) starving the per-source worker — not the meter.
**Same root cause also broke the Stop button on macOS:** the GUI drains ALL engine
events from one unbounded channel, so `Status::Idle` (which flips the Stop button
back to Record) got stuck behind the Level-event backlog — recording looked like it
never stopped. Hardened in addition to the throttle: the GUI drain now processes
events in bursts (recv one, then `try_recv` the rest) and coalesces `Level` to the
newest value per source, applying all other events in order — so a meter flood can
never starve control events (crates/zord-gui/src/main.rs event loop). Lesson: never
put a high-rate per-buffer event on the same ordered channel as control/status
events without coalescing. Optional further hardening (not done): a latest-wins
`watch` channel for levels.
Related: [[capture-design]].
