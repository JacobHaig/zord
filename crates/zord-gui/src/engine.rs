//! Threaded recording engine that backs the GUI.
//!
//! The capture handles (cpal `Stream`, `SCStream`) are `!Send`, so all
//! recording lifecycle lives on one dedicated **control thread**. A second
//! **db thread** answers read-only queries (sessions / search / load) so the UI
//! stays responsive while a recording is in progress. Both push [`Event`]s to
//! the GUI over a `tokio` unbounded channel; the GUI sends [`RecorderCmd`] /
//! [`DbCmd`] over std channels.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use zord_audio::{MonoResampler, Segmenter, SegmenterConfig, WavWriter};
use zord_capture::{AudioSource, Microphone, SystemAudio};
use zord_core::{Segment, Session, Source};
use zord_store::Store;
use zord_transcribe::{ensure_model, ModelId, Transcriber};

/// Level-meter design (shared by both channels for consistent behavior).
///
/// Two things make a meter feel right and behave the same for a quiet mic and
/// loud system audio:
/// 1. A **dB (log) scale**: map RMS → dBFS over [FLOOR_DB, 0] → [0, 1]. A linear
///    bar makes loud media peg at 100% while speech barely moves; dB compresses
///    that range so both move proportionally to perceived loudness.
/// 2. **Time-based** attack/release: per-buffer exponential smoothing using each
///    buffer's real duration, so the meter reacts at the same wall-clock speed
///    no matter how big/frequent each source's buffers are (cpal vs SCK/WASAPI).
const LEVEL_FLOOR_DB: f32 = -60.0;
const LEVEL_ATTACK_S: f32 = 0.08;
const LEVEL_RELEASE_S: f32 = 0.35;

/// Recording lifecycle status shown in the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    PreparingModel,
    Downloading(u8),
    Recording,
    Error(String),
}

/// Events from the engine to the GUI.
#[derive(Debug, Clone)]
pub enum Event {
    Status(Status),
    /// Non-fatal notice (e.g. system audio unavailable).
    Notice(String),
    /// A freshly transcribed segment (live).
    Segment(Segment),
    /// Live loudness for a channel: gained RMS with attack/decay ballistics,
    /// 0..1. Identical computation for mic and system so both bars behave alike.
    Level { source: Source, level: f32 },
    /// Result of [`DbCmd::ListSessions`].
    Sessions(Vec<Session>),
    /// Sidebar badges per session id: (has_summary, has_compressed, has_speakers).
    SessionBadges(std::collections::HashMap<String, (bool, bool, bool)>),
    /// Result of [`DbCmd::Search`].
    SearchResults(Vec<(String, Segment)>),
    /// Result of [`DbCmd::Load`] — a session's full transcript.
    Transcript(Vec<Segment>),
    /// A transcript was exported to this path.
    Exported(String),
    /// The model catalog with current download status.
    Models(Vec<ModelInfo>),
    /// Download progress for a model (0..100).
    ModelProgress { name: String, pct: u8 },
    /// A model download failed — the UI offers the manual-fetch fallback
    /// (direct URL + open models folder) for this model.
    DownloadFailed { name: String },
    /// A session's summary (loaded or freshly generated). `None` = none yet.
    Summary(Option<String>),
    /// A session's dense-prose compression (loaded or freshly generated)
    /// (Phase 23). `None` = none yet.
    Compressed(Option<String>),
    /// The cross-meeting Overview rollup (loaded or freshly synthesized)
    /// (Phase 23). `None` = none generated yet.
    Overview(Option<OverviewData>),
    /// The rolling project ledger (Phase 26), loaded or after a fold/rebuild.
    Ledger(LedgerView),
    /// An assistant reply to a chat question (Phase 23d). `scope` says which
    /// conversation it belongs to. (Only produced in `summaries` builds.)
    #[allow(dead_code)]
    ChatReply { scope: ChatScope, reply: String },
    /// A streamed piece of the in-progress chat reply (Phase 24d). Always
    /// followed by a terminal [`Event::ChatReply`] with the full text.
    #[allow(dead_code)]
    ChatDelta { scope: ChatScope, delta: String },
    /// Custom names for diarized speakers in the viewed session (index → name).
    Speakers(std::collections::HashMap<i32, String>),
    /// The viewed session's saved expected-speaker count (0 = auto-detect).
    DiarizeSpeakers(u32),
    /// Which retained per-channel WAVs exist on disk for the viewed session
    /// (absolute paths, me/others). Lines from a channel without a file get no
    /// replay button.
    AudioFiles { me: Option<String>, others: Option<String> },
    /// The transcript line (db id) currently playing back. `None` = stopped or
    /// finished.
    Playing(Option<i64>),
    /// Result of [`ModelCmd::ListRemoteLlm`]: the external server's model ids,
    /// or why it couldn't be reached (Phase 24c).
    RemoteModels { models: Vec<String>, error: Option<String> },
    /// A post-stop / on-demand transcription pass started (Phase 25) — shows
    /// up on the background-jobs board as its own entry.
    Retranscribing,
    /// Terminal counterpart of [`Event::Retranscribing`] — sent whether the
    /// pass succeeded or failed, so the GUI's busy state always clears.
    Retranscribed,
}

/// Which conversation a chat turn belongs to (Phase 23d): a single meeting, or
/// across all recent meetings.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatScope {
    Meeting(String),
    CrossMeeting,
}

/// The cross-meeting Overview rollup for the GUI (feature-independent mirror of
/// `zord_overview::Overview`, so the event type compiles without `summaries`).
#[derive(Debug, Clone, PartialEq)]
pub struct OverviewData {
    pub text: String,
    /// When it was generated (epoch ms).
    pub generated_at: u64,
    /// How many meetings it covered.
    pub meetings: usize,
}

/// The rolling project ledger for the GUI (Phase 26): a feature-independent
/// mirror of the `zord-store` rows, so the event type compiles without an LLM
/// backend. Active projects first; each carries its items.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LedgerView {
    pub projects: Vec<ProjectView>,
    /// Meetings recorded but not yet folded into the ledger (drives the
    /// "Refresh — N new" affordance). No LLM work happens until the user asks.
    pub pending: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    /// "active" | "archived".
    pub status: String,
    pub description: Option<String>,
    pub last_activity: u64,
    pub items: Vec<ItemView>,
}

