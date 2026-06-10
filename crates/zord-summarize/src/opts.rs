//! Generation options + chat types shared by every LLM backend (the local
//! llama.cpp engine and the remote OpenAI-compatible client).

/// Tunables for one generation pass. Summarizing and compressing differ mainly
/// in how much input they ingest (context size) and how much they emit.
#[derive(Debug, Clone, Copy)]
pub struct GenOpts {
    /// Model context window (tokens) to allocate for this pass.
    pub n_ctx: u32,
    /// Max tokens to generate.
    pub max_new_tokens: usize,
    /// Hard cap on transcript characters fed in (a coarse pre-truncation guard;
    /// the token-count check below is the real ceiling).
    pub max_transcript_chars: usize,
}

impl GenOpts {
    /// Notes summary: small context, short output (legacy defaults).
    pub fn summary() -> Self {
        Self {
            n_ctx: 8192,
            max_new_tokens: 640,
            max_transcript_chars: 16_000,
        }
    }

    /// Dense-prose compression (Phase 23): a large, configurable context so a
    /// full meeting fits without truncation; modest output (it's condensing).
    /// `n_ctx` is clamped to a sane [8K, 128K] range; the char budget is derived
    /// from it (≈3.5 chars/token) leaving headroom for the prompt + output.
    pub fn compress(n_ctx: u32) -> Self {
        let n_ctx = n_ctx.clamp(8192, 131_072);
        const OUT: usize = 1024;
        let reserve = OUT + 320; // generated tokens + prompt/template overhead
        let input_tokens = (n_ctx as usize).saturating_sub(reserve);
        Self {
            n_ctx,
            max_new_tokens: OUT,
            max_transcript_chars: input_tokens * 7 / 2,
        }
    }

    /// Cross-meeting Overview synthesis (Phase 23): configurable context ingesting
    /// many per-meeting compressions, with a larger output budget (the rollup is
    /// longer than a single compression).
    pub fn overview(n_ctx: u32) -> Self {
        let n_ctx = n_ctx.clamp(8192, 131_072);
        const OUT: usize = 2048;
        let reserve = OUT + 512;
        let input_tokens = (n_ctx as usize).saturating_sub(reserve);
        Self {
            n_ctx,
            max_new_tokens: OUT,
            max_transcript_chars: input_tokens * 7 / 2,
        }
    }

    /// Grounded chat (Phase 23d): configurable context for the grounding material
    /// + conversation, with a short-ish answer budget.
    pub fn chat(n_ctx: u32) -> Self {
        let n_ctx = n_ctx.clamp(8192, 131_072);
        const OUT: usize = 768;
        let reserve = OUT + 512;
        let input_tokens = (n_ctx as usize).saturating_sub(reserve);
        Self {
            n_ctx,
            max_new_tokens: OUT,
            max_transcript_chars: input_tokens * 7 / 2,
        }
    }
}

/// A turn's author in a chat conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Assistant,
}

impl ChatRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ChatRole::User => "user",
            ChatRole::Assistant => "assistant",
        }
    }
}

pub(crate) fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}
