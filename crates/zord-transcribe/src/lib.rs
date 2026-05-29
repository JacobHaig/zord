//! Local speech-to-text via whisper.cpp (whisper-rs). GPU-accelerated on
//! Apple Silicon (Metal), with CPU fallback. All inference is on-device.

mod model;

pub use model::{
    delete_model, ensure_model, is_downloaded, model_cache_dir, model_path_if_present, ModelId,
};

use anyhow::Result;
use zord_core::{Segment, Source};

/// Redirect whisper.cpp + ggml native logging into the Rust `tracing`
/// ecosystem so it respects the app's log filter. Call once at startup.
pub fn install_logging_hooks() {
    whisper_rs::install_logging_hooks();
}

/// A loaded whisper model ready to transcribe 16 kHz mono audio.
pub struct Transcriber {
    ctx: whisper_rs::WhisperContext,
    model_name: String,
    n_threads: i32,
}

impl Transcriber {
    /// Load a model from a local ggml file path.
    pub fn load(model_path: &std::path::Path, model_name: impl Into<String>) -> Result<Self> {
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

    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Transcribe one VAD segment of 16 kHz mono samples. `base_offset_ms` is
    /// the segment's start time relative to the session, so returned segment
    /// timings are session-relative. `source` tags every output segment.
    pub fn transcribe(
        &self,
        samples: &[f32],
        source: Source,
        base_offset_ms: u64,
    ) -> Result<Vec<Segment>> {
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
                source,
                t_start_ms: base_offset_ms + t0,
                t_end_ms: base_offset_ms + t1,
                text,
                words: Vec::new(),
            });
        }
        Ok(out)
    }
}
