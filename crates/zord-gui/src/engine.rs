//! Threaded recording engine that backs the GUI.
//!
//! The capture handles (cpal `Stream`, `SCStream`) are `!Send`, so all
//! recording lifecycle lives on one dedicated **control thread**. A second
//! **db thread** answers read-only queries (sessions / search / load) so the UI
//! stays responsive while a recording is in progress. Both push [`Event`]s to
//! the GUI over a `tokio` unbounded channel; the GUI sends [`RecorderCmd`] /
//! [`DbCmd`] over std channels.

use std::path::{Path, PathBuf};
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

/// Retained per-channel audio files that exist on disk for a session.
/// Used as the GUI signal type so call sites don't carry wide tuples.
#[derive(Debug, Clone, Default)]
pub struct AudioFiles {
    /// Absolute path to `me.wav`/`me.opus`, if present.
    pub me: Option<String>,
    /// Absolute path to `others.wav`/`others.opus`, if present.
    pub others: Option<String>,
    /// Per-participant tracks from integration sessions (`spk-N`), keyed by
    /// 0-based speaker index. Empty for normal sessions.
    pub speakers: std::collections::HashMap<i32, String>,
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
    Level {
        source: Source,
        level: f32,
    },
    /// Result of [`DbCmd::ListSessions`].
    Sessions(Vec<Session>),
    /// Sidebar badges per session id: (has_summary, has_compressed, has_speakers).
    SessionBadges(std::collections::HashMap<String, (bool, bool, bool)>),
    /// Result of [`DbCmd::Search`].
    SearchResults(Vec<(String, Segment)>),
    /// A session's full transcript: the result of [`DbCmd::Load`], plus live
    /// refreshes while that session is re-/post-transcribed or diarized. The
    /// id lets the GUI drop refreshes for sessions it isn't showing.
    Transcript {
        id: String,
        segments: Vec<Segment>,
    },
    /// A transcript was exported to this path.
    Exported(String),
    /// The model catalog with current download status.
    Models(Vec<ModelInfo>),
    /// Download progress for a model (0..100).
    ModelProgress {
        name: String,
        pct: u8,
    },
    /// A model download failed — the UI offers the manual-fetch fallback
    /// (direct URL + open models folder) for this model.
    DownloadFailed {
        name: String,
    },
    /// A session's summary (loaded or freshly generated). `None` = none yet.
    Summary(Option<String>),
    /// A session's dense-prose compression (loaded or freshly generated)
    /// (Phase 23). `None` = none yet.
    Compressed(Option<String>),
    /// The id of the session that just started recording — so the GUI can attach
    /// live notes to it (the row exists in the DB from the start of capture).
    SessionStarted(String),
    /// A session's host notes (loaded). `None` = none.
    Notes(Option<String>),
    /// Sessions whose notes matched a search: `(session_id, notes)`.
    NoteResults(Vec<(String, String)>),
    /// An assistant reply to a chat question (Phase 23d). `scope` says which
    /// conversation it belongs to. (Only produced in `summaries` builds.)
    #[allow(dead_code)]
    ChatReply {
        scope: ChatScope,
        reply: String,
    },
    /// A streamed piece of the in-progress chat reply (Phase 24d). Always
    /// followed by a terminal [`Event::ChatReply`] with the full text.
    #[allow(dead_code)]
    ChatDelta {
        scope: ChatScope,
        delta: String,
    },
    /// Custom names for diarized speakers in the viewed session (index → name).
    /// Emitted on session load; do NOT use it to clear diarization busy state.
    Speakers(std::collections::HashMap<i32, String>),
    /// Terminal signal of an on-demand diarization run (Phase 16), tagged with
    /// the session it ran on so the GUI clears the right busy state and only
    /// applies the labels if that session is still the one being viewed. Sent
    /// whether the run succeeded, found nothing, errored, or panicked.
    #[allow(dead_code)] // only constructed under the `diarization` feature
    Diarized {
        id: String,
        speakers: std::collections::HashMap<i32, String>,
    },
    /// The viewed session's saved expected-speaker count (0 = auto-detect).
    DiarizeSpeakers(u32),
    /// Which retained per-channel WAVs exist on disk for the viewed session.
    /// Lines from a channel without a file get no replay button.
    AudioFiles(AudioFiles),
    /// Which speaker index is the app user themself in the viewed/live session
    /// (integration sessions tag it from the configured platform user ID;
    /// `None` for mic/desktop recordings). Styling/perspective only.
    MeSpeaker(Option<i32>),
    /// The transcript line (db id) currently playing back. `None` = stopped or
    /// finished.
    Playing(Option<i64>),
    /// Result of [`ModelCmd::ListRemoteLlm`]: the external server's model ids,
    /// or why it couldn't be reached (Phase 24c).
    RemoteModels {
        models: Vec<String>,
        error: Option<String>,
    },
    /// A post-stop / on-demand transcription pass started (Phase 25) — shows
    /// up on the background-jobs board as its own entry.
    Retranscribing,
    /// Terminal counterpart of [`Event::Retranscribing`] — sent whether the
    /// pass succeeded or failed, so the GUI's busy state always clears.
    Retranscribed,
    /// A cancellable background job started. The GUI tracks these as the
    /// authoritative source for the jobs panel + the inline busy indicators —
    /// independent of which session is being viewed, so navigating/recording
    /// never clears them. `id` is unique (e.g. "diarize:<session>"); `kind` is
    /// one of summarize|compress|overview|diarize|retranscribe|download.
    JobStarted {
        id: String,
        kind: String,
        label: String,
    },
    /// A background job ended (success, no-op, error, or cancellation). Removes
    /// it from the jobs panel and clears the matching inline indicator.
    JobFinished {
        id: String,
    },
    /// The full voiceprint library (Phase 38): emitted after any list / mutate
    /// operation on `DbCmd::Voiceprints*`. GUI signal wired in Task 38d.
    #[allow(dead_code)] // payload read by the GUI in Task 38d
    Voiceprints(Vec<zord_store::VoiceprintInfo>),
    /// The living overview document (Phase 39): the current markdown and when
    /// it was last written (epoch ms). Emitted on load, after an AI update,
    /// after a user save, and after a revert.
    OverviewDoc {
        markdown: String,
        updated_at: u64,
    },
    /// Per-track amplitude profiles for the session timeline (Phase 42a).
    /// One lane per retained audio file; the GUI panel lands in Phase 42c.
    /// Fields read by the panel in Phase 42c.
    #[allow(dead_code)]
    Timeline {
        id: String,
        lanes: Vec<TimelineLane>,
    },
}

/// One audio track's amplitude profile for the session timeline (Phase 42a).
/// The GUI resolves display names from `speaker_names` / `me_speaker`.
#[derive(Debug, Clone, PartialEq)]
pub struct TimelineLane {
    /// Track suffix: "me", "others", "spk-0", "spk-1", …
    pub track: String,
    /// Diarized/integration speaker index for `spk-N` lanes; `None` for
    /// the `me` and `others` tracks.
    pub speaker: Option<i32>,
    /// Duration of the track in milliseconds.
    pub duration_ms: u64,
    /// Normalized 0..=1 peak per bucket
    /// ([`zord_audio::PEAK_BUCKETS`] buckets covering the full track).
    pub peaks: Vec<f32>,
}

/// Which conversation a chat turn belongs to (Phase 23d): a single meeting, or
/// across all recent meetings.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatScope {
    Meeting(String),
    CrossMeeting,
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
        /// Start an integration (Discord) session instead of local capture —
        /// set by the Record Discord button. The `ZORD_DISCORD` /
        /// `ZORD_FAKE_INTEGRATION` env vars still force it (dev path).
        integration: bool,
    },
    Stop,
    /// Mute/unmute the microphone ("Me") mid-recording without stopping. While
    /// muted, mic audio is dropped (recorded as silence) — no transcript, meter
    /// falls to zero.
    SetMicMuted(bool),
    /// Mute/unmute the desktop/system audio ("Others") mid-recording without
    /// stopping. Same semantics as [`SetMicMuted`] for the system channel.
    SetSystemMuted(bool),
    /// Start a microphone *test* (setup wizard): capture the chosen device and
    /// emit `Event::Level` meters only — no session, no WAV, no transcription.
    /// The OS mic-permission prompt fires here. Stopped by [`MicTestStop`] or
    /// superseded by a real recording `Start`.
    ///
    /// [`MicTestStop`]: RecorderCmd::MicTestStop
    MicTestStart {
        device: Option<String>,
    },
    /// Stop a running microphone test.
    MicTestStop,
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
    /// Mix the session's per-speaker tracks into one WAV (Phase 30e).
    ExportAudio(String),
    /// Compress kept WAV tracks of ended sessions to Opus (Phase 37).
    /// `ignore_age` (the "compress now" button) processes everything;
    /// otherwise only sessions older than `compress_after_days`.
    CompressAudio {
        ignore_age: bool,
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
    Diarize {
        id: String,
        num_speakers: u32,
    },
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
    /// Voiceprint library (Phase 38): list / rename / forget. Replies with
    /// `Event::Voiceprints`. GUI call-sites added in Task 38d.
    #[allow(dead_code)] // call sites wired in Task 38d
    Voiceprints,
    #[allow(dead_code)]
    VoiceprintRename {
        id: i64,
        name: String,
    },
    #[allow(dead_code)]
    VoiceprintForget {
        id: i64,
    },
    #[allow(dead_code)]
    VoiceprintForgetAll,
    /// Save the host's free-form notes for a session (empty clears them).
    SetNotes {
        id: String,
        notes: String,
    },
    /// Load the living overview document (Phase 39) — emits `Event::OverviewDoc`.
    LoadOverviewDoc,
    /// Persist a user-edited overview document (Phase 39): plain write (no
    /// prev snapshot — prev is reserved for AI edits) + emit.
    SaveOverviewDoc(String),
    /// Revert the last AI update: swap doc and prev in the store + emit (Phase 39).
    RevertOverviewDoc,
    /// Compute (or serve cached) per-track amplitude lanes for the session
    /// timeline (Phase 42a). Replies with [`Event::Timeline`].
    /// The worker streams each track block-by-block — never loads whole files.
    /// Call sites wired in Phase 42c (panel UI).
    #[allow(dead_code)]
    LoadTimeline(String),
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
    /// Answer a chat question grounded in a meeting / all meetings (Phase 23d).
    /// `turns` is the full conversation so far (incl. the new question last);
    /// each turn is `(is_user, text)`.
    Chat {
        scope: ChatScope,
        turns: Vec<(bool, String)>,
    },
    /// Fold one session (`Some(id)`) or every un-folded session (`None`) into
    /// the living overview document (Phase 39). Job key "overview"; cancellable
    /// between sessions.
    UpdateOverviewDoc { session: Option<String> },
    /// Re-compress every ended session with segments, oldest first (Phase 39).
    /// Job key "recompress"; cancellable between sessions.
    RecompressAll,
}

/// Registry of cancellable background jobs: job id → cooperative cancel flag.
/// Workers register a token on start, poll it at safe checkpoints, and remove it
/// on finish; the GUI flips it via [`Engine::cancel_job`]. Rust can't kill a
/// thread, so cancellation is cooperative — it takes effect at the next
/// checkpoint (instant for chunked work like downloads/streams; for an
/// uninterruptible local LLM generation the result is simply discarded once it
/// returns). Cheap to clone (shared `Arc`).
#[derive(Clone, Default)]
pub struct Jobs {
    tokens: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
}

impl Jobs {
    /// Register a job and announce it; returns its cancel token.
    fn begin(
        &self,
        ev: &UnboundedSender<Event>,
        id: &str,
        kind: &str,
        label: &str,
    ) -> Arc<AtomicBool> {
        let token = Arc::new(AtomicBool::new(false));
        if let Ok(mut m) = self.tokens.lock() {
            m.insert(id.to_string(), token.clone());
        }
        let _ = ev.send(Event::JobStarted {
            id: id.to_string(),
            kind: kind.to_string(),
            label: label.to_string(),
        });
        token
    }

    /// Deregister a job and announce its end (idempotent).
    fn end(&self, ev: &UnboundedSender<Event>, id: &str) {
        if let Ok(mut m) = self.tokens.lock() {
            m.remove(id);
        }
        let _ = ev.send(Event::JobFinished { id: id.to_string() });
    }

    /// Request cancellation of a running job (no-op if it already finished).
    pub fn cancel(&self, id: &str) {
        if let Ok(m) = self.tokens.lock() {
            if let Some(t) = m.get(id) {
                t.store(true, Ordering::Relaxed);
            }
        }
    }

    /// Whether a job with this id is currently registered (between `begin`
    /// and `end`). Used to skip spawning a duplicate worker for the same key
    /// (e.g. two rapid `LoadTimeline`s for one session).
    fn is_running(&self, id: &str) -> bool {
        self.tokens
            .lock()
            .map(|m| m.contains_key(id))
            .unwrap_or(false)
    }
}

/// True if a cancel was requested for this job's token.
fn cancelled(token: &Arc<AtomicBool>) -> bool {
    token.load(Ordering::Relaxed)
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
    /// Cancellable background-job registry (see [`Jobs`]).
    pub jobs: Jobs,
}