impl ProjectView {
    /// Items that are still open/blocked/waiting (not done).
    pub fn active_items(&self) -> impl Iterator<Item = &ItemView> {
        self.items.iter().filter(|i| i.status != "done")
    }
    /// Completed items (history).
    pub fn done_items(&self) -> impl Iterator<Item = &ItemView> {
        self.items.iter().filter(|i| i.status == "done")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ItemView {
    pub id: String,
    /// "action" | "question" | "decision".
    pub kind: String,
    pub text: String,
    pub owner: Option<String>,
    /// "open" | "blocked" | "waiting" | "done".
    pub status: String,
    /// Session that marked it done (provenance), if any.
    pub completed_session: Option<String>,
    /// Hand-edited (protected from automatic folds).
    pub manual: bool,
}

/// A model in the catalog plus whether it's downloaded locally. `kind` is
/// "transcription", "summary", or "diarization" so the UI can group them.
/// `urls` are the direct download links for the manual-fetch fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    pub name: String,
    pub size: String,
    pub description: String,
    pub downloaded: bool,
    pub kind: String,
    pub urls: Vec<String>,
}

fn catalog() -> Vec<ModelInfo> {
    // `mut` is used only when summary/diarization features push extra entries.
    #[allow(unused_mut)]
    let mut models: Vec<ModelInfo> = ModelId::listed()
        .iter()
        .map(|&m| ModelInfo {
            name: m.name().to_string(),
            size: m.size_label().to_string(),
            description: m.description().to_string(),
            downloaded: zord_transcribe::is_downloaded(m),
            kind: "transcription".to_string(),
            urls: vec![m.download_url()],
        })
        .collect();
    #[cfg(feature = "llm-local")]
    for &m in zord_summarize::SummaryModel::ALL {
        models.push(ModelInfo {
            name: m.name().to_string(),
            size: m.size_label().to_string(),
            description: m.label().to_string(),
            downloaded: zord_summarize::summary_model_present(m),
            kind: "summary".to_string(),
            // HuggingFace first, then the hf-mirror.com mirror for blocked nets.
            urls: vec![m.url().to_string(), m.mirror_url().to_string()],
        });
    }
    // Small instruct models downloadable from the Ollama registry (non-HF source).
    #[cfg(feature = "llm-local")]
    for m in zord_summarize::ollama_models() {
        models.push(ModelInfo {
            name: m.filename.to_string(),
            size: m.size_label.to_string(),
            description: m.label.to_string(),
            downloaded: zord_summarize::ollama_model_present(m.filename),
            kind: "summary".to_string(),
            urls: Vec::new(),
        });
    }
    // User-supplied GGUFs dropped into the models folder (any source — no HF).
    #[cfg(feature = "llm-local")]
    for name in zord_summarize::list_custom_models() {
        models.push(ModelInfo {
            name,
            size: "local".to_string(),
            description: "Custom GGUF (in models folder)".to_string(),
            downloaded: true,
            kind: "summary".to_string(),
            urls: Vec::new(),
        });
    }
    #[cfg(feature = "diarization")]
    {
        let seg = zord_diarize::SegmentationModel::parse_or_default(
            &zord_config::Settings::load().diarize_segmentation_model,
        );
        for &m in zord_diarize::EmbeddingModel::ALL {
            models.push(ModelInfo {
                name: m.name().to_string(),
                size: m.size_label().to_string(),
                description: m.label().to_string(),
                downloaded: zord_diarize::diar_models_present(seg, m),
                kind: "diarization".to_string(),
                urls: m.download_urls(seg),
            });
        }
    }
    models
}

/// Commands controlling recording.
pub enum RecorderCmd {
    Start {
        model: ModelId,
        keep_audio: bool,
        input_device: Option<String>,
        audio_dir: PathBuf,
        record_mic: bool,
        record_system: bool,
        /// Transcribe while recording (Phase 25). `false` = capture-only:
        /// meters + WAVs, no model load, no transcribe jobs.
        live: bool,
    },
    Stop,
    /// Mute/unmute the microphone ("Me") mid-recording without stopping. While
    /// muted, mic audio is dropped (recorded as silence) — no transcript, meter
    /// falls to zero.
    SetMicMuted(bool),
    /// Mute/unmute the desktop/system audio ("Others") mid-recording without
    /// stopping. Same semantics as [`SetMicMuted`] for the system channel.
    SetSystemMuted(bool),
    /// Stop the engine entirely (process exit normally handles this; kept for
    /// completeness / future graceful shutdown).
    #[allow(dead_code)]
    Shutdown,
}

/// Read-only database queries (plus export, which reads then writes a file).
pub enum DbCmd {
    ListSessions,
    Search(String),
    Load(String),
    Export {
        id: String,
        format: zord_export::Format,
    },
    Rename {
        id: String,
        title: String,
    },
    DeleteSession(String),
    /// Delete a session's stored AI summary (transcript is untouched).
    ClearSummary(String),
    /// Delete a session's stored dense-prose compression (transcript is untouched).
    ClearCompressed(String),
    EditSegment {
        segment_id: i64,
        text: String,
    },
    /// (Re-)run speaker diarization on a past session's retained "Others" audio.
    /// `num_speakers` pins the speaker count for this session (0 = auto-detect);
    /// it is persisted on the session so it's remembered next time.
    Diarize { id: String, num_speakers: u32 },
    /// Re-transcribe a past session from its kept WAVs with the configured
    /// re-transcription model (Phase 25). Replaces existing segments; speaker
    /// labels are re-derived afterwards when the session had them.
    Retranscribe(String),
    /// Rename a diarized speaker (0-based index) within a session.
    RenameSpeaker {
        id: String,
        speaker: i32,
        name: String,
    },
    /// Load the most recently stored cross-meeting Overview (Phase 23).
    LoadOverview,
    /// Load the rolling project ledger (Phase 26) — no LLM, just reads.
    LoadLedger,
    /// Phase 26e manual edits to the ledger. Each re-emits the updated ledger.
    /// All hand edits set the `manual` flag so later auto-folds don't clobber them.
    /// Rename a project.
    RenameProject { id: String, name: String },
    /// Set a project's one-line description/state.
    SetProjectDescription { id: String, description: String },
    /// Flip a project between active and archived.
    SetProjectArchived { id: String, archived: bool },
    /// Delete a project and all its items.
    DeleteProject(String),
    /// Edit an item's text and/or owner (owner empty = clear).
    EditItem { id: String, text: String, owner: String },
    /// Set an item's lifecycle status (open/blocked/waiting/done).
    SetItemStatus { id: String, status: String },
    /// Move an item to another project.
    MoveItem { item_id: String, project_id: String },
    /// Delete a single item.
    DeleteItem(String),
    /// Add a hand-written item to a project.
    AddItem {
        project_id: String,
        kind: String,
        text: String,
        owner: String,
    },
}

/// Replay commands for the playback worker. The rodio output stream is `!Send`
/// (like the capture streams), so a dedicated thread owns it.
pub enum PlayCmd {
    /// Play `[start_ms, end_ms)` of `wav` — a retained track (native capture
    /// rate, Phase 25d), wall-clock aligned at its own rate so segment
    /// timestamps map 1:1 onto sample offsets. `segment_id` is reported back
    /// via [`Event::Playing`] to mark the line.
    Play {
        segment_id: i64,
        wav: PathBuf,
        start_ms: u64,
        end_ms: u64,
    },
    /// Stop any current playback.
    Stop,
}

/// Model-management commands (download/delete can take minutes, so they run on
/// their own worker thread, separate from recording and DB queries).
pub enum ModelCmd {
    List,
    Download(String),
    Delete(String),
    /// Query the configured external LLM server's `/v1/models` (Phase 24c) —
    /// populates the settings model picker and doubles as "test connection".
    ListRemoteLlm,
}

/// LLM jobs for the summarize worker (heavy; load a model + generate). Both take
/// a session id. (Fields go unread in non-`summaries` builds.)
#[allow(dead_code)]
pub enum SummCmd {
    /// Generate the human-readable Markdown summary.
    Summarize(String),
    /// Generate the dense-prose compression (Phase 23).
    Compress(String),
    /// Synthesize the legacy cross-meeting Overview (Phase 23).
    Overview,
    /// Fold meetings into the rolling project ledger (Phase 26). `rebuild=false`
    /// folds only not-yet-applied sessions (lazy refresh); `rebuild=true` wipes
    /// the ledger and replays every meeting (DESTRUCTIVE to manual edits).
    FoldOverview { rebuild: bool },
    /// Answer a chat question grounded in a meeting / all meetings (Phase 23d).
    /// `turns` is the full conversation so far (incl. the new question last);
    /// each turn is `(is_user, text)`.
    Chat { scope: ChatScope, turns: Vec<(bool, String)> },
}

/// Handle the GUI keeps to drive the engine. Cheaply clonable.
#[derive(Clone)]
pub struct Engine {
    pub rec_tx: mpsc::Sender<RecorderCmd>,
    pub db_tx: mpsc::Sender<DbCmd>,
    pub model_tx: mpsc::Sender<ModelCmd>,
    /// Summarize / compress a session (heavy; runs on its own thread).
    pub summ_tx: mpsc::Sender<SummCmd>,
    /// Replay a transcript line from a retained WAV.
    pub play_tx: mpsc::Sender<PlayCmd>,
}

impl Engine {
    /// Spawn the control + db + model worker threads. Returns the handle and
    /// the event stream.
    pub fn spawn(db_path: PathBuf) -> (Engine, UnboundedReceiver<Event>) {
        let (ev_tx, ev_rx) = unbounded_channel::<Event>();
        let (rec_tx, rec_rx) = mpsc::channel::<RecorderCmd>();
        let (db_tx, db_rx) = mpsc::channel::<DbCmd>();
        let (model_tx, model_rx) = mpsc::channel::<ModelCmd>();
        let (summ_tx, summ_rx) = mpsc::channel::<SummCmd>();
        let (play_tx, play_rx) = mpsc::channel::<PlayCmd>();

        {
            let ev = ev_tx.clone();
            let dbp = db_path.clone();
            thread::spawn(move || control_loop(rec_rx, ev, dbp));
        }
        {
            let ev = ev_tx.clone();
            let dbp = db_path.clone();
            thread::spawn(move || db_loop(db_rx, ev, dbp));
        }
        {
            let ev = ev_tx.clone();
            thread::spawn(move || model_loop(model_rx, ev));
        }
        {
            let ev = ev_tx.clone();
            thread::spawn(move || summarize_loop(summ_rx, ev, db_path));
        }
        {
            let ev = ev_tx;
            thread::spawn(move || play_loop(play_rx, ev));
        }
        (
            Engine {
                rec_tx,
                db_tx,
                model_tx,
                summ_tx,
                play_tx,
            },
            ev_rx,
        )
    }
}

/// Worker that generates session summaries (local LLM, heavy). Real impl only
/// in `summaries` builds; otherwise it reports a friendly notice.
fn summarize_loop(rx: mpsc::Receiver<SummCmd>, ev: UnboundedSender<Event>, db_path: PathBuf) {
    // A chat keeps its model resident across turns so follow-ups don't reload it.
    // One-shot jobs (summarize/compress/overview) load + drop their own model, so
    // we free the resident one first to keep peak RAM at a single model.
    #[cfg(any(feature = "llm-local", feature = "llm-remote"))]
    let mut chat_model: Option<(ChatLlmKey, zord_summarize::LlmBackend)> = None;
    while let Ok(cmd) = rx.recv() {
        #[cfg(any(feature = "llm-local", feature = "llm-remote"))]
        match cmd {
            SummCmd::Summarize(id) => {
                chat_model = None;
                summarize_one(&id, &ev, &db_path);
            }
            SummCmd::Compress(id) => {
                chat_model = None;
                compress_one(&id, &ev, &db_path);
            }
            SummCmd::Overview => {
                chat_model = None;
                overview_one(&ev, &db_path);
            }
            SummCmd::FoldOverview { rebuild } => {
                chat_model = None;
                fold_overview(&ev, &db_path, rebuild);
            }
            SummCmd::Chat { scope, turns } => {
                chat_one(&mut chat_model, scope, turns, &ev, &db_path);
            }
        }
        #[cfg(not(any(feature = "llm-local", feature = "llm-remote")))]
        {
            let _ = &cmd;
            let _ = &db_path;
            let _ = ev.send(Event::Notice(
                "AI features aren't built in — rebuild with `--features llm-local` and/or `--features llm-remote`.".to_string(),
            ));
        }
    }
}

/// Render diarized segments into a speaker-labeled transcript (one line per
/// segment), used as LLM grounding input.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn render_labeled_transcript(
    segs: &[Segment],
    names: &std::collections::HashMap<i32, String>,
) -> String {
    segs
        .iter()
        .map(|s| format!("{}: {}", s.speaker_label(names), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The external-server connection from settings (Phase 24).
#[cfg(feature = "llm-remote")]
fn remote_cfg(settings: &zord_config::Settings) -> zord_summarize::RemoteConfig {
    zord_summarize::RemoteConfig {
        base_url: settings.llm_base_url.clone(),
        api_key: settings.llm_api_key.clone(),
        model: settings.llm_model.clone(),
        timeout_secs: settings.llm_timeout_secs,
    }
}

/// Whether this request should go to the external server: the setting decides
/// when both backends are compiled in; with only one compiled, that one is used
/// regardless (build-time fallback — distinct from the runtime rule of never
/// silently switching backends on failure).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn use_external(settings: &zord_config::Settings) -> bool {
    cfg!(feature = "llm-remote")
        && (settings.llm_backend == "external" || cfg!(not(feature = "llm-local")))
}

/// Build the configured LLM backend (Phase 24): the external OpenAI-compatible
/// server or the local GGUF (resolving/downloading it) — see [`use_external`]
/// for which. Sends a notice and returns `None` on failure — never silently
/// falls back at runtime.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn build_llm_backend(
    settings: &zord_config::Settings,
    ev: &UnboundedSender<Event>,
) -> Option<zord_summarize::LlmBackend> {
    if use_external(settings) {
        #[cfg(feature = "llm-remote")]
        {
            if settings.llm_model.trim().is_empty() {
                let _ = ev.send(Event::Notice(
                    "No model picked for the external LLM server — choose one in Settings → AI."
                        .to_string(),
                ));
                return None;
            }
            return Some(zord_summarize::LlmBackend::remote(remote_cfg(settings)));
        }
    }
    #[cfg(feature = "llm-local")]
    {
        if settings.llm_backend == "external" && cfg!(not(feature = "llm-remote")) {
            let _ = ev.send(Event::Notice(
                "External LLM support isn't built into this binary — using the local model."
                    .to_string(),
            ));
        }
        let model_path = resolve_summary_model_path(settings, ev)?;
        match zord_summarize::LlmBackend::load_local(&model_path) {
            Ok(llm) => Some(llm),
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("LLM: {e}")));
                None
            }
        }
    }
    #[cfg(not(feature = "llm-local"))]
    {
        // llm-remote-only build: use_external() is always true, so this point
        // is unreachable at runtime — it only satisfies the type checker.
        None
    }
}

