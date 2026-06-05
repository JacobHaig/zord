//! Recording pipeline. Two capture sources (mic = "Me", system = "Others") each
//! feed an independent resample+VAD stage; both fan into one transcription stage
//! that tags segments by source and writes them to a single timeline in SQLite.
//!
//!   Microphone ─mono f32─▶ [proc: resample+VAD] ─┐
//!                                                 ├─Job─▶ [transcribe + store]
//!   SystemAudio ─mono f32─▶ [proc: resample+VAD] ─┘
//!
//! Capture handles (cpal `Stream`, `SCStream`) are not `Send`, so they live on
//! this thread; dropping them stops capture and cascades channel closes.

use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use zord_audio::{MonoResampler, Segmenter, SegmenterConfig, WavWriter};
use zord_capture::{AudioSource, Microphone, SystemAudio};
use zord_core::Source;
use zord_store::Store;
use zord_transcribe::{ModelId, Transcriber};

/// One VAD segment plus the channel it came from (timing already session-relative).
struct Job {
    source: Source,
    vad: zord_audio::VadSegment,
}

pub fn run_record(
    model_path: PathBuf,
    model_id: ModelId,
    db_path: PathBuf,
    session_id: &str,
    seconds: u64,
    keep_audio: Option<PathBuf>,
    record_mic: bool,
    record_system: bool,
) -> Result<usize> {
    // If neither was requested, record both.
    let (record_mic, record_system) = default_both_if_none(record_mic, record_system);
    let session_start = Instant::now();
    let (job_tx, job_rx) = mpsc::channel::<Job>();
    let mut procs = Vec::new();

    // --- Microphone ("Me") ---
    let mic = if record_mic {
        let (mic_tx, mic_rx) = mpsc::channel::<Vec<f32>>();
        let mic = Microphone::start(mic_tx)?;
        procs.push(spawn_proc(
            mic_rx,
            mic.sample_rate(),
            Source::Me,
            session_start,
            job_tx.clone(),
            keep_audio.as_deref().map(|p| derive_path(p, "me")),
        ));
        Some(mic)
    } else {
        None
    };

    // --- System audio ("Others"): optional; degrade on failure. ---
    let system = if record_system {
        let (sys_tx, sys_rx) = mpsc::channel::<Vec<f32>>();
        match SystemAudio::start(sys_tx) {
            Ok(s) => {
                procs.push(spawn_proc(
                    sys_rx,
                    s.sample_rate(),
                    Source::Others,
                    session_start,
                    job_tx.clone(),
                    keep_audio.as_deref().map(|p| derive_path(p, "others")),
                ));
                Some(s)
            }
            Err(e) => {
                eprintln!("⚠ system audio unavailable ({e}). Recording microphone only.");
                None
            }
        }
    } else {
        None
    };

    drop(job_tx); // remaining senders are the proc clones; job_rx closes when they finish

    // --- Transcription + storage (shared sink for both channels). ---
    let session = session_id.to_string();
    let transcribe = thread::spawn(move || -> Result<usize> {
        let transcriber = Transcriber::load(model_id, &model_path)?;
        let store = Store::open(&db_path)?;
        let mut count = 0usize;
        while let Ok(job) = job_rx.recv() {
            for seg in transcriber.transcribe(&job.vad.samples, job.source, job.vad.t_start_ms)? {
                store.insert_segment(&session, &seg)?;
                println!("[{} {}] {}", fmt_ts(seg.t_start_ms), seg.source.label(), seg.text);
                count += 1;
            }
        }
        Ok(count)
    });

    // Wait for the stop signal, then stop capture.
    if seconds == 0 {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    } else {
        thread::sleep(Duration::from_secs(seconds));
    }
    drop(mic);
    drop(system);

    for p in procs {
        p.join().expect("proc thread panicked")?;
    }
    let count = transcribe.join().expect("transcribe thread panicked")?;
    Ok(count)
}

/// If neither channel was requested, record both.
fn default_both_if_none(record_mic: bool, record_system: bool) -> (bool, bool) {
    if !record_mic && !record_system {
        (true, true)
    } else {
        (record_mic, record_system)
    }
}

