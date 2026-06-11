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
    /// Keep the captured audio on disk after transcription (Phase 25d: one
    /// native-rate track per channel — powers replay, re-transcription, and
    /// re-diarization). On by default, bounded by `auto_delete_days`.
    pub keep_audio: bool,
    /// Auto-delete kept audio older than this many days. `None` = keep forever.
    /// Default 30 (Phase 25d) — the window for re-transcribing with a better
    /// model or re-identifying speakers.
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
    /// Fallback fixed speaker count for diarization (0 = auto-detect), used by
    /// the CLI and the post-recording auto pass. In the GUI, each session has
    /// its own count next to "Identify speakers" which takes precedence.
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
    /// Speaker-segmentation model id for diarization (currently "pyannote-3.0",
    /// MIT). Downloaded on first use. (Rev's Reverb fine-tunes were removed —
    /// non-commercial license.)
    #[serde(default = "default_segmentation_model")]
    pub diarize_segmentation_model: String,
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
    /// GUI sidebar width in px (the session-list / main-window divider is
    /// draggable; the chosen width persists here).
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: u32,
    /// Which LLM runs the AI features (summarize/compress/overview/chat/title):
    /// "local" (built-in llama.cpp GGUF) or "external" (a user-provided
    /// OpenAI-compatible server — Phase 24).
    #[serde(default = "default_llm_backend")]
    pub llm_backend: String,
    /// External server root, e.g. `http://localhost:1234` (LM Studio's default;
    /// Ollama serves on `http://localhost:11434`). Trailing `/v1` is tolerated.
    #[serde(default = "default_llm_base_url")]
    pub llm_base_url: String,
    /// Optional bearer token for the external server ("" = none — typical for
    /// LAN servers). Stored in plaintext here by decision (Phase 24).
    #[serde(default)]
    pub llm_api_key: String,
    /// Model id on the external server (as its `/v1/models` reports it).
    #[serde(default)]
    pub llm_model: String,
    /// Per-request timeout (seconds) for the external server — generations on
    /// big models can take a while.
    #[serde(default = "default_llm_timeout_secs")]
    pub llm_timeout_secs: u64,
    /// Discord integration (Phase 30): the **bot token** for the user's own bot.
    /// Plaintext here, mirroring `llm_api_key` (a LAN-style credential). Empty =
    /// not configured.
    #[serde(default)]
    pub discord_bot_token: String,
    /// The Discord **user id to follow** — the bot joins whatever voice channel
    /// this user is in, across any server the bot shares with them (no guild /
    /// channel to configure). Empty = not configured.
    #[serde(default)]
    pub discord_user_id: String,
    /// Post a "recording started" message in the voice channel's text chat when
    /// the bot joins (Phase 30e). Default ON — it's the consent/transparency
    /// signal Discord's developer policy expects.
    #[serde(default = "default_true")]
    pub discord_announce: bool,
    /// Show the dedicated "Record Discord" button in the sidebar (Phase 30f).
    /// The button additionally requires the `discord` build feature and saved
    /// credentials; this toggle lets users hide it outright.
    #[serde(default = "default_true")]
    pub discord_record_button: bool,
    /// Check GitHub for a newer release at launch (Phase 34; only in
    /// `self-update` builds on the github/dev channel — store builds never
    /// check regardless of this setting). The check is the one network call
    /// the app makes besides user-initiated downloads.
    #[serde(default = "default_true")]
    pub check_updates: bool,
    /// Whether the first-run setup wizard has been completed or skipped
    /// (Phase 36b). `false` shows the wizard on launch; re-runnable from
    /// Settings → About.
    #[serde(default)]
    pub setup_complete: bool,
    /// Compress kept audio (WAV → Opus) once a session is older than this
    /// many days (Phase 37). `Some(0)` = as soon as it has ended; `None` =
    /// never. Default 7 — recent sessions stay bit-exact WAV.
    #[serde(default = "default_compress_after_days")]
    pub compress_after_days: Option<u32>,
    /// Opus quality preset: "space" (24 kbps) | "standard" (32) | "high" (48).
    #[serde(default = "default_compress_quality")]
    pub compress_quality: String,
    /// Per-app capture target (Phase 31, capture_mode == "app"): a stable app
    /// identity — macOS bundle id, Windows executable name. Empty = unset.
    #[serde(default)]
    pub capture_app_id: String,
    /// Display name of the per-app capture target (for the picker UI only).
    #[serde(default)]
    pub capture_app_name: String,
    /// Voiceprints (Phase 38): match speakers against the local library and
    /// auto-name them. Requires the one-time consent flow; off by default.
    #[serde(default)]
    pub voiceprints_enabled: bool,
    /// Match strictness preset: "strict" | "standard" | "relaxed".
    #[serde(default = "default_voiceprints_match")]
    pub voiceprints_match: String,
    /// Unix time the user accepted the voiceprint consent dialog (0 = never).
    #[serde(default)]
    pub voiceprints_consented_at: u64,
    /// Update the living Overview document automatically after each recording
    /// is transcribed & compressed (Phase 39). Manual "Update now" always works.
    #[serde(default = "default_true")]
    pub overview_auto: bool,
    /// Transcribe while recording (Phase 25). Off = capture-only: meters + WAV
    /// writing only (~no CPU, no model RAM — for low-power machines where live
    /// whisper bursts stutter the webcam); transcription runs when you stop.
    #[serde(default = "default_true")]
    pub live_transcription: bool,
    /// Model used by Re-transcribe and by the post-stop pass after capture-only
    /// recordings (Phase 25). Real-time doesn't constrain it, so it can be
    /// bigger than the live model.
    #[serde(default = "default_retranscribe_model")]
    pub retranscribe_model: String,
    /// Tint the sidebar session badges by meaning (summary/compressed/speakers)
    /// vs render them monochrome. Appearance preference (Settings → Theme).
    /// Default monochrome (`false`).
    #[serde(default)]
    pub badge_tint: bool,
    /// Theme overrides (Settings → Theme): `#rrggbb`, empty = built-in default.
    /// `theme_accent` drives the interactive color; `theme_me`/`theme_others`
    /// drive the transcript channel colors. Danger/record red is never themed.
    #[serde(default)]
    pub theme_accent: String,
    #[serde(default)]
    pub theme_me: String,
    #[serde(default)]
    pub theme_others: String,
    /// Microphone ("Me") capture level mode: "off" | "manual" | "auto".
    #[serde(default = "default_level_mode")]
    pub mic_level_mode: String,
    /// Fixed mic gain in dB, applied when `mic_level_mode == "manual"`.
    #[serde(default)]
    pub mic_gain_db: f32,
    /// Desktop/system ("Others") capture level mode: "off" | "manual" | "auto".
    #[serde(default = "default_level_mode")]
    pub others_level_mode: String,
    /// Fixed desktop gain in dB, applied when `others_level_mode == "manual"`.
    #[serde(default)]
    pub others_gain_db: f32,
    /// Run the re-transcription pass automatically when a recording stops
    /// (Phase 25 polish). With live transcription on, this *upgrades* the live
    /// transcript with the (usually bigger) re-transcription model; with live
    /// off, it's when the transcript gets generated at all. Off = defer until
    /// the user presses 🔁 Re-transcribe.
    #[serde(default)]
    pub auto_transcribe: bool,
}

