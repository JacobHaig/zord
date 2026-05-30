//! Whisper ggml model management: resolve a model id to a local file,
//! downloading it from Hugging Face on first run (never embedded in the app).

use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::path::PathBuf;

const HF_BASE: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Which transcription engine a model runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// whisper.cpp (single ggml `.bin`).
    Whisper,
    /// NVIDIA Parakeet via sherpa-onnx (a directory of ONNX files + tokens).
    Parakeet,
}

/// A selectable model. Whisper variants default to the turbo q5_0 quant; the
/// Parakeet variant runs on the sherpa-onnx backend (only when built with the
/// `parakeet` feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelId {
    TinyEn,
    BaseEn,
    SmallEn,
    MediumEn,
    LargeV3,
    LargeV3Turbo,
    /// large-v3-turbo, q5_0 quantized. Default — best size/speed/accuracy.
    LargeV3TurboQ5,
    /// NVIDIA Parakeet TDT 0.6B v3, int8 (25 languages). sherpa-onnx backend.
    ParakeetTdtV3,
}

/// Every known model (used for `parse`/match completeness).
const EVERY: &[ModelId] = &[
    ModelId::TinyEn,
    ModelId::BaseEn,
    ModelId::SmallEn,
    ModelId::MediumEn,
    ModelId::LargeV3TurboQ5,
    ModelId::LargeV3Turbo,
    ModelId::LargeV3,
    ModelId::ParakeetTdtV3,
];

impl ModelId {
    /// Models shown in the settings UI. Parakeet only appears in `parakeet`
    /// builds (otherwise it can't be downloaded or run).
    pub fn listed() -> &'static [ModelId] {
        #[cfg(feature = "parakeet")]
        {
            EVERY
        }
        #[cfg(not(feature = "parakeet"))]
        {
            &EVERY[..EVERY.len() - 1] // drop the trailing Parakeet entry
        }
    }

    pub fn engine(self) -> Engine {
        match self {
            ModelId::ParakeetTdtV3 => Engine::Parakeet,
            _ => Engine::Whisper,
        }
    }

    /// Whisper: the ggml file name. Parakeet: the model directory name (also the
    /// release archive stem).
    pub fn filename(self) -> &'static str {
        match self {
            ModelId::TinyEn => "ggml-tiny.en.bin",
            ModelId::BaseEn => "ggml-base.en.bin",
            ModelId::SmallEn => "ggml-small.en.bin",
            ModelId::MediumEn => "ggml-medium.en.bin",
            ModelId::LargeV3 => "ggml-large-v3.bin",
            ModelId::LargeV3Turbo => "ggml-large-v3-turbo.bin",
            ModelId::LargeV3TurboQ5 => "ggml-large-v3-turbo-q5_0.bin",
            ModelId::ParakeetTdtV3 => "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ModelId::TinyEn => "tiny.en",
            ModelId::BaseEn => "base.en",
            ModelId::SmallEn => "small.en",
            ModelId::MediumEn => "medium.en",
            ModelId::LargeV3 => "large-v3",
            ModelId::LargeV3Turbo => "large-v3-turbo",
            ModelId::LargeV3TurboQ5 => "large-v3-turbo-q5_0",
            ModelId::ParakeetTdtV3 => "parakeet-tdt-0.6b-v3",
        }
    }

    /// Approximate on-disk size, for the download UI.
    pub fn size_label(self) -> &'static str {
        match self {
            ModelId::TinyEn => "75 MB",
            ModelId::BaseEn => "142 MB",
            ModelId::SmallEn => "466 MB",
            ModelId::MediumEn => "1.5 GB",
            ModelId::LargeV3 => "3.1 GB",
            ModelId::LargeV3Turbo => "1.6 GB",
            ModelId::LargeV3TurboQ5 => "574 MB",
            ModelId::ParakeetTdtV3 => "650 MB",
        }
    }

    /// One-line description for the settings UI.
    pub fn description(self) -> &'static str {
        match self {
            ModelId::TinyEn => "Fastest, lowest accuracy (English). Great on weak CPUs.",
            ModelId::BaseEn => "Fast, modest accuracy (English).",
            ModelId::SmallEn => "Balanced (English). Solid CPU fallback.",
            ModelId::MediumEn => "High accuracy (English), heavier.",
            ModelId::LargeV3 => "Highest accuracy, multilingual. Largest/slowest.",
            ModelId::LargeV3Turbo => "Near large-v3 accuracy, much faster. Multilingual.",
            ModelId::LargeV3TurboQ5 => "Quantized turbo — best all-round. Default.",
            ModelId::ParakeetTdtV3 => "NVIDIA Parakeet TDT (ONNX), 25 languages, fast on CPU.",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        EVERY.iter().copied().find(|m| m.name() == s)
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

/// Whether a model is already downloaded locally.
pub fn is_downloaded(model: ModelId) -> bool {
    model_path_if_present(model)
        .ok()
        .flatten()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// Delete a downloaded model file (no-op if absent).
pub fn delete_model(model: ModelId) -> Result<()> {
    let path = model_cache_dir()?.join(model.filename());
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("deleting {path:?}"))?;
        tracing::info!(?path, "deleted model");
    }
    Ok(())
}
