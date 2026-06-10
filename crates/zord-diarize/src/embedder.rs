//! Standalone speaker-embedding tools for voiceprints (Phase 38b).
//!
//! [`gather_speech`] is a pure-DSP energy gate — no model required, always
//! compiled. [`SpeakerEmbedder`] wraps the sherpa-onnx
//! `SpeakerEmbeddingExtractor` and is gated behind the `sherpa` feature.

/// Energy-gated speech gathering: keep ~0.48 s frames whose RMS clears a
/// floor relative to the loudest frame, up to `max_secs` of audio. Good
/// enough to skip silence on a single-speaker track (Discord per-participant
/// tracks); NOT a diarizer.
pub fn gather_speech(samples: &[f32], rate: u32, max_secs: u32) -> Vec<f32> {
    let frame = (rate as usize / 1000) * 480; // ~480 ms
    if frame == 0 || samples.is_empty() {
        return Vec::new();
    }
    let rms = |s: &[f32]| (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
    let peak = samples.chunks(frame).map(rms).fold(0.0f32, f32::max);
    let floor = (peak * 0.1).max(1e-4);
    let mut out = Vec::new();
    let cap = (rate as usize) * max_secs as usize;
    for chunk in samples.chunks(frame) {
        if rms(chunk) >= floor {
            out.extend_from_slice(chunk);
            if out.len() >= cap {
                out.truncate(cap);
                break;
            }
        }
    }
    out
}

// ── sherpa-gated section ─────────────────────────────────────────────────────

#[cfg(feature = "sherpa")]
use {
    crate::diarizer::{embedding_path, to_cfg_path, DiarSegment, EmbeddingModel},
    anyhow::{anyhow, Result},
    sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig},
    std::collections::HashMap,
};

/// Standalone speaker-embedding extractor for voiceprints (Phase 38): turns
/// speech into the same vectors the diarizer clusters with, so per-cluster
/// centroids can be matched against the persistent library.
#[cfg(feature = "sherpa")]
pub struct SpeakerEmbedder {
    extractor: SpeakerEmbeddingExtractor,
}

#[cfg(feature = "sherpa")]
impl SpeakerEmbedder {
    /// Load the extractor for the chosen embedding model. Fails if the model
    /// file has not been downloaded yet.
    pub fn load(model: EmbeddingModel) -> Result<Self> {
        let emb = embedding_path(model)?;
        if !emb.exists() {
            anyhow::bail!("speaker-embedding model is not downloaded yet");
        }
        let extractor = SpeakerEmbeddingExtractor::create(&SpeakerEmbeddingExtractorConfig {
            model: to_cfg_path(&emb),
            ..Default::default()
        })
        .ok_or_else(|| anyhow!("failed to create the embedding extractor"))?;
        Ok(Self { extractor })
    }

