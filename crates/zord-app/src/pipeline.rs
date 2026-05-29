//! Orchestrates the recording pipeline across three stages, each decoupled by
//! a channel so the slow stage (transcription) never stalls audio capture:
//!
//!   cpal callback ──Vec<f32>──▶ [proc thread] ──VadSegment──▶ [transcribe thread]
//!   (audio thread)              resample+VAD                  whisper + SQLite
//!
//! The cpal `Stream` is not `Send` on macOS, so it stays on the calling thread;
//! we stop recording by dropping it, which cascades channel closes downstream.

use crate::capture;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use zord_audio::{MonoResampler, Segmenter, SegmenterConfig, WavWriter};
use zord_core::Source;
use zord_store::Store;
use zord_transcribe::{ModelId, Transcriber};

pub fn run_record(
    model_path: PathBuf,
    model_id: ModelId,
    db_path: PathBuf,
    session_id: &str,
    source: Source,
    seconds: u64,
    keep_audio: Option<PathBuf>,
) -> Result<usize> {
    let (raw_tx, raw_rx) = mpsc::channel::<Vec<f32>>();
    let (seg_tx, seg_rx) = mpsc::channel::<zord_audio::VadSegment>();

    // Stage 1: microphone (stays on this thread; dropping it stops capture).
    let mic = capture::start_mic(raw_tx)?;
    let cfg = mic.config;

    // Stage 2: resample to 16 kHz mono + VAD segmentation (+ optional WAV).
    let proc = thread::spawn(move || -> Result<()> {
        let mut resampler = MonoResampler::new(cfg.sample_rate, cfg.channels)?;
        let mut segmenter = Segmenter::new(SegmenterConfig::default());
        let mut wav = match keep_audio {
            Some(p) => Some(WavWriter::create(p)?),
            None => None,
        };
        while let Ok(buf) = raw_rx.recv() {
            let mono = resampler.process(&buf)?;
            if let Some(w) = wav.as_mut() {
                w.write(&mono)?;
            }
            for seg in segmenter.push(&mono) {
                let _ = seg_tx.send(seg);
            }
        }
        if let Some(seg) = segmenter.flush() {
            let _ = seg_tx.send(seg);
        }
        if let Some(w) = wav {
            w.finalize()?;
        }
        Ok(())
    });

    // Stage 3: transcription + storage (owns its own DB connection — WAL lets
    // it coexist with the main thread's connection).
    let session = session_id.to_string();
    let transcribe = thread::spawn(move || -> Result<usize> {
        let transcriber = Transcriber::load(&model_path, model_id.name())?;
        let store = Store::open(&db_path)?;
        let mut count = 0usize;
        while let Ok(vad) = seg_rx.recv() {
            let segments = transcriber.transcribe(&vad.samples, source, vad.t_start_ms)?;
            for seg in segments {
                store.insert_segment(&session, &seg)?;
                println!("[{} {}] {}", fmt_ts(seg.t_start_ms), seg.source.label(), seg.text);
                count += 1;
            }
        }
        Ok(count)
    });

    // Wait for the stop condition, then drop the stream to end capture.
    if seconds == 0 {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    } else {
        thread::sleep(Duration::from_secs(seconds));
    }
    drop(mic); // closes raw_tx -> proc finishes -> closes seg_tx -> transcribe finishes

    proc.join().expect("proc thread panicked")?;
    let count = transcribe.join().expect("transcribe thread panicked")?;
    Ok(count)
}

/// Run a WAV file through the exact same resample -> VAD -> whisper -> store
/// pipeline as live capture (minus the microphone). Deterministic; used to
/// verify the pipeline end-to-end.
pub fn run_file(
    model_path: PathBuf,
    model_id: ModelId,
    db_path: PathBuf,
    session_id: &str,
    source: Source,
    wav_path: PathBuf,
) -> Result<usize> {
    let mut reader = hound::WavReader::open(&wav_path)?;
    let spec = reader.spec();
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => {
            reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?
        }
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / scale))
                .collect::<Result<Vec<_>, _>>()?
        }
    };
    tracing::info!(
        rate = spec.sample_rate,
        channels = spec.channels,
        samples = interleaved.len(),
        "loaded WAV"
    );

    let mut resampler = MonoResampler::new(spec.sample_rate, spec.channels)?;
    let mono = resampler.process(&interleaved)?;

    let mut segmenter = Segmenter::new(SegmenterConfig::default());
    let mut segments = segmenter.push(&mono);
    if let Some(seg) = segmenter.flush() {
        segments.push(seg);
    }

    let transcriber = Transcriber::load(&model_path, model_id.name())?;
    let store = Store::open(&db_path)?;
    let mut count = 0usize;
    for vad in segments {
        for seg in transcriber.transcribe(&vad.samples, source, vad.t_start_ms)? {
            store.insert_segment(session_id, &seg)?;
            println!("[{} {}] {}", fmt_ts(seg.t_start_ms), seg.source.label(), seg.text);
            count += 1;
        }
    }
    Ok(count)
}

fn fmt_ts(ms: u64) -> String {
    let total_s = ms / 1000;
    format!("{:02}:{:02}", total_s / 60, total_s % 60)
}
