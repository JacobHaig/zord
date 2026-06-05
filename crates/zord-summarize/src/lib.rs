//! Meeting-LLM features (summaries, compression, overview, chat, titles) over
//! two backends: a local instruct model (llama.cpp, `llama` feature) and a
//! user-provided OpenAI-compatible server (`remote` feature — pure HTTP, no
//! llama.cpp toolchain). The default build leaves this crate empty; consumers
//! enable `llm-local` / `llm-remote`.

#[cfg(any(feature = "llama", feature = "remote"))]
mod backend;
#[cfg(any(feature = "llama", feature = "remote"))]
mod opts;
#[cfg(feature = "remote")]
mod remote;
#[cfg(feature = "llama")]
mod summarizer;

#[cfg(any(feature = "llama", feature = "remote"))]
pub use backend::LlmBackend;
#[cfg(any(feature = "llama", feature = "remote"))]
pub use opts::{ChatRole, GenOpts};
#[cfg(feature = "remote")]
pub use remote::{list_models as list_remote_models, RemoteConfig};
#[cfg(feature = "llama")]
pub use summarizer::{
    custom_model_path, delete_custom_model, delete_summary_model, ensure_ollama_model,
    ensure_summary_model, list_custom_models, ollama_model_present, ollama_models,
    summary_model_present, OllamaModel, Summarizer, SummaryModel,
};
