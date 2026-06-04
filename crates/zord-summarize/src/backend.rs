//! Phase 24a/b — the backend seam: the one type every LLM feature talks to.
//!
//! Summarize, Compress, Overview, Chat, and auto-title all reduce to
//! "chat-style messages in → string out", so they program against [`LlmBackend`]
//! instead of a concrete engine: the in-process llama.cpp [`Summarizer`], or a
//! user-configured OpenAI-compatible server ([`RemoteLlm`] — LM Studio,
//! `ollama serve`, vLLM, …).

use anyhow::Result;
use std::path::Path;

use crate::remote::{RemoteConfig, RemoteLlm};
use crate::summarizer::{ChatRole, GenOpts, Summarizer};

/// A loaded LLM ready to run chat-style completions.
pub enum LlmBackend {
    /// In-process llama.cpp model (GGUF).
    Local(Summarizer),
    /// User-configured OpenAI-compatible server (Phase 24b).
    Remote(RemoteLlm),
}

impl LlmBackend {
    /// Load the local llama.cpp backend from a GGUF path.
    pub fn load_local(model_path: &Path) -> Result<Self> {
        Ok(Self::Local(Summarizer::load(model_path)?))
    }

    /// Connect the remote backend (no I/O happens until the first request).
    pub fn remote(cfg: RemoteConfig) -> Self {
        Self::Remote(RemoteLlm::new(cfg))
    }

    /// Summarize a transcript into Markdown notes.
    pub fn summarize(&self, transcript: &str, system_prompt: &str) -> Result<String> {
        let user = format!("Transcript:\n\n{transcript}");
        self.generate(&user, system_prompt, GenOpts::summary())
    }

    /// Compress a transcript into token-minimal dense prose (Phase 23).
    pub fn compress(&self, transcript: &str, system_prompt: &str, n_ctx: u32) -> Result<String> {
        let user = format!("Transcript:\n\n{transcript}");
        self.generate(&user, system_prompt, GenOpts::compress(n_ctx))
    }

    /// One chat completion over `user_content` with `system_prompt`, sized by `opts`.
    pub fn generate(&self, user_content: &str, system_prompt: &str, opts: GenOpts) -> Result<String> {
        match self {
            Self::Local(s) => s.generate(user_content, system_prompt, opts),
            Self::Remote(r) => r.generate(user_content, system_prompt, opts),
        }
    }

    /// Multi-turn grounded chat (Phase 23d).
    pub fn chat(&self, system_prompt: &str, turns: &[(ChatRole, String)], n_ctx: u32) -> Result<String> {
        match self {
            Self::Local(s) => s.chat(system_prompt, turns, n_ctx),
            Self::Remote(r) => r.chat(system_prompt, turns, n_ctx),
        }
    }

    /// Token count of `text` for input budgeting (Overview / chat context).
    /// Exact for the local model; a ~4 chars/token estimate for remote (the
    /// server owns its real context — this only sizes what we send).
    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        match self {
            Self::Local(s) => s.count_tokens(text),
            Self::Remote(_) => Ok(crate::remote::estimate_tokens(text)),
        }
    }
}
