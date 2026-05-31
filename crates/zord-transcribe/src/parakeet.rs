//! NVIDIA Parakeet backend via sherpa-onnx (offline transducer / "nemo_transducer").
//! Only compiled with the `parakeet` feature. Implements [`crate::TranscribeBackend`].

use crate::TranscribeBackend;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use zord_core::{Segment, Source};

pub struct ParakeetBackend {
    recognizer: sherpa_onnx::OfflineRecognizer,
    model_name: String,
}

impl ParakeetBackend {
    /// Load a Parakeet transducer model from an extracted model directory
    /// (containing encoder/decoder/joiner `.onnx` files + `tokens.txt`).
    pub fn load(model_dir: &Path, model_name: impl Into<String>) -> Result<Self> {
        let encoder = find_onnx(model_dir, "encoder")?;
        let decoder = find_onnx(model_dir, "decoder")?;
        let joiner = find_onnx(model_dir, "joiner")?;
        let tokens = model_dir.join("tokens.txt");
        if !tokens.exists() {
            bail!("tokens.txt not found in {model_dir:?}");
        }

        let mut config = sherpa_onnx::OfflineRecognizerConfig::default();
        config.model_config.transducer.encoder = Some(encoder.to_string_lossy().into_owned());
        config.model_config.transducer.decoder = Some(decoder.to_string_lossy().into_owned());
        config.model_config.transducer.joiner = Some(joiner.to_string_lossy().into_owned());
        config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
        config.model_config.model_type = Some("nemo_transducer".into());
        config.model_config.num_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);

        let recognizer = sherpa_onnx::OfflineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("failed to create sherpa-onnx Parakeet recognizer"))?;
        Ok(Self {
            recognizer,
            model_name: model_name.into(),
        })
    }
}

impl TranscribeBackend for ParakeetBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn transcribe(&self, samples: &[f32], source: Source, base_offset_ms: u64) -> Result<Vec<Segment>> {
        let stream = self.recognizer.create_stream();
        stream.accept_waveform(zord_core::WHISPER_SAMPLE_RATE as i32, samples);
        self.recognizer.decode(&stream);
        let text = stream
            .get_result()
            .map(|r| r.text.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        // Parakeet returns one result per clip; the VAD already chunked the
        // audio, so emit a single segment spanning this chunk.
        let dur_ms = samples.len() as u64 * 1000 / zord_core::WHISPER_SAMPLE_RATE as u64;
        Ok(vec![Segment {
            id: None,
            source,
            t_start_ms: base_offset_ms,
            t_end_ms: base_offset_ms + dur_ms,
            text,
            words: Vec::new(),
            speaker: None,
        }])
    }
}

/// Find the first `*<kind>*.onnx` file in `dir` (handles `.int8.onnx` variants
/// and minor naming differences across model releases).
fn find_onnx(dir: &Path, kind: &str) -> Result<PathBuf> {
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {dir:?}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "onnx").unwrap_or(false))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().contains(kind))
                .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches
        .into_iter()
        .next()
        .with_context(|| format!("no *{kind}*.onnx found in {dir:?}"))
}
