//! Whisper.cpp backend (whisper-rs). GPU-accelerated on Apple Silicon (Metal),
//! CPU elsewhere. Implements [`crate::TranscribeBackend`].

use crate::TranscribeBackend;
use anyhow::Result;
use std::path::Path;
use zord_core::{Segment, Source};

pub struct WhisperBackend {
    ctx: whisper_rs::WhisperContext,
    model_name: String,
    n_threads: i32,
}

impl WhisperBackend {
    /// Load a ggml model from a local file path.
    pub fn load(model_path: &Path, model_name: impl Into<String>) -> Result<Self> {
        let mut params = whisper_rs::WhisperContextParameters::default();
        params.use_gpu(true);
        let ctx = whisper_rs::WhisperContext::new_with_params(
            model_path.to_string_lossy().as_ref(),
            params,
        )?;
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(4);
        Ok(Self {
            ctx,
            model_name: model_name.into(),
            n_threads,
        })
    }
}

impl TranscribeBackend for WhisperBackend {
    fn model_name(&self) -> &str {
        &self.model_name
    }

    fn transcribe(&self, samples: &[f32], source: Source, base_offset_ms: u64) -> Result<Vec<Segment>> {
        let mut state = self.ctx.create_state()?;

        let mut params =
            whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.n_threads);
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);

        state.full(params, samples)?;

        let n = state.full_n_segments();
        let mut out = Vec::new();
        for i in 0..n {
            let segment = match state.get_segment(i) {
                Some(s) => s,
                None => continue,
            };
            let text = segment.to_str()?.trim().to_string();
            if text.is_empty() {
                continue;
            }
            // whisper timestamps are in centiseconds (10 ms units).
            let t0 = segment.start_timestamp().max(0) as u64 * 10;
            let t1 = segment.end_timestamp().max(0) as u64 * 10;
            out.push(Segment {
                id: None,
                source,
                t_start_ms: base_offset_ms + t0,
                t_end_ms: base_offset_ms + t1,
                text,
                words: Vec::new(),
                speaker: None,
            });
        }
        Ok(out)
    }
}