fn default_level_mode() -> String {
    "off".to_string()
}

fn default_retranscribe_model() -> String {
    "large-v3-turbo-q5_0".to_string()
}

fn default_llm_backend() -> String {
    "local".to_string()
}

fn default_llm_base_url() -> String {
    "http://localhost:1234".to_string()
}

fn default_llm_timeout_secs() -> u64 {
    300
}

fn default_sidebar_width() -> u32 {
    240
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

/// System prompt for **compressing** a meeting transcript into a faithful
/// line-by-line condensation (Phase 39). Each speaker's utterance is reworded
/// to its shortest faithful form while preserving speaker labels and original
/// order — the output is a condensed transcript, not a digest.
pub fn compress_prompt() -> &'static str {
    "You condense a meeting transcript line by line. Keep every speaker label \
     (\"Name:\" format) and preserve the original order. Reword each utterance \
     to its shortest faithful form — for example: \"What I've been working on \
     this morning and will continue to work on is the CI/CD process, and I \
     should be done by end of today, at which point Jerry will review\" becomes \
     \"continuing CI/CD work, done by EOD; Jerry reviews after\". Pure-filler \
     lines (greetings, acknowledgements, backchannel agreement) may be dropped \
     entirely. NEVER add headings, bullets, synthesized summaries, tasks, \
     decisions, interpretation, or reorder or merge distinct statements; invent nothing. \
     Output ONLY the condensed transcript lines in the same \"Name: text\" format."
}