/// Spawn a per-channel resample + VAD stage. Stamps the first-frame arrival
/// (relative to `session_start`) as the channel's base offset so the two
/// channels share one timeline despite starting at slightly different instants.
fn spawn_proc(
    rx: mpsc::Receiver<Vec<f32>>,
    sample_rate: u32,
    source: Source,
    session_start: Instant,
    job_tx: mpsc::Sender<Job>,
    wav_path: Option<PathBuf>,
) -> thread::JoinHandle<Result<()>> {
    thread::spawn(move || -> Result<()> {
        // Sources already emit mono, so channels = 1 (downmix is a no-op).
        let mut resampler = MonoResampler::new(sample_rate, 1)?;
        let mut segmenter = Segmenter::new(SegmenterConfig::default());
        let mut wav = match wav_path {
            Some(p) => Some(WavWriter::create(p)?),
            None => None,
        };
        let mut base_ms: Option<u64> = None;

        while let Ok(frame) = rx.recv() {
            let base = *base_ms.get_or_insert_with(|| session_start.elapsed().as_millis() as u64);
            let mono = resampler.process(&frame)?;
            if let Some(w) = wav.as_mut() {
                w.write(&mono)?;
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
            w.finalize()?;
        }
        Ok(())
    })
}

/// Run a WAV file through the same resample -> VAD -> whisper -> store pipeline
/// (no capture). Deterministic; used to verify the pipeline.
pub fn run_file(
    model_path: PathBuf,
    model_id: ModelId,
    db_path: PathBuf,
    session_id: &str,
    source: Source,
    wav_path: PathBuf,
) -> Result<usize> {
    let transcriber = Transcriber::load(model_id, &model_path)?;
    let store = Store::open(&db_path)?;
    transcribe_wav(&transcriber, &store, session_id, source, &wav_path)
}

/// Re-transcribe a session from its kept per-channel WAVs (`<prefix>.me.wav` /
/// `<prefix>.others.wav`), replacing its existing segments. Used to upgrade an
/// old recording to a better model without re-recording.
pub fn run_retranscribe(
    model_path: PathBuf,
    model_id: ModelId,
    db_path: PathBuf,
    session_id: &str,
    prefix: &Path,
) -> Result<usize> {
    let transcriber = Transcriber::load(model_id, &model_path)?;
    let store = Store::open(&db_path)?;
    store.clear_segments(session_id)?;
    store.set_session_model(session_id, model_id.name())?;

    let mut count = 0usize;
    for (suffix, source) in [("me", Source::Me), ("others", Source::Others)] {
        let path = prefix.with_file_name(format!(
            "{}.{suffix}.wav",
            prefix.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
        ));
        if path.exists() {
            count += transcribe_wav(&transcriber, &store, session_id, source, &path)?;
        }
    }
    if count == 0 {
        anyhow::bail!("no kept audio found for session (expected {prefix:?}.me/others.wav)");
    }
    Ok(count)
}

/// Load a WAV, resample to 16 kHz mono, VAD-segment, transcribe, and insert.
/// Thin wrapper over the shared offline pipeline (zord-transcribe) that also
/// stores + prints each segment.
fn transcribe_wav(
    transcriber: &Transcriber,
    store: &Store,
    session_id: &str,
    source: Source,
    wav_path: &Path,
) -> Result<usize> {
    let mut on_segment = |seg: zord_core::Segment| {
        let _ = store.insert_segment(session_id, &seg);
        println!("[{} {}] {}", fmt_ts(seg.t_start_ms), seg.source.label(), seg.text);
    };
    zord_transcribe::transcribe_wav_file(transcriber, source, wav_path, &mut on_segment)
}

/// `foo.wav` + "me" -> `foo.me.wav`
fn derive_path(p: &Path, tag: &str) -> PathBuf {
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    p.with_file_name(format!("{stem}.{tag}.wav"))
}

fn fmt_ts(ms: u64) -> String {
    let total_s = ms / 1000;
    format!("{:02}:{:02}", total_s / 60, total_s % 60)
}