    /// Embed one mono utterance at `sample_rate` Hz. Returns `None` if the
    /// clip is too short for the model to produce a reliable embedding. Output
    /// is L2-normalized.
    pub fn embed(&self, samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(sample_rate as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        let mut e = self.extractor.compute(&stream)?;
        l2_normalize(&mut e);
        Some(e)
    }

    /// Per-cluster centroid embeddings for diarized spans: for each speaker,
    /// feed its longest spans (up to 30 s total) into one stream and embed.
    /// Clusters with under 3 s of speech are skipped (unreliable below that).
    /// Output vectors are L2-normalized.
    pub fn embed_clusters(
        &self,
        samples: &[f32],
        sample_rate: u32,
        spans: &[DiarSegment],
    ) -> HashMap<i32, Vec<f32>> {
        let ms_to_idx = |ms: u64| ((ms as usize) * sample_rate as usize / 1000).min(samples.len());

        let mut by_speaker: HashMap<i32, Vec<&DiarSegment>> = HashMap::new();
        for sp in spans {
            by_speaker.entry(sp.speaker).or_default().push(sp);
        }

        let mut out = HashMap::new();
        for (speaker, mut sps) in by_speaker {
            sps.sort_by_key(|s| std::cmp::Reverse(s.end_ms - s.start_ms));
            let total_ms: u64 = sps.iter().map(|s| s.end_ms - s.start_ms).sum();
            if total_ms < 3_000 {
                continue;
            }
            let Some(stream) = self.extractor.create_stream() else {
                continue;
            };
            let mut fed_ms = 0u64;
            for sp in sps {
                let (a, b) = (ms_to_idx(sp.start_ms), ms_to_idx(sp.end_ms));
                if a >= b {
                    continue;
                }
                stream.accept_waveform(sample_rate as i32, &samples[a..b]);
                fed_ms += sp.end_ms - sp.start_ms;
                if fed_ms >= 30_000 {
                    break;
                }
            }
            stream.input_finished();
            if !self.extractor.is_ready(&stream) {
                continue;
            }
            if let Some(mut e) = self.extractor.compute(&stream) {
                l2_normalize(&mut e);
                out.insert(speaker, e);
            }
        }
        out
    }
}

/// In-place L2 normalization; no-op on a zero vector.
#[cfg(feature = "sherpa")]
fn l2_normalize(v: &mut [f32]) {
    let n: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::gather_speech;

    #[test]
    fn gather_speech_skips_silence_and_caps() {
        let rate = 16_000u32;
        let mut s = vec![0.0f32; rate as usize]; // 1 s silence
        s.extend((0..rate).map(|i| (i as f32 * 0.05).sin() * 0.3)); // 1 s tone
        let speech = gather_speech(&s, rate, 30);
        let secs = speech.len() as f32 / rate as f32;
        assert!(
            secs > 0.7 && secs < 1.3,
            "kept ~the tone second, got {secs}"
        );
        assert!(gather_speech(&s, rate, 0).is_empty());
        assert!(gather_speech(&[], rate, 30).is_empty());
    }

    #[test]
    fn gather_speech_all_silent_returns_empty() {
        let rate = 16_000u32;
        // All zeros — peak is 0.0, floor clamps to 1e-4, nothing clears it.
        let silence = vec![0.0f32; rate as usize * 2];
        assert!(gather_speech(&silence, rate, 30).is_empty());
    }

    #[test]
    fn gather_speech_caps_at_max_secs() {
        let rate = 16_000u32;
        // 10 s of loud tone — should be capped at max_secs = 3.
        let loud: Vec<f32> = (0..rate * 10)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        let out = gather_speech(&loud, rate, 3);
        let secs = out.len() as f32 / rate as f32;
        assert!((secs - 3.0).abs() < 0.5, "expected ~3 s cap, got {secs}");
    }
}

#[cfg(all(test, feature = "sherpa"))]
mod sherpa_tests {
    use super::*;
    use std::f32::consts::TAU;

    /// Embed 5 s of synthetic 440 Hz tone and verify the output has nonzero
    /// dimension and unit norm. Skipped (via `#[ignore]`) so fresh checkouts
    /// without the model file don't fail — run with `cargo test --ignored`.
    #[test]
    #[ignore = "requires local embedding model file"]
    fn embed_synthetic_audio() {
        let model = EmbeddingModel::TitanetSmall;
        let emb_path = match embedding_path(model) {
            Ok(p) => p,
            Err(_) => return,
        };
        if !emb_path.exists() {
            return;
        }
        let embedder = SpeakerEmbedder::load(model).expect("SpeakerEmbedder::load failed");
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..rate * 5)
            .map(|i| (TAU * 440.0 * i as f32 / rate as f32).sin() * 0.3)
            .collect();
        let result = embedder.embed(&samples, rate);
        assert!(result.is_some(), "expected Some embedding, got None");
        let vec = result.unwrap();
        assert!(!vec.is_empty(), "embedding vector must be non-empty");
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "expected unit norm, got {norm}");
    }
}