/// Resolve the configured summary model to a local GGUF path: a built-in catalog
/// model (downloading if needed) or a user-supplied custom GGUF. Sends a notice
/// and returns `None` on failure. Shared by summarize + compress.
#[cfg(feature = "llm-local")]
fn resolve_summary_model_path(
    settings: &zord_config::Settings,
    ev: &UnboundedSender<Event>,
) -> Option<PathBuf> {
    if let Some(model) = zord_summarize::SummaryModel::parse(&settings.summary_model) {
        match zord_summarize::ensure_summary_model(model, &mut |_d, _t| {}) {
            Ok(p) => Some(p),
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("summary model: {e}")));
                None
            }
        }
    } else if let Some(p) = zord_summarize::custom_model_path(&settings.summary_model) {
        Some(p)
    } else {
        let _ = ev.send(Event::Notice(format!(
            "Summary model '{}' not found — pick one in Settings or drop its .gguf in the models folder.",
            settings.summary_model
        )));
        None
    }
}

/// Compress a session's transcript into dense prose and store it (Phase 23).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn compress_one(session_id: &str, ev: &UnboundedSender<Event>, db_path: &PathBuf) {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };
    let segs = store.segments(session_id).unwrap_or_default();
    if segs.is_empty() {
        let _ = ev.send(Event::Notice("Nothing to compress in this session.".to_string()));
        return;
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = render_labeled_transcript(&segs, &names);

    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice("Preparing the LLM for compression…".to_string()));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        return;
    };
    let _ = ev.send(Event::Notice("Compressing… (runs in the background)".to_string()));
    match llm.compress(&transcript, zord_config::compress_prompt(), settings.compress_ctx) {
        Ok(text) => {
            let _ = store.set_compressed(session_id, &text);
            let _ = ev.send(Event::Compressed(Some(text)));
            let _ = ev.send(Event::Notice("Compressed.".to_string()));
        }
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("compress failed: {e}")));
        }
    }
}

/// Synthesize the cross-meeting Overview (Phase 23). Long-running; progress is
/// relayed as notices, the result emitted as `Event::Overview`.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn overview_one(ev: &UnboundedSender<Event>, db_path: &std::path::Path) {
    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice("Preparing the LLM…".to_string()));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        return;
    };
    let mut progress = |note: &str| {
        let _ = ev.send(Event::Notice(note.to_string()));
    };
    match zord_overview::synthesize(db_path, &settings, &llm, &mut progress) {
        Ok(o) => {
            let _ = ev.send(Event::Overview(Some(OverviewData {
                text: o.text,
                generated_at: o.generated_at_ms,
                meetings: o.meetings,
            })));
        }
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("overview failed: {e}")));
        }
    }
}

/// Fold meetings into the rolling project ledger (Phase 26). `rebuild` wipes the
/// ledger and replays every meeting (destructive); otherwise only not-yet-folded
/// sessions are applied. Progress is relayed as notices; the refreshed ledger is
/// emitted as `Event::Ledger` (also on failure, so the UI reflects partial work).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn fold_overview(ev: &UnboundedSender<Event>, db_path: &std::path::Path, rebuild: bool) {
    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice("Preparing the LLM…".to_string()));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        // Still refresh the ledger so the panel shows current state.
        if let Ok(store) = Store::open(db_path) {
            let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
        }
        return;
    };
    let mut progress = |note: &str| {
        let _ = ev.send(Event::Notice(note.to_string()));
    };
    let result = if rebuild {
        zord_overview::rebuild_from_history(db_path, &settings, &llm, &mut progress)
    } else {
        zord_overview::fold_pending(db_path, &settings, &llm, &mut progress)
    };
    if let Err(e) = result {
        let _ = ev.send(Event::Notice(format!("overview fold failed: {e}")));
    }
    if let Ok(store) = Store::open(db_path) {
        let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
    }
}

/// What the resident chat backend was built from — reload only when it changes
/// (different GGUF picked, or the external connection details edited).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
#[derive(PartialEq)]
enum ChatLlmKey {
    #[cfg(feature = "llm-local")]
    Local(PathBuf),
    #[cfg(feature = "llm-remote")]
    Remote(zord_summarize::RemoteConfig),
}

/// What the resident chat backend would be built from right now (mirrors
/// [`use_external`] / [`build_llm_backend`]). `None` = unresolvable (notice sent).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
// The `return`s are needed in single-backend builds where a cfg'd tail follows.
#[allow(clippy::needless_return)]
fn chat_llm_key(
    settings: &zord_config::Settings,
    ev: &UnboundedSender<Event>,
) -> Option<ChatLlmKey> {
    if use_external(settings) {
        #[cfg(feature = "llm-remote")]
        return Some(ChatLlmKey::Remote(remote_cfg(settings)));
    }
    #[cfg(feature = "llm-local")]
    {
        let model_path = resolve_summary_model_path(settings, ev)?;
        return Some(ChatLlmKey::Local(model_path));
    }
    #[cfg(not(feature = "llm-local"))]
    {
        // llm-remote-only build: unreachable at runtime (see build_llm_backend).
        let _ = ev;
        None
    }
}

/// Answer a chat question grounded in a meeting (its transcript, or compression
/// when the transcript is too big) or across all meetings (Phase 23d). Keeps the
/// model resident in `cache` across turns.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn chat_one(
    cache: &mut Option<(ChatLlmKey, zord_summarize::LlmBackend)>,
    scope: ChatScope,
    turns: Vec<(bool, String)>,
    ev: &UnboundedSender<Event>,
    db_path: &PathBuf,
) {
    use zord_summarize::ChatRole;
    let settings = zord_config::Settings::load();
    let Some(key) = chat_llm_key(&settings, ev) else {
        return;
    };
    // (Re)build the backend only on a cache miss (selection/connection changed).
    if cache.as_ref().map(|(k, _)| k != &key).unwrap_or(true) {
        let Some(llm) = build_llm_backend(&settings, ev) else {
            return;
        };
        *cache = Some((key, llm));
    }
    let llm = &cache.as_ref().expect("backend just built").1;

    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };

    // Build the grounding context + pick the context window by scope.
    let (context, n_ctx) = match &scope {
        ChatScope::Meeting(id) => match meeting_chat_context(&store, llm, &settings, id, ev) {
            Some(c) => (c, settings.compress_ctx),
            None => return,
        },
        ChatScope::CrossMeeting => {
            // Phase 26f: ground on the structured ledger when it exists; fall back
            // to the older per-meeting compressions until it's first folded.
            match zord_overview::ledger_context(&store) {
                Ok(Some(c)) => (c, settings.overview_ctx),
                _ => {
                    let mut progress = |note: &str| {
                        let _ = ev.send(Event::Notice(note.to_string()));
                    };
                    match zord_overview::cross_meeting_context(
                        &store,
                        llm,
                        &settings,
                        settings.overview_ctx,
                        &mut progress,
                    ) {
                        Ok((c, _)) => (c, settings.overview_ctx),
                        Err(e) => {
                            let _ = ev.send(Event::Notice(format!("chat: {e}")));
                            return;
                        }
                    }
                }
            }
        }
    };

    let system = format!("{}\n\n=== Context ===\n{}", zord_config::chat_system_prompt(), context);
    // Error bubbles ("⚠️ Chat failed: …") are part of the visible conversation
    // but not real assistant output — don't feed them back to the model.
    let mapped: Vec<(ChatRole, String)> = turns
        .into_iter()
        .filter(|(is_user, t)| *is_user || !t.starts_with("⚠️"))
        .map(|(is_user, t)| (if is_user { ChatRole::User } else { ChatRole::Assistant }, t))
        .collect();
    let _ = ev.send(Event::Notice("Thinking…".to_string()));
    // Stream the reply as it generates; the terminal ChatReply carries the full
    // text (it also clears the GUI's busy state, so it is sent on error too —
    // an error reply in the conversation beats a stuck spinner).
    let ev_delta = ev.clone();
    let scope_delta = scope.clone();
    let mut on_delta = |piece: &str| {
        let _ = ev_delta.send(Event::ChatDelta {
            scope: scope_delta.clone(),
            delta: piece.to_string(),
        });
    };
    match llm.chat_stream(&system, &mapped, n_ctx, &mut on_delta) {
        Ok(reply) => {
            let _ = ev.send(Event::ChatReply { scope, reply });
        }
        Err(e) => {
            let _ = ev.send(Event::ChatReply { scope, reply: format!("⚠️ Chat failed: {e}") });
        }
    }
}

/// Grounding context for a single meeting: the full transcript when it fits the
/// chat context, otherwise its compression (generated + cached if missing).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn meeting_chat_context(
    store: &Store,
    llm: &zord_summarize::LlmBackend,
    settings: &zord_config::Settings,
    session_id: &str,
    ev: &UnboundedSender<Event>,
) -> Option<String> {
    let segs = store.segments(session_id).unwrap_or_default();
    if segs.is_empty() {
        let _ = ev.send(Event::Notice("This session has no transcript to chat about.".to_string()));
        return None;
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = render_labeled_transcript(&segs, &names);

    // Reserve headroom (chat output + conversation + prompt) within the window.
    let budget = (settings.compress_ctx as usize).saturating_sub(1400);
    let fits = llm.count_tokens(&transcript).map(|t| t < budget).unwrap_or(false);
    if fits {
        return Some(format!("Meeting transcript:\n{transcript}"));
    }

    // Too long: fall back to the (cached) compression, generating it if needed.
    if let Ok(Some(c)) = store.get_compressed(session_id) {
        if !c.trim().is_empty() {
            return Some(format!("Meeting compression (dense):\n{c}"));
        }
    }
    let _ = ev.send(Event::Notice("Long meeting — compressing it first to chat…".to_string()));
    match llm.compress(&transcript, zord_config::compress_prompt(), settings.compress_ctx) {
        Ok(c) => {
            let _ = store.set_compressed(session_id, &c);
            let _ = ev.send(Event::Compressed(Some(c.clone())));
            Some(format!("Meeting compression (dense):\n{c}"))
        }
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("chat context: {e}")));
            None
        }
    }
}

