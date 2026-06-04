//! Minimal WAV writer for optional audio retention. Writes 16 kHz mono f32
//! converted to 16-bit PCM (compact, universally playable).

use anyhow::Result;
use std::path::Path;
use zord_core::WHISPER_SAMPLE_RATE;

pub struct WavWriter {
    inner: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl WavWriter {
    pub fn create(path: impl AsRef<Path>) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: WHISPER_SAMPLE_RATE,
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

/// Read `len` samples starting at `start_sample` from a WAV file as mono `f32`
/// in `[-1, 1]`. Multi-channel files are downmixed by averaging; a range running
/// past end-of-file just returns fewer samples. Used to replay one transcript
/// line's audio from a retained track.
pub fn read_wav_slice_mono_f32(
    path: impl AsRef<Path>,
    start_sample: u32,
    len: u32,
) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
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
    if channels <= 1 {
        return Ok(interleaved);
    }
    Ok(interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect())
}

/// Read a WAV file (any int/float format) into mono `f32` samples in `[-1, 1]`.
/// Multi-channel files are downmixed by averaging. Used by diarization to load
/// a retained "Others" track back off disk.
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
