---
name: capture-design
description: Dual-channel Me/Others capture via per-OS backends; channel separation instead of ML diarization
metadata:
  node_type: memory
  type: project
---

Audio is captured as **two independent channels**: microphone = "Me" (cpal,
cross-platform) and system/desktop loopback = "Others". Each is transcribed
separately, tagged by `Source`, and merged onto one timeline by per-channel
first-frame wall-clock offset.

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