#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn summarize_one(session_id: &str, ev: &UnboundedSender<Event>, db_path: &PathBuf) {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };
    let segs = store.segments(session_id).unwrap_or_default();
    if segs.is_empty() {
        let _ = ev.send(Event::Notice("Nothing to summarize in this session.".to_string()));
        return;
    }
    // Label each line by its diarized speaker (and custom name, if assigned) so
    // the LLM can attribute statements/actions to the right person.
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = render_labeled_transcript(&segs, &names);

    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice("Preparing the LLM…".to_string()));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        return;
    };
    let _ = ev.send(Event::Notice("Summarizing…".to_string()));
    match llm.summarize(&transcript, &settings.effective_summary_prompt()) {
        Ok(text) => {
            let _ = store.set_summary(session_id, &text);
            let _ = ev.send(Event::Summary(Some(text.clone())));

            // Auto-title (best-effort): reuse the loaded model to title the
            // session, unless the user already named it or turned this off.
            if settings.auto_title {
                let unnamed = store
                    .get_session(session_id)
                    .ok()
                    .flatten()
                    .map(|s| s.title.as_deref().unwrap_or("").trim().is_empty())
                    .unwrap_or(false);
                if unnamed {
                    if let Ok(raw) = llm.summarize(&text, zord_config::title_prompt()) {
                        let title = zord_config::clean_title(&raw);
                        if !title.is_empty() {
                            let _ = store.set_session_title(session_id, &title);
                            emit_sessions(&store, ev);
                        }
                    }
                }
            }
        }
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("summary failed: {e}")));
        }
    }
}

