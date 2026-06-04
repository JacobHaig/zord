//! Zord CLI (Phase 1): microphone -> resample -> VAD -> whisper -> SQLite.
//!
//! Later phases add system-audio capture, the "Me/Others" dual channel, the
//! Dioxus desktop UI, and the localhost review dashboard. For now this proves
//! the full local transcription pipeline end-to-end.

mod pipeline;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zord_store::Store;
use zord_transcribe::ModelId;

#[derive(Parser)]
#[command(name = "zord", about = "Local audio capture & transcription")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Record the microphone and transcribe it.
    Record {
        /// Stop after N seconds. 0 = record until you press Enter.
        #[arg(long, default_value_t = 0)]
        seconds: u64,
        /// Whisper model id (large-v3-turbo-q5_0 | large-v3-turbo | small.en).
        #[arg(long, default_value = "large-v3-turbo-q5_0")]
        model: String,
        /// SQLite database path. Defaults to the app data dir.
        #[arg(long)]
        db: Option<PathBuf>,
        /// Optionally retain the captured audio to this WAV path.
        #[arg(long)]
        keep_audio: Option<PathBuf>,
        /// What to capture: both | mic | system. Defaults to the config setting.
        #[arg(long)]
        capture: Option<String>,
    },
    /// Transcribe an existing WAV file (verifies the pipeline without a mic).
    File {
        /// Path to a WAV file (any sample rate / channel count).
        path: PathBuf,
        #[arg(long, default_value = "large-v3-turbo-q5_0")]
        model: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Print a stored session transcript.
    Show {
        session_id: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Full-text search across all transcripts.
    Search {
        query: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Export a session transcript to a file or stdout.
    Export {
        session_id: String,
        /// Output format: md | srt | json.
        #[arg(long, default_value = "md")]
        format: String,
        /// Write to this path. Omit to print to stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Serve the read-only review dashboard on 127.0.0.1.
    Serve {
        #[arg(long, default_value_t = 7777)]
        port: u16,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Re-transcribe a session from its kept audio with a (possibly new) model.
    Retranscribe {
        session_id: String,
        #[arg(long, default_value = "large-v3-turbo")]
        model: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Enable at-rest encryption (set a passphrase, migrate the DB).
    Encrypt {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Remember the passphrase in the OS keychain.
        #[arg(long)]
        remember: bool,
    },
    /// Disable at-rest encryption (decrypt the DB back to plaintext).
    Decrypt {
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Summarize a session with a local LLM (requires `--features summaries`).
    Summarize {
        session_id: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Compress a session into token-minimal dense prose for cross-meeting
    /// synthesis (requires `--features summaries`).
    Compress {
        session_id: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Synthesize a cross-meeting Overview across recent sessions (lazily
    /// compressing any that aren't yet). Requires `--features summaries`.
    Overview {
        /// How many recent meetings to include (default: config setting).
        #[arg(long)]
        max: Option<u32>,
        #[arg(long)]
        db: Option<PathBuf>,
    },
    /// Label individual speakers in a session's "Others" channel (requires
    /// `--features diarization` and retained audio for that session).
    Diarize {
        session_id: String,
        #[arg(long)]
        db: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zord=info,whisper_rs=warn".into()),
        )
        .init();

    // Route whisper.cpp / ggml's chatty native logging through `tracing` so it
    // respects our filter (default: warn) instead of spamming stderr.
    zord_transcribe::install_logging_hooks();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Record {
            seconds,
            model,
            db,
            keep_audio,
            capture,
        } => cmd_record(seconds, &model, db, keep_audio, capture),
        Cmd::File { path, model, db } => cmd_file(path, &model, db),
        Cmd::Show { session_id, db } => cmd_show(&session_id, db),
        Cmd::Search { query, db } => cmd_search(&query, db),
        Cmd::Export {
            session_id,
            format,
            out,
            db,
        } => cmd_export(&session_id, &format, out, db),
        Cmd::Serve { port, db } => cmd_serve(port, db),
        Cmd::Retranscribe {
            session_id,
            model,
            db,
        } => cmd_retranscribe(&session_id, &model, db),
        Cmd::Encrypt { db, remember } => cmd_encrypt(db, remember),
        Cmd::Decrypt { db } => cmd_decrypt(db),
        Cmd::Summarize { session_id, db } => cmd_summarize(&session_id, db),
        Cmd::Compress { session_id, db } => cmd_compress(&session_id, db),
        Cmd::Overview { max, db } => cmd_overview(max, db),
        Cmd::Diarize { session_id, db } => cmd_diarize(&session_id, db),
    }
}

/// Build a plain "Speaker: text" transcript for summarization, using diarized
/// speaker labels + custom names (`names` maps speaker index → name).
#[cfg(feature = "summaries")]
fn transcript_text(
    segments: &[zord_core::Segment],
    names: &std::collections::HashMap<i32, String>,
) -> String {
    segments
        .iter()
        .map(|s| format!("{}: {}", s.speaker_label(names), s.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(feature = "summaries")]
fn cmd_summarize(session_id: &str, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    let segs = store.segments(session_id)?;
    if segs.is_empty() {
        anyhow::bail!("session '{session_id}' has no transcript to summarize");
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = transcript_text(&segs, &names);

    let settings = zord_config::Settings::load();
    // Built-in catalog model (download if needed), or a custom GGUF the user
    // dropped into the models folder (any source — no HuggingFace required).
    let model_path = if let Some(model) =
        zord_summarize::SummaryModel::parse(&settings.summary_model)
    {
        eprintln!("Preparing summary model '{}'…", model.name());
        zord_summarize::ensure_summary_model(model, &mut |done, total| {
            if let Some(total) = total {
                eprint!("\r  downloading: {:.1}%   ", done as f64 / total as f64 * 100.0);
            }
        })?
    } else if let Some(p) = zord_summarize::custom_model_path(&settings.summary_model) {
        eprintln!("Using custom model '{}'…", settings.summary_model);
        p
    } else {
        anyhow::bail!(
            "summary model '{}' not found — set one in the GUI, or drop its .gguf in the models folder",
            settings.summary_model
        );
    };
    eprintln!("\r  model ready. Summarizing…              ");

    let summarizer = zord_summarize::Summarizer::load(&model_path)?;
    let summary = summarizer.summarize(&transcript, &settings.effective_summary_prompt())?;
    store.set_summary(session_id, &summary)?;
    println!("{summary}");

    // Auto-title (best-effort) if enabled and the session isn't already named.
    if settings.auto_title {
        let unnamed = store
            .get_session(session_id)?
            .map(|s| s.title.as_deref().unwrap_or("").trim().is_empty())
            .unwrap_or(false);
        if unnamed {
            if let Ok(raw) = summarizer.summarize(&summary, zord_config::title_prompt()) {
                let title = zord_config::clean_title(&raw);
                if !title.is_empty() {
                    store.set_session_title(session_id, &title)?;
                    eprintln!("Titled: {title}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(feature = "summaries"))]
fn cmd_summarize(_session_id: &str, _db: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no summary support — rebuild with `--features summaries`")
}

#[cfg(feature = "summaries")]
fn cmd_compress(session_id: &str, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    let segs = store.segments(session_id)?;
    if segs.is_empty() {
        anyhow::bail!("session '{session_id}' has no transcript to compress");
    }
    let names = store.speaker_names(session_id).unwrap_or_default();
    let transcript = transcript_text(&segs, &names);

    let settings = zord_config::Settings::load();
    let model_path = if let Some(model) =
        zord_summarize::SummaryModel::parse(&settings.summary_model)
    {
        eprintln!("Preparing summary model '{}'…", model.name());
        zord_summarize::ensure_summary_model(model, &mut |done, total| {
            if let Some(total) = total {
                eprint!("\r  downloading: {:.1}%   ", done as f64 / total as f64 * 100.0);
            }
        })?
    } else if let Some(p) = zord_summarize::custom_model_path(&settings.summary_model) {
        eprintln!("Using custom model '{}'…", settings.summary_model);
        p
    } else {
        anyhow::bail!(
            "summary model '{}' not found — set one in the GUI, or drop its .gguf in the models folder",
            settings.summary_model
        );
    };
    eprintln!("\r  model ready. Compressing (ctx {} tokens)…       ", settings.compress_ctx);

    let summarizer = zord_summarize::Summarizer::load(&model_path)?;
    let compressed =
        summarizer.compress(&transcript, zord_config::compress_prompt(), settings.compress_ctx)?;
    store.set_compressed(session_id, &compressed)?;
    println!("{compressed}");
    Ok(())
}

#[cfg(not(feature = "summaries"))]
fn cmd_compress(_session_id: &str, _db: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no summary support — rebuild with `--features summaries`")
}

#[cfg(feature = "summaries")]
fn cmd_overview(max: Option<u32>, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    let mut settings = zord_config::Settings::load();
    if let Some(m) = max {
        settings.overview_max_meetings = m;
    }
    eprintln!(
        "Synthesizing overview across up to {} recent meeting(s) (ctx {} tokens)…",
        settings.overview_max_meetings, settings.overview_ctx
    );
    let result = zord_overview::synthesize(&db_path, &settings, &mut |note| {
        eprintln!("  {note}");
    })?;
    println!("{}", result.text);
    eprintln!("\nOverview covers {} meeting(s); stored in the database.", result.meetings);
    Ok(())
}

#[cfg(not(feature = "summaries"))]
fn cmd_overview(_max: Option<u32>, _db: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no summary support — rebuild with `--features summaries`")
}

/// Align "Others" transcript segments to diarized speaker spans by max overlap,
/// returning the (segment id, speaker) assignments and the distinct speaker set.
#[cfg(feature = "diarization")]
fn compute_speaker_assignments(
    segs: &[zord_core::Segment],
    spans: &[zord_diarize::DiarSegment],
) -> (Vec<(i64, i32)>, std::collections::HashSet<i32>) {
    let mut assignments: Vec<(i64, i32)> = Vec::new();
    let mut speakers = std::collections::HashSet::new();
    for seg in segs.iter().filter(|s| s.source == zord_core::Source::Others) {
        let Some(id) = seg.id else { continue };
        let best = spans
            .iter()
            .map(|sp| {
                let lo = seg.t_start_ms.max(sp.start_ms);
                let hi = seg.t_end_ms.min(sp.end_ms);
                (sp.speaker, hi.saturating_sub(lo))
            })
            .filter(|(_, ov)| *ov > 0)
            .max_by_key(|(_, ov)| *ov);
        if let Some((speaker, _)) = best {
            assignments.push((id, speaker));
            speakers.insert(speaker);
        }
    }
    (assignments, speakers)
}

#[cfg(feature = "diarization")]
fn cmd_diarize(session_id: &str, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    let session = store
        .get_session(session_id)?
        .with_context(|| format!("no such session '{session_id}'"))?;
    let prefix = session
        .audio_path
        .with_context(|| "this session didn't retain audio, so speakers can't be identified")?;
    let wav = PathBuf::from(format!("{prefix}.others.wav"));
    anyhow::ensure!(wav.exists(), "the 'Others' audio for this session is missing: {wav:?}");

    let samples = zord_audio::read_wav_mono_f32(&wav)?;
    anyhow::ensure!(!samples.is_empty(), "no 'Others' audio to diarize");

    let settings = zord_config::Settings::load();
    let model = zord_diarize::EmbeddingModel::parse_or_default(&settings.diarize_embedding_model);
    eprintln!("Preparing speaker models '{}'…", model.name());
    zord_diarize::ensure_diar_models(model, &mut |done, total| {
        if let Some(total) = total {
            eprint!("\r  downloading: {:.1}%   ", done as f64 / total as f64 * 100.0);
        }
    })?;
    eprintln!("\r  models ready. Identifying speakers…       ");

    let num_speakers =
        (settings.diarize_num_speakers > 0).then_some(settings.diarize_num_speakers as i32);
    let diarizer =
        zord_diarize::Diarizer::load(model, num_speakers, settings.diarize_threshold.clamp(0.1, 0.95))?;
    let spans = diarizer.diarize(&samples)?;
    if spans.is_empty() {
        eprintln!(
            "No distinct speakers detected (audio may be too short, mostly silent, or a single \
             speaker). Existing labels left unchanged. Try a lower --threshold via settings or set \
             the expected speaker count."
        );
        return Ok(());
    }

    // Compute assignments first; only clear + write if we matched something, so a
    // no-result run never wipes existing speaker labels.
    let segs = store.segments(session_id)?;
    let (assignments, speakers) = compute_speaker_assignments(&segs, &spans);
    if assignments.is_empty() {
        eprintln!("Found speech but couldn't align speakers to transcript lines; left unchanged.");
        return Ok(());
    }
    store.clear_speakers(session_id)?;
    for (id, speaker) in assignments {
        store.set_segment_speaker(id, Some(speaker))?;
    }
    println!("Identified {} speaker(s) in session '{session_id}'.", speakers.len());
    Ok(())
}

#[cfg(not(feature = "diarization"))]
fn cmd_diarize(_session_id: &str, _db: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no diarization support — rebuild with `--features diarization`")
}

#[cfg(feature = "encryption")]
fn cmd_encrypt(db: Option<PathBuf>, remember: bool) -> Result<()> {
    let db_path = match db {
        Some(p) => p,
        None => default_db_path()?,
    };
    if zord_store::is_encrypted(&db_path) {
        eprintln!("Database is already encrypted.");
        return Ok(());
    }
    // Non-interactive (scripts/CI): take the passphrase from the environment.
    let p1 = match std::env::var("ZORD_PASSPHRASE") {
        Ok(v) if !v.is_empty() => v,
        _ => {
            let p = rpassword::prompt_password("New passphrase: ")?;
            if p.is_empty() {
                anyhow::bail!("passphrase must not be empty");
            }
            if rpassword::prompt_password("Confirm passphrase: ")? != p {
                anyhow::bail!("passphrases do not match");
            }
            p
        }
    };
    if db_path.exists() {
        zord_store::encrypt_existing(&db_path, &p1)?;
    } else {
        // No DB yet: create a fresh encrypted one.
        zord_store::set_db_key(Some(p1.clone()));
        let _ = Store::open(&db_path)?;
    }
    let mut s = zord_config::Settings::load();
    s.encrypted = true;
    s.save()?;
    if remember {
        zord_config::keychain::store(&p1)?;
        eprintln!("Passphrase remembered in the OS keychain.");
    }
    eprintln!("Encryption enabled (a plaintext backup was kept beside the database).");
    Ok(())
}

#[cfg(feature = "encryption")]
fn cmd_decrypt(db: Option<PathBuf>) -> Result<()> {
    let db_path = match db {
        Some(p) => p,
        None => default_db_path()?,
    };
    if !zord_store::is_encrypted(&db_path) {
        eprintln!("Database is not encrypted.");
        return Ok(());
    }
    let pass = zord_config::keychain::get()
        .or_else(|| std::env::var("ZORD_PASSPHRASE").ok())
        .or_else(|| rpassword::prompt_password("Database passphrase: ").ok())
        .filter(|p| !p.is_empty())
        .context("a passphrase is required")?;
    zord_store::decrypt_existing(&db_path, &pass)?;
    let mut s = zord_config::Settings::load();
    s.encrypted = false;
    s.save()?;
    zord_config::keychain::clear();
    eprintln!("Encryption disabled (an encrypted backup was kept beside the database).");
    Ok(())
}

#[cfg(not(feature = "encryption"))]
fn cmd_encrypt(_db: Option<PathBuf>, _remember: bool) -> Result<()> {
    anyhow::bail!("this build has no encryption support — rebuild with `--features encryption`")
}

#[cfg(not(feature = "encryption"))]
fn cmd_decrypt(_db: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no encryption support — rebuild with `--features encryption`")
}

fn cmd_retranscribe(session_id: &str, model: &str, db: Option<PathBuf>) -> Result<()> {
    let model_id = ModelId::parse(model)
        .with_context(|| format!("unknown model '{model}'"))?;
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    let session = store
        .get_session(session_id)?
        .with_context(|| format!("no such session '{session_id}'"))?;
    let prefix = session
        .audio_path
        .as_ref()
        .map(PathBuf::from)
        .context("this session has no kept audio (record with keep-audio enabled)")?;
    drop(store);

    eprintln!("Preparing model '{}'...", model_id.name());
    let model_path = zord_transcribe::ensure_model(model_id, &mut |done, total| {
        if let Some(total) = total {
            let pct = done as f64 / total as f64 * 100.0;
            eprint!("\r  downloading: {:.1}%   ", pct);
        }
    })?;
    eprintln!("\r  model ready.                    ");

    let count = pipeline::run_retranscribe(model_path, model_id, db_path, session_id, &prefix)?;
    eprintln!("\nRe-transcribed {count} segment(s) with {}.", model_id.name());
    Ok(())
}

fn cmd_export(
    session_id: &str,
    format: &str,
    out: Option<PathBuf>,
    db: Option<PathBuf>,
) -> Result<()> {
    let fmt = zord_export::Format::parse(format)
        .with_context(|| format!("unknown format '{format}' (use md|srt|json)"))?;
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    let session = store
        .get_session(session_id)?
        .with_context(|| format!("no such session '{session_id}'"))?;
    let segments = store.segments(session_id)?;
    let names = store.speaker_names(session_id).unwrap_or_default();
    let rendered = zord_export::render(&session, &segments, &names, fmt);

    match out {
        Some(path) => {
            std::fs::write(&path, rendered)?;
            eprintln!("Wrote {} ({} segments) to {}", fmt.extension(), segments.len(), path.display());
        }
        None => print!("{rendered}"),
    }
    Ok(())
}

fn cmd_serve(port: u16, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    zord_web::serve_blocking(db_path, port)
}

fn cmd_file(path: PathBuf, model: &str, db: Option<PathBuf>) -> Result<()> {
    let model_id = ModelId::parse(model)
        .with_context(|| format!("unknown model '{model}'"))?;
    eprintln!("Preparing model '{}'...", model_id.name());
    let model_path = zord_transcribe::ensure_model(model_id, &mut |done, total| {
        if let Some(total) = total {
            let pct = done as f64 / total as f64 * 100.0;
            eprint!("\r  downloading: {:.1}% ({} MB)   ", pct, done / 1_048_576);
        }
    })?;
    eprintln!("\r  model ready.                                  ");

    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;

    let started_at = now_ms();
    let session_id = format!("file-{started_at}");
    store.create_session(&zord_core::Session {
        id: session_id.clone(),
        started_at,
        ended_at: None,
        title: Some(path.display().to_string()),
        audio_path: Some(path.display().to_string()),
        model: model_id.name().to_string(),
    })?;

    let count = pipeline::run_file(
        model_path,
        model_id,
        db_path.clone(),
        &session_id,
        zord_core::Source::Others,
        path,
    )?;
    store.end_session(&session_id, now_ms())?;
    eprintln!("\n{count} segment(s) transcribed. Session {session_id}");
    Ok(())
}

fn default_db_path() -> Result<PathBuf> {
    zord_config::Settings::load().db_path()
}

/// Resolve the DB path (from `--db` or config) and unlock it if encrypted.
fn resolve_db(db: Option<PathBuf>) -> Result<PathBuf> {
    let path = match db {
        Some(p) => p,
        None => default_db_path()?,
    };
    unlock(&path)?;
    Ok(path)
}

/// If the database is encrypted, obtain the passphrase (keychain → env
/// `ZORD_PASSPHRASE` → hidden prompt) and set the process-wide DB key.
#[cfg(feature = "encryption")]
fn unlock(db_path: &std::path::Path) -> Result<()> {
    let needs = zord_config::Settings::load().encrypted || zord_store::is_encrypted(db_path);
    if !needs {
        return Ok(());
    }
    let pass = zord_config::keychain::get()
        .or_else(|| std::env::var("ZORD_PASSPHRASE").ok())
        .or_else(|| rpassword::prompt_password("Database passphrase: ").ok())
        .filter(|p| !p.is_empty())
        .context("a passphrase is required to open the encrypted database")?;
    zord_store::set_db_key(Some(pass));
    Ok(())
}

#[cfg(not(feature = "encryption"))]
fn unlock(_db_path: &std::path::Path) -> Result<()> {
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn cmd_record(
    seconds: u64,
    model: &str,
    db: Option<PathBuf>,
    keep_audio: Option<PathBuf>,
    capture: Option<String>,
) -> Result<()> {
    let model_id = ModelId::parse(model)
        .with_context(|| format!("unknown model '{model}'"))?;
    let mode = capture.unwrap_or_else(|| zord_config::Settings::load().capture_mode);
    let (record_mic, record_system) = match mode.as_str() {
        "mic" => (true, false),
        "system" => (false, true),
        _ => (true, true),
    };

    // Ensure the model exists locally (download on first run, with progress).
    eprintln!("Preparing model '{}'...", model_id.name());
    let model_path = zord_transcribe::ensure_model(model_id, &mut |done, total| {
        if let Some(total) = total {
            let pct = done as f64 / total as f64 * 100.0;
            eprint!("\r  downloading: {:.1}% ({} MB)   ", pct, done / 1_048_576);
        }
    })?;
    eprintln!("\r  model ready: {}                         ", model_path.display());

    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;

    let started_at = now_ms();
    let session_id = format!("sess-{started_at}");
    store.create_session(&zord_core::Session {
        id: session_id.clone(),
        started_at,
        ended_at: None,
        title: None,
        audio_path: keep_audio.as_ref().map(|p| p.display().to_string()),
        model: model_id.name().to_string(),
    })?;

    let what = match (record_mic, record_system) {
        (true, true) => "microphone (Me) + system audio (Others)",
        (true, false) => "microphone (Me) only",
        (false, true) => "system audio (Others) only",
        _ => "audio",
    };
    eprintln!("Session {session_id} — recording {what}.");
    if seconds == 0 {
        eprintln!("Press Enter to stop.");
    } else {
        eprintln!("Recording for {seconds}s.");
    }

    let count = pipeline::run_record(
        model_path,
        model_id,
        db_path.clone(),
        &session_id,
        seconds,
        keep_audio,
        record_mic,
        record_system,
    )?;

    store.end_session(&session_id, now_ms())?;
    eprintln!("\nDone. {count} segment(s) transcribed and stored.");
    eprintln!("View with:  zord show {session_id}");
    Ok(())
}

fn cmd_show(session_id: &str, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    for seg in store.segments(session_id)? {
        println!(
            "[{} {}] {}",
            fmt_ts(seg.t_start_ms),
            seg.source.label(),
            seg.text
        );
    }
    Ok(())
}

fn cmd_search(query: &str, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db(db)?;
    let store = Store::open(&db_path)?;
    for (sid, seg) in store.search(query)? {
        println!(
            "{sid} [{} {}] {}",
            fmt_ts(seg.t_start_ms),
            seg.source.label(),
            seg.text
        );
    }
    Ok(())
}

fn fmt_ts(ms: u64) -> String {
    let total_s = ms / 1000;
    format!("{:02}:{:02}", total_s / 60, total_s % 60)
}
