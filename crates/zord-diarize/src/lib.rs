//! Per-speaker diarization for the "Others" channel (Phase 16).
//!
//! Everything here is behind the `sherpa` feature so the default workspace build
//! never compiles sherpa-onnx. Enable it via the consumer's `diarization` feature.
//!
//! The accurate, source-of-truth path is [`Diarizer`] — an offline pass over a
//! whole recording that segments, embeds, and clusters speech into speakers.
//! [`LiveLabeler`] is an optional, *provisional* online labeler used during
//! recording; its labels are always replaced by the offline pass at stop.

#[cfg(feature = "sherpa")]
mod diarizer;

#[cfg(feature = "sherpa")]
pub use diarizer::{
    delete_embedding, diar_models_present, ensure_diar_models, list_embedding_models, DiarSegment,
    Diarizer, EmbeddingModel, LiveLabeler, SegmentationModel,
};