/// Worker for model list / download / delete.
fn model_loop(rx: mpsc::Receiver<ModelCmd>, ev: UnboundedSender<Event>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            ModelCmd::List => {
                let _ = ev.send(Event::Models(catalog()));
            }
            ModelCmd::Download(name) => {
                let ev2 = ev.clone();
                let name2 = name.clone();
                let mut progress = move |done: u64, total: Option<u64>| {
                    if let Some(total) = total.filter(|t| *t > 0) {
                        let pct = (done * 100 / total) as u8;
                        let _ = ev2.send(Event::ModelProgress {
                            name: name2.clone(),
                            pct,
                        });
                    }
                };
                #[cfg(feature = "llm-local")]
                let handled_summary = if let Some(sm) = zord_summarize::SummaryModel::parse(&name) {
                    if let Err(e) = zord_summarize::ensure_summary_model(sm, &mut progress) {
                        tracing::warn!("model download failed for {name}: {e}");
                        let _ = ev.send(Event::DownloadFailed { name: name.clone() });
                    }
                    true
                } else if zord_summarize::ollama_models().iter().any(|m| m.filename == name) {
                    if let Err(e) = zord_summarize::ensure_ollama_model(&name, &mut progress) {
                        tracing::warn!("Ollama download failed for {name}: {e}");
                        let _ = ev.send(Event::DownloadFailed { name: name.clone() });
                    }
                    true
                } else {
                    false
                };
                #[cfg(not(feature = "llm-local"))]
                let handled_summary = false;

                #[cfg(feature = "diarization")]
                let handled_diar = if let Some(dm) = zord_diarize::EmbeddingModel::parse(&name) {
                    let seg = zord_diarize::SegmentationModel::parse_or_default(
                        &zord_config::Settings::load().diarize_segmentation_model,
                    );
                    if let Err(e) = zord_diarize::ensure_diar_models(seg, dm, &mut progress) {
                        tracing::warn!("model download failed for {name}: {e}");
                        let _ = ev.send(Event::DownloadFailed { name: name.clone() });
                    }
                    true
                } else {
                    false
                };
                #[cfg(not(feature = "diarization"))]
                let handled_diar = false;

                if !handled_summary && !handled_diar {
                    if let Some(model) = ModelId::parse(&name) {
                        if let Err(e) = ensure_model(model, &mut progress) {
                            tracing::warn!("model download failed for {name}: {e}");
                            let _ = ev.send(Event::DownloadFailed { name: name.clone() });
                        }
                    }
                }
                let _ = ev.send(Event::Models(catalog()));
            }
            ModelCmd::Delete(name) => {
                #[cfg(feature = "llm-local")]
                if let Some(sm) = zord_summarize::SummaryModel::parse(&name) {
                    let _ = zord_summarize::delete_summary_model(sm);
                } else {
                    // A user-supplied custom GGUF (no-op if it's not one).
                    let _ = zord_summarize::delete_custom_model(&name);
                }
                #[cfg(feature = "diarization")]
                if let Some(dm) = zord_diarize::EmbeddingModel::parse(&name) {
                    let _ = zord_diarize::delete_embedding(dm);
                }
                if let Some(model) = ModelId::parse(&name) {
                    let _ = zord_transcribe::delete_model(model);
                }
                let _ = ev.send(Event::Models(catalog()));
            }
            ModelCmd::ListRemoteLlm => {
                #[cfg(feature = "llm-remote")]
                {
                    let settings = zord_config::Settings::load();
                    match zord_summarize::list_remote_models(&remote_cfg(&settings)) {
                        Ok(models) => {
                            let _ = ev.send(Event::RemoteModels { models, error: None });
                        }
                        Err(e) => {
                            let _ = ev.send(Event::RemoteModels {
                                models: Vec::new(),
                                error: Some(e.to_string()),
                            });
                        }
                    }
                }
                #[cfg(not(feature = "llm-remote"))]
                {
                    let _ = ev.send(Event::RemoteModels {
                        models: Vec::new(),
                        error: Some("summaries aren't built in".to_string()),
                    });
                }
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// Emit the session list plus its per-session badge flags together, so the
/// sidebar list + badges stay in sync whenever the list changes.
fn emit_sessions(store: &Store, ev: &UnboundedSender<Event>) {
    if let Ok(v) = store.list_sessions() {
        let _ = ev.send(Event::Sessions(v));
    }
    if let Ok(b) = store.session_badges() {
        let _ = ev.send(Event::SessionBadges(b));
    }
}

/// Read the stored cross-meeting Overview from `app_meta` (feature-independent —
/// the keys mirror `zord_overview`). `None` if none has been generated.
fn load_overview(store: &Store) -> Option<OverviewData> {
    let (text, generated_at) = store.get_meta("overview").ok().flatten()?;
    let meetings = store
        .get_meta("overview_meetings")
        .ok()
        .flatten()
        .and_then(|(v, _)| v.parse().ok())
        .unwrap_or(0);
    Some(OverviewData { text, generated_at, meetings })
}

/// Read the whole project ledger (Phase 26) into the GUI mirror types, plus the
/// count of meetings not yet folded. Best-effort: read errors yield an empty
/// ledger rather than failing the UI.
fn build_ledger_view(store: &Store) -> LedgerView {
    let mut projects = Vec::new();
    for p in store.list_projects().unwrap_or_default() {
        let items = store
            .list_items(&p.id)
            .unwrap_or_default()
            .into_iter()
            .map(|it| ItemView {
                id: it.id,
                kind: it.kind.as_str().to_string(),
                text: it.text,
                owner: it.owner,
                status: it.status.as_str().to_string(),
                completed_session: it.completed_session,
                manual: it.manual,
            })
            .collect();
        projects.push(ProjectView {
            id: p.id,
            name: p.name,
            status: p.status.as_str().to_string(),
            description: p.description,
            last_activity: p.last_activity_at,
            items,
        });
    }
    let pending = store.unapplied_sessions().map(|v| v.len()).unwrap_or(0);
    LedgerView { projects, pending }
}

// ---------------------------------------------------------------------------
// DB query thread
// ---------------------------------------------------------------------------

fn db_loop(rx: mpsc::Receiver<DbCmd>, ev: UnboundedSender<Event>, db_path: PathBuf) {
    let store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Status(Status::Error(format!("db open failed: {e}"))));
            return;
        }
    };
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DbCmd::ListSessions => {
                emit_sessions(&store, &ev);
            }
            DbCmd::Search(q) => {
                let q = sanitize_fts(&q);
                if q.is_empty() {
                    let _ = ev.send(Event::SearchResults(Vec::new()));
                } else if let Ok(v) = store.search(&q) {
                    let _ = ev.send(Event::SearchResults(v));
                }
            }
            DbCmd::Load(id) => {
                if let Ok(v) = store.segments(&id) {
                    let _ = ev.send(Event::Transcript(v));
                }
                let _ = ev.send(Event::Speakers(store.speaker_names(&id).unwrap_or_default()));
                let _ = ev.send(Event::Summary(store.get_summary(&id).ok().flatten()));
                let _ = ev.send(Event::Compressed(store.get_compressed(&id).ok().flatten()));
                let _ = ev.send(Event::DiarizeSpeakers(
                    store.get_diarize_speakers(&id).ok().flatten().unwrap_or(0),
                ));
                let (me, others) = session_audio_files(&store, &id);
                let _ = ev.send(Event::AudioFiles { me, others });
            }
            DbCmd::Export { id, format } => match export_session(&store, &id, format) {
                Ok(path) => {
                    let _ = ev.send(Event::Exported(path));
                }
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!("export failed: {e}")));
                }
            },
            DbCmd::Rename { id, title } => {
                let _ = store.set_session_title(&id, &title);
                emit_sessions(&store, &ev);
            }
            DbCmd::DeleteSession(id) => {
                let _ = store.delete_session(&id);
                emit_sessions(&store, &ev);
            }
            DbCmd::ClearSummary(id) => {
                let _ = store.clear_summary(&id);
                let _ = ev.send(Event::Summary(None));
                emit_sessions(&store, &ev); // refresh sidebar badges
            }
            DbCmd::ClearCompressed(id) => {
                let _ = store.clear_compressed(&id);
                let _ = ev.send(Event::Compressed(None));
                emit_sessions(&store, &ev); // refresh sidebar badges
            }
            DbCmd::EditSegment { segment_id, text } => {
                let _ = store.update_segment_text(segment_id, &text);
            }
            DbCmd::RenameSpeaker { id, speaker, name } => {
                let _ = store.set_speaker_name(&id, speaker, &name);
                let _ = ev.send(Event::Speakers(store.speaker_names(&id).unwrap_or_default()));
            }
            DbCmd::LoadOverview => {
                let data = load_overview(&store);
                let _ = ev.send(Event::Overview(data));
            }
            DbCmd::LoadLedger => {
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::RenameProject { id, name } => {
                let _ = store.rename_project(&id, name.trim(), now_ms());
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::SetProjectDescription { id, description } => {
                let desc = description.trim();
                let _ = store.set_project_description(
                    &id,
                    (!desc.is_empty()).then_some(desc),
                    now_ms(),
                );
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::SetProjectArchived { id, archived } => {
                let status = if archived {
                    zord_core::ProjectStatus::Archived
                } else {
                    zord_core::ProjectStatus::Active
                };
                let _ = store.set_project_status(&id, status, now_ms());
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::DeleteProject(id) => {
                let _ = store.delete_project(&id);
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::EditItem { id, text, owner } => {
                let owner = owner.trim();
                let now = now_ms();
                let _ = store.update_item_text(
                    &id,
                    text.trim(),
                    (!owner.is_empty()).then_some(owner),
                    now,
                );
                let _ = store.set_item_manual(&id, true); // protect the hand edit
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::SetItemStatus { id, status } => {
                let st = zord_core::ItemStatus::parse(&status);
                let now = now_ms();
                // A manual completion records no session; reopening clears it.
                let completed = if st == zord_core::ItemStatus::Done {
                    Some("manual")
                } else {
                    None
                };
                let _ = store.update_item_status(&id, st, Some("manual"), completed, now);
                let _ = store.set_item_manual(&id, true);
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::MoveItem { item_id, project_id } => {
                let _ = store.move_item(&item_id, &project_id, now_ms());
                let _ = store.set_item_manual(&item_id, true);
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::DeleteItem(id) => {
                let _ = store.delete_item(&id);
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::AddItem { project_id, kind, text, owner } => {
                let now = now_ms();
                let owner = owner.trim();
                let item = zord_core::ProjectItem {
                    id: format!("manual-{now}"),
                    project_id,
                    kind: zord_core::ItemKind::parse(&kind),
                    text: text.trim().to_string(),
                    owner: (!owner.is_empty()).then(|| owner.to_string()),
                    status: zord_core::ItemStatus::Open,
                    created_session: None,
                    updated_session: None,
                    completed_session: None,
                    created_at: now,
                    updated_at: now,
                    manual: true,
                };
                let _ = store.add_item(&item);
                let _ = ev.send(Event::Ledger(build_ledger_view(&store)));
            }
            DbCmd::Diarize { id, num_speakers } => {
                // Remember the chosen count on the session for next time.
                let _ = store.set_diarize_speakers(&id, num_speakers);
                // Heavy (loads ONNX + clusters); run off the db thread so queries
                // stay responsive. The worker opens its own Store.
                let ev = ev.clone();
                let db_path = db_path.clone();
                thread::spawn(move || diarize_session_ondemand(&db_path, &id, num_speakers, &ev));
            }
            DbCmd::Retranscribe(id) => {
                // Heavy (model load + minutes of inference); own thread + Store.
                let ev = ev.clone();
                let db_path = db_path.clone();
                thread::spawn(move || retranscribe_session_ondemand(&db_path, &id, &ev));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Playback thread (replay one transcript line from a retained WAV)
// ---------------------------------------------------------------------------

/// Absolute paths of the retained per-channel WAVs that actually exist on disk
/// for a session, as (me, others). Either may be missing: with default settings
/// only the Others track is kept (for re-diarization), and retention may have
/// deleted old audio.
fn session_audio_files(store: &Store, session_id: &str) -> (Option<String>, Option<String>) {
    let prefix = store
        .get_session(session_id)
        .ok()
        .flatten()
        .and_then(|s| s.audio_path);
    let Some(prefix) = prefix else {
        return (None, None);
    };
    let existing = |suffix: &str| {
        let p = format!("{prefix}.{suffix}.wav");
        std::path::Path::new(&p).exists().then_some(p)
    };
    (existing("me"), existing("others"))
}

/// Owns the audio output stream (created lazily on first play) and plays one
/// clip at a time: a new `Play` replaces the current clip, `Stop` silences.
/// Emits [`Event::Playing`] transitions so the UI can mark the active line.
fn play_loop(rx: mpsc::Receiver<PlayCmd>, ev: UnboundedSender<Event>) {
    let mut output: Option<rodio::MixerDeviceSink> = None;
    let mut sink: Option<rodio::Player> = None;
    let mut current: Option<i64> = None;
    loop {
        // Block when idle; poll while playing so we can report "finished".
        let cmd = if current.is_some() {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        };
        match cmd {
            Some(PlayCmd::Play { segment_id, wav, start_ms, end_ms }) => {
                if let Some(s) = sink.take() {
                    s.stop();
                }
                current = None;
                if output.is_none() {
                    output = rodio::DeviceSinkBuilder::open_default_sink().ok();
                }
                let Some(device) = output.as_ref() else {
                    let _ = ev.send(Event::Notice("No audio output device available.".to_string()));
                    let _ = ev.send(Event::Playing(None));
                    continue;
                };
                // Retained WAVs are wall-clock aligned (silence-padded) at their
                // own rate, so timestamps map directly onto sample offsets — the
                // reader derives them from the file header (native-rate tracks
                // from Phase 25d and older 16 kHz ones both work). Native rate
                // also means playback at full capture quality.
                let (samples, rate) =
                    zord_audio::read_wav_slice_ms(&wav, start_ms, end_ms).unwrap_or_default();
                if samples.is_empty() {
                    let _ = ev.send(Event::Notice("Couldn't read this line's audio.".to_string()));
                    let _ = ev.send(Event::Playing(None));
                    continue;
                }
                // rodio 0.22: Sink → Player (connect_new is infallible); rate +
                // channel count are NonZero. We play raw, wall-clock-aligned PCM.
                let player = rodio::Player::connect_new(device.mixer());
                player.append(rodio::buffer::SamplesBuffer::new(
                    std::num::NonZeroU16::new(1).unwrap(),
                    std::num::NonZeroU32::new(rate.max(1)).unwrap(),
                    samples,
                ));
                sink = Some(player);
                current = Some(segment_id);
                let _ = ev.send(Event::Playing(Some(segment_id)));
            }
            Some(PlayCmd::Stop) => {
                if let Some(s) = sink.take() {
                    s.stop();
                }
                if current.take().is_some() {
                    let _ = ev.send(Event::Playing(None));
                }
            }
            // Poll tick: did the current clip finish on its own?
            None => {
                if sink.as_ref().is_some_and(|s| s.empty()) {
                    sink = None;
                    current = None;
                    let _ = ev.send(Event::Playing(None));
                }
            }
        }
    }
}

/// On-demand diarization for a past session: locate its retained "Others" WAV
/// from the stored audio prefix, then run the offline pass.
/// `num_speakers` pins the speaker count (0 = auto-detect).
fn diarize_session_ondemand(
    db_path: &PathBuf,
    session_id: &str,
    num_speakers: u32,
    ev: &UnboundedSender<Event>,
) {
    #[cfg(not(feature = "diarization"))]
    {
        let _ = (db_path, session_id, num_speakers);
        let _ = ev.send(Event::Notice(
            "Diarization isn't built in — rebuild with `--features diarization`.".to_string(),
        ));
    }
    #[cfg(feature = "diarization")]
    {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        // Run the work catching any Rust panic, then ALWAYS emit a terminal
        // Event::Speakers so the GUI's "Identifying…" busy state clears no matter
        // how this exits (success, no-result, error, or panic) — otherwise a
        // failed run leaves the button stuck and the user sees nothing happen.
        let ran = catch_unwind(AssertUnwindSafe(|| {
            diarize_session_inner(db_path, session_id, num_speakers, ev)
        }));
        if ran.is_err() {
            let _ = ev.send(Event::Notice(
                "Speaker identification crashed on this recording — try a different speaker \
                 model (Settings → Speakers) or set the expected speaker count, then retry."
                    .to_string(),
            ));
        }
        if let Ok(store) = Store::open(db_path) {
            let _ = ev.send(Event::Speakers(store.speaker_names(session_id).unwrap_or_default()));
        }
    }
}

/// The actual on-demand diarization work; wrapped by [`diarize_session_ondemand`]
/// for panic-safety + guaranteed busy-state clearing.
#[cfg(feature = "diarization")]
fn diarize_session_inner(
    db_path: &PathBuf,
    session_id: &str,
    num_speakers: u32,
    ev: &UnboundedSender<Event>,
) {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };
    let session = match store.get_session(session_id) {
        Ok(Some(s)) => s,
        _ => {
            let _ = ev.send(Event::Notice("No such session.".to_string()));
            return;
        }
    };
    let Some(prefix) = session.audio_path else {
        let _ = ev.send(Event::Notice(
            "This session didn't keep its audio, so speakers can't be identified after the fact. \
             Turn on Settings → Speakers → \"Keep audio for re-diarization\" before recording \
             (speakers are still identified automatically right after a recording stops)."
                .to_string(),
        ));
        return;
    };
    let wav = PathBuf::from(format!("{prefix}.others.wav"));
    if !wav.exists() {
        let _ = ev.send(Event::Notice(
            "The 'Others' audio for this session is missing from disk, so speakers can't be \
             re-identified."
                .to_string(),
        ));
        return;
    }
    apply_diarization(&store, session_id, &wav, Some(num_speakers), ev);
}

/// Load the "Others" WAV, run the offline diarizer, and write speaker labels
/// onto the session's segments. Emits progress notices + a refreshed transcript.
/// `num_speakers`: `Some(n)` pins the count for this run (`Some(0)` = auto);
/// `None` falls back to the config-file setting (post-recording auto pass).
#[cfg(feature = "diarization")]
fn apply_diarization(
    store: &Store,
    session_id: &str,
    others_wav: &std::path::Path,
    num_speakers: Option<u32>,
    ev: &UnboundedSender<Event>,
) {
    // Streams + resamples the (possibly native-rate) track down to the 16 kHz
    // the diarizer expects, without loading the whole file (Phase 25d).
    let samples = match zord_audio::read_wav_mono_16k(others_wav) {
        Ok(s) if !s.is_empty() => s,
        Ok(_) => {
            let _ = ev.send(Event::Notice("No 'Others' audio to diarize.".to_string()));
            return;
        }
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("reading audio: {e}")));
            return;
        }
    };

    let settings = zord_config::Settings::load();
    let model = zord_diarize::EmbeddingModel::parse_or_default(&settings.diarize_embedding_model);
    let seg =
        zord_diarize::SegmentationModel::parse_or_default(&settings.diarize_segmentation_model);

    if !zord_diarize::diar_models_present(seg, model) {
        let _ = ev.send(Event::Notice("Downloading speaker models…".to_string()));
        let ev2 = ev.clone();
        let mut progress = move |done: u64, total: Option<u64>| {
            if let Some(total) = total.filter(|t| *t > 0) {
                let _ = ev2.send(Event::ModelProgress {
                    name: model.name().to_string(),
                    pct: (done * 100 / total) as u8,
                });
            }
        };
        if let Err(e) = zord_diarize::ensure_diar_models(seg, model, &mut progress) {
            let _ = ev.send(Event::Notice(format!("speaker models: {e}")));
            return;
        }
    }

    let _ = ev.send(Event::Notice("Identifying speakers…".to_string()));
    // Pin the speaker count when the user set one (0 = auto-detect). The
    // per-session value (next to "Identify speakers") wins over the config file.
    let pinned = num_speakers.unwrap_or(settings.diarize_num_speakers);
    let num_speakers = (pinned > 0).then_some(pinned as i32);
    let threshold = settings.diarize_threshold.clamp(0.1, 0.95);
    let diarizer = match zord_diarize::Diarizer::load(seg, model, num_speakers, threshold) {
        Ok(d) => d,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("diarizer: {e}")));
            return;
        }
    };
    let spans = match diarizer.diarize(&samples) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("diarization failed: {e}")));
            return;
        }
    };

    // The diarizer ran but found no distinct speaker segments (short / mostly
    // silent / single-speaker audio, or clustering collapsed at this threshold).
    // Bail WITHOUT touching existing labels so a no-result run isn't destructive.
    if spans.is_empty() {
        let _ = ev.send(Event::Notice(
            "No distinct speakers were detected — the recording may be too short, mostly \
             silence, or a single speaker. Try lowering the clustering threshold or setting the \
             expected speaker count in Settings → Speakers."
                .to_string(),
        ));
        return;
    }

    // Map speaker spans onto the stored "Others" segments by max time overlap.
    // Compute all assignments first; only clear + write if we actually matched
    // something, so a failed mapping never wipes existing speaker labels/names.
    let segs = store.segments(session_id).unwrap_or_default();
    let mut assignments: Vec<(i64, i32)> = Vec::new();
    let mut speakers = std::collections::HashSet::new();
    for seg in segs.iter().filter(|s| s.source == Source::Others) {
        let Some(id) = seg.id else { continue };
        let best = spans
            .iter()
            .map(|sp| (sp.speaker, overlap_ms(seg.t_start_ms, seg.t_end_ms, sp.start_ms, sp.end_ms)))
            .filter(|(_, ov)| *ov > 0)
            .max_by_key(|(_, ov)| *ov);
        if let Some((speaker, _)) = best {
            assignments.push((id, speaker));
            speakers.insert(speaker);
        }
    }

    if assignments.is_empty() {
        let _ = ev.send(Event::Notice(
            "Found speech but couldn't align speakers to the transcript lines. Existing labels \
             were left unchanged."
                .to_string(),
        ));
        return;
    }

    let _ = store.clear_speakers(session_id);
    for (id, speaker) in assignments {
        let _ = store.set_segment_speaker(id, Some(speaker));
    }

    if let Ok(v) = store.segments(session_id) {
        let _ = ev.send(Event::Transcript(v));
    }
    let _ = ev.send(Event::Speakers(store.speaker_names(session_id).unwrap_or_default()));
    let _ = ev.send(Event::Notice(format!(
        "Identified {} speaker(s) in this conversation.",
        speakers.len()
    )));
}

