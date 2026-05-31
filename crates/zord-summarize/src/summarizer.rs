//! llama.cpp-backed summarizer. Loads a GGUF instruct model and runs a single
//! chat completion to turn a transcript into Markdown notes.

use std::io::{Read, Write};
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

/// Default summary model: Qwen2.5 3B Instruct, Q4_K_M (~2 GB). Good quality,
/// runs comfortably on Apple Silicon / modern CPUs.
const HF_URL: &str =
    "https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf";
const FILE: &str = "qwen2.5-3b-instruct-q4_k_m.gguf";

const N_CTX: u32 = 8192;
const MAX_NEW_TOKENS: usize = 640;
/// Leave headroom for the system prompt + generated tokens.
const MAX_TRANSCRIPT_CHARS: usize = 16_000;

pub fn summary_model_filename() -> &'static str {
    FILE
}

fn models_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("io", "zord", "zord")
        .ok_or_else(|| anyhow!("could not resolve a data directory"))?;
    let dir = dirs.data_dir().join("models");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn summary_model_present() -> bool {
    models_dir()
        .map(|d| d.join(FILE))
        .map(|p| p.exists() && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false))
        .unwrap_or(false)
}

/// Ensure the summary model is downloaded; returns its path. `progress` is
/// called with (downloaded, total).
pub fn ensure_summary_model(progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    let path = models_dir()?.join(FILE);
    if path.exists() && std::fs::metadata(&path)?.len() > 0 {
        return Ok(path);
    }
    tracing::info!(%HF_URL, "downloading summary model (first run)");
    let resp = ureq::get(HF_URL).call().with_context(|| format!("requesting {HF_URL}"))?;
    let total = resp.header("Content-Length").and_then(|h| h.parse::<u64>().ok());
    let tmp = path.with_extension("partial");
    let mut file = std::fs::File::create(&tmp)?;
    let mut reader = resp.into_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut downloaded = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        downloaded += n as u64;
        progress(downloaded, total);
    }
    file.flush()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
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
        let model = LlamaModel::load_from_file(backend, model_path, &params)
            .with_context(|| format!("loading {model_path:?}"))?;
        Ok(Self { model })
    }

    /// Summarize a transcript into Markdown (TL;DR / key points / action items).
    pub fn summarize(&self, transcript: &str) -> Result<String> {
        let backend = backend()?;
        let system = "You are a meeting-notes assistant. The transcript is labeled by \
            speaker: \"Me\" is the local user, \"Others\" is everyone else. Produce concise \
            Markdown with three sections: a one-sentence **TL;DR**, a short **Key points** \
            bullet list, and **Action items** (who + what) if any. Be faithful to the \
            transcript and do not invent details.";
        let user = format!("Transcript:\n\n{}", truncate_chars(transcript, MAX_TRANSCRIPT_CHARS));

        let messages = vec![
            LlamaChatMessage::new("system".to_string(), system.to_string())?,
            LlamaChatMessage::new("user".to_string(), user)?,
        ];
        let tmpl = self
            .model
            .chat_template(None)
            .map_err(|e| anyhow!("model has no chat template: {e}"))?;
        let prompt = self.model.apply_chat_template(&tmpl, &messages, true)?;

        let tokens = self.model.str_to_token(&prompt, AddBos::Always)?;
        if tokens.len() as u32 >= N_CTX {
            return Err(anyhow!("transcript too long for the model context"));
        }

        let mut ctx = self
            .model
            .new_context(backend, LlamaContextParams::default().with_n_ctx(NonZeroU32::new(N_CTX)))?;

        let mut batch = LlamaBatch::new(N_CTX as usize, 1);
        let last = tokens.len() - 1;
        for (i, tok) in tokens.iter().enumerate() {
            batch.add(*tok, i as i32, &[0], i == last)?;
        }
        ctx.decode(&mut batch)?;

        let mut sampler = LlamaSampler::greedy();
        let mut out = String::new();
        let mut n_cur = tokens.len() as i32;

        for _ in 0..MAX_NEW_TOKENS {
            let token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(token);
            if self.model.is_eog_token(token) {
                break;
            }
            out.push_str(&self.model.token_to_str(token, Special::Plaintext).unwrap_or_default());
            batch.clear();
            batch.add(token, n_cur, &[0], true)?;
            n_cur += 1;
            ctx.decode(&mut batch)?;
        }

        Ok(out.trim().to_string())
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}
