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
    /// Whether the database is encrypted (SQLCipher). Requires an `encryption`
    /// build to actually open.
    pub encrypted: bool,
    /// Set by the GUI to request encrypting the DB on next launch (migration
    /// runs before the engine opens any connection — safe). Uses the keychain
    /// passphrase.
    #[serde(default)]
    pub encrypt_pending: bool,
    /// Likewise, request decrypting on next launch.
    #[serde(default)]
    pub decrypt_pending: bool,
    /// Which audio to record: "both" | "mic" | "system".
    #[serde(default = "default_capture_mode")]
    pub capture_mode: String,
    /// Summary LLM id (see zord-summarize catalog).
    #[serde(default = "default_summary_model")]
    pub summary_model: String,
    /// Summary style preset id (see `summary_presets`).
    #[serde(default = "default_summary_preset")]
    pub summary_preset: String,
    /// Freeform system-prompt override; `None`/empty = use the preset.
    #[serde(default)]
    pub summary_prompt: Option<String>,
    /// Run speaker diarization automatically when a recording stops.
    #[serde(default = "default_true")]
    pub diarize_auto: bool,
    /// Show provisional speaker labels live during recording. Accurate labels
    /// are always recomputed by the offline pass at stop; this only affects the
    /// in-progress display. Off by default to spare constrained hardware.
    #[serde(default)]
    pub diarize_live: bool,
    /// Speaker-embedding model id for diarization (see zord-diarize catalog).
    #[serde(default = "default_embedding_model")]
    pub diarize_embedding_model: String,
    /// Keep the "Others" track after recording so speakers can be re-identified
    /// later (e.g. with a different/bigger model), even when `keep_audio` is off.
    /// On by default so "Identify speakers" works on past recordings; the kept
    /// track lives in the audio dir and is pruned by `auto_delete_days`.
    #[serde(default = "default_true")]
    pub diarize_keep_audio: bool,
    /// Force a fixed number of speakers for diarization (0 = auto-detect).
    /// Set this to a known headcount when auto-clustering over-splits (e.g. a
    /// 10-person call coming out as 80 "speakers").
    #[serde(default)]
    pub diarize_num_speakers: u32,
    /// Auto-generate a short session title from the summary (needs a summary
    /// model). Never overwrites a title you set manually.
    #[serde(default = "default_true")]
    pub auto_title: bool,
    /// Diarization clustering threshold (0.1–0.95) used when the speaker count is
    /// auto. Lower = split into more speakers; higher = merge into fewer.
    #[serde(default = "default_diarize_threshold")]
    pub diarize_threshold: f32,
    /// Context window (tokens) used when *compressing* a meeting into dense prose
    /// (Phase 23). Larger ingests a longer meeting without truncation but costs
    /// more KV-cache RAM + CPU prefill time. 16K fits ~an hour; a 3B model is the
    /// sweet spot on 16 GB. Clamped to [8K, 128K] by the summarizer.
    #[serde(default = "default_compress_ctx")]
    pub compress_ctx: u32,
    /// Context window (tokens) for the cross-meeting **Overview** synthesis pass
    /// (Phase 23). Default 32K; raise toward 64–128K for more meetings at once
    /// (3B model recommended beyond 32K on a 16 GB machine). Clamped to [8K, 128K].
    #[serde(default = "default_overview_ctx")]
    pub overview_ctx: u32,
    /// How many of the most recent meetings to feed into the Overview synthesis.
    #[serde(default = "default_overview_max_meetings")]
    pub overview_max_meetings: u32,
}

fn default_diarize_threshold() -> f32 {
    0.5
}

fn default_compress_ctx() -> u32 {
    16_384
}

fn default_overview_ctx() -> u32 {
    32_768
}

fn default_overview_max_meetings() -> u32 {
    50
}

/// System-prompt instructions for grounded **chat** (Phase 23d) — answering
/// questions about a meeting or across meetings. The caller appends the context
/// (a transcript / compression / the assembled compressions) after these.
pub fn chat_system_prompt() -> &'static str {
    "You are a helpful assistant answering the user's questions about their \
     meeting(s). The user is \"Me\". Answer ONLY from the context provided below \
     — do not use outside knowledge or invent facts, owners, dates, or decisions. \
     If the answer isn't in the context, say so plainly (e.g. it wasn't discussed). \
     Be direct and specific; when it helps, attribute to a speaker and cite the \
     meeting by its date/title. Keep answers concise."
}

