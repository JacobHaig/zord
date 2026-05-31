//! Local meeting-summary generation via a small instruct LLM (llama.cpp).
//!
//! Everything here is behind the `llama` feature so the default workspace build
//! never compiles llama.cpp. Enable it via the consumer's `summaries` feature.

#[cfg(feature = "llama")]
mod summarizer;

#[cfg(feature = "llama")]
pub use summarizer::{
    delete_summary_model, ensure_summary_model, summary_model_present, Summarizer, SummaryModel,
};
