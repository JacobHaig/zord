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
    /// Custom names for diarized speakers in the viewed session (index → name).
    Speakers(std::collections::HashMap<i32, String>),
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
    #[cfg(feature = "summaries")]
    for &m in zord_summarize::SummaryModel::ALL {
        models.push(ModelInfo {
            name: m.name().to_string(),
            size: m.size_label().to_string(),
            description: m.label().to_string(),
            downloaded: zord_summarize::summary_model_present(m),
            kind: "summary".to_string(),
            // HuggingFace first, then a non-HF mirror (ModelScope) for blocked nets.
            urls: vec![m.url().to_string(), m.mirror_url().to_string()],
        });
    }
    // User-supplied GGUFs dropped into the models folder (any source — no HF).
    #[cfg(feature = "summaries")]
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
    for &m in zord_diarize::EmbeddingModel::ALL {
        models.push(ModelInfo {
            name: m.name().to_string(),
            size: m.size_label().to_string(),
            description: m.label().to_string(),
            downloaded: zord_diarize::diar_models_present(m),
            kind: "diarization".to_string(),
            urls: m.download_urls(),
        });
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
    },
    Stop,
    /// Mute/unmute the microphone ("Me") mid-recording without stopping. While
    /// muted, mic audio is dropped (recorded as silence) — no transcript, meter
    /// falls to zero.
    SetMicMuted(bool),
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
    EditSegment {
        segment_id: i64,
        text: String,
    },
    /// (Re-)run speaker diarization on a past session's retained "Others" audio.
    Diarize(String),
    /// Rename a diarized speaker (0-based index) within a session.
    RenameSpeaker {
        id: String,
        speaker: i32,
        name: String,
    },
}

/// Model-management commands (download/delete can take minutes, so they run on
/// their own worker thread, separate from recording and DB queries).
pub enum ModelCmd {
    List,
    Download(String),
    Delete(String),
}

/// Handle the GUI keeps to drive the engine. Cheaply clonable.
#[derive(Clone)]
pub struct Engine {
    pub rec_tx: mpsc::Sender<RecorderCmd>,
    pub db_tx: mpsc::Sender<DbCmd>,
    pub model_tx: mpsc::Sender<ModelCmd>,
    /// Send a session id to summarize it (heavy; runs on its own thread).
    pub summ_tx: mpsc::Sender<String>,
}

impl Engine {
    /// Spawn the control + db + model worker threads. Returns the handle and
    /// the event stream.
    pub fn spawn(db_path: PathBuf) -> (Engine, UnboundedReceiver<Event>) {
        let (ev_tx, ev_rx) = unbounded_channel::<Event>();
        let (rec_tx, rec_rx) = mpsc::channel::<RecorderCmd>();
        let (db_tx, db_rx) = mpsc::channel::<DbCmd>();
        let (model_tx, model_rx) = mpsc::channel::<ModelCmd>();
        let (summ_tx, summ_rx) = mpsc::channel::<String>();

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
            let ev = ev_tx;
            thread::spawn(move || summarize_loop(summ_rx, ev, db_path));
        }
        (
            Engine {
                rec_tx,
                db_tx,
                model_tx,
                summ_tx,
            },
            ev_rx,
        )
    }
}

/// Worker that generates session summaries (local LLM, heavy). Real impl only
/// in `summaries` builds; otherwise it reports a friendly notice.
fn summarize_loop(rx: mpsc::Receiver<String>, ev: UnboundedSender<Event>, db_path: PathBuf) {
    while let Ok(session_id) = rx.recv() {
        #[cfg(feature = "summaries")]
        summarize_one(&session_id, &ev, &db_path);
        #[cfg(not(feature = "summaries"))]
        {
            let _ = &session_id;
            let _ = &db_path;
            let _ = ev.send(Event::Notice(
                "Summaries aren't built in — rebuild with `--features summaries`.".to_string(),
            ));
        }
    }
}

