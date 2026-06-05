//! llama.cpp-backed summarizer. Loads a GGUF instruct model and runs a single
//! chat completion to turn a transcript into Markdown notes.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{anyhow, Context, Result};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel, Special};
use llama_cpp_2::sampling::LlamaSampler;

use crate::opts::{truncate_chars, ChatRole, GenOpts};

/// Selectable summary LLM (Qwen2.5 Instruct GGUF, Q4_K_M).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryModel {
    Qwen1_5B,
    Qwen3B,
    Qwen7B,
}

impl SummaryModel {
    pub const ALL: &'static [SummaryModel] =
        &[SummaryModel::Qwen1_5B, SummaryModel::Qwen3B, SummaryModel::Qwen7B];

    pub fn name(self) -> &'static str {
        match self {
            SummaryModel::Qwen1_5B => "qwen2.5-1.5b-instruct",
            SummaryModel::Qwen3B => "qwen2.5-3b-instruct",
            SummaryModel::Qwen7B => "qwen2.5-7b-instruct",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SummaryModel::Qwen1_5B => "Qwen2.5 1.5B — fastest, lighter quality",
            SummaryModel::Qwen3B => "Qwen2.5 3B — balanced (default)",
            SummaryModel::Qwen7B => "Qwen2.5 7B — best quality, slower",
        }
    }

    pub fn size_label(self) -> &'static str {
        match self {
            SummaryModel::Qwen1_5B => "~1 GB",
            SummaryModel::Qwen3B => "~2 GB",
            SummaryModel::Qwen7B => "~4.7 GB",
        }
    }

    fn filename(self) -> &'static str {
        match self {
            SummaryModel::Qwen1_5B => "qwen2.5-1.5b-instruct-q4_k_m.gguf",
            SummaryModel::Qwen3B => "qwen2.5-3b-instruct-q4_k_m.gguf",
            SummaryModel::Qwen7B => "qwen2.5-7b-instruct-q4_k_m.gguf",
        }
    }

    pub fn url(self) -> &'static str {
        match self {
            SummaryModel::Qwen1_5B => "https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/main/qwen2.5-1.5b-instruct-q4_k_m.gguf",
            SummaryModel::Qwen3B => "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf",
            SummaryModel::Qwen7B => "https://huggingface.co/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/main/qwen2.5-7b-instruct-q4_k_m.gguf",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|m| m.name() == s)
    }

    /// Non-HuggingFace mirror URL (ModelScope) for the same GGUF — for users
    /// whose network blocks HuggingFace. Same filename as [`filename`], so a
    /// browser-download dropped into the models folder is recognized as this
    /// built-in model.
    pub fn mirror_url(self) -> &'static str {
        match self {
            SummaryModel::Qwen1_5B => "https://modelscope.cn/models/Qwen/Qwen2.5-1.5B-Instruct-GGUF/resolve/master/qwen2.5-1.5b-instruct-q4_k_m.gguf",
            SummaryModel::Qwen3B => "https://modelscope.cn/models/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/master/qwen2.5-3b-instruct-q4_k_m.gguf",
            SummaryModel::Qwen7B => "https://modelscope.cn/models/Qwen/Qwen2.5-7B-Instruct-GGUF/resolve/master/qwen2.5-7b-instruct-q4_k_m.gguf",
        }
    }
}

fn models_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "Zord")
        .ok_or_else(|| anyhow!("could not resolve a data directory"))?;
    let dir = dirs.data_dir().join("models");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Delete a downloaded summary model (no-op if absent).
pub fn delete_summary_model(model: SummaryModel) -> Result<()> {
    if let Ok(dir) = models_dir() {
        let path = dir.join(model.filename());
        if path.exists() {
            std::fs::remove_file(&path).with_context(|| format!("deleting {path:?}"))?;
        }
    }
    Ok(())
}