/// Milliseconds of overlap between two [start, end] intervals.
#[cfg(feature = "diarization")]
fn overlap_ms(a0: u64, a1: u64, b0: u64, b1: u64) -> u64 {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    hi.saturating_sub(lo)
}

/// Render a session and write it to the app data `exports/` directory.
fn export_session(
    store: &Store,
    id: &str,
    format: zord_export::Format,
) -> anyhow::Result<String> {
    let session = store
        .get_session(id)?
        .ok_or_else(|| anyhow::anyhow!("no such session"))?;
    let segments = store.segments(id)?;
    let names = store.speaker_names(id).unwrap_or_default();
    let rendered = zord_export::render(&session, &segments, &names, format);

    let dir = zord_transcribe::model_cache_dir()?
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("exports");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.{}", format.extension()));
    std::fs::write(&path, rendered)?;
    Ok(path.display().to_string())
}

/// Turn free-text into a safe FTS5 MATCH expression: each whitespace token
/// becomes a quoted prefix term, AND-ed together.
fn sanitize_fts(q: &str) -> String {
    q.split_whitespace()
        .map(|t| t.replace('"', ""))
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Recording control thread
// ---------------------------------------------------------------------------

fn control_loop(rx: mpsc::Receiver<RecorderCmd>, ev: UnboundedSender<Event>, db_path: PathBuf) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            RecorderCmd::Start {
                model,
                keep_audio,
                input_device,
                audio_dir,
                record_mic,
                record_system,
                live,
            } => {
                // Guard: if neither was requested, record both.
                let (record_mic, record_system) = if !record_mic && !record_system {
                    (true, true)
                } else {
                    (record_mic, record_system)
                };
                let opts = SessionOpts {
                    model,
                    keep_audio,
                    input_device,
                    audio_dir,
                    record_mic,
                    record_system,
                    live,
                };
                if run_session(opts, &rx, &ev, &db_path) {
                    break; // session ended due to Shutdown
                }
            }
            RecorderCmd::Shutdown => break,
            RecorderCmd::Stop => {}               // nothing recording
            RecorderCmd::SetMicMuted(_) => {}     // nothing recording
            RecorderCmd::SetSystemMuted(_) => {}  // nothing recording
        }
    }
}

struct Job {
    source: Source,
    vad: zord_audio::VadSegment,
}

struct SessionOpts {
    model: ModelId,
    keep_audio: bool,
    input_device: Option<String>,
    audio_dir: PathBuf,
    record_mic: bool,
    record_system: bool,
    /// `false` = capture-only (Phase 25): no model, no transcribe jobs.
    live: bool,
}

/// Block until Stop / Shutdown, applying live mic/desktop mute toggles in the
/// meantime. Returns `true` if it ended because of `Shutdown`.
fn wait_for_stop(
    rx: &mpsc::Receiver<RecorderCmd>,
    mic_muted: &Arc<AtomicBool>,
    sys_muted: &Arc<AtomicBool>,
) -> bool {
    let mut shutdown = false;
    loop {
        match rx.recv() {
            Ok(RecorderCmd::Stop) => {
                tracing::info!("control: Stop received — tearing down recording");
                break;
            }
            Err(_) => break,
            Ok(RecorderCmd::Shutdown) => {
                shutdown = true;
                break;
            }
            Ok(RecorderCmd::SetMicMuted(m)) => mic_muted.store(m, Ordering::Relaxed),
            Ok(RecorderCmd::SetSystemMuted(m)) => sys_muted.store(m, Ordering::Relaxed),
            Ok(RecorderCmd::Start { .. }) => {} // ignore double-start
        }
    }
    shutdown
}