/// System prompt for the cross-meeting **Overview** synthesis (Phase 23). Input
/// is the per-meeting dense compressions (each headed by date + title), newest
/// first; output is one holistic, project-grouped Markdown rollup oriented around
/// the user ("Me").
pub fn overview_prompt() -> &'static str {
    "You are given dense, machine-written summaries of the user's recent meetings, \
     each headed by its date and title and ordered newest first. The user is \
     \"Me\". Synthesize ONE holistic, current picture across all of them — not a \
     per-meeting recap. Group everything by project/topic: infer the projects \
     yourself and merge duplicate or near-duplicate names into one consistent \
     label. Output Markdown. Start with \"## My open action items\": a checklist \
     of what *Me* still owns or is waiting on, most urgent first, each citing the \
     meeting it came from. Then one \"## <Project>\" section per project, each \
     with: a one-line **State** (where it stands now); **Pending** (in-progress / \
     upcoming work as owner → task → status); **Done** (recently completed + who); \
     **Owners**; and **Open questions** (unknowns / blockers). When meetings \
     conflict, prefer the most recent; drop items that were resolved or closed. \
     Attribute to names where known and cite source meetings by title in \
     parentheses. Be faithful and specific — do not invent facts, owners, or \
     statuses; if something is unknown, say so."
}

/// System prompt for **compressing** a meeting transcript into token-minimal
/// dense prose (Phase 23). The output is working memory for the cross-meeting
/// Overview synthesis — written for a machine to re-read, not for a human — so it
/// drops all formatting and pleasantries while keeping every concrete fact.
pub fn compress_prompt() -> &'static str {
    "You compress a meeting transcript into the most information-dense form \
     possible, to be re-read later by another model — not by a human. Preserve \
     every concrete fact while removing all redundancy, filler, hedging, and \
     pleasantries. Write FREE-FORM DENSE PROSE: compact declarative sentences, \
     no headings, no bullet lists, no markdown. Capture which projects/topics \
     were discussed and their current state; action items as owner → task → \
     status; what was completed and by whom; decisions made; and open questions \
     or blockers. Attribute facts to the speaker labels in the transcript (\"Me\" \
     is the local user; others appear by name or as \"Speaker N\"). Prefer names \
     and specifics over vague references. Omit nothing factual; invent nothing. \
     Be as short as the facts allow."
}

/// System prompt for auto-titling a recorded session from its summary/transcript.
pub fn title_prompt() -> &'static str {
    "You write a short, specific title for a meeting from its notes. Reply with \
     ONLY the title: 4–8 words, no surrounding quotes, no trailing punctuation, \
     no preamble or explanation."
}

/// Clean an LLM-produced title: first non-empty line, strip wrapping quotes and
/// a leading "Title:" label, trim trailing punctuation, and cap the length.
pub fn clean_title(raw: &str) -> String {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line
        .trim_start_matches("Title:")
        .trim_start_matches("title:")
        .trim();
    let line = line.trim_matches(|c| c == '"' || c == '\'' || c == '`').trim();
    let line = line.trim_end_matches(['.', ',', ';', ':']).trim();
    line.chars().take(80).collect()
}

fn default_true() -> bool {
    true
}
fn default_embedding_model() -> String {
    "3dspeaker-eres2netv2".to_string()
}

fn default_capture_mode() -> String {
    "both".to_string()
}
fn default_summary_model() -> String {
    "qwen2.5-3b-instruct".to_string()
}
fn default_summary_preset() -> String {
    "balanced".to_string()
}

