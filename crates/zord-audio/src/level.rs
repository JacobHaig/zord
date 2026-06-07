//! Per-channel capture level control: Off / Manual gain / Auto-level (AGC).
//!
//! Runs in the capture hot path *before* resample + VAD, so the model input,
//! the retained WAV, and the level meters all see the same adjusted signal. A
//! `tanh` soft-limiter keeps any boost from clipping (it's transparent for
//! low-level samples and saturates smoothly only as it approaches full scale).

/// How a channel's level is adjusted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LevelMode {
    /// Pass through unchanged (zero-cost).
    Off,
    /// Apply a fixed gain in decibels (then soft-limit).
    Manual(f32),
    /// Automatically normalize toward a target loudness (then soft-limit).
    Auto,
}

impl LevelMode {
    /// Build a mode from a settings `(mode, gain_db)` pair: `"manual"` →
    /// `Manual(gain_db)`, `"auto"` → `Auto`, anything else → `Off`.
    pub fn parse(mode: &str, gain_db: f32) -> Self {
        match mode {
            "manual" => LevelMode::Manual(gain_db),
            "auto" => LevelMode::Auto,
            _ => LevelMode::Off,
        }
    }
}

/// Auto-level target RMS (~-20 dBFS) and the gain bounds it may apply.
const AUTO_TARGET_RMS: f32 = 0.1;
const AUTO_MAX_GAIN: f32 = 16.0; // ~+24 dB
const AUTO_MIN_GAIN: f32 = 0.25; // ~-12 dB
/// Below this input RMS the channel is treated as silent — hold the gain rather
/// than ramping it up and amplifying background noise/hiss during quiet gaps.
const SILENCE_RMS: f32 = 0.003;

/// Stateful per-channel level processor. One per capture channel; `process` is
/// called on each incoming buffer in order (Auto mode carries smoothing state).
pub struct LevelControl {
    mode: LevelMode,
    gain: f32, // current smoothed linear gain (Auto)
    env: f32,  // smoothed input RMS envelope (Auto)
}

impl LevelControl {
    pub fn new(mode: LevelMode) -> Self {
        Self { mode, gain: 1.0, env: 0.0 }
    }

    /// Apply the configured level control to `samples` in place. `sample_rate`
    /// sizes the Auto-mode smoothing time constants.
    pub fn process(&mut self, samples: &mut [f32], sample_rate: u32) {
        match self.mode {
            LevelMode::Off => {}
            LevelMode::Manual(db) => {
                let g = 10f32.powf(db / 20.0);
                if (g - 1.0).abs() < 1e-4 {
                    return; // 0 dB — true passthrough, no soft-limit reshaping
                }
                for s in samples.iter_mut() {
                    *s = (*s * g).tanh();
                }
            }
            LevelMode::Auto => {
                if samples.is_empty() {
                    return;
                }
                let n = samples.len() as f32;
                let rms = (samples.iter().map(|s| s * s).sum::<f32>() / n).sqrt();
                let dt = n / sample_rate.max(1) as f32;
                // Track the input envelope (~300 ms), then glide the gain toward
                // the target more slowly (~1.5 s) so levels settle without pumping.
                let env_a = 1.0 - (-dt / 0.3).exp();
                self.env += (rms - self.env) * env_a;
                if self.env > SILENCE_RMS {
                    let desired = (AUTO_TARGET_RMS / self.env).clamp(AUTO_MIN_GAIN, AUTO_MAX_GAIN);
                    let gain_a = 1.0 - (-dt / 1.5).exp();
                    self.gain += (desired - self.gain) * gain_a;
                }
                for s in samples.iter_mut() {
                    *s = (*s * self.gain).tanh();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_passthrough() {
        let mut lc = LevelControl::new(LevelMode::Off);
        let mut buf = vec![0.1, -0.2, 0.3];
        lc.process(&mut buf, 16_000);
        assert_eq!(buf, vec![0.1, -0.2, 0.3]);
    }

    #[test]
    fn manual_zero_db_is_passthrough() {
        let mut lc = LevelControl::new(LevelMode::Manual(0.0));
        let mut buf = vec![0.1, -0.2, 0.3];
        lc.process(&mut buf, 16_000);
        assert_eq!(buf, vec![0.1, -0.2, 0.3]);
    }

    #[test]
    fn manual_gain_boosts_but_never_clips() {
        let mut lc = LevelControl::new(LevelMode::Manual(24.0)); // ~16x
        let mut buf = vec![0.2, -0.5, 0.9, -1.0];
        lc.process(&mut buf, 16_000);
        assert!(buf.iter().all(|s| s.abs() <= 1.0), "soft-limiter must keep |s| <= 1");
        assert!(buf[0].abs() > 0.2, "a quiet sample should be boosted");
    }
}
