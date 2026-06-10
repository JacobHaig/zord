//! Downmix to mono + high-quality resample to 16 kHz, the format whisper
//! requires. Capture devices typically deliver 44.1/48 kHz interleaved stereo;
//! this module turns that into a clean 16 kHz mono `f32` stream.

use anyhow::Result;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use zord_core::WHISPER_SAMPLE_RATE;

/// Streaming resampler that accepts arbitrarily-sized interleaved input buffers
/// and emits 16 kHz mono samples.
pub struct MonoResampler {
    channels: usize,
    /// `None` means the input is already 16 kHz mono — pass-through.
    resampler: Option<Async<f32>>,
    /// Mono samples awaiting a full resampler chunk.
    pending: Vec<f32>,
    /// Scratch output buffer reused across calls to avoid per-frame allocation.
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
        // FixedAsync::Input keeps the fixed-input-size behavior the streaming
        // loop relies on (input_frames_next() is constant) — rubato 3.0's
        // replacement for the old SincFixedIn.
        let resampler = Async::<f32>::new_sinc(ratio, 2.0, &params, chunk, 1, FixedAsync::Input)?;

        Ok(Self {
            channels,
            resampler: Some(resampler),
            pending: Vec::new(),
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
            let frames_out = resampler.output_frames_next();
            if self.out_buf.len() < frames_out {
                self.out_buf.resize(frames_out, 0.0);
            }
            // rubato 3.0 takes audioadapter buffers; mono = 1-channel
            // interleaved. Input borrows `pending` (drained after), output
            // borrows the scratch buffer (copied out after).
            let input = InterleavedSlice::new(&self.pending[..needed], 1, needed)
                .map_err(|e| anyhow::anyhow!("resample input buffer: {e:?}"))?;
            let mut output =
                InterleavedSlice::new_mut(&mut self.out_buf[..frames_out], 1, frames_out)
                    .map_err(|e| anyhow::anyhow!("resample output buffer: {e:?}"))?;
            let (_in_used, out_used) = resampler.process_into_buffer(&input, &mut output, None)?;
            // `output`'s borrow of `out_buf` ends here (NLL), freeing it to be read.
            out.extend_from_slice(&self.out_buf[..out_used]);
            self.pending.drain(..needed);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_already_16k() {
        let mut r = MonoResampler::new(WHISPER_SAMPLE_RATE, 1).unwrap();
        let out = r.process(&[0.1, 0.2, 0.3]).unwrap();
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn resamples_48k_stereo_to_16k_mono() {
        // 1 s of 48 kHz stereo → expect ~16 kHz mono out (≈ 1/3 the frames).
        let mut r = MonoResampler::new(48_000, 2).unwrap();
        let frames = 48_000usize;
        // A 440 Hz sine, duplicated across both channels (interleaved).
        let mut interleaved = Vec::with_capacity(frames * 2);
        for n in 0..frames {
            let s = (n as f32 / 48_000.0 * 440.0 * std::f32::consts::TAU).sin();
            interleaved.push(s);
            interleaved.push(s);
        }
        let mut out = Vec::new();
        // Feed in capture-sized chunks, as the real pipeline does.
        for chunk in interleaved.chunks(2048) {
            out.extend(r.process(chunk).unwrap());
        }
        // Output rate is 1/3 of input; allow a chunk of slack for buffering.
        let expected = frames / 3;
        assert!(
            (out.len() as isize - expected as isize).unsigned_abs() < 2048,
            "got {} samples, expected ~{expected}",
            out.len()
        );
        assert!(
            out.iter().all(|s| s.is_finite()),
            "non-finite samples produced"
        );
        assert!(out.iter().any(|&s| s.abs() > 0.05), "output is silent");
    }
}