/// Run one recording session. Returns `true` if it ended because of `Shutdown`.
fn run_session(
    opts: SessionOpts,
    rx: &mpsc::Receiver<RecorderCmd>,
    ev: &UnboundedSender<Event>,
    db_path: &PathBuf,
) -> bool {
    let SessionOpts {
        model,
        keep_audio,
        input_device,
        audio_dir,
        record_mic,
        record_system,
        live,
    } = opts;
    // Capture-only (Phase 25): no model is loaded and no transcription runs —
    // the WAVs written below are the input for the post-stop pass.
    let model_path = if live {
        let _ = ev.send(Event::Status(Status::PreparingModel));
        let ev = ev.clone();
        match ensure_model(model, &mut |done, total| {
            if let Some(total) = total {
                let pct = (done as f64 / total as f64 * 100.0) as u8;
                let _ = ev.send(Event::Status(Status::Downloading(pct)));
            }
        }) {
            Ok(p) => Some(p),
            Err(e) => {
                let _ = ev.send(Event::Status(Status::Error(format!("model: {e}"))));
                return false;
            }
        }
    } else {
        None
    };

    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Status(Status::Error(format!("db: {e}"))));
            return false;
        }
    };
    let started_at = now_ms();
    let session_id = format!("sess-{started_at}");
    // When keeping audio, we store per-channel WAVs as <audio_dir>/<id>.<src>.wav;
    // record the prefix so re-transcribe / playback can find them.
    let audio_prefix = audio_dir.join(&session_id);

    let settings = zord_config::Settings::load();
    // `diarize_auto` runs diarization at stop (needs the Others WAV, written as
    // a temp file even when audio isn't kept).
    let diarize_auto = cfg!(feature = "diarization") && record_system && settings.diarize_auto;
    // We persist audio (so replay / re-transcribe / re-diarize can find it) when
    // the user keeps audio or recorded capture-only (the WAVs ARE the pending
    // transcript — Phase 25).
    let persist_audio = keep_audio || !live;

    let _ = store.create_session(&Session {
        id: session_id.clone(),
        started_at,
        ended_at: None,
        title: None,
        audio_path: if persist_audio {
            Some(audio_prefix.display().to_string())
        } else {
            None
        },
        model: model.name().to_string(),
    });
    let wav_path = |src: &str| -> Option<PathBuf> {
        // Capture-only always writes — the WAV is the transcription input.
        if keep_audio || !live {
            let _ = std::fs::create_dir_all(&audio_dir);
            Some(audio_dir.join(format!("{session_id}.{src}.wav")))
        } else {
            None
        }
    };

    // Write the Others WAV if anything needs it: kept audio, the auto pass,
    // retention for later re-diarization, or a capture-only recording.
    let others_wav: Option<PathBuf> =
        if record_system && (keep_audio || diarize_auto || !live) {
            let _ = std::fs::create_dir_all(&audio_dir);
            Some(audio_dir.join(format!("{session_id}.others.wav")))
        } else {
            None
        };

    let session_start = Instant::now();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let mut procs = Vec::new();
    // Toggled live by RecorderCmd::SetMicMuted/SetSystemMuted; read by the
    // respective proc threads.
    let mic_muted = Arc::new(AtomicBool::new(false));
    let sys_muted = Arc::new(AtomicBool::new(false));
    // Set on Stop/Shutdown so the worker threads bail out *promptly* instead of
    // draining a whole queued backlog (which made Stop feel unresponsive when the
    // pipeline was behind real time). Any not-yet-transcribed tail is dropped; if
    // audio was kept it can be re-transcribed.
    let stopping = Arc::new(AtomicBool::new(false));

    // Microphone ("Me") — only if the capture mode includes it.
    let mic = if record_mic {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>();
        match Microphone::start_with(mic_tx, input_device.as_deref()) {
            Ok(m) => {
                let mic_level = zord_audio::LevelControl::new(
                    zord_audio::LevelMode::parse(&settings.mic_level_mode, settings.mic_gain_db));
                procs.push(spawn_proc(mic_rx, m.sample_rate(), Source::Me, session_start, job_tx.clone(), ev.clone(), wav_path("me"), Some(mic_muted.clone()), mic_level, stopping.clone()));
                Some(m)
            }
            Err(e) => {
                let _ = ev.send(Event::Status(Status::Error(format!("microphone: {e}"))));
                return false;
            }
        }
    } else {
        None
    };

    // System audio ("Others") — optional; only if the capture mode includes it.
    let system = if record_system {
        let (sys_tx, sys_rx) = mpsc::channel::<Vec<f32>>();
        match SystemAudio::start(sys_tx) {
            Ok(s) => {
                let sys_level = zord_audio::LevelControl::new(
                    zord_audio::LevelMode::parse(&settings.others_level_mode, settings.others_gain_db));
                procs.push(spawn_proc(sys_rx, s.sample_rate(), Source::Others, session_start, job_tx.clone(), ev.clone(), others_wav.clone(), Some(sys_muted.clone()), sys_level, stopping.clone()));
                Some(s)
            }
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("System audio off: {e}")));
                None
            }
        }
    } else {
        None
    };
    drop(job_tx);

    let _ = ev.send(Event::Status(Status::Recording));
    if !live {
        let _ = ev.send(Event::Notice(
            "Recording (capture only) — transcription will run when you stop.".to_string(),
        ));
    }

    // Transcription + storage thread: consumes jobs from both channels.
    // Capture-only recordings spawn none — VAD jobs are simply dropped.
    let transcribe = model_path.clone().map(|model_path| {
        let ev = ev.clone();
        let session = session_id.clone();
        let db_path = db_path.clone();
        let stopping = stopping.clone();
        thread::spawn(move || {
            let transcriber = match Transcriber::load(model, &model_path) {
                Ok(t) => t,
                Err(e) => {
                    let _ = ev.send(Event::Status(Status::Error(format!("whisper: {e}"))));
                    return;
                }
            };
            let store = match Store::open(&db_path) {
                Ok(s) => s,
                Err(e) => {
                    let _ = ev.send(Event::Status(Status::Error(format!("db: {e}"))));
                    return;
                }
            };
            // Optional live (provisional) speaker labeling for the "Others"
            // channel. These rough labels are replaced by the accurate offline
            // pass at stop. Silently disabled if the model isn't downloaded.
            #[cfg(feature = "diarization")]
            let mut live_labeler = {
                let s = zord_config::Settings::load();
                if s.diarize_live {
                    let m =
                        zord_diarize::EmbeddingModel::parse_or_default(&s.diarize_embedding_model);
                    zord_diarize::LiveLabeler::new_default(m).ok()
                } else {
                    None
                }
            };
            while let Ok(job) = job_rx.recv() {
                // Stop requested: drop the remaining backlog instead of running
                // whisper over all of it, so teardown is prompt.
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                // Provisional speaker for this whole VAD chunk (Others only).
                #[allow(unused_mut)]
                let mut live_speaker: Option<i32> = None;
                #[cfg(feature = "diarization")]
                if job.source == Source::Others {
                    if let Some(ll) = live_labeler.as_mut() {
                        live_speaker = ll.label(&job.vad.samples, zord_core::WHISPER_SAMPLE_RATE);
                    }
                }
                match transcriber.transcribe(&job.vad.samples, job.source, job.vad.t_start_ms) {
                    Ok(segs) => {
                        for mut seg in segs {
                            if seg.speaker.is_none() {
                                seg.speaker = live_speaker;
                            }
                            let _ = store.insert_segment(&session, &seg);
                            let _ = ev.send(Event::Segment(seg));
                        }
                    }
                    Err(e) => {
                        let _ = ev.send(Event::Notice(format!("transcribe error: {e}")));
                    }
                }
            }
        })
    });

    // Wait for Stop / Shutdown (also handle live mic/desktop mute toggles).
    let shutdown = wait_for_stop(rx, &mic_muted, &sys_muted);

    // Tell the worker threads to bail out of any queued backlog promptly.
    stopping.store(true, Ordering::Relaxed);
    drop(mic);
    drop(system);
    for p in procs {
        let _ = p.join();
    }
    if let Some(t) = transcribe {
        let _ = t.join();
    }
    let _ = store.end_session(&session_id, now_ms());
    tracing::info!("control: recording torn down");
    // The recording is over NOW — flip the UI out of "Recording" before any
    // post-stop work (transcription/diarization show up as their own
    // background jobs, not as a stuck recording indicator).
    let _ = ev.send(Event::Status(Status::Idle));

    // Post-stop transcription pass (Phase 25): when auto-transcribe is on it
    // runs from the WAVs we just wrote — with live on it *upgrades* the live
    // transcript with the re-transcription model; with live off it's where the
    // transcript comes from. Runs before diarization (which labels segments).
    let post_pass = settings.auto_transcribe;
    if post_pass {
        post_transcribe_session(&store, &session_id, &audio_prefix, ev);
    } else if !live {
        let _ = ev.send(Event::Notice(
            "Recording saved — transcription deferred. Open the session and press \
             Re-transcribe (or turn on 'Transcribe automatically after recording' \
             in Settings)."
                .to_string(),
        ));
    }

    // Offline speaker diarization (accurate, source of truth) over the "Others"
    // track, then drop the temp WAV unless we're retaining it (kept audio, or
    // kept-for-re-diarization).
    // Diarization needs segments to label — with a fully deferred transcript
    // (live off, no post pass) it runs after the eventual 🔁 Re-transcribe.
    #[cfg(feature = "diarization")]
    if diarize_auto && (live || post_pass) {
        if let Some(wav) = others_wav.as_ref() {
            apply_diarization(&store, &session_id, wav, None, ev);
        }
    }
    if !persist_audio {
        if let Some(wav) = others_wav.as_ref() {
            let _ = std::fs::remove_file(wav);
        }
    }
    // Capture-only WAVs were forced on as the transcription input; if the user
    // doesn't keep audio, drop them once the post-pass produced a transcript.
    // A *deferred* recording keeps them regardless — they ARE the pending
    // transcript (the safety rule: never purge an untranscribed capture).
    if !live && post_pass && !keep_audio {
        for suffix in ["me", "others"] {
            let _ = std::fs::remove_file(audio_dir.join(format!("{session_id}.{suffix}.wav")));
        }
        let _ = store.set_audio_path(&session_id, None);
        emit_sessions(&store, ev); // 🎧 badge off
    }

    tracing::info!("control: session idle");
    shutdown
}

/// On-demand re-transcription of a past session (the 🔁 button / Phase 25):
/// post-transcribe from the kept WAVs, then re-derive speaker labels when the
/// session had them (re-transcribing wipes segments, labels included). Always
/// ends with [`Event::Retranscribed`] so the GUI busy state clears.
fn retranscribe_session_ondemand(db_path: &PathBuf, session_id: &str, ev: &UnboundedSender<Event>) {
    let done = |ev: &UnboundedSender<Event>| {
        let _ = ev.send(Event::Retranscribed);
    };
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return done(ev);
        }
    };
    let prefix = store
        .get_session(session_id)
        .ok()
        .flatten()
        .and_then(|s| s.audio_path);
    let Some(prefix) = prefix else {
        let _ = ev.send(Event::Notice(
            "This session has no kept audio to re-transcribe.".to_string(),
        ));
        return done(ev);
    };
    let had_speakers = store
        .session_badges()
        .ok()
        .and_then(|b| b.get(session_id).map(|(_, _, spk)| *spk))
        .unwrap_or(false);
    // A deferred capture-only session has no segments yet — its *first*
    // transcription should honor the diarize-auto setting like a normal stop.
    let first_transcription = store.segments(session_id).map(|v| v.is_empty()).unwrap_or(false);

    let ok = post_transcribe_session(&store, session_id, std::path::Path::new(&prefix), ev);
    // Segments were replaced — any custom speaker labels were on the old rows.
    let _ = ev.send(Event::Speakers(store.speaker_names(session_id).unwrap_or_default()));

    let want_diarize = had_speakers
        || (first_transcription && zord_config::Settings::load().diarize_auto);
    if ok && want_diarize && cfg!(feature = "diarization") {
        let _ = ev.send(Event::Notice(
            "Re-identifying speakers on the new transcript…".to_string(),
        ));
        let pinned = store.get_diarize_speakers(session_id).ok().flatten().unwrap_or(0);
        diarize_session_ondemand(db_path, session_id, pinned, ev);
    }
    done(ev)
}