#[cfg(feature = "summaries")]
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
    let transcript = segs
        .iter()
        .map(|s| format!("{}: {}", s.speaker_label(&names), s.text))
        .collect::<Vec<_>>()
        .join("\n");

    let settings = zord_config::Settings::load();
    let _ = ev.send(Event::Notice("Preparing summary model…".to_string()));
    // Resolve the selected id: a built-in catalog model (download if needed), or
    // a user-supplied custom GGUF already in the models folder.
    let model_path = if let Some(model) = zord_summarize::SummaryModel::parse(&settings.summary_model) {
        match zord_summarize::ensure_summary_model(model, &mut |_d, _t| {}) {
            Ok(p) => p,
            Err(e) => {
                let _ = ev.send(Event::Notice(format!("summary model: {e}")));
                return;
            }
        }
    } else if let Some(p) = zord_summarize::custom_model_path(&settings.summary_model) {
        p
    } else {
        let _ = ev.send(Event::Notice(format!(
            "Summary model '{}' not found — pick one in Settings or drop its .gguf in the models folder.",
            settings.summary_model
        )));
        return;
    };
    let _ = ev.send(Event::Notice("Summarizing…".to_string()));
    let summarizer = match zord_summarize::Summarizer::load(&model_path) {
        Ok(s) => s,
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("summary: {e}")));
            return;
        }
    };
    match summarizer.summarize(&transcript, &settings.effective_summary_prompt()) {
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
                    if let Ok(raw) = summarizer.summarize(&text, zord_config::title_prompt()) {
                        let title = zord_config::clean_title(&raw);
                        if !title.is_empty() {
                            let _ = store.set_session_title(session_id, &title);
                            if let Ok(v) = store.list_sessions() {
                                let _ = ev.send(Event::Sessions(v));
                            }
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
                #[cfg(feature = "summaries")]
                let handled_summary = if let Some(sm) = zord_summarize::SummaryModel::parse(&name) {
                    if let Err(e) = zord_summarize::ensure_summary_model(sm, &mut progress) {
                        tracing::warn!("model download failed for {name}: {e}");
                        let _ = ev.send(Event::DownloadFailed { name: name.clone() });
                    }
                    true
                } else {
                    false
                };
                #[cfg(not(feature = "summaries"))]
                let handled_summary = false;

                #[cfg(feature = "diarization")]
                let handled_diar = if let Some(dm) = zord_diarize::EmbeddingModel::parse(&name) {
                    if let Err(e) = zord_diarize::ensure_diar_models(dm, &mut progress) {
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
                #[cfg(feature = "summaries")]
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
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
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
                if let Ok(v) = store.list_sessions() {
                    let _ = ev.send(Event::Sessions(v));
                }
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
                if let Ok(v) = store.list_sessions() {
                    let _ = ev.send(Event::Sessions(v));
                }
            }
            DbCmd::DeleteSession(id) => {
                let _ = store.delete_session(&id);
                if let Ok(v) = store.list_sessions() {
                    let _ = ev.send(Event::Sessions(v));
                }
            }
            DbCmd::EditSegment { segment_id, text } => {
                let _ = store.update_segment_text(segment_id, &text);
            }
            DbCmd::RenameSpeaker { id, speaker, name } => {
                let _ = store.set_speaker_name(&id, speaker, &name);
                let _ = ev.send(Event::Speakers(store.speaker_names(&id).unwrap_or_default()));
            }
            DbCmd::Diarize(id) => {
                // Heavy (loads ONNX + clusters); run off the db thread so queries
                // stay responsive. The worker opens its own Store.
                let ev = ev.clone();
                let db_path = db_path.clone();
                thread::spawn(move || diarize_session_ondemand(&db_path, &id, &ev));
            }
        }
    }
}

/// On-demand diarization for a past session: locate its retained "Others" WAV
/// from the stored audio prefix, then run the offline pass.
fn diarize_session_ondemand(db_path: &PathBuf, session_id: &str, ev: &UnboundedSender<Event>) {
    #[cfg(not(feature = "diarization"))]
    {
        let _ = (db_path, session_id);
        let _ = ev.send(Event::Notice(
            "Diarization isn't built in — rebuild with `--features diarization`.".to_string(),
        ));
    }
    #[cfg(feature = "diarization")]
    {
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
                "This session didn't retain audio, so speakers can't be re-identified. Turn on \
                 Settings → Speakers → \"Keep audio for re-diarization\" (or Keep audio) before \
                 recording to enable re-running with a different model."
                    .to_string(),
            ));
            return;
        };
        let wav = PathBuf::from(format!("{prefix}.others.wav"));
        if !wav.exists() {
            let _ = ev.send(Event::Notice(
                "The 'Others' audio for this session is missing.".to_string(),
            ));
            return;
        }
        apply_diarization(&store, session_id, &wav, ev);
    }
}

