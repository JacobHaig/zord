//! Downmix to mono + high-quality resample to 16 kHz, the format whisper
//! requires. Capture devices typically deliver 44.1/48 kHz interleaved stereo;
//! this module turns that into a clean 16 kHz mono `f32` stream.

use anyhow::Result;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use zord_core::WHISPER_SAMPLE_RATE;

/// Streaming resampler that accepts arbitrarily-sized interleaved input buffers
/// and emits 16 kHz mono samples.
pub struct MonoResampler {
    channels: usize,
    /// `None` means the input is already 16 kHz mono — pass-through.
    resampler: Option<SincFixedIn<f32>>,
    /// Mono samples awaiting a full resampler chunk.
    pending: Vec<f32>,
    /// Scratch buffers reused across calls to avoid per-frame allocation.
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
}

impl MonoResampler {
    pub fn new(input_rate: u32, channels: u16) -> Result<Self> {
        let channels = channels.max(1) as usize;

        if input_rate == WHISPER_SAMPLE_RATE {
            return Ok(Self {
                channels,
                resampler: None,
                pending: Vec::new(),
                in_buf: Vec::new(),
                out_buf: Vec::new(),
            });
        }

        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = WHISPER_SAMPLE_RATE as f64 / input_rate as f64;
        // 1024-frame input chunks: a good latency/throughput balance.
        let chunk = 1024;
        let resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, chunk, 1)?;

        Ok(Self {
            channels,
            resampler: Some(resampler),
            pending: Vec::new(),
            in_buf: Vec::with_capacity(chunk),
            out_buf: Vec::new(),
        })
    }

    /// Feed an interleaved input buffer; returns any 16 kHz mono samples ready.
    pub fn process(&mut self, interleaved: &[f32]) -> Result<Vec<f32>> {
        // Downmix interleaved frames to mono by averaging channels.
        let mono = downmix(interleaved, self.channels);

        let resampler = match self.resampler.as_mut() {
            None => return Ok(mono), // already 16 kHz mono
            Some(r) => r,
        };

        self.pending.extend_from_slice(&mono);

        let mut out = Vec::new();
        loop {
            let needed = resampler.input_frames_next();
            if self.pending.len() < needed {
                break;
            }
            self.in_buf.clear();
            self.in_buf.extend_from_slice(&self.pending[..needed]);
            self.pending.drain(..needed);

            let frames_out = resampler.output_frames_next();
            if self.out_buf.len() < frames_out {
                self.out_buf.resize(frames_out, 0.0);
            }
            let (_in_used, out_used) = resampler.process_into_buffer(
                &[&self.in_buf],
                &mut [&mut self.out_buf],
                None,
            )?;
            out.extend_from_slice(&self.out_buf[..out_used]);
        }
        Ok(out)
    }
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    let frames = interleaved.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * channels;
        let sum: f32 = interleaved[base..base + channels].iter().sum();
        mono.push(sum / channels as f32);
    }
    mono
}
