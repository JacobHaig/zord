//! Local meeting-summary generation via a small instruct LLM (llama.cpp).
//!
//! Everything here is behind the `llama` feature so the default workspace build
//! never compiles llama.cpp. Enable it via the consumer's `summaries` feature.

#[cfg(feature = "llama")]
mod summarizer;

#[cfg(feature = "llama")]
pub use summarizer::{
    custom_model_path, delete_custom_model, delete_summary_model, ensure_ollama_model,
    ensure_summary_model, list_custom_models, ollama_model_present, ollama_models,
    summary_model_present, GenOpts, OllamaModel, Summarizer, SummaryModel,
};
