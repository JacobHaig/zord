//! Audio DSP for Zord: downmix + resample to 16 kHz mono, voice-activity
//! segmentation, and WAV retention. Pure processing — no device I/O lives here.

mod compress;
mod level;
mod resample;
mod segment;
pub mod timeline;
mod wav;

pub use compress::{
    compress_wav_to_opus, opus_bitrate, read_audio_mono_16k, read_audio_mono_f32,
    read_audio_slice_ms, OpusBlocks,
};
pub use level::{LevelControl, LevelMode};
pub use resample::MonoResampler;
pub use segment::{Segmenter, SegmenterConfig, VadSegment};
pub use timeline::{
    compute_track_peaks, fold_peaks, fold_peaks_and_rms, speech_from_rms, PEAK_BUCKETS,
};
pub use wav::{
    mix_tracks, read_wav_mono_16k, read_wav_mono_f32, read_wav_slice_ms, repair_wav_header,
    validate_wav_spec, wav_duration, MixReader, WavWriter,
};