/// Every `Engine` in the process is a clone of the single handle returned by
/// [`Engine::spawn`] (the same channels), so any two are interchangeable.
/// Exists so `Engine` can be a Dioxus component prop: "equal" is correct for
/// memoization — an engine handle never carries render-relevant state.
impl PartialEq for Engine {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Engine {
    /// Request cancellation of the background job with this id.
    pub fn cancel_job(&self, id: &str) {
        self.jobs.cancel(id);
    }
}

/// Run `f`, catching any panic so the worker's death is *visible*: the
/// process-wide hook (main.rs) has already written the panic to crash.log by
/// the time this catches it — without this the UI just hangs in whatever busy
/// state the dead worker left behind, with no indication anything went wrong.
fn supervise(name: &str, ev: &UnboundedSender<Event>, f: impl FnOnce()) {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    if catch_unwind(AssertUnwindSafe(f)).is_err() {
        let _ = ev.send(Event::Status(Status::Error(format!(
            "internal error: the {name} worker crashed — details in logs/crash.log; restart the app to recover"
        ))));
    }
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
        let jobs = Jobs::default();

        {
            let ev = ev_tx.clone();
            let dbp = db_path.clone();
            let stx = summ_tx.clone();
            thread::spawn(move || {
                let sup = ev.clone();
                supervise("recorder", &sup, move || control_loop(rec_rx, ev, dbp, stx));
            });
        }
        {
            let ev = ev_tx.clone();
            let dbp = db_path.clone();
            let jobs = jobs.clone();
            thread::spawn(move || {
                let sup = ev.clone();
                supervise("database", &sup, move || db_loop(db_rx, ev, dbp, jobs));
            });
        }
        {
            let ev = ev_tx.clone();
            thread::spawn(move || {
                let sup = ev.clone();
                supervise("model", &sup, move || model_loop(model_rx, ev));
            });
        }
        {
            let ev = ev_tx.clone();
            let jobs = jobs.clone();
            thread::spawn(move || {
                let sup = ev.clone();
                supervise("summarize", &sup, move || {
                    summarize_loop(summ_rx, ev, db_path, jobs)
                });
            });
        }
        {
            let ev = ev_tx;
            thread::spawn(move || {
                let sup = ev.clone();
                supervise("playback", &sup, move || play_loop(play_rx, ev));
            });
        }
        {
            // Age-based compression sweep (Phase 37): shortly after startup,
            // then periodically. The worker re-reads settings on every run,
            // so toggling the feature needs no restart.
            let db_tx = db_tx.clone();
            thread::spawn(move || {
                thread::sleep(std::time::Duration::from_secs(90));
                loop {
                    if db_tx
                        .send(DbCmd::CompressAudio { ignore_age: false })
                        .is_err()
                    {
                        break;
                    }
                    thread::sleep(std::time::Duration::from_secs(6 * 3600));
                }
            });
        }
        (
            Engine {
                rec_tx,
                db_tx,
                model_tx,
                summ_tx,
                play_tx,
                jobs,
            },
            ev_rx,
        )
    }
}

/// Worker that generates session summaries (local LLM, heavy). Real impl only
/// in `summaries` builds; otherwise it reports a friendly notice.
fn summarize_loop(
    rx: mpsc::Receiver<SummCmd>,
    ev: UnboundedSender<Event>,
    db_path: PathBuf,
    jobs: Jobs,
) {
    // A chat keeps its model resident across turns so follow-ups don't reload it.
    // One-shot jobs (summarize/compress/overview) load + drop their own model, so
    // we free the resident one first to keep peak RAM at a single model.
    #[cfg(any(feature = "llm-local", feature = "llm-remote"))]
    let mut chat_model: Option<(ChatLlmKey, zord_summarize::LlmBackend)> = None;
    while let Ok(cmd) = rx.recv() {
        #[cfg(any(feature = "llm-local", feature = "llm-remote"))]
        match cmd {
            // These run a single (uninterruptible) generation, so cancellation is
            // "detach": the token is passed in and the result is discarded if it
            // was cancelled by the time generation returns. Chat is not a tracked
            // job (it has its own inline busy state).
            SummCmd::Summarize(id) => {
                chat_model = None;
                let jid = format!("summarize:{id}");
                let token = jobs.begin(&ev, &jid, "summarize", "Summarizing meeting");
                summarize_one(&id, &ev, &db_path, &token);
                jobs.end(&ev, &jid);
            }
            SummCmd::Compress(id) => {
                chat_model = None;
                let jid = format!("compress:{id}");
                let token = jobs.begin(&ev, &jid, "compress", "Compressing meeting");
                compress_one(&id, &ev, &db_path, &token);
                jobs.end(&ev, &jid);
            }
            SummCmd::Chat { scope, turns } => {
                chat_one(&mut chat_model, scope, turns, &ev, &db_path);
            }
            SummCmd::UpdateOverviewDoc { session } => {
                chat_model = None;
                let token = jobs.begin(&ev, "overview", "overview", "Updating overview");
                update_overview_doc(session.as_deref(), &ev, &db_path, &token);
                jobs.end(&ev, "overview");
            }
            SummCmd::RecompressAll => {
                chat_model = None;
                let token = jobs.begin(&ev, "recompress", "compress", "Re-compressing transcripts");
                recompress_all(&ev, &db_path, &token);
                jobs.end(&ev, "recompress");
            }
        }
        #[cfg(not(any(feature = "llm-local", feature = "llm-remote")))]
        {
            let _ = (&cmd, &db_path, &jobs);
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
    segs.iter()
        .map(|s| format!("{}: {}", s.speaker_label(names), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prepend the host's session notes (links, action items, reminders) to LLM
/// grounding input as an authoritative block, so summaries / compression / chat
/// all see them alongside the transcript. No-op when there are no notes.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn with_notes(store: &Store, session_id: &str, transcript: String) -> String {
    match store.get_notes(session_id).ok().flatten() {
        Some(n) if !n.trim().is_empty() => format!(
            "Notes from the session host (links, action items, and reminders — \
             treat these as authoritative and include them where relevant):\n{}\n\n{transcript}",
            n.trim()
        ),
        _ => transcript,
    }
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

/// Compress a session's transcript into a condensed line-by-line form and
/// store it (Phase 23). Returns `true` when a compression was generated and
/// stored (so the re-compress sweep can count its work).
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn compress_one(
    session_id: &str,
    ev: &UnboundedSender<Event>,
    db_path: &PathBuf,
    token: &Arc<AtomicBool>,
) -> bool {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return false;
        }
    };
    let segs = store.segments(session_id).unwrap_or_default();
    if segs.is_empty() {
        let _ = ev.send(Event::Notice(
            "Nothing to compress in this session.".to_string(),
        ));
        return false;
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = with_notes(&store, session_id, render_labeled_transcript(&segs, &names));

    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice(
        "Preparing the LLM for compression…".to_string(),
    ));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        return false;
    };
    let _ = ev.send(Event::Notice(
        "Compressing… (runs in the background)".to_string(),
    ));
    match llm.compress(
        &transcript,
        zord_config::compress_prompt(),
        settings.compress_ctx,
    ) {
        Ok(text) => {
            if cancelled(token) {
                return false; // cancelled mid-generation → discard (detach)
            }
            let _ = store.set_compressed(session_id, &text);
            let _ = ev.send(Event::Compressed(Some(text)));
            let _ = ev.send(Event::Notice("Compressed.".to_string()));
            true
        }
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("compress failed: {e}")));
            false
        }
    }
}

/// Fold one or more sessions into the living overview document (Phase 39).
/// `target_id = Some(id)` folds that session; `None` folds every un-folded
/// ended session, oldest-first.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn update_overview_doc(
    target_id: Option<&str>,
    ev: &UnboundedSender<Event>,
    db_path: &std::path::Path,
    token: &Arc<AtomicBool>,
) {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };
    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice(
        "Preparing the LLM for overview update…".to_string(),
    ));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        return;
    };

    // Determine which sessions to fold.
    let sessions_to_fold: Vec<zord_core::Session> = if let Some(id) = target_id {
        match store.get_session(id) {
            Ok(Some(s)) if s.ended_at.is_some() => vec![s],
            _ => {
                let _ = ev.send(Event::Notice(format!(
                    "Session {id} not found or not yet ended — skipping."
                )));
                return;
            }
        }
    } else {
        let all = store.list_sessions().unwrap_or_default();
        unfolded_sessions(&all).into_iter().cloned().collect()
    };

    if sessions_to_fold.is_empty() {
        let _ = ev.send(Event::Notice(
            "Overview is up to date — no new sessions to fold.".to_string(),
        ));
        return;
    }

    for session in &sessions_to_fold {
        if cancelled(token) {
            break;
        }

        // Build the session input: stored compression first, else raw transcript.
        let segs = store.segments(&session.id).unwrap_or_default();
        if segs.is_empty() {
            // No transcript — skip without stamping: a later re-transcription
            // makes this session foldable, and the next fold-all retries it.
            continue;
        }
        let session_input = match store.get_compressed(&session.id).ok().flatten() {
            Some(c) if !c.trim().is_empty() => c,
            _ => {
                let names = store.speaker_names(&session.id).unwrap_or_default();
                render_labeled_transcript(&segs, &names)
            }
        };

        let label = overview_session_label(session);
        let mut progress = |note: &str| {
            let _ = ev.send(Event::Notice(note.to_string()));
        };

        // Read the document just before this fold so we work against the latest
        // version (important when folding multiple sessions in one run).
        let (doc_before, ts_before) = load_overview_doc(&store);
        let result = zord_overview::update_document(
            &doc_before,
            &session_input,
            &label,
            &llm,
            &settings,
            &mut progress,
        );
        if cancelled(token) {
            break; // cancelled mid-generation → discard (detach)
        }
        let folded = match result {
            Ok(d) => d,
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("overview update failed: {e}")));
                continue;
            }
        };

        // Optimistic write: re-read just before writing — if the document
        // changed underneath us (user edited mid-run), redo the fold ONCE
        // against the fresh text. `base` is whatever the result replaces.
        let (doc_now, ts_now) = load_overview_doc(&store);
        let (base, folded) = if ts_now != ts_before {
            match zord_overview::update_document(
                &doc_now,
                &session_input,
                &label,
                &llm,
                &settings,
                &mut progress,
            ) {
                Ok(d) => (doc_now, d),
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!("overview update failed: {e}")));
                    continue;
                }
            }
        } else {
            (doc_before, folded)
        };

        // Sanity floor: when folding into a non-empty document, reject output
        // shorter than 20% of it — keep the old doc (and _prev), leave this
        // session unstamped (the next fold-all retries it), and continue with
        // the rest: each fold is independent and the document was kept intact.
        if !base.trim().is_empty() && folded.len() < base.len() / 5 {
            let _ = ev.send(Event::Notice(
                "overview update looked destructive — kept the previous document".to_string(),
            ));
            continue;
        }

        // Snapshot the current doc to _prev, then write the new one.
        if let Err(e) = save_overview_doc_with_snapshot(&store, &folded) {
            let _ = ev.send(Event::Notice(format!("overview write failed: {e}")));
            break;
        }
        // Stamp the session as folded so it isn't picked up again.
        let _ = store.set_overview_folded(&session.id, now_ms());

        // Emit the updated document.
        let (markdown, updated_at) = load_overview_doc(&store);
        let _ = ev.send(Event::OverviewDoc {
            markdown,
            updated_at,
        });
        let _ = ev.send(Event::Notice(format!("Overview updated from {label}.")));
    }
}

/// Re-compress every ended session that has segments, oldest-first (Phase 39):
/// the new line-by-line compress prompt applied over the whole library. Reuses
/// [`compress_one`] per session; cancellable between sessions.
#[cfg(any(feature = "llm-local", feature = "llm-remote"))]
fn recompress_all(ev: &UnboundedSender<Event>, db_path: &PathBuf, token: &Arc<AtomicBool>) {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };
    // Ended sessions, oldest-first (list_sessions is newest-first); sessions
    // without segments are filtered here so compress_one's "nothing to
    // compress" notice doesn't spam.
    let mut sessions: Vec<zord_core::Session> = store
        .list_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s.ended_at.is_some())
        .filter(|s| !store.segments(&s.id).unwrap_or_default().is_empty())
        .collect();
    sessions.reverse();

    let total = sessions.len();
    let mut count = 0usize;
    for (i, session) in sessions.iter().enumerate() {
        if cancelled(token) {
            break;
        }
        let _ = ev.send(Event::Notice(format!(
            "Re-compressing session {}/{total}…",
            i + 1
        )));
        if compress_one(&session.id, ev, db_path, token) {
            count += 1;
        }
    }
    let _ = ev.send(Event::Notice(format!("Re-compressed {count} session(s).")));
}

