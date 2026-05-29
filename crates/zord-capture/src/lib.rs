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
pub use system::SystemAudio;

/// Channel that receives mono `f32` capture frames.
pub type FrameSink = Sender<Vec<f32>>;

/// Common surface over capture sources.
pub trait AudioSource {
    /// Native sample rate of the mono stream this source emits.
    fn sample_rate(&self) -> u32;
}

/// Reinterpret a little-endian byte slice as `f32` PCM samples.
pub(crate) fn bytes_as_f32(data: &[u8]) -> Vec<f32> {
    data.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}
