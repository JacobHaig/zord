//! Phase 25 — offline (post-hoc) transcription of a whole WAV file.
//!
//! Shared by the GUI's post-stop / Re-transcribe pass and the CLI's
//! `retranscribe`. Reads the file in ~1-second blocks (a long device-rate
//! recording never gets slurped into RAM), resamples whatever rate/channels it
//! finds down to 16 kHz mono, VAD-segments, and runs the transcriber over each
//! chunk in order.

use anyhow::{Context, Result};
use std::path::Path;
use zord_audio::{MonoResampler, Segmenter, SegmenterConfig};
use zord_core::{Segment, Source};

use crate::Transcriber;

/// Transcribe an entire WAV offline. Calls `on_segment` for each transcribed
/// segment in chronological order (the caller stores/prints/emits them) and
/// returns how many were produced. Timestamps are sample-position-derived, so
/// for Zord's wall-clock-aligned session WAVs they land on the session
/// timeline exactly like live ones.
pub fn transcribe_wav_file(
    transcriber: &Transcriber,
    source: Source,
    wav_path: &Path,
    on_segment: &mut dyn FnMut(Segment),
) -> Result<usize> {
    let mut reader =
        hound::WavReader::open(wav_path).with_context(|| format!("opening {wav_path:?}"))?;
    let spec = reader.spec();
    zord_audio::validate_wav_spec(spec)?;
    tracing::info!(rate = spec.sample_rate, channels = spec.channels, ?source, "offline transcription");

    let channels = spec.channels.max(1) as usize;
    let mut resampler = MonoResampler::new(spec.sample_rate, spec.channels)?;
    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    let block_len = spec.sample_rate as usize * channels; // ~1 s of interleaved samples
    let mut block: Vec<f32> = Vec::with_capacity(block_len);
    let mut count = 0usize;

    let mut handle_block = |block: &[f32],
                            resampler: &mut MonoResampler,
                            segmenter: &mut Segmenter,
                            count: &mut usize|
     -> Result<()> {
        let mono = resampler.process(block)?;
        for vad in segmenter.push(&mono) {
            for seg in transcriber.transcribe(&vad.samples, source, vad.t_start_ms)? {
                on_segment(seg);
                *count += 1;
            }
        }
        Ok(())
    };

    // Normalize int formats to [-1, 1] as we stream.
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                block.push(s?);
                if block.len() >= block_len {
                    handle_block(&block, &mut resampler, &mut segmenter, &mut count)?;
                    block.clear();
                }
            }
        }
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.samples::<i32>() {
                block.push(s? as f32 / scale);
                if block.len() >= block_len {
                    handle_block(&block, &mut resampler, &mut segmenter, &mut count)?;
                    block.clear();
                }
            }
        }
    }
    if !block.is_empty() {
        handle_block(&block, &mut resampler, &mut segmenter, &mut count)?;
    }
    // Flush the trailing partial VAD chunk.
    if let Some(vad) = segmenter.flush() {
        for seg in transcriber.transcribe(&vad.samples, source, vad.t_start_ms)? {
            on_segment(seg);
            count += 1;
        }
    }
    Ok(count)
}
