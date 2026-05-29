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
