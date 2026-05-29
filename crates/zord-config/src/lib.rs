//! Persisted application settings and canonical filesystem paths.
//!
//! Settings live in `config.json` in the OS app-data dir. Recordings, the
//! database, and exports live under a configurable `storage_dir` (defaulting to
//! that same app-data dir), so a user can point Zord at, say, an encrypted
//! volume without rebuilding.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User-tunable settings. Everything has a sensible default so a missing or
/// partial config file still works.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Whisper model id (see `zord_transcribe::ModelId::parse`).
    pub model: String,
    /// Keep the captured audio on disk after transcription.
    pub keep_audio: bool,
    /// Auto-delete kept audio older than this many days. `None` = keep forever.
    pub auto_delete_days: Option<u32>,
    /// Preferred microphone device name. `None` = system default.
    pub input_device: Option<String>,
    /// Override for where recordings/db/exports live. `None` = app data dir.
    pub storage_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo-q5_0".to_string(),
            keep_audio: false,
            auto_delete_days: None,
            input_device: None,
            storage_dir: None,
        }
    }
}

/// The OS app-data directory (`~/Library/Application Support/zord` on macOS).
pub fn app_data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("io", "zord", "zord")
        .context("could not resolve an app data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

fn config_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("config.json"))
}

impl Settings {
    /// Load settings, or defaults if the file is missing/unreadable.
    pub fn load() -> Self {
        match config_path().and_then(|p| Ok(std::fs::read_to_string(p)?)) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("config parse failed ({e}); using defaults");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    /// Persist settings to disk (creates the app data dir if needed).
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Root for db/exports/audio (override or app data dir).
    pub fn storage_dir(&self) -> Result<PathBuf> {
        let dir = match &self.storage_dir {
            Some(p) => p.clone(),
            None => app_data_dir()?,
        };
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn db_path(&self) -> Result<PathBuf> {
        Ok(self.storage_dir()?.join("zord.db"))
    }

    pub fn exports_dir(&self) -> Result<PathBuf> {
        let d = self.storage_dir()?.join("exports");
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }

    pub fn audio_dir(&self) -> Result<PathBuf> {
        let d = self.storage_dir()?.join("audio");
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }
}

/// Delete kept-audio files under `audio_dir` older than `days`. No-op when
/// `days` is `None`. Returns how many files were removed.
pub fn apply_retention(audio_dir: &std::path::Path, days: Option<u32>) -> usize {
    let Some(days) = days else { return 0 };
    let max_age = std::time::Duration::from_secs(days as u64 * 86_400);
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    let Ok(entries) = std::fs::read_dir(audio_dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else { continue };
        if now.duration_since(modified).map(|age| age > max_age).unwrap_or(false) {
            if std::fs::remove_file(entry.path()).is_ok() {
                removed += 1;
            }
        }
    }
    if removed > 0 {
        tracing::info!(removed, "retention: deleted old audio files");
    }
    removed
}