/// List user-supplied GGUF files in the models folder that aren't one of the
/// built-in catalog models. Lets people use a model from any source (e.g. a
/// GitHub mirror) by simply dropping the `.gguf` in — no HuggingFace needed.
pub fn list_custom_models() -> Vec<String> {
    let Ok(dir) = models_dir() else {
        return Vec::new();
    };
    let mut known: Vec<&str> = SummaryModel::ALL.iter().map(|m| m.filename()).collect();
    known.extend(ollama_models().iter().map(|m| m.filename)); // shown via the Ollama catalog
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_gguf = name.to_ascii_lowercase().ends_with(".gguf");
            let nonempty = entry.metadata().map(|m| m.is_file() && m.len() > 0).unwrap_or(false);
            if is_gguf && nonempty && !known.contains(&name.as_str()) {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

/// Resolve a custom (non-catalog) summary model file by name to its path. Only
/// accepts a bare `.gguf` filename in the models folder (no path traversal).
pub fn custom_model_path(name: &str) -> Option<PathBuf> {
    if name.contains('/') || name.contains('\\') || !name.to_ascii_lowercase().ends_with(".gguf") {
        return None;
    }
    let path = models_dir().ok()?.join(name);
    path.is_file().then_some(path)
}

/// Delete a user-supplied custom GGUF from the models folder (no-op if absent).
pub fn delete_custom_model(name: &str) -> Result<()> {
    if let Some(path) = custom_model_path(name) {
        std::fs::remove_file(&path).with_context(|| format!("deleting {path:?}"))?;
    }
    Ok(())
}

/// A small instruct model downloadable from the **Ollama registry** (used purely
/// as a model CDN — no Ollama engine/daemon). Downloaded as a `.gguf` and run via
/// the same llama.cpp path as the built-ins. Reachable on many networks that
/// block HuggingFace.
pub struct OllamaModel {
    pub repo: &'static str,
    pub tag: &'static str,
    pub filename: &'static str,
    pub label: &'static str,
    pub size_label: &'static str,
}

/// Curated small instruct models offered via the Ollama registry.
pub fn ollama_models() -> &'static [OllamaModel] {
    &[
        OllamaModel { repo: "qwen2.5", tag: "3b", filename: "qwen2.5-3b-ollama.gguf", label: "Qwen2.5 3B Instruct — GGUF download from the Ollama registry (non-HF)", size_label: "~1.9 GB" },
        OllamaModel { repo: "qwen2.5", tag: "1.5b", filename: "qwen2.5-1.5b-ollama.gguf", label: "Qwen2.5 1.5B Instruct — GGUF download from the Ollama registry (non-HF)", size_label: "~1 GB" },
        OllamaModel { repo: "llama3.2", tag: "3b", filename: "llama3.2-3b-ollama.gguf", label: "Llama 3.2 3B Instruct — GGUF download from the Ollama registry (non-HF)", size_label: "~2 GB" },
        OllamaModel { repo: "phi3.5", tag: "latest", filename: "phi3.5-ollama.gguf", label: "Phi-3.5 mini Instruct — GGUF download from the Ollama registry (non-HF)", size_label: "~2.2 GB" },
    ]
}

/// Whether a curated Ollama model has been downloaded into the models folder.
pub fn ollama_model_present(filename: &str) -> bool {
    ollama_models().iter().any(|m| m.filename == filename)
        && models_dir()
            .map(|d| d.join(filename))
            .map(|p| p.exists() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false))
            .unwrap_or(false)
}

/// Download a curated Ollama model (by its `.gguf` filename) to the models folder.
pub fn ensure_ollama_model(
    filename: &str,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let spec = ollama_models()
        .iter()
        .find(|m| m.filename == filename)
        .ok_or_else(|| anyhow!("unknown Ollama model '{filename}'"))?;
    let path = models_dir()?.join(spec.filename);
    if path.exists() && std::fs::metadata(&path)?.len() > 0 {
        return Ok(path);
    }
    tracing::info!(repo = spec.repo, tag = spec.tag, "downloading model via Ollama registry");
    zord_net::download_ollama_model(spec.repo, spec.tag, &path, progress)?;
    Ok(path)
}

pub fn summary_model_present(model: SummaryModel) -> bool {
    models_dir()
        .map(|d| d.join(model.filename()))
        .map(|p| p.exists() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false))
        .unwrap_or(false)
}

/// Ensure `model` is downloaded; returns its path. `progress` → (downloaded, total).
pub fn ensure_summary_model(
    model: SummaryModel,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let path = models_dir()?.join(model.filename());
    if path.exists() && std::fs::metadata(&path)?.len() > 0 {
        return Ok(path);
    }
    let url = model.url();
    tracing::info!(%url, "downloading summary model (first run)");
    zord_net::download_to_file(url, &path, progress)?;
    Ok(path)
}

