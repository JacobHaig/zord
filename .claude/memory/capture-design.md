---
name: capture-design
description: Dual-channel Me/Others capture via per-OS backends; channel separation instead of ML diarization
metadata:
  node_type: memory
  type: project
---

Audio is captured as **two independent channels**: microphone = "Me" (cpal,
cross-platform) and system/desktop loopback = "Others". Each is transcribed
separately, tagged by `Source`, and merged onto one timeline.

**Timeline sync (critical — fixed after a drift bug):** segment timestamps are
sample-based, but the two devices have independent clocks/cadence and loopback
capture (esp. **WASAPI**) emits **no samples during silence**. A raw per-channel
sample count drifts behind real time by the total silence (the mic, always
continuous, did not) — starting aligned and growing to minutes by the end,
scrambling Me/Others ordering. Fix (`spawn_proc` in `zord-gui/src/engine.rs`):
each channel **pads silence so its emitted sample count == wall-clock**
(`produced` vs `session_start.elapsed()*16kHz`; pad the gap, 30 ms jitter slack,
5-min cap). Both channels — plus the saved WAV and diarization — then share the
real meeting clock by construction. Do NOT revert to a one-shot first-frame
offset; it drifts.

System loopback backends (in `zord-capture/src/system.rs`, cfg-gated per OS):
- **macOS** → ScreenCaptureKit (`screencapturekit` crate). Needs the user to
  grant **Screen Recording** permission; degrades to mic-only with a notice if not.
- **Windows** → WASAPI render-device loopback via the `wasapi` crate (NOT cpal's
  loopback, which is flaky). Runs on a dedicated COM thread.
- **Linux** → not implemented (stub bails).

**Why:** capture-time separation is far more reliable than ML speaker
diarization. (Per-speaker diarization within "Others" is the only remaining
backlog phase, 16.)

**How to apply:** sources emit **mono f32 at native rate**; the pipeline
resamples to 16 kHz. Capture mode (mic/system/both) is a setting honored by both
the GUI engine and the CLI pipeline. Related: [[architecture]], [[macos-build]].
