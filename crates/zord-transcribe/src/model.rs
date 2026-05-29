//! Whisper ggml model management: resolve a model id to a local file,
//! downloading it from Hugging Face on first run (never embedded in the app).

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// A selectable model. Defaults to the turbo q5_0 quant: ~95% of large-v3
/// accuracy, a fraction of the size and several times faster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelId {
    /// large-v3-turbo, q5_0 quantized (~574 MB). Default.
    LargeV3TurboQ5,
    /// large-v3-turbo, full precision (~1.5 GB). Highest accuracy.
    LargeV3Turbo,
    /// Small English-only model (~466 MB). Good CPU fallback.
    SmallEn,
}

impl ModelId {
    pub fn filename(self) -> &'static str {
        match self {
            ModelId::LargeV3TurboQ5 => "ggml-large-v3-turbo-q5_0.bin",
            ModelId::LargeV3Turbo => "ggml-large-v3-turbo.bin",
            ModelId::SmallEn => "ggml-small.en.bin",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ModelId::LargeV3TurboQ5 => "large-v3-turbo-q5_0",
            ModelId::LargeV3Turbo => "large-v3-turbo",
            ModelId::SmallEn => "small.en",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "large-v3-turbo-q5_0" => Some(ModelId::LargeV3TurboQ5),
            "large-v3-turbo" => Some(ModelId::LargeV3Turbo),
            "small.en" => Some(ModelId::SmallEn),
            _ => None,
        }
    }
}

/// Directory where models are cached (`~/Library/Application Support/zord/models`
/// on macOS, platform-appropriate elsewhere).
pub fn model_cache_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("io", "zord", "zord")
        .ok_or_else(|| anyhow!("could not resolve a data directory"))?;
    let dir = dirs.data_dir().join("models");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Ensure the model file exists locally, downloading it if absent. Returns the
/// path. `progress` is called with (downloaded_bytes, total_bytes_opt).
pub fn ensure_model(
    model: ModelId,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    let dir = model_cache_dir()?;
    let path = dir.join(model.filename());
    if path.exists() && std::fs::metadata(&path)?.len() > 0 {
        return Ok(path);
    }

    let url = format!("{HF_BASE}/{}", model.filename());
    tracing::info!(%url, "downloading whisper model (first run)");

    let resp = ureq::get(&url)
        .call()
        .with_context(|| format!("requesting {url}"))?;
    let total: Option<u64> = resp
        .header("Content-Length")
        .and_then(|h| h.parse::<u64>().ok());

    // Download to a temp file, then atomically rename, so an interrupted
    // download never leaves a corrupt model in place.
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
    tracing::info!(bytes = downloaded, ?path, "model download complete");
    Ok(path)
}

pub fn model_path_if_present(model: ModelId) -> Result<Option<PathBuf>> {
    let path = model_cache_dir()?.join(model.filename());
    Ok(if path.exists() { Some(path) } else { None })
}

#[allow(dead_code)]
fn _assert_path_send(_p: &Path) {}