fn backend() -> Result<&'static LlamaBackend> {
    static CELL: OnceLock<LlamaBackend> = OnceLock::new();
    if let Some(b) = CELL.get() {
        return Ok(b);
    }
    let b = LlamaBackend::init().map_err(|e| anyhow!("llama backend init: {e}"))?;
    Ok(CELL.get_or_init(|| b))
}

pub struct Summarizer {
    model: LlamaModel,
}

impl Summarizer {
    pub fn load(model_path: &std::path::Path) -> Result<Self> {
        let backend = backend()?;
        #[allow(unused_mut)]
        let mut params = LlamaModelParams::default();
        #[cfg(target_os = "macos")]
        {
            params = params.with_n_gpu_layers(999); // offload all layers to Metal
        }
        let size_mb = std::fs::metadata(model_path).map(|m| m.len() / 1_048_576).unwrap_or(0);
        breadcrumb(&format!("load:start {} ({size_mb} MB)", model_path.display()));
        let model = LlamaModel::load_from_file(backend, model_path, &params)
            .with_context(|| format!("loading {model_path:?}"))?;
        breadcrumb("load:done");
        Ok(Self { model })
    }

    /// Summarize a transcript into Markdown notes (small context, short output).
    pub fn summarize(&self, transcript: &str, system_prompt: &str) -> Result<String> {
        let user = format!("Transcript:\n\n{transcript}");
        self.generate(&user, system_prompt, GenOpts::summary())
    }

    /// Compress a transcript into token-minimal dense prose (Phase 23). `n_ctx`
    /// sizes the context window so a full meeting fits without truncation.
    pub fn compress(&self, transcript: &str, system_prompt: &str, n_ctx: u32) -> Result<String> {
        let user = format!("Transcript:\n\n{transcript}");
        self.generate(&user, system_prompt, GenOpts::compress(n_ctx))
    }

    /// Count how many tokens `text` is for this model (used to budget the
    /// Overview synthesis input against the context window).
    pub fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(self.model.str_to_token(text, AddBos::Never)?.len())
    }

    /// Run one chat completion over `user_content` with `system_prompt`, sized by
    /// `opts`. The user message is sent verbatim (callers add any framing such as
    /// a "Transcript:" prefix). Shared by [`summarize`], [`compress`], and the
    /// Overview synthesis.
    pub fn generate(&self, user_content: &str, system_prompt: &str, opts: GenOpts) -> Result<String> {
        let user = truncate_chars(user_content, opts.max_transcript_chars);
        let messages = vec![
            LlamaChatMessage::new("system".to_string(), system_prompt.to_string())?,
            LlamaChatMessage::new("user".to_string(), user)?,
        ];
        self.complete(messages, opts)
    }

    /// Multi-turn chat grounded in `system_prompt` (which carries the context to
    /// answer from) over `turns` of alternating user/assistant messages. Returns
    /// the assistant's next reply.
    pub fn chat(&self, system_prompt: &str, turns: &[(ChatRole, String)], n_ctx: u32) -> Result<String> {
        self.chat_stream(system_prompt, turns, n_ctx, &mut |_| {})
    }

    /// Like [`chat`], but calls `on_delta` with each decoded piece as it is
    /// generated (Phase 24d streaming). Returns the full reply at the end.
    pub fn chat_stream(
        &self,
        system_prompt: &str,
        turns: &[(ChatRole, String)],
        n_ctx: u32,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let mut messages = vec![LlamaChatMessage::new("system".to_string(), system_prompt.to_string())?];
        for (role, content) in turns {
            messages.push(LlamaChatMessage::new(role.as_str().to_string(), content.clone())?);
        }
        self.complete_with(messages, GenOpts::chat(n_ctx), on_delta)
    }

    /// Apply the model's chat template to `messages`, prefill, and greedily decode
    /// up to `opts.max_new_tokens`. The shared core under [`generate`]/[`chat`].
    fn complete(&self, messages: Vec<LlamaChatMessage>, opts: GenOpts) -> Result<String> {
        self.complete_with(messages, opts, &mut |_| {})
    }

    /// [`complete`], reporting each decoded piece to `on_delta` as it lands.
    fn complete_with(
        &self,
        messages: Vec<LlamaChatMessage>,
        opts: GenOpts,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<String> {
        let backend = backend()?;
        let tmpl = self
            .model
            .chat_template(None)
            .map_err(|e| anyhow!("model has no chat template: {e}"))?;
        let prompt = self.model.apply_chat_template(&tmpl, &messages, true)?;

        let tokens = self.model.str_to_token(&prompt, AddBos::Always)?;
        if tokens.len() as u32 >= opts.n_ctx {
            return Err(anyhow!("input too long for the model context window"));
        }

        let n_ctx = opts.n_ctx;
        // Breadcrumbs flush to disk so a hard native crash (e.g. CPU-instruction
        // fault or OOM during CPU inference) still leaves the last phase reached.
        breadcrumb(&format!(
            "infer:ctx-alloc n_ctx={n_ctx} prompt_tokens={} max_new={}",
            tokens.len(),
            opts.max_new_tokens
        ));
        let mut ctx = self
            .model
            .new_context(backend, LlamaContextParams::default().with_n_ctx(NonZeroU32::new(n_ctx)))?;

        // Prefill the prompt in chunks no larger than the batch size. Submitting
        // the whole prompt in a single `decode` aborts inside ggml (ggml_abort)
        // once it exceeds n_batch (default 2048) — which a large Overview prompt
        // easily does. 512-token chunks stay safely under n_batch on every backend.
        const PREFILL_CHUNK: usize = 512;
        let mut batch = LlamaBatch::new(PREFILL_CHUNK, 1);
        let last = tokens.len() - 1;
        breadcrumb(&format!("infer:prefill ({} tokens, chunk {PREFILL_CHUNK})", tokens.len()));
        let mut start = 0usize;
        while start < tokens.len() {
            let end = (start + PREFILL_CHUNK).min(tokens.len());
            batch.clear();
            for (offset, tok) in tokens[start..end].iter().enumerate() {
                let pos = (start + offset) as i32;
                batch.add(*tok, pos, &[0], (start + offset) == last)?;
            }
            ctx.decode(&mut batch)?;
            start = end;
        }
        breadcrumb("infer:prefill-done");

        let mut sampler = LlamaSampler::greedy();
        let mut out = String::new();
        let mut n_cur = tokens.len() as i32;

        for _ in 0..opts.max_new_tokens {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if self.model.is_eog_token(token) {
                break;
            }
            let piece = self.model.token_to_str(token, Special::Plaintext).unwrap_or_default();
            if !piece.is_empty() {
                on_delta(&piece);
            }
            out.push_str(&piece);
            batch.clear();
            batch.add(token, n_cur, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch)?;
        }
        breadcrumb("infer:done");

        Ok(out.trim().to_string())
    }
}