/// Post-hoc transcription of a session from its kept WAVs (Phase 25): used
/// after capture-only recordings and by Re-transcribe. Replaces any existing
/// segments, stamps the session with the re-transcription model, and emits
/// progress + the refreshed transcript. Returns `true` on success.
fn post_transcribe_session(
    store: &Store,
    session_id: &str,
    audio_prefix: &std::path::Path,
    ev: &UnboundedSender<Event>,
) -> bool {
    let _ = ev.send(Event::Retranscribing);
    let ok = post_transcribe_inner(store, session_id, audio_prefix, ev);
    let _ = ev.send(Event::Retranscribed);
    ok
}

/// [`post_transcribe_session`] body — split out so the bracketing
/// Retranscribing/Retranscribed events cover every early return.
fn post_transcribe_inner(
    store: &Store,
    session_id: &str,
    audio_prefix: &std::path::Path,
    ev: &UnboundedSender<Event>,
) -> bool {
    let settings = zord_config::Settings::load();
    let model = ModelId::parse(&settings.retranscribe_model)
        .unwrap_or(ModelId::LargeV3TurboQ5);
    let _ = ev.send(Event::Notice(format!(
        "Transcribing with {}… (first run downloads the model)",
        model.name()
    )));
    let model_path = {
        let ev2 = ev.clone();
        match ensure_model(model, &mut |done, total| {
            if let Some(total) = total.filter(|t| *t > 0) {
                let _ = ev2.send(Event::ModelProgress {
                    name: model.name().to_string(),
                    pct: (done * 100 / total) as u8,
                });
            }
        }) {
            Ok(p) => p,
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("transcription model: {e}")));
                return false;
            }
        }
    };
    let transcriber = match Transcriber::load(model, &model_path) {
        Ok(t) => t,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("transcriber: {e}")));
            return false;
        }
    };

    let _ = store.clear_segments(session_id);
    let _ = store.set_session_model(session_id, model.name());
    let mut total = 0usize;
    for (suffix, source) in [("me", Source::Me), ("others", Source::Others)] {
        let wav = audio_prefix.with_file_name(format!(
            "{}.{suffix}.wav",
            audio_prefix
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default()
        ));
        if !wav.exists() {
            continue;
        }
        let mut on_segment = |seg: Segment| {
            let _ = store.insert_segment(session_id, &seg);
        };
        match zord_transcribe::transcribe_wav_file(&transcriber, source, &wav, &mut on_segment) {
            Ok(n) => total += n,
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("transcribing {suffix}: {e}")));
            }
        }
    }
    if total == 0 {
        let _ = ev.send(Event::Notice(
            "No speech found in the kept audio — nothing transcribed.".to_string(),
        ));
        return false;
    }
    // Refresh whatever the GUI is showing: the transcript (if this session is
    // open), the sidebar (model name changed), and the badges.
    if let Ok(v) = store.segments(session_id) {
        let _ = ev.send(Event::Transcript(v));
    }
    emit_sessions(store, ev);
    let _ = ev.send(Event::Notice(format!(
        "Transcribed {total} segment(s) with {}.",
        model.name()
    )));
    true
}

/// Prepend silence to `mono` so the channel's produced-sample count catches up
/// to real elapsed time at `rate` (sub-30ms jitter ignored; capped at 5 min per
/// buffer). Phase 25d: runs at the *device* rate, before resampling, so the
/// stored native-rate WAV is wall-clock aligned at its own rate.
fn pad_to_wallclock(session_start: Instant, produced: u64, mono: Vec<f32>, rate: u32) -> Vec<f32> {
    let sr = rate.max(1) as u64;
    let elapsed_ms = session_start.elapsed().as_millis() as u64;
    let target = elapsed_ms * sr / 1000;
    let mut pad = target.saturating_sub(produced + mono.len() as u64) as usize;
    if pad <= (sr * 30 / 1000) as usize {
        pad = 0; // ignore sub-30ms jitter
    }
    pad = pad.min((sr * 300) as usize); // never inject more than 5 min at once

    if pad > 0 {
        let mut b = vec![0.0f32; pad];
        b.extend_from_slice(&mono);
        b
    } else {
        mono
    }
}

/// One time-based attack/release smoothing step for the level meter: map this
/// buffer's `rms` to dBFS-normalized [0,1] and integrate it into the running
/// `level` using the buffer's real duration (`n` mono samples at `sample_rate`).
fn smooth_level(rms: f32, n: usize, sample_rate: u32, mut level: f32) -> f32 {
    // RMS -> dBFS -> normalized [0,1] over [FLOOR_DB, 0 dB].
    let db = 20.0 * rms.max(1e-6).log10();
    let target = ((db - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0);
    let dt = n as f32 / sample_rate.max(1) as f32; // seconds this buffer spans (mono)
    let tau = if target > level { LEVEL_ATTACK_S } else { LEVEL_RELEASE_S };
    let alpha = 1.0 - (-dt / tau).exp();
    level += (target - level) * alpha;
    level
}

/// Per-channel resample + VAD stage that also emits live level meters.
fn spawn_proc(
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    source: Source,
    session_start: Instant,
    job_tx: mpsc::Sender<Job>,
    ev: UnboundedSender<Event>,
    wav_path: Option<PathBuf>,
    muted: Option<Arc<AtomicBool>>,
    mut level_ctl: zord_audio::LevelControl,
    stopping: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut resampler = match MonoResampler::new(sample_rate, 1) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut segmenter = Segmenter::new(SegmenterConfig::default());
        // The stored track keeps the device's native rate (Phase 25d).
        let mut wav = wav_path.and_then(|p| WavWriter::create(p, sample_rate).ok());
        // Wall-clock-aligned mono sample count emitted so far. Capture sources
        // (notably WASAPI loopback) deliver no samples during silence, so a raw
        // running count drifts behind real time; we pad the gaps with silence so
        // this channel's timeline == wall-clock — keeping mic + desktop aligned
        // (and the saved WAV / diarization in sync).
        let mut produced: u64 = 0;
        // Smoothed loudness state for the level meter (see constants above).
        let mut level = 0.0f32;
        // Throttle level emission to a fixed cadence, *decoupled from the capture
        // buffer rate*. The smoothing below still updates `level` every buffer (so
        // it stays accurate), but we only forward it to the GUI ~30×/s. Without
        // this, macOS CoreAudio's many small mic buffers (hundreds/sec) flood the
        // unbounded event channel faster than the UI drains it, so the meter lags
        // tens of seconds behind real audio (Windows' larger WASAPI buffers don't
        // hit the limit, which is why the bug was macOS-only).
        let level_send_interval = std::time::Duration::from_millis(33);
        let mut last_level_send = std::time::Duration::ZERO;
        // Opt-in meter diagnostics (set ZORD_METER_DEBUG=1).
        let debug = std::env::var("ZORD_METER_DEBUG").is_ok();
        let (mut dbg_bufs, mut dbg_samps) = (0u64, 0u64);
        let mut dbg_last = session_start.elapsed();

        while let Ok(frame) = rx.recv() {
            // Stop requested: abandon any queued backlog so teardown is prompt.
            if stopping.load(Ordering::Relaxed) {
                break;
            }
            // Muted channel: replace this buffer with silence so timing stays
            // aligned (segmenter/WAV keep advancing) but nothing is transcribed
            // and the meter naturally falls to zero.
            let mut frame = match muted {
                Some(ref m) if m.load(Ordering::Relaxed) => vec![0.0f32; frame.len()],
                _ => frame,
            };
            // Per-channel level control (Phase 26) — before the meter, the WAV,
            // and the model input, so all three see the same adjusted signal.
            level_ctl.process(&mut frame, sample_rate);
            // RMS loudness of this buffer, gained, smoothed with time-based
            // exponential attack/release so both channels react at the same
            // real-world speed regardless of their buffer size/cadence.
            let n = frame.len().max(1);
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();
            level = smooth_level(rms, n, sample_rate, level);
            // Emit at most ~30×/s (see `level_send_interval` above). The meter
            // tracks moment-to-moment because `level` is integrated every buffer;
            // we just don't enqueue an event per buffer.
            let elapsed = session_start.elapsed();
            if elapsed.saturating_sub(last_level_send) >= level_send_interval {
                let _ = ev.send(Event::Level { source, level });
                last_level_send = elapsed;
            }

            if debug {
                dbg_bufs += 1;
                dbg_samps += n as u64;
                let now = session_start.elapsed();
                if now.saturating_sub(dbg_last).as_millis() >= 500 {
                    let secs = (now - dbg_last).as_secs_f32().max(1e-3);
                    eprintln!(
                        "[meter {:?}] {:.0} buf/s, avg {} samp/buf, rms {:.3}, level {:.3}",
                        source,
                        dbg_bufs as f32 / secs,
                        dbg_samps / dbg_bufs.max(1),
                        rms,
                        level
                    );
                    dbg_bufs = 0;
                    dbg_samps = 0;
                    dbg_last = now;
                }
            }

            // Pad the gap (if any) between real elapsed time and samples produced
            // with silence, so timestamps equal the shared wall clock. This is
            // what keeps the two channels in sync: a capture source that goes
            // quiet (WASAPI loopback emits nothing during silence) no longer
            // falls behind real time. Phase 25d: padding happens at the DEVICE
            // rate, before resampling, so the stored native-rate WAV is itself
            // wall-clock aligned (`ms × rate/1000` = sample offset).
            let out: Vec<f32> = pad_to_wallclock(session_start, produced, frame, sample_rate);
            produced += out.len() as u64;
            if let Some(w) = wav.as_mut() {
                let _ = w.write(&out);
            }

            // Models always consume 16 kHz — derived here on the fly, never stored.
            let mono = match resampler.process(&out) {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Timestamps are wall-clock (the input stream is padded to real time;
            // the resampler adds only ~tens of ms of buffering latency).
            for seg in segmenter.push(&mono) {
                let _ = job_tx.send(Job { source, vad: seg });
            }
        }
        if let Some(seg) = segmenter.flush() {
            let _ = job_tx.send(Job { source, vad: seg });
        }
        if let Some(w) = wav {
            let _ = w.finalize();
        }
    })
}
