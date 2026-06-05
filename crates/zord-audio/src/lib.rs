//! Audio DSP for Zord: downmix + resample to 16 kHz mono, voice-activity
//! segmentation, and WAV retention. Pure processing — no device I/O lives here.

mod resample;
mod segment;
mod wav;

pub use resample::MonoResampler;
pub use segment::{Segmenter, SegmenterConfig, VadSegment};
pub use wav::{read_wav_mono_16k, read_wav_mono_f32, read_wav_slice_ms, WavWriter};