/// Append a phase marker to `<app-data>/logs/llm-trace.log`, flushed + synced so
/// it survives even a hard native crash (the app vanishes with no Rust panic).
/// The tail of that file tells us exactly how far the last LLM run got.
fn breadcrumb(line: &str) {
    tracing::info!(target: "zord::llm", "{line}");
    let Some(dir) = directories::ProjectDirs::from("", "", "Zord").map(|d| d.data_dir().join("logs"))
    else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("llm-trace.log"))
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
        let _ = f.sync_all();
    }
}

#[cfg(all(test, feature = "llama"))]
mod prefill_tests {
    use super::*;

    /// Regression test for the ggml_abort on large prompts: feed a prompt well
    /// over n_batch (default 2048) and confirm generation returns Ok instead of
    /// aborting the process. Needs a downloaded GGUF; run with:
    ///   cargo test -p zord-summarize --features llama -- --ignored --nocapture
    #[test]
    #[ignore]
    fn long_prompt_prefills_without_abort() {
        let dir = directories::ProjectDirs::from("", "", "Zord")
            .map(|d| d.data_dir().join("models"))
            .expect("data dir");
        let model = std::fs::read_dir(&dir)
            .expect("models dir")
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().map(|x| x == "gguf").unwrap_or(false))
            .expect("no .gguf model downloaded to test with");
        eprintln!("loading {}", model.display());
        let s = Summarizer::load(&model).expect("load model");
        // ~7000 tokens — well over the default n_batch of 2048 (Overview-sized).
        let long = "The quarterly roadmap review covered staffing, budget, and timelines. "
            .repeat(400);
        let toks = s.count_tokens(&long).unwrap();
        eprintln!("prompt ~{toks} tokens");
        assert!(toks > 2048, "prompt should exceed n_batch to exercise chunking");
        let out = s
            .generate(&long, "Summarize the following in one sentence.", GenOpts::overview(32768))
            .expect("generate should not abort");
        eprintln!("reply: {out}");
        assert!(!out.trim().is_empty());
    }
}
