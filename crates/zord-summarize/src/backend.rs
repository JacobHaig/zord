//! Phase 24 — the backend seam: the one type every LLM feature talks to.
//!
//! Summarize, Compress, Overview, Chat, and auto-title all reduce to
//! "chat-style messages in → string out", so they program against [`LlmBackend`]
//! instead of a concrete engine: the in-process llama.cpp model (`llama`
//! feature), or a user-configured OpenAI-compatible server (`remote` feature —
//! LM Studio, `ollama serve`, vLLM, …). The variants compile independently, so
//! a build can carry either backend or both.

use anyhow::Result;

use crate::opts::{ChatRole, GenOpts};

/// A loaded LLM ready to run chat-style completions.
pub enum LlmBackend {
    /// In-process llama.cpp model (GGUF).
    #[cfg(feature = "llama")]
    Local(crate::summarizer::Summarizer),
    /// User-configured OpenAI-compatible server (Phase 24b).
    #[cfg(feature = "remote")]
    Remote(crate::remote::RemoteLlm),
}

impl LlmBackend {
    /// Load the local llama.cpp backend from a GGUF path.
    #[cfg(feature = "llama")]
    pub fn load_local(model_path: &std::path::Path) -> Result<Self> {
        Ok(Self::Local(crate::summarizer::Summarizer::load(model_path)?))
    }

    /// Connect the remote backend (no I/O happens until the first request).
    #[cfg(feature = "remote")]
    pub fn remote(cfg: crate::remote::RemoteConfig) -> Self {
        Self::Remote(crate::remote::RemoteLlm::new(cfg))
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
            #[cfg(feature = "llama")]
            Self::Local(s) => s.generate(user_content, system_prompt, opts),
            #[cfg(feature = "remote")]
            Self::Remote(r) => r.generate(user_content, system_prompt, opts),
        }
    }

    /// Multi-turn grounded chat (Phase 23d).
    pub fn chat(&self, system_prompt: &str, turns: &[(ChatRole, String)], n_ctx: u32) -> Result<String> {
        match self {
            #[cfg(feature = "llama")]
            Self::Local(s) => s.chat(system_prompt, turns, n_ctx),
            #[cfg(feature = "remote")]
            Self::Remote(r) => r.chat(system_prompt, turns, n_ctx),
        }
    }

    /// Like [`chat`], but reports pieces of the reply to `on_delta` as they are
    /// generated (Phase 24d streaming) — token pieces locally, SSE deltas
    /// remotely. Returns the full reply at the end.
    pub fn chat_stream(
        &self,
        system_prompt: &str,
        turns: &[(ChatRole, String)],
        n_ctx: u32,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String> {
        match self {
            #[cfg(feature = "llama")]
            Self::Local(s) => s.chat_stream(system_prompt, turns, n_ctx, on_delta),
            #[cfg(feature = "remote")]
            Self::Remote(r) => r.chat_stream(system_prompt, turns, n_ctx, on_delta),
        }
    }

    /// Token count of `text` for input budgeting (Overview / chat context).
    /// Exact for the local model; a ~4 chars/token estimate for remote (the
    /// server owns its real context — this only sizes what we send).
    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        match self {
            #[cfg(feature = "llama")]
            Self::Local(s) => s.count_tokens(text),
            #[cfg(feature = "remote")]
            Self::Remote(_) => Ok(crate::remote::estimate_tokens(text)),
        }
    }
}
