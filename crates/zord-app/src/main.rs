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
        } => cmd_record(seconds, &model, db, keep_audio),
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
    }
}

/// Build a plain "Speaker: text" transcript for summarization.
#[cfg(feature = "summaries")]
fn transcript_text(segments: &[zord_core::Segment]) -> String {
    segments
        .iter()
        .map(|s| format!("{}: {}", s.source.label(), s.text))
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
    let transcript = transcript_text(&segs);

    eprintln!("Preparing summary model…");
    let model_path = zord_summarize::ensure_summary_model(&mut |done, total| {
        if let Some(total) = total {
            eprint!("\r  downloading: {:.1}%   ", done as f64 / total as f64 * 100.0);
        }
    })?;
    eprintln!("\r  model ready. Summarizing…              ");

    let summarizer = zord_summarize::Summarizer::load(&model_path)?;
    let summary = summarizer.summarize(&transcript)?;
    store.set_summary(session_id, &summary)?;
    println!("{summary}");
    Ok(())
}

#[cfg(not(feature = "summaries"))]
fn cmd_summarize(_session_id: &str, _db: Option<PathBuf>) -> Result<()> {
    anyhow::bail!("this build has no summary support — rebuild with `--features summaries`")
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
    let rendered = zord_export::render(&session, &segments, fmt);

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
) -> Result<()> {
    let model_id = ModelId::parse(model)
        .with_context(|| format!("unknown model '{model}'"))?;

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

    eprintln!("Session {session_id} — recording microphone (Me) + system audio (Others).");
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