/// System prompt for the Phase 39 **living-document** Overview maintainer.
/// Takes the current Overview markdown and a new meeting's condensed transcript,
/// and returns the full updated document.
pub fn overview_doc_prompt() -> &'static str {
    "You maintain a running markdown status document organized by project \
     (\"##\" sections). Fold the new meeting's condensed transcript in: update \
     affected sections in place; create a new section only for a clearly \
     distinct ongoing project. Track action items as \"- [ ]\" / \"- [x]\" with \
     owners; check off items that were completed. Move resolved or stale content \
     to a trailing \"## Archive\" section with the date; delete Archive entries \
     older than 30 days. Preserve any user-written content and wording you are \
     not updating — the user edits this document too. Output the FULL updated \
     document and nothing else — no commentary, no code fences."
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
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line
        .trim_start_matches("Title:")
        .trim_start_matches("title:")
        .trim();
    let line = line
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    let line = line.trim_end_matches(['.', ',', ';', ':']).trim();
    line.chars().take(80).collect()
}

fn default_true() -> bool {
    true
}
fn default_compress_after_days() -> Option<u32> {
    Some(7)
}
fn default_compress_quality() -> String {
    "standard".to_string()
}
fn default_embedding_model() -> String {
    "3dspeaker-eres2netv2".to_string()
}

fn default_segmentation_model() -> String {
    "pyannote-3.0".to_string()
}

fn default_voiceprints_match() -> String {
    "standard".to_string()
}

fn default_capture_mode() -> String {
    "both".to_string()
}
fn default_summary_model() -> String {
    // Commercially licensed (Gemma Terms allow commercial use). The previous
    // default, qwen2.5-3b-instruct, was removed — Qwen Research License is
    // non-commercial. See `migrate_removed_models`.
    "gemma-2-2b-it".to_string()
}

/// Ids of models removed for non-commercial licensing — a saved selection
/// pointing at one is reset to the current default on load.
const REMOVED_SUMMARY_MODELS: &[&str] = &[
    "qwen2.5-3b-instruct",    // Qwen Research License (non-commercial)
    "qwen2.5-3b-ollama.gguf", // same model via the Ollama registry
];
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
        // The four presets above pick an output *format*; the ones below target
        // a meeting *type*, structuring the notes around what that kind of
        // conversation is actually for.
        (
            "decisions",
            "Decision log",
            "You are keeping a decision log. Each transcript line is prefixed with its speaker: \"Me\" is the local user; others appear by name or as \"Speaker N\". Produce Markdown with two sections: **Decisions** — one bullet per decision: what was decided, the key reasoning, and who made or owns it; and **Open questions** — things discussed but left unresolved, with what's blocking them. Ignore chit-chat and status updates. Be faithful to the transcript and do not invent details.",
        ),
        (
            "technical",
            "Engineering notes",
            "You are taking engineering notes. Each transcript line is prefixed with its speaker. Produce Markdown sections, omitting any that don't apply: **Problems & bugs** (symptoms, suspected causes), **Design & architecture** (approaches considered, trade-offs, what was chosen), **Decisions** (one line each), and **Action items** (who + what). Use the transcript's precise technical terminology; keep names, versions and numbers exact. Be faithful; don't invent details.",
        ),
        (
            "standup",
            "Standup (per person)",
            "Summarize this status meeting per person. Each transcript line is prefixed with its speaker: \"Me\" is the local user; others appear by name or as \"Speaker N\". For each speaker who gave an update, write a Markdown subsection with up to three bullets: **Done**, **In progress**, and **Blocked / needs** — only the ones mentioned. End with a short **Team-wide** list for anything affecting everyone (announcements, shared blockers). Be faithful; don't invent details.",
        ),
        (
            "interview",
            "Interview / research debrief",
            "Debrief this interview or research conversation. Each transcript line is prefixed with its speaker. Produce Markdown with: **Context** (one line — who was talked to, about what), **Key responses** (the interviewee's main answers, grouped by topic), **Notable quotes** (short, verbatim, attributed), **Signals & concerns** (strengths, risks, contradictions), and **Follow-ups** (open questions or promised materials). Be faithful; don't invent details.",
        ),
        (
            "oneonone",
            "1:1 debrief",
            "Summarize this one-on-one conversation. Lines are prefixed with the speaker; \"Me\" is the local user. Produce Markdown with: **Topics discussed** (a few bullets), **Feedback** (given and received, attributed), **Agreements & commitments** (who committed to what), and **For next time** (items deferred or to revisit). Keep it brief and discreet in tone. Be faithful; don't invent details.",
        ),
        (
            "study",
            "Learning notes (lecture/webinar)",
            "Turn this lecture, webinar or training transcript into study notes. Produce Markdown with: **In one paragraph** (what the session taught), **Concepts** (each as a bold term followed by a one-to-two line explanation as taught), **Examples** (concrete examples or demos walked through), and **To review** (anything emphasized, referenced for later, or assigned). Ignore housekeeping and small talk. Be faithful to the material; don't invent details.",
        ),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "large-v3-turbo-q5_0".to_string(),
            keep_audio: true,
            auto_delete_days: Some(30),
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
            diarize_num_speakers: 0,
            auto_title: true,
            diarize_threshold: default_diarize_threshold(),
            diarize_segmentation_model: default_segmentation_model(),
            compress_ctx: default_compress_ctx(),
            overview_ctx: default_overview_ctx(),
            overview_max_meetings: default_overview_max_meetings(),
            sidebar_width: default_sidebar_width(),
            llm_backend: default_llm_backend(),
            llm_base_url: default_llm_base_url(),
            llm_api_key: String::new(),
            llm_model: String::new(),
            llm_timeout_secs: default_llm_timeout_secs(),
            discord_bot_token: String::new(),
            discord_user_id: String::new(),
            discord_announce: true,
            discord_record_button: true,
            check_updates: true,
            setup_complete: false,
            compress_after_days: default_compress_after_days(),
            compress_quality: default_compress_quality(),
            capture_app_id: String::new(),
            capture_app_name: String::new(),
            badge_tint: false,
            theme_accent: String::new(),
            theme_me: String::new(),
            theme_others: String::new(),
            mic_level_mode: default_level_mode(),
            mic_gain_db: 0.0,
            others_level_mode: default_level_mode(),
            others_gain_db: 0.0,
            overview_auto: true,
            live_transcription: true,
            retranscribe_model: default_retranscribe_model(),
            auto_transcribe: false,
            voiceprints_enabled: false,
            voiceprints_match: default_voiceprints_match(),
            voiceprints_consented_at: 0,
        }
    }
}