/// Cheap check that an LLM backend is *configured* (mirrors
/// [`build_llm_backend`]'s requirements without constructing anything) — safe
/// to call from the recorder thread. Always `false` in builds without an LLM
/// feature, so the auto overview chain no-ops there.
#[allow(clippy::needless_return)] // cfg'd tails (same shape as chat_llm_key)
fn llm_backend_configured(settings: &zord_config::Settings) -> bool {
    #[cfg(any(feature = "llm-local", feature = "llm-remote"))]
    if use_external(settings) {
        // External server: a model must be picked (build_llm_backend refuses
        // otherwise; the base URL has a default).
        return !settings.llm_model.trim().is_empty();
    }
    #[cfg(feature = "llm-local")]
    {
        // Local GGUF: a summary model must be selected (it is downloaded /
        // resolved lazily by the worker, so "selected" is the cheap check).
        return !settings.summary_model.trim().is_empty();
    }
    #[cfg(not(feature = "llm-local"))]
    {
        // llm-remote-only build: use_external() is always true, so this point
        // is unreachable at runtime. Featureless build: the chain no-ops.
        let _ = settings;
        false
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
            // Phase 39: ground on the living overview document when it has
            // content; fall back to the older per-meeting compressions until
            // the document is first populated.
            let (doc, _) = load_overview_doc(&store);
            if !doc.trim().is_empty() {
                (doc, settings.overview_ctx)
            } else {
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
    };

    let system = format!(
        "{}\n\n=== Context ===\n{}",
        zord_config::chat_system_prompt(),
        context
    );
    // Error bubbles ("⚠️ Chat failed: …") are part of the visible conversation
    // but not real assistant output — don't feed them back to the model.
    let mapped: Vec<(ChatRole, String)> = turns
        .into_iter()
        .filter(|(is_user, t)| *is_user || !t.starts_with("⚠️"))
        .map(|(is_user, t)| {
            (
                if is_user {
                    ChatRole::User
                } else {
                    ChatRole::Assistant
                },
                t,
            )
        })
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
            let _ = ev.send(Event::ChatReply {
                scope,
                reply: format!("⚠️ Chat failed: {e}"),
            });
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
        let _ = ev.send(Event::Notice(
            "This session has no transcript to chat about.".to_string(),
        ));
        return None;
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = with_notes(store, session_id, render_labeled_transcript(&segs, &names));

    // Reserve headroom (chat output + conversation + prompt) within the window.
    let budget = (settings.compress_ctx as usize).saturating_sub(1400);
    let fits = llm
        .count_tokens(&transcript)
        .map(|t| t < budget)
        .unwrap_or(false);
    if fits {
        return Some(format!("Meeting transcript:\n{transcript}"));
    }

    // Too long: fall back to the (cached) compression, generating it if needed.
    if let Ok(Some(c)) = store.get_compressed(session_id) {
        if !c.trim().is_empty() {
            return Some(format!("Meeting compression (dense):\n{c}"));
        }
    }
    let _ = ev.send(Event::Notice(
        "Long meeting — compressing it first to chat…".to_string(),
    ));
    match llm.compress(
        &transcript,
        zord_config::compress_prompt(),
        settings.compress_ctx,
    ) {
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
fn summarize_one(
    session_id: &str,
    ev: &UnboundedSender<Event>,
    db_path: &PathBuf,
    token: &Arc<AtomicBool>,
) {
    let store = match Store::open(db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("db: {e}")));
            return;
        }
    };
    let segs = store.segments(session_id).unwrap_or_default();
    if segs.is_empty() {
        let _ = ev.send(Event::Notice(
            "Nothing to summarize in this session.".to_string(),
        ));
        return;
    }
    // Label each line by its diarized speaker (and custom name, if assigned) so
    // the LLM can attribute statements/actions to the right person.
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = with_notes(&store, session_id, render_labeled_transcript(&segs, &names));

    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice("Preparing the LLM…".to_string()));
    let Some(llm) = build_llm_backend(&settings, ev) else {
        return;
    };
    let _ = ev.send(Event::Notice("Summarizing…".to_string()));
    match llm.summarize(&transcript, &settings.effective_summary_prompt()) {
        Ok(text) => {
            // Cancelled mid-generation → discard the result (detach).
            if cancelled(token) {
                return;
            }
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
                } else if zord_summarize::ollama_models()
                    .iter()
                    .any(|m| m.filename == name)
                {
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
                            let _ = ev.send(Event::RemoteModels {
                                models,
                                error: None,
                            });
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
    // unwrap_or_default: a pre-1970 system clock yields 0 instead of a panic.
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

// ---------------------------------------------------------------------------
// Phase 39 — living-overview document storage helpers
// ---------------------------------------------------------------------------

const OVERVIEW_DOC_KEY: &str = "overview_doc";
const OVERVIEW_DOC_PREV_KEY: &str = "overview_doc_prev";

/// Load the living overview document. Returns `("", 0)` when nothing is stored yet.
fn load_overview_doc(store: &Store) -> (String, u64) {
    store
        .get_meta(OVERVIEW_DOC_KEY)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Snapshot the current document to `*_prev`, then write the new content.
/// Used only for AI-generated updates so `prev` can be reverted.
#[allow(dead_code)] // used by the llm-gated fold worker + tests
fn save_overview_doc_with_snapshot(store: &Store, doc: &str) -> anyhow::Result<()> {
    // Snapshot: read current → write to _prev (best-effort; ignore missing).
    if let Ok(Some((current, _))) = store.get_meta(OVERVIEW_DOC_KEY) {
        store.set_meta(OVERVIEW_DOC_PREV_KEY, &current)?;
    }
    store.set_meta(OVERVIEW_DOC_KEY, doc)?;
    Ok(())
}

/// Ended sessions not yet stamped into the living overview
/// (`overview_folded_ms IS NULL`), sorted oldest-first (sessions still
/// recording — `ended_at == None` — never qualify). Transcript presence is
/// checked by the fold worker, keeping this pure for unit tests.
#[allow(dead_code)] // used by the llm-gated fold worker + tests
fn unfolded_sessions(sessions: &[Session]) -> Vec<&Session> {
    let mut out: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.ended_at.is_some() && s.overview_folded_ms.is_none())
        .collect();
    out.sort_by_key(|s| s.ended_at.unwrap_or(0));
    out
}

/// "YYYY-MM-DD" (local timezone) from an epoch-ms timestamp.
#[allow(dead_code)] // used by the llm-gated fold worker + tests
fn fmt_date_ms(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// Build the human-readable session label used in the overview fold:
/// `"YYYY-MM-DD — <title or id>"` (the model uses it to date Archive entries).
#[allow(dead_code)] // used by the llm-gated fold worker + tests
fn overview_session_label(session: &Session) -> String {
    let date = fmt_date_ms(session.started_at);
    let title = session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&session.id);
    format!("{date} — {title}")
}

// ---------------------------------------------------------------------------
// DB query thread
// ---------------------------------------------------------------------------

/// Shared timeline cache: session_id → (fingerprint, lanes). Accessed by the
/// db thread (read) and by the timeline worker thread (write), so it is wrapped
/// in `Arc<Mutex>`.
type TimelineCache =
    Arc<std::sync::Mutex<std::collections::HashMap<String, (u64, Vec<TimelineLane>)>>>;

fn db_loop(rx: mpsc::Receiver<DbCmd>, ev: UnboundedSender<Event>, db_path: PathBuf, jobs: Jobs) {
    let store = match Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Status(Status::Error(format!("db open failed: {e}"))));
            return;
        }
    };
    // Phase 42a: in-process timeline cache — fingerprint (sum of file len + mtime)
    // keyed by session id.  Capped at 8 entries: simple eviction by clearing when
    // we exceed the limit (the next load refills from disk).
    // Wrapped in Arc<Mutex> so spawned worker threads can insert their results.
    let timeline_cache: TimelineCache =
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    while let Ok(cmd) = rx.recv() {
        match cmd {
            DbCmd::ListSessions => {
                emit_sessions(&store, &ev);
            }
            DbCmd::Search(raw) => {
                let raw = raw.trim().to_string();
                let q = sanitize_fts(&raw);
                if raw.is_empty() {
                    let _ = ev.send(Event::SearchResults(Vec::new()));
                    let _ = ev.send(Event::NoteResults(Vec::new()));
                } else {
                    // Transcript FTS (skip if the query sanitizes to nothing).
                    let segs = if q.is_empty() {
                        Vec::new()
                    } else {
                        store.search(&q).unwrap_or_default()
                    };
                    let _ = ev.send(Event::SearchResults(segs));
                    // Host notes: literal substring of the RAW query (links/URLs
                    // aren't FTS tokens, so search them verbatim).
                    if let Ok(n) = store.search_notes(&raw) {
                        let _ = ev.send(Event::NoteResults(n));
                    }
                }
            }
            DbCmd::Load(id) => {
                if let Ok(v) = store.segments(&id) {
                    let _ = ev.send(Event::Transcript {
                        id: id.clone(),
                        segments: v,
                    });
                }
                let _ = ev.send(Event::Speakers(
                    store.speaker_names(&id).unwrap_or_default(),
                ));
                let _ = ev.send(Event::Summary(store.get_summary(&id).ok().flatten()));
                let _ = ev.send(Event::Compressed(store.get_compressed(&id).ok().flatten()));
                let _ = ev.send(Event::Notes(store.get_notes(&id).ok().flatten()));
                let _ = ev.send(Event::DiarizeSpeakers(
                    store.get_diarize_speakers(&id).ok().flatten().unwrap_or(0),
                ));
                let _ = ev.send(Event::AudioFiles(session_audio_files(&store, &id)));
                let _ = ev.send(Event::MeSpeaker(store.me_speaker(&id).unwrap_or(None)));
            }
            DbCmd::Export { id, format } => match export_session(&store, &id, format) {
                Ok(path) => {
                    let _ = ev.send(Event::Exported(path));
                }
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!("export failed: {e}")));
                }
            },
            DbCmd::ExportAudio(id) => {
                // Mixing reads every track of the session — off the db thread.
                let ev = ev.clone();
                let db_path = db_path.clone();
                let jobs = jobs.clone();
                thread::spawn(move || {
                    let jid = format!("exportaudio:{id}");
                    let _token = jobs.begin(&ev, &jid, "export", "Merging session audio");
                    supervise("audio export", &ev, || {
                        match export_merged_audio(&db_path, &id) {
                            Ok(path) => {
                                let _ = ev.send(Event::Exported(path));
                            }
                            Err(e) => {
                                let _ = ev.send(Event::Notice(format!("merged audio failed: {e}")));
                            }
                        }
                    });
                    jobs.end(&ev, &jid);
                });
            }
            DbCmd::CompressAudio { ignore_age } => {
                // Encoding hours of audio is heavy — off the db thread, as a
                // visible, cancellable job.
                let ev = ev.clone();
                let db_path = db_path.clone();
                let jobs = jobs.clone();
                thread::spawn(move || {
                    let jid = "compress-audio".to_string();
                    let token = jobs.begin(&ev, &jid, "compress", "Compressing kept audio");
                    supervise("audio compression", &ev, || {
                        match compress_sweep(&db_path, ignore_age, &token) {
                            Ok((sessions, bytes)) if sessions > 0 => {
                                let _ = ev.send(Event::Notice(format!(
                                    "Compressed {sessions} session(s) — reclaimed {:.1} MB.",
                                    bytes as f64 / 1_048_576.0
                                )));
                                if let Ok(store) = Store::open(&db_path) {
                                    emit_sessions(&store, &ev);
                                }
                            }
                            Ok(_) => {
                                if ignore_age {
                                    let _ = ev.send(Event::Notice(
                                        "Nothing to compress — kept audio is already Opus."
                                            .to_string(),
                                    ));
                                }
                            }
                            Err(e) => {
                                let _ = ev.send(Event::Notice(format!("audio compression: {e}")));
                            }
                        }
                    });
                    jobs.end(&ev, &jid);
                });
            }
            DbCmd::Rename { id, title } => {
                let _ = store.set_session_title(&id, &title);
                emit_sessions(&store, &ev);
            }
            DbCmd::SetNotes { id, notes } => {
                let _ = store.set_notes(&id, &notes);
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
                #[cfg(feature = "voiceprints")]
                {
                    let settings = zord_config::Settings::load();
                    if settings.voiceprints_enabled && !name.trim().is_empty() {
                        if let Ok(Some((model, emb))) =
                            store.session_speaker_embedding(&id, speaker)
                        {
                            if let Ok(vid) =
                                store.enroll_voiceprint(name.trim(), &model, &emb, Some(&id))
                            {
                                let _ = store.link_speaker_voiceprint(&id, speaker, vid);
                                let _ = ev.send(Event::Voiceprints(
                                    store.voiceprints().unwrap_or_default(),
                                ));
                            }
                        }
                    }
                }
                let _ = ev.send(Event::Speakers(
                    store.speaker_names(&id).unwrap_or_default(),
                ));
            }
            DbCmd::Voiceprints => {
                let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
            }
            DbCmd::VoiceprintRename { id, name } => {
                let _ = store.rename_voiceprint(id, &name);
                let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
            }
            DbCmd::VoiceprintForget { id } => {
                let _ = store.forget_voiceprint(id);
                let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
            }
            DbCmd::VoiceprintForgetAll => {
                let _ = store.forget_all_voiceprints();
                let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
            }
            DbCmd::LoadOverviewDoc => {
                let (markdown, updated_at) = load_overview_doc(&store);
                let _ = ev.send(Event::OverviewDoc {
                    markdown,
                    updated_at,
                });
            }
            DbCmd::SaveOverviewDoc(doc) => {
                // Plain user-edit write: no _prev snapshot (that's for AI edits).
                let _ = store.set_meta(OVERVIEW_DOC_KEY, &doc);
                let (markdown, updated_at) = load_overview_doc(&store);
                let _ = ev.send(Event::OverviewDoc {
                    markdown,
                    updated_at,
                });
            }
            DbCmd::RevertOverviewDoc => {
                // Swap doc ↔ prev: the previous AI version becomes current again.
                let prev = store
                    .get_meta(OVERVIEW_DOC_PREV_KEY)
                    .ok()
                    .flatten()
                    .map(|(v, _)| v)
                    .unwrap_or_default();
                let current = store
                    .get_meta(OVERVIEW_DOC_KEY)
                    .ok()
                    .flatten()
                    .map(|(v, _)| v)
                    .unwrap_or_default();
                let _ = store.set_meta(OVERVIEW_DOC_KEY, &prev);
                let _ = store.set_meta(OVERVIEW_DOC_PREV_KEY, &current);
                let (markdown, updated_at) = load_overview_doc(&store);
                let _ = ev.send(Event::OverviewDoc {
                    markdown,
                    updated_at,
                });
            }
            DbCmd::Diarize { id, num_speakers } => {
                // Remember the chosen count on the session for next time.
                let _ = store.set_diarize_speakers(&id, num_speakers);
                // Heavy (loads ONNX + clusters); run off the db thread so queries
                // stay responsive. The worker opens its own Store.
                let ev = ev.clone();
                let db_path = db_path.clone();
                let jobs = jobs.clone();
                thread::spawn(move || {
                    let jid = format!("diarize:{id}");
                    let token = jobs.begin(&ev, &jid, "diarize", "Identifying speakers");
                    supervise("diarize", &ev, || {
                        diarize_session_ondemand(&db_path, &id, num_speakers, &ev, &token)
                    });
                    jobs.end(&ev, &jid);
                });
            }
            DbCmd::Retranscribe(id) => {
                // Heavy (model load + minutes of inference); own thread + Store.
                let ev = ev.clone();
                let db_path = db_path.clone();
                let jobs = jobs.clone();
                thread::spawn(move || {
                    let jid = format!("retranscribe:{id}");
                    let token = jobs.begin(&ev, &jid, "retranscribe", "Re-transcribing meeting");
                    supervise("retranscribe", &ev, || {
                        retranscribe_session_ondemand(&db_path, &id, &ev, &token)
                    });
                    jobs.end(&ev, &jid);
                });
            }
            DbCmd::LoadTimeline(id) => {
                // Discover tracks for this session so we can fingerprint them.
                let prefix = store
                    .get_session(&id)
                    .ok()
                    .flatten()
                    .and_then(|s| s.audio_path);
                let Some(prefix) = prefix else {
                    // No audio retained — emit an empty timeline immediately.
                    let _ = ev.send(Event::Timeline {
                        id,
                        lanes: Vec::new(),
                    });
                    continue;
                };
                let tracks = discover_session_tracks(&prefix);
                if tracks.is_empty() {
                    let _ = ev.send(Event::Timeline {
                        id,
                        lanes: Vec::new(),
                    });
                    continue;
                }

                // Build a fingerprint: sum of (file_len + mtime_secs) over all
                // track files. Cheap and sufficient for detecting re-compression
                // or file replacement.
                let fingerprint: u64 = tracks
                    .iter()
                    .filter_map(|(_, p)| std::fs::metadata(p).ok())
                    .map(|m| {
                        let len = m.len();
                        let mtime = m
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        len.wrapping_add(mtime)
                    })
                    .fold(0u64, u64::wrapping_add);

                // Cache hit — serve immediately without spawning a job.
                if let Ok(cache) = timeline_cache.lock() {
                    if let Some((cached_fp, cached_lanes)) = cache.get(&id) {
                        if *cached_fp == fingerprint {
                            let _ = ev.send(Event::Timeline {
                                id,
                                lanes: cached_lanes.clone(),
                            });
                            continue;
                        }
                    }
                }

                // Already building this session's timeline? Don't spawn a
                // duplicate worker — the running one will emit the event when
                // done (two rapid re-opens would otherwise double-compute and
                // make the job bar flicker).
                let jid = format!("timeline:{id}");
                if jobs.is_running(&jid) {
                    continue;
                }

                // Cache miss — spawn a supervised job to stream-compute peaks.
                // The job is cancellable between tracks via the cancel token.
                // `begin` runs HERE (db thread) so the in-flight check above is
                // race-free: registration is visible before the next command.
                let token = jobs.begin(&ev, &jid, "timeline", "Building timeline");
                let ev2 = ev.clone();
                let jobs2 = jobs.clone();
                let cache2 = Arc::clone(&timeline_cache);
                thread::spawn(move || {
                    supervise("timeline", &ev2, || {
                        build_timeline(&id, &tracks, &token, &ev2, fingerprint, &cache2);
                    });
                    jobs2.end(&ev2, &jid);
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Playback thread (replay one transcript line from a retained WAV)
// ---------------------------------------------------------------------------

/// Discover every retained audio track for a session, returning a list of
/// `(suffix, path)` pairs, e.g. `[("me", …), ("others", …), ("spk-0", …)]`.
///
/// Handles both the folder layout (Phase 28+) and the legacy flat prefix, and
/// honours the Phase 37 `.opus` fallback via [`zord_config::resolve_track`].
/// Shared between [`session_audio_files`] and the Phase 42a timeline worker so
/// the spk-N enumeration logic lives in exactly one place.
fn discover_session_tracks(prefix: &str) -> Vec<(String, PathBuf)> {
    let resolve =
        |role: &str| zord_config::resolve_track(prefix, role).map(|p| (role.to_string(), p));

    // Enumerate spk-N indices from the directory (integration sessions only).
    let folder = std::path::Path::new(prefix);
    let mut spk_indices: Vec<i32> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem = name
                .strip_prefix("spk-")?
                .strip_suffix(".wav")
                .or_else(|| name.strip_prefix("spk-")?.strip_suffix(".opus"))?;
            stem.parse::<i32>().ok()
        })
        .collect::<std::collections::HashSet<i32>>()
        .into_iter()
        .collect();
    spk_indices.sort_unstable();

    let mut tracks: Vec<(String, PathBuf)> = Vec::new();
    for role in &["me", "others"] {
        if let Some(pair) = resolve(role) {
            tracks.push(pair);
        }
    }
    for idx in spk_indices {
        let suffix = format!("spk-{idx}");
        if let Some(pair) = resolve(&suffix) {
            tracks.push(pair);
        }
    }
    tracks
}

/// Absolute paths of the retained per-channel WAVs that actually exist on disk
/// for a session. Returns an [`AudioFiles`] covering the standard `me`/`others`
/// roles and any per-participant `spk-N` tracks written by integration sessions.
/// Either `me`/`others` may be absent (retention or default settings may have
/// removed them). `speakers` is empty for normal (non-integration) sessions.
fn session_audio_files(store: &Store, session_id: &str) -> AudioFiles {
    let prefix = store
        .get_session(session_id)
        .ok()
        .flatten()
        .and_then(|s| s.audio_path);
    let Some(prefix) = prefix else {
        return AudioFiles::default();
    };
    let mut af = AudioFiles::default();
    for (suffix, path) in discover_session_tracks(&prefix) {
        let path_str = path.display().to_string();
        if suffix == "me" {
            af.me = Some(path_str);
        } else if suffix == "others" {
            af.others = Some(path_str);
        } else if let Some(idx_str) = suffix.strip_prefix("spk-") {
            if let Ok(idx) = idx_str.parse::<i32>() {
                af.speakers.insert(idx, path_str);
            }
        }
    }
    af
}

// ---------------------------------------------------------------------------
// Phase 42a: timeline peak computation worker
// ---------------------------------------------------------------------------

/// Compute per-track amplitude peaks for `tracks` (streaming, never slurps
/// whole files), emit [`Event::Timeline`], and insert into the shared cache.
/// Called from a detached supervised thread; checks `token` between tracks so
/// the job is cancellable.
fn build_timeline(
    session_id: &str,
    tracks: &[(String, PathBuf)],
    token: &Arc<AtomicBool>,
    ev: &UnboundedSender<Event>,
    fingerprint: u64,
    cache: &TimelineCache,
) {
    let mut lanes: Vec<TimelineLane> = Vec::with_capacity(tracks.len());
    for (suffix, path) in tracks {
        if cancelled(token) {
            return; // aborted between tracks
        }
        match zord_audio::compute_track_peaks(path) {
            Ok((peaks, duration_ms)) => {
                let speaker = suffix
                    .strip_prefix("spk-")
                    .and_then(|s| s.parse::<i32>().ok());
                lanes.push(TimelineLane {
                    track: suffix.clone(),
                    speaker,
                    duration_ms,
                    peaks,
                });
            }
            Err(e) => {
                tracing::warn!(
                    session = session_id,
                    track = %suffix,
                    path = %path.display(),
                    "timeline: skipping track — {e}"
                );
            }
        }
    }

    // Insert into cache (evict when at the 8-entry cap).
    if let Ok(mut guard) = cache.lock() {
        if guard.len() >= 8 {
            guard.clear();
        }
        guard.insert(session_id.to_string(), (fingerprint, lanes.clone()));
    }

    let _ = ev.send(Event::Timeline {
        id: session_id.to_string(),
        lanes,
    });
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
            Some(PlayCmd::Play {
                segment_id,
                wav,
                start_ms,
                end_ms,
            }) => {
                if let Some(s) = sink.take() {
                    s.stop();
                }
                current = None;
                if output.is_none() {
                    output = rodio::DeviceSinkBuilder::open_default_sink().ok();
                }
                let Some(device) = output.as_ref() else {
                    let _ = ev.send(Event::Notice(
                        "No audio output device available.".to_string(),
                    ));
                    let _ = ev.send(Event::Playing(None));
                    continue;
                };
                // Retained WAVs are wall-clock aligned (silence-padded) at their
                // own rate, so timestamps map directly onto sample offsets — the
                // reader derives them from the file header (native-rate tracks
                // from Phase 25d and older 16 kHz ones both work). Native rate
                // also means playback at full capture quality.
                let (samples, rate) =
                    zord_audio::read_audio_slice_ms(&wav, start_ms, end_ms).unwrap_or_default();
                if samples.is_empty() {
                    let _ = ev.send(Event::Notice(
                        "Couldn't read this line's audio.".to_string(),
                    ));
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
    token: &Arc<AtomicBool>,
) {
    #[cfg(not(feature = "diarization"))]
    {
        let _ = (db_path, session_id, num_speakers, token);
        let _ = ev.send(Event::Notice(
            "Diarization isn't built in — rebuild with `--features diarization`.".to_string(),
        ));
    }
    #[cfg(feature = "diarization")]
    {
        use std::panic::{catch_unwind, AssertUnwindSafe};
        // Run the work catching any Rust panic, then ALWAYS emit a terminal
        // Event::Diarized so the GUI's "Identifying…" busy state clears no matter
        // how this exits (success, no-result, error, or panic) — otherwise a
        // failed run leaves the button stuck and the user sees nothing happen.
        // It's tagged with `session_id` (not the overloaded Event::Speakers, which
        // also fires on every session load) so navigating away / recording can't
        // clear it, and the labels apply only if that session is still in view.
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
        // If cancelled, don't repaint the view with the result (detach); the
        // panel entry is cleared by JobFinished from the wrapper either way.
        if !cancelled(token) {
            let speakers = Store::open(db_path)
                .ok()
                .and_then(|s| s.speaker_names(session_id).ok())
                .unwrap_or_default();
            let _ = ev.send(Event::Diarized {
                id: session_id.to_string(),
                speakers,
            });
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
    let Some(wav) = zord_config::resolve_track(&prefix, "others") else {
        let _ = ev.send(Event::Notice(
            "The 'Others' audio for this session is missing from disk, so speakers can't be \
             re-identified."
                .to_string(),
        ));
        return;
    };
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
    let samples = match zord_audio::read_audio_mono_16k(others_wav) {
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
            .map(|sp| {
                (
                    sp.speaker,
                    overlap_ms(seg.t_start_ms, seg.t_end_ms, sp.start_ms, sp.end_ms),
                )
            })
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

    #[cfg(feature = "voiceprints")]
    apply_voiceprints(store, session_id, &samples, &spans, model, ev);

    if let Ok(v) = store.segments(session_id) {
        let _ = ev.send(Event::Transcript {
            id: session_id.to_string(),
            segments: v,
        });
    }
    let _ = ev.send(Event::Speakers(
        store.speaker_names(session_id).unwrap_or_default(),
    ));
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

/// Phase 38: persist per-cluster embeddings for this session, and (when the
/// user opted in) match them against the voiceprint library to auto-name
/// speakers. Best-effort — failures notice and return, never blocking the
/// diarization result.
#[cfg(feature = "voiceprints")]
fn apply_voiceprints(
    store: &Store,
    session_id: &str,
    samples: &[f32],
    spans: &[zord_diarize::DiarSegment],
    model: zord_diarize::EmbeddingModel,
    ev: &UnboundedSender<Event>,
) {
    let embedder = match zord_diarize::SpeakerEmbedder::load(model) {
        Ok(e) => e,
        Err(e) => {
            // Defensive: apply_diarization already ran ensure_diar_models and
            // loaded the Diarizer with this same model, so it's effectively
            // guaranteed present here — a failure is worth a loud notice.
            let _ = ev.send(Event::Notice(format!("voiceprints: {e}")));
            return;
        }
    };
    let clusters = embedder.embed_clusters(samples, 16_000, spans);
    for (speaker, emb) in &clusters {
        let _ = store.set_session_speaker_embedding(session_id, *speaker, model.name(), emb);
    }
    let settings = zord_config::Settings::load();
    if !settings.voiceprints_enabled {
        return;
    }
    let cands = store.voiceprint_centroids(model.name()).unwrap_or_default();
    if cands.is_empty() {
        return;
    }
    let threshold = zord_config::voiceprint_threshold(&settings.voiceprints_match);
    let mut recognized: Vec<String> = Vec::new();
    for (speaker, emb) in &clusters {
        if let Some((vid, name, _score)) =
            zord_store::best_voiceprint_match(&cands, emb, threshold, 0.05)
        {
            let _ = store.set_speaker_name(session_id, *speaker, &name);
            let _ = store.link_speaker_voiceprint(session_id, *speaker, vid);
            recognized.push(name);
        }
    }
    if !recognized.is_empty() {
        let _ = ev.send(Event::Notice(format!(
            "Recognized {}.",
            recognized.join(", ")
        )));
    }
}

/// The app data `exports/` directory (created on demand).
fn exports_dir() -> anyhow::Result<PathBuf> {
    let dir = zord_transcribe::model_cache_dir()?
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
        .join("exports");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Compress one kept track in place (Phase 37): WAV → `<stem>.opus` via a
/// `.partial` — verify the decoded length, promote, and only then delete the
/// WAV. Returns the bytes reclaimed.
fn compress_track(wav: &Path, bitrate: i32) -> anyhow::Result<u64> {
    let opus = wav.with_extension("opus");
    let partial = wav.with_extension("opus.partial");
    let _ = std::fs::remove_file(&partial); // stale from a crash
    zord_audio::compress_wav_to_opus(wav, &partial, bitrate)?;
    // Verify before deleting anything: decoded length within 1% (+1 frame).
    let (wav_samples, wav_rate) = zord_audio::wav_duration(wav)?;
    let expect_48k = wav_samples * 48_000 / wav_rate.max(1) as u64;
    let got = zord_audio::OpusBlocks::open(&partial)?
        .total_samples()
        .ok_or_else(|| anyhow::anyhow!("compressed file carries no length"))?;
    let tolerance = expect_48k / 100 + 960;
    anyhow::ensure!(
        got.abs_diff(expect_48k) <= tolerance,
        "verification failed: {got} vs {expect_48k} samples"
    );
    let wav_bytes = std::fs::metadata(wav)?.len();
    std::fs::rename(&partial, &opus)?;
    let opus_bytes = std::fs::metadata(&opus)?.len();
    std::fs::remove_file(wav)?;
    Ok(wav_bytes.saturating_sub(opus_bytes))
}

/// The compression sweep (Phase 37): every **ended** session with kept WAV
/// tracks, old enough per `compress_after_days` (all of them when
/// `ignore_age`), gets each track compressed via [`compress_track`]. Returns
/// `(sessions touched, bytes reclaimed)`. Cancellable between tracks.
fn compress_sweep(
    db_path: &PathBuf,
    ignore_age: bool,
    token: &Arc<AtomicBool>,
) -> anyhow::Result<(usize, u64)> {
    let settings = zord_config::Settings::load();
    if settings.compress_after_days.is_none() && !ignore_age {
        return Ok((0, 0)); // compression turned off
    }
    let bitrate = zord_audio::opus_bitrate(&settings.compress_quality);
    let min_age_ms = settings.compress_after_days.unwrap_or(0) as u64 * 86_400_000;
    let now = now_ms();
    let store = Store::open(db_path)?;
    let (mut touched, mut reclaimed) = (0usize, 0u64);
    for s in store.list_sessions()? {
        if cancelled(token) {
            break;
        }
        let Some(prefix) = s.audio_path else { continue };
        if s.ended_at.is_none() {
            continue; // never the live session
        }
        if !ignore_age && now.saturating_sub(s.started_at) < min_age_ms {
            continue;
        }
        let dir = PathBuf::from(&prefix);
        let mut wavs: Vec<PathBuf> = if dir.is_dir() {
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut found = Vec::new();
            for e in rd.flatten() {
                let p = e.path();
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name.ends_with(".partial") {
                    let _ = std::fs::remove_file(&p); // orphan from a crash
                } else if p.extension().is_some_and(|x| x == "wav") {
                    found.push(p);
                }
            }
            found
        } else {
            // Legacy flat layout: <prefix>.<role>.wav
            ["me", "others"]
                .iter()
                .map(|r| PathBuf::from(format!("{prefix}.{r}.wav")))
                .filter(|p| p.is_file())
                .collect()
        };
        wavs.sort();
        let mut any = false;
        for wav in wavs {
            if cancelled(token) {
                return Ok((touched, reclaimed));
            }
            match compress_track(&wav, bitrate) {
                Ok(b) => {
                    reclaimed += b;
                    any = true;
                }
                Err(e) => tracing::warn!("compress {}: {e}", wav.display()),
            }
        }
        if any {
            touched += 1;
        }
    }
    Ok((touched, reclaimed))
}

/// Render a session and write it to the app data `exports/` directory.
fn export_session(store: &Store, id: &str, format: zord_export::Format) -> anyhow::Result<String> {
    let session = store
        .get_session(id)?
        .ok_or_else(|| anyhow::anyhow!("no such session"))?;
    let segments = store.segments(id)?;
    let names = store.speaker_names(id).unwrap_or_default();
    let rendered = zord_export::render(&session, &segments, &names, format);

    let path = exports_dir()?.join(format!("{id}.{}", format.extension()));
    std::fs::write(&path, rendered)?;
    Ok(path.display().to_string())
}

/// Mix every retained track of a session into `exports/<id>.merged.wav`
/// (Phase 30e). Tracks are session-aligned by construction, so the mix is a
/// plain sample-wise sum (see [`zord_audio::mix_tracks`]). Works for both the
/// folder layout (me/others/spk-N) and the legacy flat prefix.
fn export_merged_audio(db_path: &PathBuf, id: &str) -> anyhow::Result<String> {
    let store = Store::open(db_path)?;
    let prefix = store
        .get_session(id)?
        .and_then(|s| s.audio_path)
        .ok_or_else(|| anyhow::anyhow!("this session kept no audio"))?;

    let dir = PathBuf::from(&prefix);
    let mut paths: Vec<PathBuf> = if dir.is_dir() {
        std::fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "wav" || x == "opus"))
            .collect()
    } else {
        ["me", "others"]
            .iter()
            .filter_map(|role| zord_config::resolve_track(&prefix, role))
            .collect()
    };
    paths.sort(); // me.wav, spk-0.wav, … — deterministic mix order
    anyhow::ensure!(!paths.is_empty(), "this session kept no audio");

    let out = exports_dir()?.join(format!("{id}.merged.wav"));
    zord_audio::mix_tracks(&paths, &out)?;
    Ok(out.display().to_string())
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

fn control_loop(
    rx: mpsc::Receiver<RecorderCmd>,
    ev: UnboundedSender<Event>,
    db_path: PathBuf,
    summ_tx: mpsc::Sender<SummCmd>,
) {
    // Active microphone *test* (setup wizard) between sessions: the capture
    // source plus the stop flag of its level-pump thread. Dropped (capture
    // stops) on MicTestStop, a new MicTestStart, or a real recording Start.
    let mut mic_test: Option<(zord_capture::Microphone, Arc<AtomicBool>)> = None;
    let stop_mic_test = |slot: &mut Option<(zord_capture::Microphone, Arc<AtomicBool>)>| {
        if let Some((mic, stop)) = slot.take() {
            stop.store(true, Ordering::Relaxed);
            drop(mic); // closes the stream → the pump thread's recv ends
        }
    };
    while let Ok(cmd) = rx.recv() {
        match cmd {
            RecorderCmd::MicTestStart { device } => {
                stop_mic_test(&mut mic_test);
                let (tx, rx_frames) = mpsc::channel::<Vec<f32>>();
                match Microphone::start_with(tx, device.as_deref()) {
                    Ok(mic) => {
                        let rate = mic.sample_rate();
                        let stop = Arc::new(AtomicBool::new(false));
                        let pump_stop = stop.clone();
                        let pump_ev = ev.clone();
                        thread::spawn(move || {
                            let mut level = 0.0f32;
                            let mut last_send = std::time::Instant::now();
                            while let Ok(frame) = rx_frames.recv() {
                                if pump_stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                let n = frame.len().max(1);
                                let rms =
                                    (frame.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();
                                level = smooth_level(rms, n, rate, level);
                                if last_send.elapsed().as_millis() >= 33 {
                                    let _ = pump_ev.send(Event::Level {
                                        source: Source::Me,
                                        level,
                                    });
                                    last_send = std::time::Instant::now();
                                }
                            }
                            // Leave the meter at rest when the test ends.
                            let _ = pump_ev.send(Event::Level {
                                source: Source::Me,
                                level: 0.0,
                            });
                        });
                        mic_test = Some((mic, stop));
                    }
                    Err(e) => {
                        let hint = if cfg!(target_os = "macos") {
                            " (check Microphone permission in System Settings → Privacy & Security)"
                        } else {
                            " (check the OS microphone privacy settings and that a mic is connected)"
                        };
                        let _ = ev.send(Event::Notice(format!("microphone test: {e}{hint}")));
                    }
                }
            }
            RecorderCmd::MicTestStop => stop_mic_test(&mut mic_test),
            RecorderCmd::Start {
                model,
                keep_audio,
                input_device,
                audio_dir,
                record_mic,
                record_system,
                live,
                integration,
            } => {
                // A real recording owns the mic — end any wizard test first.
                stop_mic_test(&mut mic_test);
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
                // Integration session when the Record Discord button asked for
                // one, or a dev trigger forces it — `ZORD_DISCORD` (real
                // provider) / `ZORD_FAKE_INTEGRATION` (fake). The button only
                // renders in discord builds, so the old "discord mode in a
                // featureless build" guard went away with the capture mode.
                let integration = integration
                    || std::env::var("ZORD_DISCORD").is_ok()
                    || std::env::var("ZORD_FAKE_INTEGRATION").is_ok();
                let ended = if integration {
                    run_integration_session(opts, &rx, &ev, &db_path, &summ_tx)
                } else {
                    run_session(opts, &rx, &ev, &db_path, &summ_tx)
                };
                if ended {
                    break; // session ended due to Shutdown
                }
            }
            RecorderCmd::Shutdown => break,
            RecorderCmd::Stop => {}              // nothing recording
            RecorderCmd::SetMicMuted(_) => {}    // nothing recording
            RecorderCmd::SetSystemMuted(_) => {} // nothing recording
        }
    }
}

struct Job {
    source: Source,
    /// Pre-assigned speaker index for this chunk (integration sessions, where
    /// identity is ground truth). `None` for mic/desktop — diarization (or the
    /// live labeler) decides the speaker for those.
    speaker: Option<i32>,
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
            // Mic tests are a between-sessions (wizard) affair.
            Ok(RecorderCmd::MicTestStart { .. } | RecorderCmd::MicTestStop) => {}
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
    summ_tx: &mpsc::Sender<SummCmd>,
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

    let settings = zord_config::Settings::load();
    // `diarize_auto` runs diarization at stop (needs the Others WAV, written as
    // a temp file even when audio isn't kept).
    let diarize_auto = cfg!(feature = "diarization") && record_system && settings.diarize_auto;
    // We persist audio (so replay / re-transcribe / re-diarize can find it) when
    // the user keeps audio or recorded capture-only (the WAVs ARE the pending
    // transcript — Phase 25).
    let persist_audio = keep_audio || !live;

    // Phase 28: per-session audio **folder**, named with the start date-time
    // (`audio/2026-06-09_18-15-47/`), holding `me.wav` / `others.wav` (and later
    // `spk-N.wav`). Created only when we'll write audio (persisted, or a temp
    // Others track for the diarize-auto pass); otherwise an uncreated placeholder.
    // The stored `audio_path` is this folder; readers resolve tracks within it
    // (with back-compat for the old flat `<prefix>.<role>.wav` layout).
    let writes_audio = persist_audio || diarize_auto;
    let session_dir = if writes_audio {
        settings
            .session_audio_dir(started_at)
            .unwrap_or_else(|_| audio_dir.join(&session_id))
    } else {
        audio_dir.join(&session_id)
    };

    let _ = store.create_session(&Session {
        id: session_id.clone(),
        started_at,
        ended_at: None,
        title: None,
        audio_path: if persist_audio {
            Some(session_dir.display().to_string())
        } else {
            None
        },
        model: model.name().to_string(),
        overview_folded_ms: None,
    });
    // Tell the GUI which session is live so it can attach notes during capture.
    let _ = ev.send(Event::SessionStarted(session_id.clone()));
    // Fresh session: no "me" speaker tagged yet (integration on_join sets it).
    let _ = ev.send(Event::MeSpeaker(None));
    let wav_path = |src: &str| -> Option<PathBuf> {
        // Capture-only always writes — the WAV is the transcription input.
        if keep_audio || !live {
            let _ = std::fs::create_dir_all(&session_dir);
            Some(zord_config::track_path(&session_dir, src))
        } else {
            None
        }
    };

    // Write the Others WAV if anything needs it: kept audio, the auto pass,
    // retention for later re-diarization, or a capture-only recording.
    let others_wav: Option<PathBuf> = if record_system && (keep_audio || diarize_auto || !live) {
        let _ = std::fs::create_dir_all(&session_dir);
        Some(zord_config::track_path(&session_dir, "others"))
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
                let mic_level = zord_audio::LevelControl::new(zord_audio::LevelMode::parse(
                    &settings.mic_level_mode,
                    settings.mic_gain_db,
                ));
                procs.push(spawn_proc(
                    mic_rx,
                    m.sample_rate(),
                    Source::Me,
                    None,
                    session_start,
                    job_tx.clone(),
                    ev.clone(),
                    wav_path("me"),
                    Some(mic_muted.clone()),
                    mic_level,
                    stopping.clone(),
                ));
                Some(m)
            }
            Err(e) => {
                let hint = if cfg!(target_os = "macos") {
                    " (check Microphone permission in System Settings → Privacy & Security)"
                } else {
                    " (check the OS microphone privacy settings and that a mic is connected)"
                };
                let _ = ev.send(Event::Status(Status::Error(format!(
                    "microphone: {e}{hint}"
                ))));
                return false;
            }
        }
    } else {
        None
    };

    // System audio ("Others") — optional; only if the capture mode includes it.
    // Capture mode "app" (Phase 31) scopes it to one chosen application.
    let system = if record_system {
        let (sys_tx, sys_rx) = mpsc::channel::<Vec<f32>>();
        let app_target = (settings.capture_mode == "app" && !settings.capture_app_id.is_empty())
            .then(|| settings.capture_app_id.clone());
        if settings.capture_mode == "app" && app_target.is_none() {
            let _ = ev.send(Event::Notice(
                "no app selected — capturing the whole system mix (pick one in Settings → Recording)".into(),
            ));
        }
        let started = match app_target.as_deref() {
            Some(app) => SystemAudio::start_app(sys_tx, app),
            None => SystemAudio::start(sys_tx),
        };
        match started {
            Ok(s) => {
                let sys_level = zord_audio::LevelControl::new(zord_audio::LevelMode::parse(
                    &settings.others_level_mode,
                    settings.others_gain_db,
                ));
                procs.push(spawn_proc(
                    sys_rx,
                    s.sample_rate(),
                    Source::Others,
                    None,
                    session_start,
                    job_tx.clone(),
                    ev.clone(),
                    others_wav.clone(),
                    Some(sys_muted.clone()),
                    sys_level,
                    stopping.clone(),
                ));
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
                            // Ground-truth speaker (integration) wins; else the
                            // live diarization label, if any.
                            if let Some(spk) = job.speaker {
                                seg.speaker = Some(spk);
                            } else if seg.speaker.is_none() {
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
    let mut crashed = false;
    for p in procs {
        crashed |= p.join().is_err();
    }
    if let Some(t) = transcribe {
        crashed |= t.join().is_err();
    }
    if crashed {
        let _ = ev.send(Event::Notice(
            "a recording worker crashed during this session — parts of the audio or transcript may be missing (details in logs/crash.log)".into(),
        ));
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
        post_transcribe_session(&store, &session_id, &session_dir, ev, None);
    } else if !live {
        let _ = ev.send(Event::Notice(
            "Recording saved — transcription deferred. Open the session and press \
             Re-transcribe (or turn on 'Transcribe automatically after recording' \
             in Settings)."
                .to_string(),
        ));
    }

    // Auto overview chain (Phase 39): when the transcript is final, enqueue
    // compression (if not already done) then a document fold for this session.
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

    // Auto compress→fold into the living Overview AFTER diarization, so the
    // condensed transcript carries named speakers, not bare "Others" (the
    // diarize pass above runs synchronously on this thread). Only when
    // overview_auto is on and a backend is configured — the summ worker is a
    // single thread so the ordering compress→fold is guaranteed.
    if (post_pass || live) && settings.overview_auto && llm_backend_configured(&settings) {
        let already_compressed = store
            .get_compressed(&session_id)
            .ok()
            .flatten()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
        if !already_compressed {
            let _ = summ_tx.send(SummCmd::Compress(session_id.clone()));
        }
        let _ = summ_tx.send(SummCmd::UpdateOverviewDoc {
            session: Some(session_id.clone()),
        });
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
            let _ = std::fs::remove_file(zord_config::track_path(&session_dir, suffix));
        }
        let _ = store.set_audio_path(&session_id, None);
        emit_sessions(&store, ev); // 🎧 badge off
    }

    tracing::info!("control: session idle");
    shutdown
}

/// Run an **integration** recording session (Phase 29b). Instead of the system
/// loopback, an [`zord_integrations::Integration`] (here the built-in
/// `FakeProvider`) supplies one identity-labeled audio stream per participant;
/// The **followed user's own stream → the "Me" track** (captured through the
/// platform, so Discord's noise suppression etc. apply — no local mic), and every
/// other participant → an `Others` track (`spk-N.wav`, wall-clock aligned). The
/// session ends on the provider's `Ended` or a user Stop. No diarization runs —
/// speakers are known. Triggered for now by the `ZORD_FAKE_INTEGRATION` env var;
/// the real Discord provider + Settings UI land in Phase 30.
fn run_integration_session(
    opts: SessionOpts,
    rx: &mpsc::Receiver<RecorderCmd>,
    ev: &UnboundedSender<Event>,
    db_path: &PathBuf,
    summ_tx: &mpsc::Sender<SummCmd>,
) -> bool {
    // No mic/desktop capture in integration mode — all audio (Me included) comes
    // from the platform's per-participant streams.
    let SessionOpts { model, live, .. } = opts;

    let model_path = if live {
        let _ = ev.send(Event::Status(Status::PreparingModel));
        let ev2 = ev.clone();
        match ensure_model(model, &mut |done, total| {
            if let Some(total) = total {
                let pct = (done as f64 / total as f64 * 100.0) as u8;
                let _ = ev2.send(Event::Status(Status::Downloading(pct)));
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
    let settings = zord_config::Settings::load();
    // Resolve the backend up front: a session that can never connect (missing
    // bot token / user id) must fail here, visibly — not record empty audio.
    let provider = match build_integration_provider(
        settings.discord_bot_token.clone(),
        settings.discord_user_id.trim().parse::<u64>().ok(),
        settings.discord_announce,
        std::env::var("ZORD_FAKE_INTEGRATION").is_ok(),
    ) {
        Ok(p) => p,
        Err(msg) => {
            let _ = ev.send(Event::Status(Status::Error(msg)));
            return false;
        }
    };
    // Integration sessions always persist per-speaker tracks (their WAVs are the
    // transcription input + feed future re-transcription).
    let session_dir = settings
        .session_audio_dir(started_at)
        .unwrap_or_else(|_| settings.audio_dir().unwrap_or_default().join(&session_id));
    let _ = std::fs::create_dir_all(&session_dir);
    let _ = store.create_session(&Session {
        id: session_id.clone(),
        started_at,
        ended_at: None,
        title: None,
        audio_path: Some(session_dir.display().to_string()),
        model: model.name().to_string(),
        overview_folded_ms: None,
    });
    let _ = ev.send(Event::SessionStarted(session_id.clone()));
    // Fresh session: no "me" speaker tagged yet (integration on_join sets it).
    let _ = ev.send(Event::MeSpeaker(None));

    let session_start = Instant::now();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let stopping = Arc::new(AtomicBool::new(false));
    let procs: Arc<std::sync::Mutex<Vec<thread::JoinHandle<()>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));

    let _ = ev.send(Event::Status(Status::Recording));

    // Transcription + storage thread (same shape as run_session); ground-truth
    // `job.speaker` lands on each segment.
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
                Err(_) => return,
            };
            while let Ok(job) = job_rx.recv() {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                match transcriber.transcribe(&job.vad.samples, job.source, job.vad.t_start_ms) {
                    Ok(segs) => {
                        for mut seg in segs {
                            if let Some(spk) = job.speaker {
                                seg.speaker = Some(spk);
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

    // Drive the integration on its own thread: each participant → a per-speaker
    // proc (Others + ground-truth index → spk-N.wav); names → speaker_names.
    let ended = Arc::new(AtomicBool::new(false));
    let it_job_tx = job_tx.clone();
    let integration_thread = {
        let (stopping, ended, ev) = (stopping.clone(), ended.clone(), ev.clone());
        let (session_id, db_path, session_dir, procs) = (
            session_id.clone(),
            db_path.clone(),
            session_dir.clone(),
            procs.clone(),
        );
        let (others_mode, others_gain) =
            (settings.others_level_mode.clone(), settings.others_gain_db);
        let mut provider = provider;
        thread::spawn(move || {
            let store = Store::open(&db_path).ok();
            let announce = |idx: i32, name: &str| {
                if let Some(s) = store.as_ref() {
                    let _ = s.set_speaker_name(&session_id, idx, name);
                    let _ = ev.send(Event::Speakers(
                        s.speaker_names(&session_id).unwrap_or_default(),
                    ));
                }
            };
            let on_join = |idx: i32,
                           name: String,
                           is_me: bool,
                           sample_rate: u32,
                           audio: zord_integrations::AudioStream| {
                // Unified tracks: every participant — the app user included —
                // records as spk-N with their platform name. "Me" is a tag
                // (from the configured user ID), not a separate channel.
                announce(idx, &name);
                if is_me {
                    if let Some(s) = store.as_ref() {
                        let _ = s.set_me_speaker(&session_id, idx);
                    }
                    let _ = ev.send(Event::MeSpeaker(Some(idx)));
                }
                let level = zord_audio::LevelControl::new(zord_audio::LevelMode::parse(
                    &others_mode,
                    others_gain,
                ));
                let wav = Some(zord_config::track_path(&session_dir, &format!("spk-{idx}")));
                let h = spawn_proc(
                    audio,
                    sample_rate,
                    Source::Others,
                    Some(idx),
                    session_start,
                    it_job_tx.clone(),
                    ev.clone(),
                    wav,
                    None,
                    level,
                    stopping.clone(),
                );
                if let Ok(mut p) = procs.lock() {
                    p.push(h);
                }
            };
            let on_rename = |idx: i32, name: String| announce(idx, &name);
            match zord_integrations::drive_session(provider.as_mut(), &stopping, on_join, on_rename)
            {
                Ok(reason) => {
                    tracing::info!("integration session ended: {reason:?}");
                    // Provider-flagged errors (join refused, bad token, gateway
                    // failure) reach the notice banner — a session that never
                    // captured audio must say why, not end silently (the
                    // "bot never joined" confusion). Benign ends (the user
                    // left voice, normal disconnect) stay log-only.
                    match reason {
                        zord_integrations::EndReason::Provider {
                            reason,
                            error: true,
                        } => {
                            let _ = ev.send(Event::Notice(format!("Discord: {reason}")));
                        }
                        zord_integrations::EndReason::Disconnected => {
                            let _ = ev.send(Event::Notice(
                                "Discord: the session ended unexpectedly — check Settings → Integrations and try again.".to_string(),
                            ));
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!("integration error: {e}")));
                }
            }
            ended.store(true, Ordering::Relaxed);
        })
    };
    drop(job_tx); // only the procs + integration thread hold senders now

    // Wait for a user Stop/Shutdown *or* the provider ending the session.
    let mut shutdown = false;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(RecorderCmd::Stop) => break,
            Ok(RecorderCmd::Shutdown) => {
                shutdown = true;
                break;
            }
            Ok(_) => {} // mute toggles are N/A in integration mode
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if ended.load(Ordering::Relaxed) {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    stopping.store(true, Ordering::Relaxed);
    let mut crashed = integration_thread.join().is_err();
    if let Ok(mut p) = procs.lock() {
        for h in p.drain(..) {
            crashed |= h.join().is_err();
        }
    }
    if let Some(t) = transcribe {
        crashed |= t.join().is_err();
    }
    if crashed {
        let _ = ev.send(Event::Notice(
            "a recording worker crashed during this session — parts of the audio or transcript may be missing (details in logs/crash.log)".into(),
        ));
    }
    let _ = store.end_session(&session_id, now_ms());
    // Integration sessions carry ground-truth speakers → no diarization pass.
    let _ = ev.send(Event::Status(Status::Idle));

    // Post-stop transcription pass (Phase 25 parity — was missing for
    // integration sessions, so a Discord recording with live transcription
    // off produced no transcript at all until a manual Re-transcribe): with
    // live off this is where the transcript comes from; with live on it
    // upgrades the live transcript with the re-transcription model. The
    // per-speaker spk-N tracks keep their ground-truth indices, so the real
    // names recorded in speaker_names re-attach to the new segments.
    let post_pass = settings.auto_transcribe;
    if post_pass {
        post_transcribe_session(&store, &session_id, &session_dir, ev, None);
    } else if !live {
        let _ = ev.send(Event::Notice(
            "Recording saved — transcription deferred. Open the session and press \
             Re-transcribe (or turn on 'Transcribe automatically after recording' \
             in Settings)."
                .to_string(),
        ));
    }

    // Auto overview chain (Phase 39): same as run_session — enqueue compress
    // then fold when the transcript is final.
    if (post_pass || live) && settings.overview_auto && llm_backend_configured(&settings) {
        let already_compressed = store
            .get_compressed(&session_id)
            .ok()
            .flatten()
            .map(|c| !c.trim().is_empty())
            .unwrap_or(false);
        if !already_compressed {
            let _ = summ_tx.send(SummCmd::Compress(session_id.clone()));
        }
        let _ = summ_tx.send(SummCmd::UpdateOverviewDoc {
            session: Some(session_id.clone()),
        });
    }

    #[cfg(feature = "voiceprints")]
    enroll_integration_tracks(&store, &session_id, &session_dir, ev);

    let _ = ev.send(Event::Speakers(
        store.speaker_names(&session_id).unwrap_or_default(),
    ));
    emit_sessions(&store, ev);
    tracing::info!("control: integration session torn down");
    shutdown
}

/// Pick the integration backend: the real Discord provider when built with the
/// `discord` feature (token + user id required — missing credentials are an
/// error, not a silent fake), or the dependency-free `FakeProvider` when the
/// fake is forced (`ZORD_FAKE_INTEGRATION`) / the feature isn't compiled in
/// (dev-only paths — a featureless build can't reach integration mode from the
/// UI).
fn build_integration_provider(
    _token: String,
    _user: Option<u64>,
    _announce: bool,
    force_fake: bool,
) -> Result<Box<dyn zord_integrations::Integration + Send>, String> {
    if force_fake {
        tracing::info!("integration: using fake provider (forced)");
        return Ok(Box::new(zord_integrations::FakeProvider::default()));
    }
    #[cfg(feature = "discord")]
    {
        // Settings first; fall back to env vars (same as the spike's
        // config.temp) so the real provider is testable from a shell.
        let token = if _token.is_empty() {
            std::env::var("DISCORD_TOKEN").unwrap_or_default()
        } else {
            _token
        };
        let user = _user.or_else(|| {
            std::env::var("DISCORD_USER_ID")
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
        });
        if token.is_empty() {
            return Err("no Discord bot token — paste one in Settings → Integrations".to_string());
        }
        let Some(uid) = user else {
            return Err(
                "no Discord user ID to follow (or it isn't a number) — set it in Settings → Integrations".to_string()
            );
        };
        tracing::info!("integration: using Discord provider (following user {uid})");
        let announce = _announce.then(|| {
            "🔴 Zord is recording this voice channel — audio is captured per participant \
             for a private, local transcript."
                .to_string()
        });
        Ok(Box::new(
            zord_integrations::DiscordProvider::new(token, uid).with_announce(announce),
        ))
    }
    #[cfg(not(feature = "discord"))]
    {
        tracing::info!("integration: using fake provider (no discord feature)");
        Ok(Box::new(zord_integrations::FakeProvider::default()))
    }
}

/// Phase 38: enroll Discord per-participant tracks under their ground-truth
/// names — the cleanest enrollment source there is (no clustering involved).
/// Skips placeholder "Speaker N" names (unmapped-SSRC fallbacks, not identities)
/// and tracks shorter than 3 s of speech. Bails silently when the embedding
/// model hasn't been downloaded — no notice spam.
#[cfg(feature = "voiceprints")]
fn enroll_integration_tracks(
    store: &Store,
    session_id: &str,
    session_dir: &std::path::Path,
    ev: &UnboundedSender<Event>,
) {
    let settings = zord_config::Settings::load();
    if !settings.voiceprints_enabled {
        return;
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let model = zord_diarize::EmbeddingModel::parse_or_default(&settings.diarize_embedding_model);
    let embedder = match zord_diarize::SpeakerEmbedder::load(model) {
        Ok(e) => e,
        Err(_) => return, // model not downloaded — bail silently
    };
    // Enumerate spk-N.wav tracks written by the integration session. `.wav`
    // only — this runs immediately post-stop, before the compression sweep,
    // so `.opus` tracks can't exist yet.
    let spk_indices: Vec<i32> = {
        let Ok(rd) = std::fs::read_dir(session_dir) else {
            return;
        };
        let mut v: Vec<i32> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                name.strip_prefix("spk-")?
                    .strip_suffix(".wav")?
                    .parse()
                    .ok()
            })
            .collect();
        v.sort_unstable();
        v
    };
    let audio_path = session_dir.to_string_lossy();
    let mut enrolled = 0;
    for speaker in spk_indices {
        let Some(name) = names.get(&speaker).filter(|n| !n.starts_with("Speaker ")) else {
            continue;
        };
        let Some(path) = zord_config::resolve_track(&audio_path, &format!("spk-{speaker}")) else {
            continue;
        };
        let Ok(samples) = zord_audio::read_audio_mono_16k(&path) else {
            continue;
        };
        let speech = zord_diarize::gather_speech(&samples, 16_000, 30);
        if speech.len() < 3 * 16_000 {
            continue; // < 3 s of speech — skip
        }
        let Some(emb) = embedder.embed(&speech, 16_000) else {
            continue;
        };
        let _ = store.set_session_speaker_embedding(session_id, speaker, model.name(), &emb);
        if let Ok(vid) = store.enroll_voiceprint(name, model.name(), &emb, Some(session_id)) {
            let _ = store.link_speaker_voiceprint(session_id, speaker, vid);
            enrolled += 1;
        }
    }
    if enrolled > 0 {
        let _ = ev.send(Event::Notice(format!(
            "Saved voiceprints for {enrolled} Discord speaker(s)."
        )));
        let _ = ev.send(Event::Voiceprints(store.voiceprints().unwrap_or_default()));
    }
}

/// On-demand re-transcription of a past session (the 🔁 button / Phase 25):
/// post-transcribe from the kept WAVs, then re-derive speaker labels when the
/// session had them (re-transcribing wipes segments, labels included). Always
/// ends with [`Event::Retranscribed`] so the GUI busy state clears.
fn retranscribe_session_ondemand(
    db_path: &PathBuf,
    session_id: &str,
    ev: &UnboundedSender<Event>,
    token: &Arc<AtomicBool>,
) {
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
    let first_transcription = store
        .segments(session_id)
        .map(|v| v.is_empty())
        .unwrap_or(false);

    let ok = post_transcribe_session(
        &store,
        session_id,
        std::path::Path::new(&prefix),
        ev,
        Some(token),
    );
    // Segments were replaced — any custom speaker labels were on the old rows.
    let _ = ev.send(Event::Speakers(
        store.speaker_names(session_id).unwrap_or_default(),
    ));

    let want_diarize =
        had_speakers || (first_transcription && zord_config::Settings::load().diarize_auto);
    if ok && want_diarize && !cancelled(token) && cfg!(feature = "diarization") {
        let _ = ev.send(Event::Notice(
            "Re-identifying speakers on the new transcript…".to_string(),
        ));
        let pinned = store
            .get_diarize_speakers(session_id)
            .ok()
            .flatten()
            .unwrap_or(0);
        diarize_session_ondemand(db_path, session_id, pinned, ev, token);
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
    token: Option<&Arc<AtomicBool>>,
) -> bool {
    let _ = ev.send(Event::Retranscribing);
    let ok = post_transcribe_inner(store, session_id, audio_prefix, ev, token);
    let _ = ev.send(Event::Retranscribed);
    ok
}

/// [`post_transcribe_session`] body — split out so the bracketing
/// Retranscribing/Retranscribed events cover every early return. `token`, when
/// present, makes it cancellable: on cancel it stops persisting further segments
/// (keep-partial — segments transcribed so far are retained).
fn post_transcribe_inner(
    store: &Store,
    session_id: &str,
    audio_prefix: &std::path::Path,
    ev: &UnboundedSender<Event>,
    token: Option<&Arc<AtomicBool>>,
) -> bool {
    let settings = zord_config::Settings::load();
    let model = ModelId::parse(&settings.retranscribe_model).unwrap_or(ModelId::LargeV3TurboQ5);
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
    // Restart the on-screen transcript from empty, then stream it back in as
    // lines land (the viewer guard in the GUI drops these when this session
    // isn't the one on screen).
    let _ = ev.send(Event::Transcript {
        id: session_id.to_string(),
        segments: Vec::new(),
    });
    let _ = store.set_session_model(session_id, model.name());
    let mut total = 0usize;
    let audio_path = audio_prefix.to_string_lossy();
    // Track list: the fixed me/others pair, plus any per-speaker tracks an
    // integration session wrote (spk-0.wav / spk-0.opus, … — folder layout
    // only). Their ground-truth speaker index is re-applied to each segment,
    // so the existing speaker_names labels survive a re-transcription.
    let mut track_specs: Vec<(String, Source, Option<i32>)> = vec![
        ("me".to_string(), Source::Me, None),
        ("others".to_string(), Source::Others, None),
    ];
    if let Ok(rd) = std::fs::read_dir(audio_prefix) {
        let mut spk: Vec<i32> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let stem = name
                    .strip_prefix("spk-")?
                    .strip_suffix(".wav")
                    .or_else(|| name.strip_prefix("spk-")?.strip_suffix(".opus"))?;
                stem.parse().ok()
            })
            .collect::<std::collections::HashSet<i32>>()
            .into_iter()
            .collect();
        spk.sort_unstable();
        for idx in spk {
            track_specs.push((format!("spk-{idx}"), Source::Others, Some(idx)));
        }
    }
    // Resolve all tracks up-front; skip unresolvable (no file on disk).
    // WorkItem: (suffix, source, speaker, resolved path).
    let resolved: Vec<(String, Source, Option<i32>, PathBuf)> = track_specs
        .into_iter()
        .filter_map(|(suffix, source, speaker)| {
            let wav = zord_config::resolve_track(&audio_path, &suffix)?;
            Some((suffix, source, speaker, wav))
        })
        .collect();

    let workers =
        effective_transcribe_workers(settings.transcribe_workers.clamp(1, 4), resolved.len());

    if workers <= 1 {
        // ── Sequential path (default) ── byte-for-byte identical to Phase 25.
        // Live-refresh throttle: push the growing transcript to the GUI at most
        // ~every 700 ms so a watcher sees lines stream in.
        let mut last_push = std::time::Instant::now();
        for (suffix, source, speaker, wav) in resolved {
            let cancel = || token.map(cancelled).unwrap_or(false);
            let mut on_segment = |mut seg: Segment| {
                // Keep-partial: segments transcribed before the cancel are kept.
                if !cancel() {
                    if speaker.is_some() {
                        seg.speaker = speaker;
                    }
                    let _ = store.insert_segment(session_id, &seg);
                    if last_push.elapsed() >= std::time::Duration::from_millis(700) {
                        last_push = std::time::Instant::now();
                        if let Ok(v) = store.segments(session_id) {
                            let _ = ev.send(Event::Transcript {
                                id: session_id.to_string(),
                                segments: v,
                            });
                        }
                    }
                }
            };
            // `cancel` also stops the decode loop within ~1s — not just
            // persistence — so a cancelled re-transcribe frees the CPU promptly.
            match zord_transcribe::transcribe_wav_file(
                &transcriber,
                source,
                &wav,
                &mut on_segment,
                &cancel,
            ) {
                Ok(n) => total += n,
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!("transcribing {suffix}: {e}")));
                }
            }
            if cancel() {
                break; // don't start the next channel
            }
        }
    } else {
        // ── Parallel path (transcribe_workers > 1) ──
        // Workers pop items from a shared queue and send (speaker, Segment)
        // over an mpsc channel; the main thread (inside the scope) drains the
        // channel, stamps speakers, inserts into the store, and throttles GUI
        // pushes — keeping all store writes on one thread.
        //
        // Cancel semantics: the token is cloned into each worker; when it fires
        // the worker stops popping new items (keep-partial — segments already
        // received by the drain loop are committed).
        use std::collections::VecDeque;
        use std::sync::Mutex;

        // A notice-or-segment message from a worker.
        enum WorkerMsg {
            Segment(Option<i32>, Segment),
            Notice(String),
        }

        let queue = Arc::new(Mutex::new(resolved.into_iter().collect::<VecDeque<(
            String,
            Source,
            Option<i32>,
            PathBuf,
        )>>()));
        let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();

        thread::scope(|s| {
            // Spawn N worker threads; each pops items until the queue is empty
            // or the cancel token fires.
            for _ in 0..workers {
                let queue = Arc::clone(&queue);
                let tx = tx.clone();
                let transcriber = &transcriber;
                s.spawn(move || loop {
                    if token.map(cancelled).unwrap_or(false) {
                        break;
                    }
                    let item = {
                        let mut q = queue.lock().unwrap();
                        q.pop_front()
                    };
                    let Some((suffix, source, speaker, wav)) = item else {
                        break; // queue exhausted
                    };
                    if token.map(cancelled).unwrap_or(false) {
                        break;
                    }
                    let tx2 = tx.clone();
                    let cancel_fn = || token.map(cancelled).unwrap_or(false);
                    let mut on_segment = |mut seg: Segment| {
                        seg.speaker = speaker; // always stamp (None is fine)
                        let _ = tx2.send(WorkerMsg::Segment(speaker, seg));
                    };
                    if let Err(e) = zord_transcribe::transcribe_wav_file(
                        transcriber,
                        source,
                        &wav,
                        &mut on_segment,
                        &cancel_fn,
                    ) {
                        let _ = tx.send(WorkerMsg::Notice(format!("transcribing {suffix}: {e}")));
                    }
                });
            }
            // Drop our tx clone so the channel closes when all workers finish.
            drop(tx);

            // Drain loop: stamp speaker, insert, throttle GUI pushes.
            // Keep-partial: we insert every segment that arrives — workers only
            // stop sending on cancel, so anything already in-flight is committed.
            let mut last_push = std::time::Instant::now();
            for msg in rx {
                match msg {
                    WorkerMsg::Segment(speaker, mut seg) => {
                        if speaker.is_some() {
                            seg.speaker = speaker;
                        }
                        let _ = store.insert_segment(session_id, &seg);
                        total += 1;
                        if last_push.elapsed() >= std::time::Duration::from_millis(700) {
                            last_push = std::time::Instant::now();
                            if let Ok(v) = store.segments(session_id) {
                                let _ = ev.send(Event::Transcript {
                                    id: session_id.to_string(),
                                    segments: v,
                                });
                            }
                        }
                    }
                    WorkerMsg::Notice(msg) => {
                        let _ = ev.send(Event::Notice(msg));
                    }
                }
            }
            // scope end: all worker threads are joined before we continue.
        });
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
        let _ = ev.send(Event::Transcript {
            id: session_id.to_string(),
            segments: v,
        });
    }
    emit_sessions(store, ev);
    let _ = ev.send(Event::Notice(format!(
        "Transcribed {total} segment(s) with {}.",
        model.name()
    )));
    true
}

/// Effective parallel transcription workers: the user cap (clamped 1..=4)
/// bounded by the number of tracks actually present.
///
/// "If you set 4 workers on a standard desktop+mic (2 tracks) only 2 workers
/// spin up — one per track."
fn effective_transcribe_workers(cap: u32, tracks: usize) -> usize {
    (cap as usize).min(tracks).max(1)
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
    let tau = if target > level {
        LEVEL_ATTACK_S
    } else {
        LEVEL_RELEASE_S
    };
    let alpha = 1.0 - (-dt / tau).exp();
    level += (target - level) * alpha;
    level
}

/// Per-channel resample + VAD stage that also emits live level meters.
#[allow(clippy::too_many_arguments)]
fn spawn_proc(
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    source: Source,
    // Ground-truth speaker for every job from this proc (integration per-speaker
    // tracks); `None` for mic/desktop.
    speaker: Option<i32>,
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
        let mut wav = match wav_path {
            Some(p) => match WavWriter::create(&p, sample_rate) {
                Ok(w) => Some(w),
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!(
                        "couldn't create the {} audio file: {e} — recording continues without saved audio",
                        source.as_str()
                    )));
                    None
                }
            },
            None => None,
        };
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
                if let Err(e) = w.write(&out) {
                    // Disk full / IO error: notify once and stop writing rather
                    // than failing silently on every buffer.
                    let _ = ev.send(Event::Notice(format!(
                        "writing the {} audio file failed: {e} — audio saving stopped (transcription continues)",
                        source.as_str()
                    )));
                    wav = None;
                }
            }

            // Models always consume 16 kHz — derived here on the fly, never stored.
            let mono = match resampler.process(&out) {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Timestamps are wall-clock (the input stream is padded to real time;
            // the resampler adds only ~tens of ms of buffering latency).
            for seg in segmenter.push(&mono) {
                let _ = job_tx.send(Job {
                    source,
                    speaker,
                    vad: seg,
                });
            }
        }
        if let Some(seg) = segmenter.flush() {
            let _ = job_tx.send(Job {
                source,
                speaker,
                vad: seg,
            });
        }
        if let Some(w) = wav {
            if let Err(e) = w.finalize() {
                let _ = ev.send(Event::Notice(format!(
                    "finalizing the {} audio file failed: {e} — the saved track may be unreadable",
                    source.as_str()
                )));
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_fts_quotes_and_joins() {
        assert_eq!(sanitize_fts("hello world"), "\"hello\"* \"world\"*");
        // Embedded quotes can't escape the term.
        assert_eq!(sanitize_fts("a\"b"), "\"ab\"*");
        assert_eq!(sanitize_fts("   "), "");
        assert_eq!(sanitize_fts("\" \""), "");
    }

    #[test]
    fn pad_to_wallclock_fills_gap_to_elapsed() {
        let rate = 1_000u32; // 1 sample == 1 ms, keeps the math readable
        let start = Instant::now() - std::time::Duration::from_secs(2);
        let frame = vec![0.5f32; 100];
        let out = pad_to_wallclock(start, 0, frame.clone(), rate);
        // ~2000 ms elapsed → ~1900 samples of leading silence + the frame.
        assert!(out.len() >= 1_900, "padded to {} samples", out.len());
        assert_eq!(&out[out.len() - frame.len()..], &frame[..]);
        assert!(out[..out.len() - frame.len()].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn pad_to_wallclock_ignores_jitter_and_keeps_up_to_date_streams() {
        let rate = 48_000u32;
        let start = Instant::now() - std::time::Duration::from_secs(1);
        let frame = vec![0.5f32; 480];
        // Producer already at (or ahead of) wall-clock: frame passes through.
        let out = pad_to_wallclock(start, 10 * 48_000, frame.clone(), rate);
        assert_eq!(out, frame);
        // Sub-30ms shortfall is jitter, not a gap.
        let out = pad_to_wallclock(start, 48_000 - 480 - 100, frame.clone(), rate);
        assert_eq!(out, frame);
    }

    #[test]
    fn compress_track_swaps_wav_for_opus() {
        let dir = std::env::temp_dir().join(format!("zord-ctrack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("me.wav");
        let mut w = zord_audio::WavWriter::create(&wav, 16_000).unwrap();
        let tone: Vec<f32> = (0..16_000)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 16_000.0).sin() * 0.4)
            .collect();
        w.write(&tone).unwrap();
        w.finalize().unwrap();
        // A stale partial from a "crash" must not break the swap.
        std::fs::write(dir.join("me.opus.partial"), b"garbage").unwrap();

        let reclaimed = compress_track(&wav, 32_000).unwrap();
        assert!(!wav.exists(), "wav must be deleted after verify");
        assert!(dir.join("me.opus").exists());
        assert!(!dir.join("me.opus.partial").exists());
        assert!(reclaimed > 0);
        // The result decodes to ~1 s at 48k.
        let (decoded, rate) = zord_audio::read_audio_mono_f32(&dir.join("me.opus")).unwrap();
        assert_eq!(rate, 48_000);
        assert!((decoded.len() as i64 - 48_000).unsigned_abs() < 1_000);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn smooth_level_tracks_loudness() {
        let rate = 48_000u32;
        // A second of full-scale signal drives the level up from silence…
        let mut level = 0.0f32;
        for _ in 0..10 {
            level = smooth_level(1.0, 4_800, rate, level);
        }
        assert!(level > 0.5, "level after loud second: {level}");
        // …and a second of silence releases it back down.
        let peak = level;
        for _ in 0..10 {
            level = smooth_level(0.0, 4_800, rate, level);
        }
        assert!(level < peak * 0.7, "level after silent second: {level}");
        // Always normalized.
        assert!((0.0..=1.0).contains(&level));
    }

    // -------------------------------------------------------------------------
    // Phase 39 — living-overview fold helpers
    // -------------------------------------------------------------------------

    fn make_session(id: &str, ended_at: Option<u64>) -> zord_core::Session {
        zord_core::Session {
            id: id.to_string(),
            started_at: ended_at.unwrap_or(0).saturating_sub(3_600_000),
            ended_at,
            title: None,
            audio_path: None,
            model: "test".to_string(),
            overview_folded_ms: None,
        }
    }

    fn stamped(mut s: zord_core::Session, at_ms: u64) -> zord_core::Session {
        s.overview_folded_ms = Some(at_ms);
        s
    }

    #[test]
    fn unfolded_sessions_empty_when_all_stamped() {
        let sessions = vec![
            stamped(make_session("a", Some(1000)), 5000),
            stamped(make_session("b", Some(2000)), 5000),
        ];
        let result = unfolded_sessions(&sessions);
        assert!(result.is_empty(), "stamped sessions must not be selected");
    }

    #[test]
    fn unfolded_sessions_returns_unstamped_oldest_first() {
        let sessions = vec![
            make_session("newest", Some(5000)),
            stamped(make_session("folded", Some(4000)), 9000),
            make_session("middle", Some(3000)),
            make_session("old", Some(1000)),
        ];
        let result = unfolded_sessions(&sessions);
        assert_eq!(result.len(), 3);
        // oldest-first order by ended_at; the stamped one is excluded even
        // though it sits between unstamped ones (no high-water skipping).
        assert_eq!(result[0].id, "old");
        assert_eq!(result[1].id, "middle");
        assert_eq!(result[2].id, "newest");
    }

    #[test]
    fn unfolded_sessions_skips_live_sessions() {
        // Sessions with ended_at = None are still recording — must not appear.
        let sessions = vec![make_session("live", None), make_session("done", Some(2000))];
        let result = unfolded_sessions(&sessions);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "done");
    }

    #[test]
    fn unfolded_sessions_retries_older_unstamped_after_newer_fold() {
        // The regression the per-session stamp fixes: a newer session folding
        // (auto path) must NOT hide an older session that never folded.
        let sessions = vec![
            stamped(make_session("auto-folded-new", Some(9000)), 9001),
            make_session("missed-old", Some(1000)),
        ];
        let result = unfolded_sessions(&sessions);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "missed-old");
    }

    #[test]
    fn load_overview_doc_returns_empty_when_unset() {
        let dir = std::env::temp_dir().join(format!("zord-ovdoc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        let store = zord_store::Store::open(&db).unwrap();

        let (text, ts) = load_overview_doc(&store);
        assert!(text.is_empty());
        assert_eq!(ts, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_overview_doc_with_snapshot_round_trips() {
        let dir = std::env::temp_dir().join(format!("zord-snap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        let store = zord_store::Store::open(&db).unwrap();

        // First write: no prev (nothing to snapshot yet).
        save_overview_doc_with_snapshot(&store, "v1").unwrap();
        let (text, _) = load_overview_doc(&store);
        assert_eq!(text, "v1");

        // Second write: prev should now hold "v1".
        save_overview_doc_with_snapshot(&store, "v2").unwrap();
        let (text, _) = load_overview_doc(&store);
        assert_eq!(text, "v2");
        let prev = store
            .get_meta(OVERVIEW_DOC_PREV_KEY)
            .unwrap()
            .map(|(v, _)| v);
        assert_eq!(prev.as_deref(), Some("v1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overview_folded_stamp_round_trips() {
        let dir = std::env::temp_dir().join(format!("zord-fstamp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        let _ = std::fs::remove_file(&db);
        let store = zord_store::Store::open(&db).unwrap();

        store.create_session(&make_session("sess-1", None)).unwrap();
        // New session: unstamped through both lookup paths.
        assert_eq!(
            store
                .get_session("sess-1")
                .unwrap()
                .unwrap()
                .overview_folded_ms,
            None
        );
        store.end_session("sess-1", 2000).unwrap();

        store.set_overview_folded("sess-1", 12345).unwrap();
        assert_eq!(
            store
                .get_session("sess-1")
                .unwrap()
                .unwrap()
                .overview_folded_ms,
            Some(12345)
        );
        // The listing the fold selection reads surfaces the stamp too.
        let listed = store.list_sessions().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].overview_folded_ms, Some(12345));
        assert!(unfolded_sessions(&listed).is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overview_session_label_uses_title_else_id() {
        // 2026-06-11 12:00 UTC — mid-day so every timezone agrees on the year.
        let mut s = make_session("sess-99", Some(1_781_179_200_000));
        s.started_at = 1_781_179_200_000;
        let label = overview_session_label(&s);
        assert!(label.ends_with(" — sess-99"), "id fallback: {label}");
        // Date prefix is YYYY-MM-DD (local timezone, so only check the shape).
        let date = label.split(" — ").next().unwrap();
        assert_eq!(date.len(), 10, "date shape: {date}");
        assert!(date.starts_with("202"), "plausible year: {date}");

        s.title = Some("  Standup  ".into());
        let label = overview_session_label(&s);
        assert!(label.ends_with(" — Standup"), "trimmed title: {label}");

        // Whitespace-only title falls back to the id.
        s.title = Some("   ".into());
        let label = overview_session_label(&s);
        assert!(label.ends_with(" — sess-99"), "blank title: {label}");
    }

    #[test]
    fn effective_transcribe_workers_caps_and_bounds() {
        // cap bounds by tracks: 4 workers / 2 tracks → 2 effective.
        assert_eq!(effective_transcribe_workers(4, 2), 2);
        // cap is the binding limit: 2 workers / 5 tracks → 2 effective.
        assert_eq!(effective_transcribe_workers(2, 5), 2);
        // exact match: 3 workers / 3 tracks → 3 effective.
        assert_eq!(effective_transcribe_workers(3, 3), 3);
        // sequential default: 1 worker / any tracks → 1.
        assert_eq!(effective_transcribe_workers(1, 10), 1);
        // zero tracks → floor to 1 (no divide-by-zero, no panic).
        assert_eq!(effective_transcribe_workers(4, 0), 1);
        // cap already clamped to 1 by caller; floor holds.
        assert_eq!(effective_transcribe_workers(1, 0), 1);
    }
}
