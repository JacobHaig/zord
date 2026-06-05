//! Minimal WAV writer for audio retention. Writes mono f32 at the caller's
//! sample rate (Phase 25d: the capture device's native rate — the single
//! stored track; models derive 16 kHz from it on the fly), converted to
//! 16-bit PCM (compact, universally playable).

use anyhow::Result;
use std::path::Path;

pub struct WavWriter {
    inner: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl WavWriter {
    /// Create a mono 16-bit WAV at `sample_rate`.
    pub fn create(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Self {
            inner: hound::WavWriter::create(path, spec)?,
        })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            self.inner.write_sample((clamped * i16::MAX as f32) as i16)?;
        }
        Ok(())
    }

    pub fn finalize(self) -> Result<()> {
        self.inner.finalize()?;
        Ok(())
    }
}

/// Read the `[start_ms, end_ms)` span of a WAV as mono `f32` in `[-1, 1]`,
/// returning `(samples, sample_rate)`. Rate-agnostic (Phase 25d): offsets are
/// computed from the file's own header, so a wall-clock-aligned track maps
/// `ms → sample` exactly at any rate. Multi-channel files are downmixed; a
/// range past end-of-file just returns fewer samples. Used for per-line replay.
pub fn read_wav_slice_ms(
    path: impl AsRef<Path>,
    start_ms: u64,
    end_ms: u64,
) -> Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let rate = spec.sample_rate;
    let start_sample = (start_ms * rate as u64 / 1000) as u32;
    let len = (end_ms.saturating_sub(start_ms) * rate as u64 / 1000) as u32;
    let channels = spec.channels.max(1) as usize;
    reader.seek(start_sample)?;
    let want = len as usize * channels;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .take(want)
            .filter_map(|s| s.ok())
            .collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .take(want)
                .filter_map(|s| s.ok())
                .map(|s| s as f32 * scale)
                .collect()
        }
    };
    let mono = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok((mono, rate))
}

/// Read a WAV file (any rate/format) into **16 kHz** mono `f32` in `[-1, 1]`,
/// resampling on the fly in ~1 s blocks so a long device-rate recording never
/// gets slurped whole (Phase 25d). Used to feed the diarizer from the single
/// stored native-rate track. A 16 kHz file passes through untouched.
pub fn read_wav_mono_16k(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let mut resampler = crate::MonoResampler::new(spec.sample_rate, spec.channels)?;
    let block_len = spec.sample_rate as usize * channels; // ~1 s interleaved
    let mut block: Vec<f32> = Vec::with_capacity(block_len);
    // Rough capacity: output is 16 kHz mono.
    let mut out: Vec<f32> = Vec::new();

    let mut flush = |block: &mut Vec<f32>, out: &mut Vec<f32>| -> Result<()> {
        if !block.is_empty() {
            out.extend(resampler.process(block)?);
            block.clear();
        }
        Ok(())
    };
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                block.push(s?);
                if block.len() >= block_len {
                    flush(&mut block, &mut out)?;
                }
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.samples::<i32>() {
                block.push(s? as f32 * scale);
                if block.len() >= block_len {
                    flush(&mut block, &mut out)?;
                }
            }
        }
    }
    flush(&mut block, &mut out)?;
    Ok(out)
}

/// Read a WAV file (any int/float format) into mono `f32` samples in `[-1, 1]`
/// at its **native** rate. Multi-channel files are downmixed by averaging.
pub fn read_wav_mono_f32(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 * scale)
                .collect()
        }
    };
    if channels <= 1 {
        return Ok(interleaved);
    }
    Ok(interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect())
}