/// Load the "Others" WAV, run the offline diarizer, and write speaker labels
/// onto the session's segments. Emits progress notices + a refreshed transcript.
#[cfg(feature = "diarization")]
fn apply_diarization(
    store: &Store,
    session_id: &str,
    others_wav: &std::path::Path,
    ev: &UnboundedSender<Event>,
) {
    let samples = match zord_audio::read_wav_mono_f32(others_wav) {
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

    if !zord_diarize::diar_models_present(model) {
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
        if let Err(e) = zord_diarize::ensure_diar_models(model, &mut progress) {
            let _ = ev.send(Event::Notice(format!("speaker models: {e}")));
            return;
        }
    }

    let _ = ev.send(Event::Notice("Identifying speakers…".to_string()));
    // Pin the speaker count when the user set one (0 = auto-detect).
    let num_speakers = (settings.diarize_num_speakers > 0).then_some(settings.diarize_num_speakers as i32);
    let diarizer = match zord_diarize::Diarizer::load_with_speakers(model, num_speakers) {
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

    // Map speaker spans onto the stored "Others" segments by max time overlap.
    let segs = store.segments(session_id).unwrap_or_default();
    let _ = store.clear_speakers(session_id);
    let mut speakers = std::collections::HashSet::new();
    for seg in segs.iter().filter(|s| s.source == Source::Others) {
        let Some(id) = seg.id else { continue };
        let best = spans
            .iter()
            .map(|sp| (sp.speaker, overlap_ms(seg.t_start_ms, seg.t_end_ms, sp.start_ms, sp.end_ms)))
            .filter(|(_, ov)| *ov > 0)
            .max_by_key(|(_, ov)| *ov);
        if let Some((speaker, _)) = best {
            let _ = store.set_segment_speaker(id, Some(speaker));
            speakers.insert(speaker);
        }
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
                };
                if run_session(opts, &rx, &ev, &db_path) {
                    break; // session ended due to Shutdown
                }
            }
            RecorderCmd::Shutdown => break,
            RecorderCmd::Stop => {}            // nothing recording
            RecorderCmd::SetMicMuted(_) => {} // nothing recording
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
    } = opts;
    let _ = ev.send(Event::Status(Status::PreparingModel));
    let model_path = {
        let ev = ev.clone();
        match ensure_model(model, &mut |done, total| {
            if let Some(total) = total {
                let pct = (done as f64 / total as f64 * 100.0) as u8;
                let _ = ev.send(Event::Status(Status::Downloading(pct)));
            }
        }) {
            Ok(p) => p,
            Err(e) => {
                let _ = ev.send(Event::Status(Status::Error(format!("model: {e}"))));
                return false;
            }
        }
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

    // Diarization retention. The offline pass needs the full "Others" track:
    // - `diarize_auto`: run diarization at stop (writes a temp Others WAV).
    // - `keep_others_for_diar`: retain that WAV (+ record audio_path) so the user
    //   can re-identify speakers later with a different model, even with
    //   keep_audio off.
    let settings = zord_config::Settings::load();
    let diarize_auto = cfg!(feature = "diarization") && record_system && settings.diarize_auto;
    let keep_others_for_diar =
        cfg!(feature = "diarization") && record_system && settings.diarize_keep_audio;
    // We persist audio (so re-transcribe / re-diarize can find it) when the user
    // keeps audio, or opts to keep the Others track for re-diarization.
    let persist_audio = keep_audio || keep_others_for_diar;

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
        if keep_audio {
            let _ = std::fs::create_dir_all(&audio_dir);
            Some(audio_dir.join(format!("{session_id}.{src}.wav")))
        } else {
            None
        }
    };

    // Write the Others WAV if anything needs it: kept audio, the auto pass, or
    // retaining it for later re-diarization.
    let others_wav: Option<PathBuf> =
        if record_system && (keep_audio || diarize_auto || keep_others_for_diar) {
            let _ = std::fs::create_dir_all(&audio_dir);
            Some(audio_dir.join(format!("{session_id}.others.wav")))
        } else {
            None
        };

    let session_start = Instant::now();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let mut procs = Vec::new();
    // Toggled live by RecorderCmd::SetMicMuted; read by the mic proc thread.
    let mic_muted = Arc::new(AtomicBool::new(false));

    // Microphone ("Me") — only if the capture mode includes it.
    let mic = if record_mic {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>();
        match Microphone::start_with(mic_tx, input_device.as_deref()) {
            Ok(m) => {
                procs.push(spawn_proc(mic_rx, m.sample_rate(), Source::Me, session_start, job_tx.clone(), ev.clone(), wav_path("me"), Some(mic_muted.clone())));
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
                procs.push(spawn_proc(sys_rx, s.sample_rate(), Source::Others, session_start, job_tx.clone(), ev.clone(), others_wav.clone(), None));
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

    // Transcription + storage thread: consumes jobs from both channels.
    let transcribe = {
        let ev = ev.clone();
        let session = session_id.clone();
        let model_path = model_path.clone();
        let db_path = db_path.clone();
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
    };

    // Wait for Stop / Shutdown (also handle live mic mute toggles).
    let mut shutdown = false;
    loop {
        match rx.recv() {
            Ok(RecorderCmd::Stop) | Err(_) => break,
            Ok(RecorderCmd::Shutdown) => {
                shutdown = true;
                break;
            }
            Ok(RecorderCmd::SetMicMuted(m)) => mic_muted.store(m, Ordering::Relaxed),
            Ok(RecorderCmd::Start { .. }) => {} // ignore double-start
        }
    }

    drop(mic);
    drop(system);
    for p in procs {
        let _ = p.join();
    }
    let _ = transcribe.join();
    let _ = store.end_session(&session_id, now_ms());

    // Offline speaker diarization (accurate, source of truth) over the "Others"
    // track, then drop the temp WAV unless we're retaining it (kept audio, or
    // kept-for-re-diarization).
    #[cfg(feature = "diarization")]
    if diarize_auto {
        if let Some(wav) = others_wav.as_ref() {
            apply_diarization(&store, &session_id, wav, ev);
        }
    }
    if !persist_audio {
        if let Some(wav) = others_wav.as_ref() {
            let _ = std::fs::remove_file(wav);
        }
    }

    let _ = ev.send(Event::Status(Status::Idle));
    shutdown
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
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut resampler = match MonoResampler::new(sample_rate, 1) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut segmenter = Segmenter::new(SegmenterConfig::default());
        let mut wav = wav_path.and_then(|p| WavWriter::create(p).ok());
        let mut base_ms: Option<u64> = None;
        // Smoothed loudness state for the level meter (see constants above).
        let mut level = 0.0f32;
        // Opt-in meter diagnostics (set ZORD_METER_DEBUG=1).
        let debug = std::env::var("ZORD_METER_DEBUG").is_ok();
        let (mut dbg_bufs, mut dbg_samps) = (0u64, 0u64);
        let mut dbg_last = session_start.elapsed();

        while let Ok(frame) = rx.recv() {
            // Muted mic: replace this buffer with silence so timing stays aligned
            // (segmenter/WAV keep advancing) but nothing is transcribed and the
            // meter naturally falls to zero.
            let frame = match muted {
                Some(ref m) if m.load(Ordering::Relaxed) => vec![0.0f32; frame.len()],
                _ => frame,
            };
            let base = *base_ms.get_or_insert_with(|| session_start.elapsed().as_millis() as u64);
            // RMS loudness of this buffer, gained, smoothed with time-based
            // exponential attack/release so both channels react at the same
            // real-world speed regardless of their buffer size/cadence.
            let n = frame.len().max(1);
            let rms = (frame.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();
            // RMS -> dBFS -> normalized [0,1] over [FLOOR_DB, 0 dB].
            let db = 20.0 * rms.max(1e-6).log10();
            let target = ((db - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0);
            let dt = n as f32 / sample_rate.max(1) as f32; // seconds this buffer spans (mono)
            let tau = if target > level { LEVEL_ATTACK_S } else { LEVEL_RELEASE_S };
            let alpha = 1.0 - (-dt / tau).exp();
            level += (target - level) * alpha;
            let _ = ev.send(Event::Level { source, level });

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

            let mono = match resampler.process(&frame) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let Some(w) = wav.as_mut() {
                let _ = w.write(&mono);
            }
            for mut seg in segmenter.push(&mono) {
                seg.t_start_ms += base;
                seg.t_end_ms += base;
                let _ = job_tx.send(Job { source, vad: seg });
            }
        }
        if let Some(mut seg) = segmenter.flush() {
            let base = base_ms.unwrap_or(0);
            seg.t_start_ms += base;
            seg.t_end_ms += base;
            let _ = job_tx.send(Job { source, vad: seg });
        }
        if let Some(w) = wav {
            let _ = w.finalize();
        }
    })
}
