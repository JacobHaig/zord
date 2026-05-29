//! Threaded recording engine that backs the GUI.
//!
//! The capture handles (cpal `Stream`, `SCStream`) are `!Send`, so all
//! recording lifecycle lives on one dedicated **control thread**. A second
//! **db thread** answers read-only queries (sessions / search / load) so the UI
//! stays responsive while a recording is in progress. Both push [`Event`]s to
//! the GUI over a `tokio` unbounded channel; the GUI sends [`RecorderCmd`] /
//! [`DbCmd`] over std channels.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use zord_audio::{MonoResampler, Segmenter, SegmenterConfig, WavWriter};
use zord_capture::{AudioSource, Microphone, SystemAudio};
use zord_core::{Segment, Session, Source};
use zord_store::Store;
use zord_transcribe::{ensure_model, ModelId, Transcriber};

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
    /// Live input level (peak amplitude 0..1) for a channel.
    Level { source: Source, peak: f32 },
    /// Result of [`DbCmd::ListSessions`].
    Sessions(Vec<Session>),
    /// Result of [`DbCmd::Search`].
    SearchResults(Vec<(String, Segment)>),
    /// Result of [`DbCmd::Load`] — a session's full transcript.
    Transcript(Vec<Segment>),
    /// A transcript was exported to this path.
    Exported(String),
}

/// Commands controlling recording.
pub enum RecorderCmd {
    Start {
        model: ModelId,
        keep_audio: bool,
        input_device: Option<String>,
        audio_dir: PathBuf,
    },
    Stop,
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
}

/// Handle the GUI keeps to drive the engine. Cheaply clonable.
#[derive(Clone)]
pub struct Engine {
    pub rec_tx: mpsc::Sender<RecorderCmd>,
    pub db_tx: mpsc::Sender<DbCmd>,
}

impl Engine {
    /// Spawn the control + db threads. Returns the handle and the event stream.
    pub fn spawn(db_path: PathBuf) -> (Engine, UnboundedReceiver<Event>) {
        let (ev_tx, ev_rx) = unbounded_channel::<Event>();
        let (rec_tx, rec_rx) = mpsc::channel::<RecorderCmd>();
        let (db_tx, db_rx) = mpsc::channel::<DbCmd>();

        {
            let ev = ev_tx.clone();
            let dbp = db_path.clone();
            thread::spawn(move || control_loop(rec_rx, ev, dbp));
        }
        {
            let ev = ev_tx;
            thread::spawn(move || db_loop(db_rx, ev, db_path));
        }
        (Engine { rec_tx, db_tx }, ev_rx)
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
            }
            DbCmd::Export { id, format } => match export_session(&store, &id, format) {
                Ok(path) => {
                    let _ = ev.send(Event::Exported(path));
                }
                Err(e) => {
                    let _ = ev.send(Event::Notice(format!("export failed: {e}")));
                }
            },
        }
    }
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
    let rendered = zord_export::render(&session, &segments, format);

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
            } => {
                let opts = SessionOpts {
                    model,
                    keep_audio,
                    input_device,
                    audio_dir,
                };
                if run_session(opts, &rx, &ev, &db_path) {
                    break; // session ended due to Shutdown
                }
            }
            RecorderCmd::Shutdown => break,
            RecorderCmd::Stop => {} // nothing recording
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
    let _ = store.create_session(&Session {
        id: session_id.clone(),
        started_at,
        ended_at: None,
        title: None,
        audio_path: if keep_audio {
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

    let session_start = Instant::now();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let mut procs = Vec::new();

    // Microphone (required).
    let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>();
    let mic = match Microphone::start_with(mic_tx, input_device.as_deref()) {
        Ok(m) => m,
        Err(e) => {
            let _ = ev.send(Event::Status(Status::Error(format!("microphone: {e}"))));
            return false;
        }
    };
    procs.push(spawn_proc(mic_rx, mic.sample_rate(), Source::Me, session_start, job_tx.clone(), ev.clone(), wav_path("me")));

    // System audio (optional).
    let (sys_tx, sys_rx) = mpsc::channel::<Vec<f32>>();
    let system = match SystemAudio::start(sys_tx) {
        Ok(s) => Some(s),
        Err(e) => {
            let _ = ev.send(Event::Notice(format!("System audio off: {e}")));
            None
        }
    };
    if let Some(sys) = &system {
        procs.push(spawn_proc(sys_rx, sys.sample_rate(), Source::Others, session_start, job_tx.clone(), ev.clone(), wav_path("others")));
    }
    drop(job_tx);

    let _ = ev.send(Event::Status(Status::Recording));

    // Transcription + storage thread: consumes jobs from both channels.
    let transcribe = {
        let ev = ev.clone();
        let session = session_id.clone();
        let model_path = model_path.clone();
        let db_path = db_path.clone();
        thread::spawn(move || {
            let transcriber = match Transcriber::load(&model_path, model.name()) {
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
            while let Ok(job) = job_rx.recv() {
                match transcriber.transcribe(&job.vad.samples, job.source, job.vad.t_start_ms) {
                    Ok(segs) => {
                        for seg in segs {
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

    // Wait for Stop / Shutdown.
    let mut shutdown = false;
    loop {
        match rx.recv() {
            Ok(RecorderCmd::Stop) | Err(_) => break,
            Ok(RecorderCmd::Shutdown) => {
                shutdown = true;
                break;
            }
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
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut resampler = match MonoResampler::new(sample_rate, 1) {
            Ok(r) => r,
            Err(_) => return,
        };
        let mut segmenter = Segmenter::new(SegmenterConfig::default());
        let mut wav = wav_path.and_then(|p| WavWriter::create(p).ok());
        let mut base_ms: Option<u64> = None;

        while let Ok(frame) = rx.recv() {
            let base = *base_ms.get_or_insert_with(|| session_start.elapsed().as_millis() as u64);
            // Live level: peak amplitude of this buffer.
            let peak = frame.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
            let _ = ev.send(Event::Level { source, peak });

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