/// Summary style presets: (id, label, system prompt).
pub fn summary_presets() -> &'static [(&'static str, &'static str, &'static str)] {
    &[
        (
            "balanced",
            "Balanced (TL;DR + points + actions)",
            "You are a meeting-notes assistant. Each line of the transcript is prefixed with its speaker: \"Me\" is the local user, and other participants appear by name (e.g. \"Alex\") or as \"Speaker 1\", \"Speaker 2\", … Attribute key points and action items to the relevant speaker by that label wherever possible. Produce concise Markdown with three sections: a one-sentence **TL;DR**, a short **Key points** bullet list, and **Action items** (who + what) if any. Be faithful to the transcript and do not invent details.",
        ),
        (
            "bullets",
            "Bulleted key points",
            "Summarize the transcript as a tight Markdown bullet list of the main points, in order. No preamble, no headings. Be faithful; don't invent details.",
        ),
        (
            "exec",
            "Executive brief",
            "Write a 2–3 sentence executive brief of the transcript capturing the key decisions and outcomes. Plain prose, no bullet points. Be faithful; don't invent details.",
        ),
        (
            "actions",
            "Action items only",
            "Extract only the action items from the transcript as a Markdown checklist: who is responsible (use the speaker label prefixing each line — a name or \"Speaker N\"), what they will do, and any due date mentioned. If there are none, say \"No action items.\"",
        ),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo-q5_0".to_string(),
            keep_audio: false,
            auto_delete_days: None,
            input_device: None,
            storage_dir: None,
            encrypted: false,
            encrypt_pending: false,
            decrypt_pending: false,
            capture_mode: default_capture_mode(),
            summary_model: default_summary_model(),
            summary_preset: default_summary_preset(),
            summary_prompt: None,
            diarize_auto: true,
            diarize_live: false,
            diarize_embedding_model: default_embedding_model(),
            diarize_keep_audio: true,
            diarize_num_speakers: 0,
            auto_title: true,
            diarize_threshold: default_diarize_threshold(),
            compress_ctx: default_compress_ctx(),
            overview_ctx: default_overview_ctx(),
            overview_max_meetings: default_overview_max_meetings(),
        }
    }
}

impl Settings {
    /// The system prompt to summarize with: the custom override if set,
    /// otherwise the selected preset's prompt (falling back to "balanced").
    pub fn effective_summary_prompt(&self) -> String {
        if let Some(p) = self.summary_prompt.as_ref().filter(|p| !p.trim().is_empty()) {
            return p.clone();
        }
        let presets = summary_presets();
        presets
            .iter()
            .find(|(id, _, _)| *id == self.summary_preset)
            .or_else(|| presets.first())
            .map(|(_, _, prompt)| prompt.to_string())
            .unwrap_or_default()
    }
}

/// Optional OS-keychain storage for the database passphrase
/// (macOS Keychain / Windows Credential Manager / Linux Secret Service).
#[cfg(feature = "encryption")]
pub mod keychain {
    const SERVICE: &str = "io.zord.zord";
    const ACCOUNT: &str = "db-passphrase";

    fn entry() -> Option<keyring::Entry> {
        keyring::Entry::new(SERVICE, ACCOUNT).ok()
    }

    /// Remember the passphrase in the OS keychain.
    pub fn store(passphrase: &str) -> anyhow::Result<()> {
        entry()
            .ok_or_else(|| anyhow::anyhow!("no keychain available"))?
            .set_password(passphrase)?;
        Ok(())
    }

    /// Retrieve a remembered passphrase, if any.
    pub fn get() -> Option<String> {
        entry()?.get_password().ok()
    }

    /// Forget any remembered passphrase.
    pub fn clear() {
        if let Some(e) = entry() {
            let _ = e.delete_credential();
        }
    }
}

/// The OS app-data directory (`~/Library/Application Support/Zord` on macOS).
pub fn app_data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "Zord")
        .context("could not resolve an app data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

/// Path to the `config.json` settings file.
pub fn config_path() -> Result<PathBuf> {
    Ok(app_data_dir()?.join("config.json"))
}

/// Directory where downloaded models live. Always under the app-data dir
/// (independent of `storage_dir`) — matches `zord_transcribe::model_cache_dir`.
pub fn models_dir() -> Result<PathBuf> {
    let d = app_data_dir()?.join("models");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Directory for app log files (`zord.log`). Kept in the app-data dir so
/// diagnostics are always writable regardless of any `storage_dir` relocation.
pub fn logs_dir() -> Result<PathBuf> {
    let d = app_data_dir()?.join("logs");
    std::fs::create_dir_all(&d)?;
    Ok(d)
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
