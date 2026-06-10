//! Per-speaker diarization for the "Others" channel (Phase 16).
//!
//! Everything here is behind the `sherpa` feature so the default workspace build
//! never compiles sherpa-onnx. Enable it via the consumer's `diarization` feature.
//!
//! The accurate, source-of-truth path is [`Diarizer`] — an offline pass over a
//! whole recording that segments, embeds, and clusters speech into speakers.
//! [`LiveLabeler`] is an optional, *provisional* online labeler used during
//! recording; its labels are always replaced by the offline pass at stop.
//!
//! [`gather_speech`] (Phase 38b) is a pure-DSP energy gate with no model
//! dependency; it is always compiled. [`SpeakerEmbedder`] (Phase 38b) produces
//! the same embedding vectors the diarizer clusters with, enabling persistent
//! per-speaker voiceprints — gated behind `sherpa`.

#[cfg(feature = "sherpa")]
mod diarizer;

mod embedder;

#[cfg(feature = "sherpa")]
pub use diarizer::{
    delete_embedding, diar_models_present, ensure_diar_models, list_embedding_models, DiarSegment,
    Diarizer, EmbeddingModel, LiveLabeler, SegmentationModel,
};

pub use embedder::gather_speech;

#[cfg(feature = "sherpa")]
pub use embedder::SpeakerEmbedder;