impl Settings {
    /// The system prompt to summarize with: the custom override if set,
    /// otherwise the selected preset's prompt (falling back to "balanced").
    pub fn effective_summary_prompt(&self) -> String {
        if let Some(p) = self
            .summary_prompt
            .as_ref()
            .filter(|p| !p.trim().is_empty())
        {
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

/// Cosine threshold for a voiceprint match (research-tuned presets).
pub fn voiceprint_threshold(preset: &str) -> f32 {
    match preset {
        "strict" => 0.78,
        "relaxed" => 0.66,
        _ => 0.72,
    }
}

/// Strictly `#rrggbb` — anything else is rejected (theme inputs keep the last
/// valid value rather than injecting arbitrary text into a style attribute).
pub fn is_valid_hex_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Tighten a file to owner-only read/write (`0600`) on Unix; no-op elsewhere.
/// Best-effort — a failure here shouldn't fail the write that preceded it.
pub fn restrict_to_owner(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
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
        let mut s = match config_path().and_then(|p| Ok(std::fs::read_to_string(p)?)) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("config parse failed ({e}); using defaults");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        };
        s.apply_migrations();
        s
    }

    /// One-time migrations for values left behind by removed features, applied
    /// on every load (idempotent).
    fn apply_migrations(&mut self) {
        // Migrate away from models removed for non-commercial licensing so an
        // upgraded install doesn't keep pointing at one. (Reverb segmentation is
        // handled by SegmentationModel::parse_or_default falling back to pyannote.)
        if REMOVED_SUMMARY_MODELS.contains(&self.summary_model.as_str()) {
            tracing::info!(
                "summary model '{}' is non-commercial and was removed; resetting to default",
                self.summary_model
            );
            self.summary_model = default_summary_model();
        }
        // The "discord" capture mode became the Record Discord button
        // (June 2026); leftover configs fall back to default local capture.
        if self.capture_mode == "discord" {
            self.capture_mode = "both".to_string();
        }
    }

    /// Persist settings to disk (creates the app data dir if needed). The file
    /// holds the external-LLM API key in plaintext, so it's written `0600` on
    /// Unix (owner read/write only) — defense against same-machine readers,
    /// sync/backup daemons, and sandboxed helpers.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write-temp + rename so a crash mid-write can't truncate the file (a
        // half-written config parses as nothing and silently resets every
        // setting on the next launch). Rename is atomic on one filesystem.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        restrict_to_owner(&tmp); // set perms before it lands at the real path
        std::fs::rename(&tmp, &path)?;
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

    /// Per-session audio **folder**, created and returned (Phase 28). Named with
    /// the session's local start date-time — `audio/2026-06-09_18-15-47/` — for
    /// every recording type. Tracks (`me.wav` / `others.wav` / `spk-N.wav`) live
    /// inside it; `sessions.audio_path` stores this folder path. A numeric suffix
    /// disambiguates the rare same-second collision.
    pub fn session_audio_dir(&self, started_at_ms: u64) -> Result<PathBuf> {
        let base = self.audio_dir()?;
        let name = session_dir_name(started_at_ms);
        let mut dir = base.join(&name);
        let mut n = 2;
        while dir.exists() {
            dir = base.join(format!("{name}_{n}"));
            n += 1;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}

/// Folder name for a session's audio from its start time, in **local** time:
/// `2026-06-09_18-15-47` (sortable, filesystem-safe). Falls back to a
/// timestamp-tagged name if the instant can't be represented.
pub fn session_dir_name(started_at_ms: u64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(started_at_ms as i64).single() {
        Some(dt) => dt.format("%Y-%m-%d_%H-%M-%S").to_string(),
        None => format!("session-{started_at_ms}"),
    }
}

/// Path to write a track file inside a session folder, e.g. `track_path(dir,
/// "me")` → `<dir>/me.wav`. Roles: `me`, `others`, `spk-0`, `spk-1`, …
pub fn track_path(session_dir: &std::path::Path, role: &str) -> PathBuf {
    session_dir.join(format!("{role}.wav"))
}

/// Resolve an existing track file for a session given its stored `audio_path`,
/// transparently handling **both** layouts: the new per-session folder
/// (`<audio_path>/<role>.wav`) and the legacy flat prefix
/// (`<audio_path>.<role>.wav`). Returns `None` if neither exists.
pub fn resolve_track(audio_path: &str, role: &str) -> Option<PathBuf> {
    // WAV first (exact, recent), then the compressed form (Phase 37) — in
    // both the folder and the legacy flat layout.
    for ext in ["wav", "opus"] {
        let folder = std::path::Path::new(audio_path).join(format!("{role}.{ext}"));
        if folder.is_file() {
            return Some(folder);
        }
        let flat = PathBuf::from(format!("{audio_path}.{role}.{ext}"));
        if flat.is_file() {
            return Some(flat);
        }
    }
    None
}

/// Delete kept audio older than `days`. No-op when `days` is `None`. Returns how
/// many entries were removed. Handles **both** layouts: new per-session
/// **folders** (`remove_dir_all`) and legacy flat `<id>.<role>.wav` **files**.
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
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let too_old = now
            .duration_since(modified)
            .map(|age| age > max_age)
            .unwrap_or(false);
        if !too_old {
            continue;
        }
        let path = entry.path();
        let ok = if meta.is_dir() {
            std::fs::remove_dir_all(&path).is_ok()
        } else {
            std::fs::remove_file(&path).is_ok()
        };
        if ok {
            removed += 1;
        }
    }
    if removed > 0 {
        tracing::info!(removed, "retention: deleted old audio (files/folders)");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zord-cfg-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dir_name_is_sortable_datetime() {
        let n = session_dir_name(1_781_374_547_000);
        // YYYY-MM-DD_HH-MM-SS
        assert_eq!(n.len(), 19, "got {n}");
        assert_eq!(&n[4..5], "-");
        assert_eq!(&n[7..8], "-");
        assert_eq!(&n[10..11], "_");
        assert_eq!(&n[13..14], "-");
    }

    #[test]
    fn resolves_new_folder_layout() {
        let dir = tmp("new");
        fs::write(track_path(&dir, "me"), b"x").unwrap();
        fs::write(track_path(&dir, "spk-0"), b"x").unwrap();
        let ap = dir.to_str().unwrap();
        assert_eq!(resolve_track(ap, "me"), Some(dir.join("me.wav")));
        assert_eq!(resolve_track(ap, "spk-0"), Some(dir.join("spk-0.wav")));
        assert_eq!(resolve_track(ap, "others"), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolves_opus_fallback() {
        // A compressed session (Phase 37): WAV gone, role.opus remains.
        let dir = tmp("opus");
        fs::write(dir.join("me.opus"), b"x").unwrap();
        fs::write(dir.join("spk-1.opus"), b"x").unwrap();
        let ap = dir.to_str().unwrap();
        assert_eq!(resolve_track(ap, "me"), Some(dir.join("me.opus")));
        assert_eq!(resolve_track(ap, "spk-1"), Some(dir.join("spk-1.opus")));
        // WAV wins when both exist (compression not yet verified/promoted).
        fs::write(dir.join("me.wav"), b"x").unwrap();
        assert_eq!(resolve_track(ap, "me"), Some(dir.join("me.wav")));
        // Legacy flat layout compresses too.
        let prefix = dir.join("sess-9");
        fs::write(format!("{}.others.opus", prefix.display()), b"x").unwrap();
        assert_eq!(
            resolve_track(prefix.to_str().unwrap(), "others"),
            Some(PathBuf::from(format!("{}.others.opus", prefix.display())))
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compression_settings_defaults() {
        let s = Settings::default();
        assert_eq!(s.compress_after_days, Some(7));
        assert_eq!(s.compress_quality, "standard");
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.compress_after_days, Some(7));
    }

    #[test]
    fn resolves_old_flat_layout() {
        let dir = tmp("flat");
        // Legacy: audio_path is a prefix, files are `<prefix>.<role>.wav`.
        let prefix = dir.join("sess-123");
        let others = format!("{}.others.wav", prefix.display());
        fs::write(&others, b"x").unwrap();
        let ap = prefix.to_str().unwrap();
        assert_eq!(resolve_track(ap, "others"), Some(PathBuf::from(&others)));
        assert_eq!(resolve_track(ap, "me"), None);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discord_capture_mode_migrates_to_both() {
        // The "discord" capture mode was replaced by the Record Discord
        // button; leftover configs must fall back to local capture.
        let mut s: Settings = serde_json::from_str(r#"{ "capture_mode": "discord" }"#).unwrap();
        s.apply_migrations();
        assert_eq!(s.capture_mode, "both");
        // Other modes pass through untouched.
        let mut s: Settings = serde_json::from_str(r#"{ "capture_mode": "mic" }"#).unwrap();
        s.apply_migrations();
        assert_eq!(s.capture_mode, "mic");
        // The new button toggle defaults on.
        assert!(s.discord_record_button);
    }

    #[test]
    fn theme_settings_roundtrip_and_hex_validation() {
        // Defaults: unset (empty) = use the built-in palette.
        let s = Settings::default();
        assert_eq!(s.theme_accent, "");
        assert_eq!(s.theme_me, "");
        assert_eq!(s.theme_others, "");
        // Validation: exactly #rrggbb.
        assert!(is_valid_hex_color("#4cc2ff"));
        assert!(is_valid_hex_color("#FFB454"));
        assert!(!is_valid_hex_color("4cc2ff"));
        assert!(!is_valid_hex_color("#4cc2f"));
        assert!(!is_valid_hex_color("#4cc2fg"));
        assert!(!is_valid_hex_color("#4cc2ff00"));
    }

    #[test]
    fn setup_complete_defaults_false() {
        // First launch shows the wizard; finishing or skipping flips this.
        let s = Settings::default();
        assert!(!s.setup_complete);
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.setup_complete);
    }

    #[test]
    fn voiceprint_defaults_and_thresholds() {
        let s = Settings::default();
        assert!(!s.voiceprints_enabled);
        assert_eq!(s.voiceprints_consented_at, 0);
        assert_eq!(voiceprint_threshold(&s.voiceprints_match), 0.72);
        assert_eq!(voiceprint_threshold("strict"), 0.78);
        assert_eq!(voiceprint_threshold("bogus"), 0.72);
    }

    #[test]
    fn phase39_prompts_and_defaults() {
        let c = compress_prompt().to_lowercase();
        assert!(
            c.contains("line"),
            "compress prompt must demand line-by-line output"
        );
        assert!(
            !c.contains("action item"),
            "compress prompt must not synthesize"
        );
        let o = overview_doc_prompt().to_lowercase();
        assert!(o.contains("archive") && o.contains("30 days"));
        assert!(o.contains("full updated document"));
        assert!(Settings::default().overview_auto);
    }
}
