//! Audio capture sources behind one small abstraction.
//!
//! Every source delivers **mono `f32` frames at its own native sample rate**
//! over an [`mpsc::Sender`]; downmixing happens inside each source so the rest
//! of the pipeline is uniform. The OS resource (cpal `Stream` / `SCStream`) is
//! held by the returned struct and released on `Drop`, which stops capture.
//!
//! - [`Microphone`] — cross-platform mic input via cpal ("Me").
//! - [`SystemAudio`] — desktop/system loopback ("Others"). macOS via
//!   ScreenCaptureKit; other platforms land in a later phase.

use std::sync::mpsc::Sender;

mod microphone;
mod system;

pub use microphone::{input_devices, Microphone};
pub use system::{list_capturable_apps, SystemAudio};

/// A running application that per-app capture can target (Phase 31).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturableApp {
    /// Identity that stays stable across launches — what settings store:
    /// the bundle id on macOS (`com.discordapp.Discord`), the executable
    /// name on Windows (`Discord.exe`).
    pub id: String,
    /// Human-readable name for the picker.
    pub name: String,
    /// Current process id (resolution-time only; never persisted).
    pub pid: u32,
}

/// Channel that receives mono `f32` capture frames.
pub type FrameSink = Sender<Vec<f32>>;

/// Common surface over capture sources.
pub trait AudioSource {
    /// Native sample rate of the mono stream this source emits.
    fn sample_rate(&self) -> u32;
}

/// Reinterpret a little-endian byte slice as `f32` PCM samples.
#[cfg(target_os = "macos")]
pub(crate) fn bytes_as_f32(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    #[test]
    fn bytes_as_f32_roundtrip() {
        let samples = [1.0f32, -0.5, 0.0, 0.25];
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        assert_eq!(super::bytes_as_f32(&bytes), samples);
        // A trailing partial sample is dropped, not misread.
        let mut short = bytes.clone();
        short.pop();
        assert_eq!(super::bytes_as_f32(&short).len(), samples.len() - 1);
    }
}
