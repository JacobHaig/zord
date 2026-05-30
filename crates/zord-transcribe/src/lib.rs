//! Local speech-to-text. Whisper.cpp (whisper-rs) is always available;
//! NVIDIA Parakeet (sherpa-onnx) is available behind the `parakeet` feature.
//! Both implement [`TranscribeBackend`]; [`Transcriber`] dispatches by model.

mod model;
mod whisper;

#[cfg(feature = "parakeet")]
mod parakeet;

pub use model::{
    delete_model, ensure_model, is_downloaded, model_cache_dir, model_path_if_present, Engine,
    ModelId,
};

use anyhow::Result;
use std::path::Path;
use zord_core::{Segment, Source};

/// Redirect whisper.cpp + ggml native logging into the Rust `tracing`
/// ecosystem so it respects the app's log filter. Call once at startup.
pub fn install_logging_hooks() {
    whisper_rs::install_logging_hooks();
}

/// A transcription engine that turns 16 kHz mono audio into tagged segments.
pub trait TranscribeBackend: Send {
    /// Transcribe one VAD segment. `base_offset_ms` is the segment's start time
    /// relative to the session, so returned timings are session-relative;
    /// `source` tags every output segment.
    fn transcribe(&self, samples: &[f32], source: Source, base_offset_ms: u64)
        -> Result<Vec<Segment>>;
    fn model_name(&self) -> &str;
}

/// Loaded model that dispatches to the right backend (Whisper or Parakeet).
pub struct Transcriber {
    backend: Box<dyn TranscribeBackend>,
}

impl Transcriber {
    /// Load `model` from a resolved local path (a `.bin` for Whisper, a model
    /// directory for Parakeet — see [`ensure_model`]).
    pub fn load(model: ModelId, model_path: &Path) -> Result<Self> {
        let backend: Box<dyn TranscribeBackend> = match model.engine() {
            Engine::Whisper => Box::new(whisper::WhisperBackend::load(model_path, model.name())?),
            Engine::Parakeet => {
                #[cfg(feature = "parakeet")]
                {
                    Box::new(parakeet::ParakeetBackend::load(model_path, model.name())?)
                }
                #[cfg(not(feature = "parakeet"))]
                {
                    anyhow::bail!(
                        "Parakeet support is not built in — rebuild with `--features parakeet`"
                    )
                }
            }
        };
        Ok(Self { backend })
    }

    pub fn model_name(&self) -> &str {
        self.backend.model_name()
    }

    pub fn transcribe(
        &self,
        samples: &[f32],
        source: Source,
        base_offset_ms: u64,
    ) -> Result<Vec<Segment>> {
        self.backend.transcribe(samples, source, base_offset_ms)
    }
}
