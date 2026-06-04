//! Local meeting-summary generation via a small instruct LLM (llama.cpp).
//!
//! Everything here is behind the `llama` feature so the default workspace build
//! never compiles llama.cpp. Enable it via the consumer's `summaries` feature.

#[cfg(feature = "llama")]
mod backend;
#[cfg(feature = "llama")]
mod remote;
#[cfg(feature = "llama")]
mod summarizer;

#[cfg(feature = "llama")]
pub use backend::LlmBackend;
#[cfg(feature = "llama")]
pub use remote::{list_models as list_remote_models, RemoteConfig};
#[cfg(feature = "llama")]
pub use summarizer::{
    custom_model_path, delete_custom_model, delete_summary_model, ensure_ollama_model,
    ensure_summary_model, list_custom_models, ollama_model_present, ollama_models,
    summary_model_present, ChatRole, GenOpts, OllamaModel, Summarizer, SummaryModel,
};
