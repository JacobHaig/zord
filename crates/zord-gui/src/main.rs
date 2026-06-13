//! Zord desktop GUI (Dioxus 0.7). Record mic + system audio, watch the
//! transcript stream in live, browse past sessions, and full-text search.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod osutil;
mod overview;
mod speakers;
mod timeline;
#[cfg(feature = "self-update")]
mod update;
mod wizard;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use engine::{
    AudioFiles, ChatScope, DbCmd, EmbedCmd, Engine, Event, ModelCmd, ModelInfo, PlayCmd,
    RecorderCmd, Status, SummCmd, TimelineLane,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zord_config::Settings;
use zord_core::{Segment, Session, SessionStats, Source};
use zord_export::Format;
use zord_transcribe::ModelId;

const CSS: &str = include_str!("style.css");

/// Width of the left icon rail in px (kept in sync with `.rail` in style.css);
/// subtracted from the pointer x when dragging the sidebar splitter.
const RAIL_W: u32 = 56;

/// **The single source of truth for every icon in the app.** Returns the inner
/// SVG markup for a named icon (24px viewBox, `currentColor` so it inherits its
/// control's text color). Clean line style, pixel-identical on every platform —
/// the emoji this replaces were drawn by the OS font and looked different
/// everywhere. To change an icon everywhere it's used, edit it here.
fn icon_paths(name: &str) -> &'static str {
    match name {
        // Navigation / chrome
        "overview" => "<line x1='6' y1='20' x2='6' y2='13'/><line x1='12' y1='20' x2='12' y2='8'/><line x1='18' y1='20' x2='18' y2='4'/>",
        "search" => "<circle cx='11' cy='11' r='7'/><line x1='21' y1='21' x2='16.5' y2='16.5'/>",
        "settings" => "<line x1='4' y1='8' x2='20' y2='8'/><line x1='4' y1='16' x2='20' y2='16'/><circle cx='9' cy='8' r='2.6'/><circle cx='15' cy='16' r='2.6'/>",
        "close" => "<line x1='6' y1='6' x2='18' y2='18'/><line x1='18' y1='6' x2='6' y2='18'/>",
        "check" => "<polyline points='4 12 10 18 20 6'/>",
        "alert" => "<path d='M12 3l9 16H3z'/><line x1='12' y1='9' x2='12' y2='14'/><circle cx='12' cy='17' r='0.7' fill='currentColor' stroke='none'/>",
        // Recording controls
        "record" => "<circle cx='12' cy='12' r='6' fill='currentColor' stroke='none'/>",
        "stop" => "<rect x='7' y='7' width='10' height='10' rx='2' fill='currentColor' stroke='none'/>",
        "mic" => "<rect x='9' y='3' width='6' height='11' rx='3'/><path d='M5 11a7 7 0 0 0 14 0'/><line x1='12' y1='18' x2='12' y2='21'/>",
        "mic-off" => "<path d='M15 9.3V6a3 3 0 0 0-5.7-1.3'/><path d='M9 9v2a3 3 0 0 0 4.6 2.5'/><path d='M5 11a7 7 0 0 0 11 5.3'/><line x1='12' y1='18' x2='12' y2='21'/><line x1='3' y1='3' x2='21' y2='21'/>",
        "speaker" => "<path d='M11 5L6 9H3v6h3l5 4z'/><path d='M15.5 8.5a5 5 0 0 1 0 7'/><path d='M18.5 5.5a9 9 0 0 1 0 13'/>",
        "speaker-off" => "<path d='M11 5L6 9H3v6h3l5 4z'/><line x1='22' y1='9' x2='16' y2='15'/><line x1='16' y1='9' x2='22' y2='15'/>",
        "play" => "<path d='M7 5v14l11-7z'/>",
        "pause" => "<rect x='6' y='5' width='4' height='14' rx='1' fill='currentColor' stroke='none'/><rect x='14' y='5' width='4' height='14' rx='1' fill='currentColor' stroke='none'/>",
        "waveform" => "<path d='M2 12h2'/><path d='M5 7v10'/><path d='M9 5v14'/><path d='M13 9v6'/><path d='M17 4v16'/><path d='M21 7v10'/>",
        // AI / speaker actions
        "sparkles" => "<path d='M12 3l1.7 5.3a2 2 0 0 0 1.3 1.3L20.3 11l-5.3 1.7a2 2 0 0 0-1.3 1.3L12 19.3l-1.7-5.3a2 2 0 0 0-1.3-1.3L3.7 11l5.3-1.7a2 2 0 0 0 1.3-1.3z'/>",
        "archive" => "<rect x='3' y='4' width='18' height='4' rx='1'/><path d='M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8'/><line x1='10' y1='12' x2='14' y2='12'/>",
        "users" => "<circle cx='9' cy='8' r='3.5'/><path d='M3 20v-1a5 5 0 0 1 5-5h2a5 5 0 0 1 5 5v1'/><path d='M16 5.5a3.5 3.5 0 0 1 0 6.8'/><path d='M21 20v-1a5 5 0 0 0-3.5-4.8'/>",
        "people" => "<circle cx='8' cy='8' r='3'/><path d='M2 20v-1a5 5 0 0 1 5-5h2a5 5 0 0 1 5 5v1'/><circle cx='16' cy='8' r='3'/><path d='M22 20v-1a5 5 0 0 0-4-4.9'/>",
        "refresh" => "<path d='M3 12a9 9 0 0 1 15-6.7L21 8'/><path d='M21 3v5h-5'/><path d='M21 12a9 9 0 0 1-15 6.7L3 16'/><path d='M3 21v-5h5'/>",
        "plus" => "<line x1='12' y1='5' x2='12' y2='19'/><line x1='5' y1='12' x2='19' y2='12'/>",
        "chat" => "<path d='M21 12a8 8 0 0 1-11.3 7.3L3 21l1.7-6.7A8 8 0 1 1 21 12z'/>",
        "headphones" => "<path d='M4 14v-1a8 8 0 0 1 16 0v1'/><rect x='2.5' y='13' width='4.5' height='7' rx='1.6'/><rect x='17' y='13' width='4.5' height='7' rx='1.6'/>",
        // Output / files
        "copy" => "<rect x='9' y='9' width='11' height='11' rx='2'/><path d='M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1'/>",
        "export" | "download" => "<path d='M12 3v12'/><path d='M7 11l5 4 5-4'/><path d='M4 21h16'/>",
        "folder" => "<path d='M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z'/>",
        "file" => "<path d='M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z'/><path d='M14 3v6h6'/>",
        "file-text" => "<path d='M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z'/><path d='M14 3v6h6'/><line x1='8' y1='13' x2='16' y2='13'/><line x1='8' y1='17' x2='13' y2='17'/>",
        "database" => "<ellipse cx='12' cy='6' rx='8' ry='3'/><path d='M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6'/><path d='M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3'/>",
        "external" => "<path d='M14 4h6v6'/><path d='M20 4l-9 9'/><path d='M18 13v5a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h5'/>",
        // Row actions
        "pen" => "<path d='M12 20h9'/><path d='M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4z'/>",
        "trash" => "<path d='M4 7h16'/><path d='M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2'/><path d='M6 7l1 13a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1l1-13'/><line x1='10' y1='11' x2='10' y2='17'/><line x1='14' y1='11' x2='14' y2='17'/>",
        _ => "<circle cx='12' cy='12' r='8'/>",
    }
}

/// Render a named icon as an inline element (see [`icon_paths`]).
fn icon(name: &str) -> Element {
    let svg = format!(
        "<svg viewBox='0 0 24 24' fill='none' stroke='currentColor' stroke-width='1.9' \
         stroke-linecap='round' stroke-linejoin='round'>{}</svg>",
        icon_paths(name)
    );
    rsx! { span { class: "ic", dangerous_inner_html: svg } }
}

fn main() {
    // Logging: always to stderr, plus a rotating file at <app-data>/logs/zord.log
    // so a bundled GUI leaves a copy/pasteable trail when something fails. The
    // returned guard must outlive the app, so keep it in `main`'s scope.
    let _log_guard = init_logging();
    zord_transcribe::install_logging_hooks();

    let window = WindowBuilder::new()
        .with_title("Zord")
        .with_inner_size(LogicalSize::new(1120.0, 760.0));
    let mut cfg = Config::new().with_window(window);
    // Dock / taskbar icon when run directly (the bundle uses icons/icon.icns).
    if let Ok(icon) = dioxus::desktop::icon_from_memory(include_bytes!("../icons/icon-256.png")) {
        cfg = cfg.with_icon(icon);
    }
    LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

/// Set up tracing: an stderr layer always, plus a file layer at
/// `<app-data>/logs/zord.log` when that directory is writable. Returns the file
/// writer's guard, which must be held for the process lifetime so buffered logs
/// flush (hence it lives in `main`).
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    install_panic_hook();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "zord=info,whisper_rs=warn".into());
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    match zord_config::logs_dir() {
        Ok(dir) => {
            let (writer, guard) =
                tracing_appender::non_blocking(tracing_appender::rolling::never(&dir, "zord.log"));
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(writer),
                )
                .init();
            tracing::info!(path = %dir.join("zord.log").display(), "file logging enabled");
            Some(guard)
        }
        Err(e) => {
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .init();
            tracing::warn!("file logging disabled: {e}");
            None
        }
    }
}

/// Capture Rust panics to a flushed `<app-data>/logs/crash.log` (and the tracing
/// log). On the Windows GUI build there's no console, and the buffered file
/// appender can lose a panic on exit — so a panic otherwise just closes the
/// window silently. This makes "the app vanished" diagnosable: if crash.log has a
/// fresh entry it was a Rust panic; if only `llm-trace.log` advanced with nothing
/// in crash.log, it was a native crash (e.g. CPU-instruction fault / OOM in
/// llama.cpp during CPU inference).
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = format!("PANIC: {info}");
        tracing::error!("{msg}");
        if let Ok(dir) = zord_config::logs_dir() {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                use std::io::Write;
                let _ = writeln!(f, "{msg}");
                let _ = f.flush();
            }
        }
        prev(info);
    }));
}

#[derive(Clone, PartialEq)]
enum View {
    Live,
    Session(String),
    Search,
    Overview,
    Speakers,
}

/// A running background job, mirrored from the engine's `Event::JobStarted`
/// (the jobs panel renders these and cancels them by `id`).
#[derive(Clone, PartialEq)]
struct JobView {
    id: String,
    kind: String,
    label: String,
    started_at: u64,
}

// ---------------------------------------------------------------------------
// Encryption gate: runs before the main app so the DB key is set (and any
// pending migration applied) before the engine opens any connection.
// ---------------------------------------------------------------------------

#[cfg(feature = "encryption")]
fn gate_db_path() -> PathBuf {
    Settings::load()
        .db_path()
        .unwrap_or_else(|_| PathBuf::from("zord.db"))
}

#[cfg(feature = "encryption")]
mod crypto_gate {
    use std::path::Path;
    use zord_config::{keychain, Settings};

    pub enum Gate {
        Unlocked,
        NeedsPassphrase,
    }

    /// Apply pending migrations + auto-unlock at startup; decide the lock state.
    pub fn run(db_path: &Path) -> Gate {
        let mut s = Settings::load();

        if s.encrypt_pending {
            match keychain::get() {
                Some(pass) => {
                    if db_path.exists() {
                        if let Err(e) = zord_store::encrypt_existing(db_path, &pass) {
                            tracing::error!("encrypt-on-launch failed: {e}");
                        }
                    }
                    zord_store::set_db_key(Some(pass));
                    s.encrypted = true;
                    s.encrypt_pending = false;
                    let _ = s.save();
                    return Gate::Unlocked;
                }
                None => {
                    s.encrypt_pending = false;
                    let _ = s.save();
                }
            }
        }

        if s.decrypt_pending {
            if let Some(pass) = keychain::get() {
                let _ = zord_store::decrypt_existing(db_path, &pass);
            }
            zord_store::set_db_key(None);
            keychain::clear();
            s.encrypted = false;
            s.decrypt_pending = false;
            let _ = s.save();
            return Gate::Unlocked;
        }

        if s.encrypted || zord_store::is_encrypted(db_path) {
            if let Some(pass) = keychain::get() {
                zord_store::set_db_key(Some(pass));
                if zord_store::Store::open(db_path).is_ok() {
                    return Gate::Unlocked;
                }
            }
            return Gate::NeedsPassphrase;
        }

        Gate::Unlocked
    }

    /// Try a user-entered passphrase; on success set the key (and optionally
    /// remember it) and return true.
    pub fn try_unlock(db_path: &Path, pass: &str, remember: bool) -> bool {
        zord_store::set_db_key(Some(pass.to_string()));
        if zord_store::Store::open(db_path).is_ok() {
            if remember {
                let _ = keychain::store(pass);
            }
            true
        } else {
            zord_store::set_db_key(None);
            false
        }
    }
}

/// Launched root. With `encryption`, gates the app behind an unlock screen when
/// the DB is encrypted; otherwise just renders the main app.
#[cfg(feature = "encryption")]
#[component]
fn App() -> Element {
    let db = use_hook(gate_db_path);
    let unlocked = use_signal(|| matches!(crypto_gate::run(&db), crypto_gate::Gate::Unlocked));
    if *unlocked.read() {
        rsx! { MainApp {} }
    } else {
        rsx! { UnlockScreen { db: db.clone(), unlocked } }
    }
}

#[cfg(not(feature = "encryption"))]
#[component]
fn App() -> Element {
    rsx! { MainApp {} }
}

#[cfg(feature = "encryption")]
#[component]
fn UnlockScreen(db: PathBuf, unlocked: Signal<bool>) -> Element {
    let mut pass = use_signal(String::new);
    let mut remember = use_signal(|| true);
    let mut error = use_signal(|| Option::<String>::None);

    let submit = move |_| {
        let p = pass.peek().clone();
        if crypto_gate::try_unlock(&db, &p, *remember.peek()) {
            unlocked.set(true);
        } else {
            error.set(Some("Wrong passphrase — try again.".to_string()));
        }
    };

    rsx! {
        style { dangerous_inner_html: CSS }
        div { class: "unlock",
            div { class: "unlock-card",
                div { class: "brand", "ZORD" }
                h2 { "Unlock" }
                p { class: "field-note", "This database is encrypted. Enter your passphrase to continue." }
                input {
                    r#type: "password",
                    class: "search",
                    placeholder: "Passphrase",
                    autofocus: true,
                    value: "{pass}",
                    oninput: move |e| pass.set(e.value()),
                }
                button {
                    class: if remember() { "toggle on" } else { "toggle" },
                    onclick: move |_| { let v = *remember.peek(); remember.set(!v); },
                    if remember() { "Remember in keychain" } else { "Don't remember" }
                }
                if let Some(err) = error.read().clone() {
                    div { class: "notice", "{err}" }
                }
                button { class: "record", onclick: submit, "Unlock" }
            }
        }
    }
}

#[component]
fn MainApp() -> Element {
    let mut status = use_signal(|| Status::Idle);
    let mut notice = use_signal(|| Option::<String>::None);
    let mut segments = use_signal(Vec::<Segment>::new);
    // Live recording transcript, buffered separately from `segments` (which holds
    // the currently-viewed *saved* session) so you can navigate away during a
    // recording and come back without losing the lines that streamed meanwhile.
    let mut live_segments = use_signal(Vec::<Segment>::new);
    let mut me_level = use_signal(|| 0.0f32);
    let mut others_level = use_signal(|| 0.0f32);
    let mut sessions = use_signal(Vec::<Session>::new);
    // Per-session badge flags (summary/compressed/speakers) + a title filter box.
    let mut session_badges =
        use_signal(std::collections::HashMap::<String, (bool, bool, bool)>::new);
    let session_filter = use_signal(String::new);
    let mut search_results = use_signal(Vec::<(String, Segment)>::new);
    // Semantic search: whether the search view is in Keyword or Semantic mode.
    // Only meaningful in `semantic` builds; always "keyword" otherwise.
    let search_mode = use_signal(|| "keyword".to_string());
    // Dedicated search view: the live query text, plus a pending "scroll to this
    // segment after the session loads" + which line to briefly highlight.
    let mut search_query = use_signal(String::new);
    let mut scroll_to_seg = use_signal(|| Option::<i64>::None);
    let mut highlight_seg = use_signal(|| Option::<i64>::None);
    let mut view = use_signal(|| View::Live);
    // Path of the most recently exported file (drives the Reveal/Open buttons).
    let mut last_export = use_signal(|| Option::<String>::None);
    // Current session's AI summary, if any.
    let mut summary = use_signal(|| Option::<String>::None);
    // Current session's dense-prose compression, if any (Phase 23), and whether
    // its (machine-oriented) body is expanded for the user to read.
    let mut compressed = use_signal(|| Option::<String>::None);
    // Collapse state for the AI panels (sticky across navigation so it acts as a
    // preference — on a small screen these can otherwise bury the transcript).
    let show_summary = use_signal(|| true);
    let show_compressed = use_signal(|| false);
    // Host notes for the viewed session (links / action items / reminders),
    // editable + searchable + fed to the AI. `notes` is the saved value (drives
    // whether the panel shows content); `notes_draft` is the textarea buffer,
    // persisted on blur. `note_results` holds notes that matched a search.
    let mut notes = use_signal(|| Option::<String>::None);
    let mut notes_draft = use_signal(String::new);
    let show_notes = use_signal(|| false);
    let mut note_results = use_signal(Vec::<(String, String)>::new);
    // Id of the session currently recording (from Event::SessionStarted), so the
    // notes drawer can attach notes live — the row exists in the DB from the
    // start of capture. Cleared when recording stops.
    let mut live_session_id = use_signal(|| Option::<String>::None);
    // Per-action busy flags for the session toolbar (set on click, cleared when
    // the corresponding result event lands) so buttons show progress + can't be
    // double-fired.
    let mut summarizing = use_signal(|| false);
    let mut compressing = use_signal(|| false);
    let mut diarizing = use_signal(|| false);
    let mut retranscribing = use_signal(|| false);
    // Expected speaker count for the viewed session's diarization, as typed
    // (empty = auto-detect). Loaded from / persisted on the session row.
    let mut diar_speakers = use_signal(String::new);
    // Retained WAV paths that exist on disk for the viewed session. A line only
    // gets a replay button when its channel's file is present. `speakers` holds
    // per-participant tracks for integration sessions (spk-N).
    let mut audio_files = use_signal(AudioFiles::default);
    // Which speaker index is the app user themself (integration sessions tag
    // it from the configured platform user ID; None for mic/desktop sessions).
    let mut me_speaker = use_signal(|| Option::<i32>::None);
    // The transcript line (db id) currently playing back.
    let mut playing_seg = use_signal(|| Option::<i64>::None);
    // Phase 39d: living overview document signals.
    let mut overview_doc = use_signal(String::new);
    let mut overview_doc_updated = use_signal(|| 0u64);
    let overview_editing = use_signal(|| false);
    let overview_draft = use_signal(String::new);
    // Chat (Phase 23d): the active conversation (per-meeting or cross-meeting),
    // its input buffer, busy flag, and which scope the history belongs to.
    let mut chat = use_signal(Vec::<(bool, String)>::new);
    let chat_input = use_signal(String::new);
    let mut chat_busy = use_signal(|| false);
    let chat_scope = use_signal(|| Option::<ChatScope>::None);
    // Collapse state for the chat panel (sticky, like the Summary/Compressed ones).
    // Chat panels start collapsed — expand on demand.
    let show_chat = use_signal(|| false);
    // Custom names for diarized speakers in the viewed session (index → name).
    let mut speaker_names = use_signal(std::collections::HashMap::<i32, String>::new);
    // Session id currently being renamed (+ its edit buffer); pending delete.
    let editing = use_signal(|| Option::<String>::None);
    let edit_text = use_signal(String::new);
    let confirm_delete = use_signal(|| Option::<String>::None);
    // Phase 43f: multi-session selection state.
    let mut selected_sessions = use_signal(std::collections::HashSet::<String>::new);
    let select_anchor = use_signal(|| Option::<String>::None);
    // Whether to show the batch-delete confirm dialog.
    let confirm_bulk_delete = use_signal(|| false);
    // Session id awaiting Re-transcribe confirmation (Phase 25c).
    let confirm_retranscribe = use_signal(|| Option::<String>::None);
    // Seconds elapsed in the current recording (0 when idle).
    let mut rec_secs = use_signal(|| 0u64);
    // Whether the mic ("Me") / desktop ("Others") channels are muted during the
    // current recording.
    let mut mic_muted = use_signal(|| false);
    let mut sys_muted = use_signal(|| false);
    // Whether the current recording is an integration (Discord) session —
    // set at Start; drives hiding the local-capture mute buttons.
    let mut recording_discord = use_signal(|| false);
    let mut settings = use_signal(Settings::load);
    let mut show_settings = use_signal(|| false);
    // First-run setup wizard (Phase 36b): shown until completed/skipped once;
    // re-runnable from Settings → About.
    let show_wizard = use_signal(|| !Settings::load().setup_complete);
    // Sidebar width (px) — adjusted by dragging the splitter, persisted in
    // settings on release.
    let mut sidebar_w = use_signal(|| settings.peek().sidebar_width.clamp(160, 480));
    let mut dragging_split = use_signal(|| false);
    // Model ids reported by the external LLM server (Phase 24c picker).
    let mut remote_models = use_signal(Vec::<String>::new);
    let devices = use_hook(zord_capture::input_devices);
    let mut models = use_signal(Vec::<ModelInfo>::new);
    // (model name currently downloading, percent).
    let mut model_progress = use_signal(|| Option::<(String, u8)>::None);
    // Name of a model whose download failed → show the manual-fetch fallback.
    let mut download_help = use_signal(|| Option::<String>::None);
    // Background-jobs board: a live, cancellable list of running jobs driven by
    // the engine's job registry (Event::JobStarted/JobFinished). `job_tick`
    // forces the elapsed timers to re-render each second; `diarize_est_secs` is a
    // rough ETA for diarization scaled to the meeting length (captured at click).
    let mut show_jobs = use_signal(|| false);
    // Whether the Export format dropdown is open.
    let show_export_menu = use_signal(|| false);
    // The contextual "Generate ▾" menu (Summarize/Compress/Identify/Re-transcribe).
    let show_generate_menu = use_signal(|| false);
    // Phase 40: find-in-session bar state.
    let mut find_open = use_signal(|| false);
    let find_query = use_signal(String::new);
    let mut find_active = use_signal(|| 0usize);
    // Computed hit-id list for the active view (live vs saved). Updated whenever
    // the query or transcript changes; empty when find is closed.
    let mut find_hits_computed = use_signal(Vec::<i64>::new);
    // Active tab in the settings overlay's left nav (Phase 3).
    let settings_tab = use_signal(|| "transcription".to_string());
    // Authoritative list of running background jobs (Phase: cancellable jobs),
    // driven by Event::JobStarted/JobFinished from the engine — independent of
    // the viewed session, so navigating/recording never clears them. The inline
    // per-button busy flags (summarizing/diarizing/…) are separate.
    let mut jobs = use_signal(Vec::<JobView>::new);
    let mut job_tick = use_signal(|| 0u64);
    let mut diarize_est_secs = use_signal(|| Option::<u64>::None);
    // Phase 38d: voiceprint library (updated by Event::Voiceprints).
    let mut voiceprints = use_signal(Vec::<zord_store::VoiceprintInfo>::new);

    // Phase 42c: session timeline panel signals.
    // `timeline_open` toggles the panel; opening it fires DbCmd::LoadTimeline.
    let mut timeline_open = use_signal(|| false);
    // Amplitude lanes received from Phase 42a (reset when the session changes).
    let mut timeline_lanes = use_signal(Vec::<TimelineLane>::new);
    // Current playback position tick from Phase 42b (~250ms).
    let mut timeline_pos = use_signal(|| Option::<u64>::None);
    // Per-lane enabled state (missing key = enabled).
    let lane_enabled = use_signal(std::collections::HashMap::<String, bool>::new);
    // Whether to overlay all lanes into a single merged graph.
    let timeline_merged = use_signal(|| false);

    // Phase 46: conversation analytics ("Meeting DNA") stats.
    // `stats_open` toggles the stats card; closed when the session changes.
    let mut stats_open = use_signal(|| false);
    let mut session_stats = use_signal(|| Option::<zord_core::SessionStats>::None);

    // Create the engine once and drain its events into signals.
    let engine = use_hook(|| {
        let initial = settings.peek().clone();
        // Apply audio retention on startup.
        if let Ok(dir) = initial.audio_dir() {
            zord_config::apply_retention(&dir, initial.auto_delete_days);
        }
        let db = initial
            .db_path()
            .unwrap_or_else(|_| PathBuf::from("zord.db"));
        let (engine, mut ev_rx) = Engine::spawn(db);
        // Clone for the event loop below (it issues the follow-up Load when a
        // finished recording auto-selects its session).
        let engine_ev = engine.clone();
        spawn(async move {
            // Per-event application. `Level` is handled separately (coalesced
            // below) so a burst of meter updates can never starve control events
            // like `Status::Idle` (which is what flips the Stop button back).
            let mut apply = move |ev: Event| match ev {
                Event::Status(s) => {
                    // Recording finished while watching Live → follow into the
                    // just-saved session and watch its (post-)transcription
                    // stream in. Never yank the view off another session.
                    let finished =
                        matches!(*status.peek(), Status::Recording) && matches!(s, Status::Idle);
                    status.set(s);
                    if finished {
                        if matches!(&*view.peek(), View::Live) {
                            if let Some(id) = live_session_id.peek().clone() {
                                view.set(View::Session(id.clone()));
                                let _ = engine_ev.db_tx.send(DbCmd::Load(id));
                            }
                        }
                        live_session_id.set(None); // recording's over
                    }
                }
                Event::Notice(n) => notice.set(Some(n)),
                Event::Segment(seg) => {
                    // Always buffer the live stream so it's intact when you return
                    // to the Live view, even if you navigated away mid-recording.
                    live_segments.write().push(seg);
                }
                Event::Level { .. } => {} // handled via coalescing
                Event::Sessions(v) => {
                    // Keep only ids that still exist in the new list (clear
                    // anything that was deleted or is no longer visible).
                    let existing: std::collections::HashSet<&str> =
                        v.iter().map(|s| s.id.as_str()).collect();
                    selected_sessions
                        .write()
                        .retain(|id| existing.contains(id.as_str()));
                    sessions.set(v);
                }
                Event::SessionBadges(b) => session_badges.set(b),
                Event::SearchResults(v) => search_results.set(v),
                Event::Transcript { id, segments: v } => {
                    // Apply only to the session on screen — background workers
                    // (re-transcribe, diarize) refresh detached, so the user
                    // may be reading a different session by now.
                    if matches!(&*view.peek(), View::Session(cur) if *cur == id) {
                        segments.set(v);
                    }
                }
                Event::Exported(p) => {
                    notice.set(Some(format!("Exported to {p}")));
                    last_export.set(Some(p));
                }
                Event::Models(v) => {
                    models.set(v);
                    model_progress.set(None);
                }
                Event::ModelProgress { name, pct } => {
                    model_progress.set(Some((name, pct)));
                }
                Event::DownloadFailed { name } => {
                    model_progress.set(None);
                    download_help.set(Some(name));
                }
                Event::Summary(v) => {
                    summary.set(v);
                    summarizing.set(false);
                }
                Event::Compressed(v) => {
                    compressed.set(v);
                    compressing.set(false);
                }
                Event::SessionStarted(id) => {
                    // New recording → fresh, empty notes attached to its id.
                    live_session_id.set(Some(id));
                    notes_draft.set(String::new());
                    notes.set(None);
                }
                Event::Notes(v) => {
                    notes_draft.set(v.clone().unwrap_or_default());
                    notes.set(v); // the drawer tab shows a dot when present
                }
                Event::NoteResults(v) => note_results.set(v),
                Event::OverviewDoc {
                    markdown,
                    updated_at,
                } => {
                    // Don't clobber the user's open editor — update the backing
                    // doc and stamp; the rendered view re-derives from doc.
                    overview_doc.set(markdown);
                    overview_doc_updated.set(updated_at);
                }
                Event::ChatReply { scope, reply } => {
                    // Only land the reply if it belongs to the open conversation.
                    // If pieces of it were streamed, the partial assistant
                    // message is already last — replace it with the full text.
                    if chat_scope.peek().as_ref() == Some(&scope) {
                        let mut c = chat.write();
                        match c.last_mut() {
                            Some((false, text)) if *chat_busy.peek() => *text = reply,
                            _ => c.push((false, reply)),
                        }
                    }
                    chat_busy.set(false);
                }
                Event::ChatDelta { scope, delta } => {
                    // Append to the in-progress assistant message (creating it
                    // on the first piece) — only for the open conversation.
                    if chat_scope.peek().as_ref() == Some(&scope) && *chat_busy.peek() {
                        let mut c = chat.write();
                        match c.last_mut() {
                            Some((false, text)) => text.push_str(&delta),
                            _ => c.push((false, delta)),
                        }
                    }
                }
                Event::Speakers(v) => {
                    // Load-only (fires on every session open) — must NOT touch
                    // diarization busy state; that's Event::Diarized's job.
                    speaker_names.set(v);
                }
                Event::Diarized { id, speakers } => {
                    // Terminal signal from the background diarization worker.
                    // Clear the busy/ETA state and apply the labels only if that
                    // session is still the one on screen (the job ran detached, so
                    // the user may have navigated elsewhere meanwhile).
                    diarizing.set(false);
                    diarize_est_secs.set(None);
                    if matches!(&*view.read(), View::Session(cur) if *cur == id) {
                        speaker_names.set(speakers);
                    }
                }
                Event::DiarizeSpeakers(n) => {
                    diar_speakers.set(if n > 0 { n.to_string() } else { String::new() });
                }
                Event::AudioFiles(af) => audio_files.set(af),
                Event::MeSpeaker(idx) => me_speaker.set(idx),
                Event::Retranscribing => retranscribing.set(true),
                Event::Retranscribed => retranscribing.set(false),
                Event::JobStarted { id, kind, label } => {
                    let mut v = jobs.write();
                    if !v.iter().any(|j| j.id == id) {
                        v.push(JobView {
                            id,
                            kind,
                            label,
                            started_at: now_ms(),
                        });
                    }
                }
                Event::JobFinished { id } => {
                    jobs.write().retain(|j| j.id != id);
                }
                Event::Playing(v) => playing_seg.set(v),
                // Phase 38d: voiceprint library — update the Speakers view signal.
                Event::Voiceprints(v) => voiceprints.set(v),

                // Phase 42a data layer: apply timeline lanes only when the
                // event's session id matches the currently viewed session
                // (same guard as Event::Transcript).
                Event::Timeline { id, lanes } => {
                    if matches!(&*view.peek(), View::Session(cur) if *cur == id) {
                        timeline_lanes.set(lanes);
                    }
                }

                // Phase 42b: timeline playback position tick — update the
                // scrubber and stop-state detection.
                Event::TimelinePos { ms } => {
                    timeline_pos.set(ms);
                }

                // Phase 46: conversation analytics — session-guarded apply.
                Event::Stats { id, stats } => {
                    if matches!(&*view.peek(), View::Session(cur) if *cur == id) {
                        session_stats.set(Some(stats));
                    }
                }

                Event::RemoteModels { models, error } => {
                    if let Some(e) = error {
                        notice.set(Some(format!("External LLM: {e}")));
                    } else {
                        notice.set(Some(format!(
                            "Connected — {} model(s) available.",
                            models.len()
                        )));
                        // Auto-pick the first model when none is chosen yet.
                        if settings.peek().llm_model.trim().is_empty() {
                            if let Some(first) = models.first() {
                                let mut s = settings.peek().clone();
                                s.llm_model = first.clone();
                                let _ = s.save();
                                settings.set(s);
                            }
                        }
                    }
                    remote_models.set(models);
                }
            };

            while let Some(first) = ev_rx.recv().await {
                // Drain everything already queued into one burst, applying
                // non-Level events in order and keeping only the newest Level per
                // source. This guarantees a meter flood can't delay Status/etc.
                let (mut last_me, mut last_others) = (None, None);
                let mut ev = first;
                loop {
                    match ev {
                        Event::Level {
                            source: Source::Me,
                            level,
                        } => last_me = Some(level),
                        Event::Level {
                            source: Source::Others,
                            level,
                        } => last_others = Some(level),
                        other => apply(other),
                    }
                    match ev_rx.try_recv() {
                        Ok(next) => ev = next,
                        Err(_) => break,
                    }
                }
                if let Some(l) = last_me {
                    me_level.set(l);
                }
                if let Some(l) = last_others {
                    others_level.set(l);
                }
            }
        });
        let _ = engine.db_tx.send(DbCmd::ListSessions);
        let _ = engine.model_tx.send(ModelCmd::List);
        // Phase 38d: pre-load the voiceprint library so the Speakers view is
        // ready when the user first navigates to it (harmless when empty).
        let _ = engine.db_tx.send(DbCmd::Voiceprints);
        // Phase 39d: pre-load the living overview document.
        let _ = engine.db_tx.send(DbCmd::LoadOverviewDoc);
        engine
    });

    // Recording timer: one ticker that counts up while recording, resets idle.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if matches!(*status.peek(), Status::Recording) {
                let v = *rec_secs.peek();
                rec_secs.set(v + 1);
            } else if *rec_secs.peek() != 0 {
                rec_secs.set(0);
            }
        }
    });

    // Launch-time update check (Phase 34): `self-update` builds on the
    // github/dev channel only, opt-out via Settings → About. One request to
    // the GitHub releases API; a hit becomes a toast pointing at About.
    #[cfg(feature = "self-update")]
    use_future(move || async move {
        if !update::channel_self_updates() || !zord_config::Settings::load().check_updates {
            return;
        }
        if let Ok(Ok(Some(info))) = tokio::task::spawn_blocking(update::check).await {
            notice.set(Some(format!(
                "Update available: v{} (you have v{}) — install from Settings → About",
                info.version,
                update::CURRENT_VERSION
            )));
        }
    });

    // Toast-style notices: auto-dismiss after ~5s (unless replaced meanwhile).
    use_effect(move || {
        if let Some(text) = notice.read().clone() {
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if notice.peek().as_deref() == Some(text.as_str()) {
                    notice.set(None);
                }
            });
        }
    });

    // Auto-scroll the transcript to the latest line while recording.
    use_effect(move || {
        let _ = live_segments.read().len(); // subscribe to new live segments
        if matches!(*status.peek(), Status::Recording) {
            let _ = document::eval(
                "const t=document.querySelector('.transcript'); if(t){t.scrollTop=t.scrollHeight;}",
            );
        }
    });

    // Keep the chat log pinned to the newest message.
    use_effect(move || {
        let _ = chat.read().len();
        let _ = chat_busy.read();
        let _ = document::eval(
            "const c=document.querySelector('.chat-log'); if(c){c.scrollTop=c.scrollHeight;}",
        );
    });

    // After a search result opens a session, scroll its transcript to the picked
    // line and briefly highlight it.
    use_effect(move || {
        let _ = segments.read().len(); // re-run when a transcript loads
        let target = *scroll_to_seg.peek();
        if let Some(id) = target {
            let _ = document::eval(&format!(
                "requestAnimationFrame(()=>{{const e=document.getElementById('seg-{id}');if(e){{e.scrollIntoView({{block:'center',behavior:'smooth'}});}}}})"
            ));
            highlight_seg.set(Some(id));
            scroll_to_seg.set(None);
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                if *highlight_seg.peek() == Some(id) {
                    highlight_seg.set(None);
                }
            });
        }
    });

    // Phase 40: recompute find hits whenever the query, transcript, or live
    // segments change (subscribes to all three by reading them).
    use_effect(move || {
        let q = find_query.read().clone();
        let is_live = *view.read() == View::Live;
        let hits = if q.is_empty() || !*find_open.read() {
            Vec::new()
        } else if is_live {
            find_hits(&live_segments.read(), &q)
        } else {
            find_hits(&segments.read(), &q)
        };
        // Only write if the list actually changed to avoid spurious re-renders.
        if *find_hits_computed.peek() != hits {
            find_hits_computed.set(hits);
            find_active.set(0);
        }
    });

    // The background-jobs board is driven directly by Event::JobStarted/
    // JobFinished (see the event loop) — the authoritative, view-independent
    // source. No reconciliation from busy bools.

    // Tick once a second while any job is running so elapsed timers update.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if !jobs.peek().is_empty() {
                let v = *job_tick.peek();
                job_tick.set(v.wrapping_add(1));
            } else if *show_jobs.peek() {
                show_jobs.set(false); // auto-close the panel when nothing's running
            }
        }
    });

    let st = status.read().clone();
    let recording = matches!(
        st,
        Status::Recording | Status::PreparingModel | Status::Downloading(_)
    );
    let status_text = status_label(&st);

    // Cancel a background job from the jobs panel: request cancellation in the
    // engine and optimistically drop it from the list (JobFinished also removes
    // it). Cancellation is cooperative — see Engine::cancel_job.
    let on_cancel_job = {
        let engine = engine.clone();
        move |id: String| {
            engine.cancel_job(&id);
            // Clear the matching inline button flag now (a cancelled job's result
            // event is skipped, so the flag wouldn't otherwise reset).
            if let Some(kind) = jobs
                .read()
                .iter()
                .find(|j| j.id == id)
                .map(|j| j.kind.clone())
            {
                match kind.as_str() {
                    "summarize" => summarizing.set(false),
                    "compress" => compressing.set(false),
                    "diarize" => {
                        diarizing.set(false);
                        diarize_est_secs.set(None);
                    }
                    "retranscribe" => retranscribing.set(false),
                    _ => {}
                }
            }
            jobs.write().retain(|j| j.id != id);
        }
    };

    let on_record = {
        let engine = engine.clone();
        move |integration: bool| {
            if recording {
                tracing::info!("record button: Stop clicked");
                let _ = engine.rec_tx.send(RecorderCmd::Stop);
                let _ = engine.db_tx.send(DbCmd::ListSessions);
                // live_session_id stays set until Status::Idle arrives — the
                // event loop uses it to follow into the finished session.
            } else {
                tracing::info!("record button: Record clicked");
                segments.write().clear();
                live_segments.write().clear();
                notice.set(None);
                summary.set(None);
                compressed.set(None);
                summarizing.set(false);
                compressing.set(false);
                diarizing.set(false);
                retranscribing.set(false);
                audio_files.set(AudioFiles::default());
                me_speaker.set(None);
                let _ = engine.play_tx.send(PlayCmd::Stop);
                reset_chat(chat, chat_input, chat_busy, chat_scope);
                speaker_names.write().clear();
                mic_muted.set(false);
                sys_muted.set(false);
                recording_discord.set(integration);
                view.set(View::Live);
                let s = settings.peek().clone();
                let model = ModelId::parse(&s.model).unwrap_or(ModelId::LargeV3TurboQ5);
                let audio_dir = s.audio_dir().unwrap_or_else(|_| PathBuf::from("audio"));
                let (record_mic, record_system) = capture_sources(s.capture_mode.as_str());
                let _ = engine.rec_tx.send(RecorderCmd::Start {
                    model,
                    keep_audio: s.keep_audio,
                    input_device: s.input_device.clone(),
                    audio_dir,
                    record_mic,
                    record_system,
                    live: s.live_transcription,
                    integration,
                });
            }
        }
    };

    // Which channels the current capture mode includes (drives the mute buttons).
    let mic_in_capture = settings.read().capture_mode != "system";
    let system_in_capture = settings.read().capture_mode != "mic";
    // Record Discord button (spec 2026-06-10): discord build + credentials
    // saved + not hidden by the Integrations toggle.
    let discord_button = cfg!(feature = "discord")
        && !settings.read().discord_bot_token.is_empty()
        && !settings.read().discord_user_id.trim().is_empty()
        && settings.read().discord_record_button;
    // Appearance: tint session badges by meaning vs monochrome (Settings → Theme).
    let tint_badges = settings.read().badge_tint;

    let on_mute = {
        let engine = engine.clone();
        move |_| {
            let next = !*mic_muted.peek();
            let _ = engine.rec_tx.send(RecorderCmd::SetMicMuted(next));
            mic_muted.set(next);
        }
    };

    let on_mute_system = {
        let engine = engine.clone();
        move |_| {
            let next = !*sys_muted.peek();
            let _ = engine.rec_tx.send(RecorderCmd::SetSystemMuted(next));
            sys_muted.set(next);
        }
    };

    // Toggle settings; re-scan the models folder when opening so a freshly
    // dropped-in custom GGUF (or manually-placed model) shows up without a restart.
    let on_toggle_settings = {
        let engine = engine.clone();
        move |_| {
            let opening = !*show_settings.peek();
            show_settings.set(opening);
            if opening {
                let _ = engine.model_tx.send(ModelCmd::List);
            }
        }
    };

    // Open the Overview view and load the stored document.
    let on_open_overview = {
        let engine = engine.clone();
        move |_| {
            view.set(View::Overview);
            find_open.set(false); // leaving the transcript closes the find bar
            close_timeline(&engine, timeline_open, timeline_lanes, timeline_pos);
            reset_chat(chat, chat_input, chat_busy, chat_scope);
            let _ = engine.db_tx.send(DbCmd::LoadOverviewDoc);
        }
    };

    // Open the dedicated search view (does not clear the prior query/results).
    let on_open_search = {
        let engine = engine.clone();
        move |_| {
            view.set(View::Search);
            find_open.set(false); // leaving the transcript closes the find bar
            close_timeline(&engine, timeline_open, timeline_lanes, timeline_pos);
        }
    };

    // Open the Speakers view and refresh the voiceprint list.
    let on_open_speakers = {
        let engine = engine.clone();
        move |_| {
            if cfg!(feature = "voiceprints") {
                view.set(View::Speakers);
                find_open.set(false); // leaving the transcript closes the find bar
                close_timeline(&engine, timeline_open, timeline_lanes, timeline_pos);
                let _ = engine.db_tx.send(DbCmd::Voiceprints);
            }
        }
    };

    // Run a query from the search view's input.
    let on_query = {
        let engine = engine.clone();
        move |q: String| {
            search_query.set(q.clone());
            if q.trim().is_empty() {
                search_results.set(Vec::new());
                note_results.set(Vec::new());
            } else if cfg!(feature = "semantic") && *search_mode.peek() == "semantic" {
                // Semantic mode: route to the embed worker for cosine search.
                let _ = engine.embed_tx.send(EmbedCmd::Query(q));
            } else {
                let _ = engine.db_tx.send(DbCmd::Search(q));
            }
        }
    };

    // Open a session from a search result, optionally scrolling to a line.
    let on_open_result = {
        let engine = engine.clone();
        move |(sid, seg_id): (String, Option<i64>)| {
            view.set(View::Session(sid.clone()));
            last_export.set(None);
            summary.set(None);
            compressed.set(None);
            summarizing.set(false);
            compressing.set(false);
            diarizing.set(false);
            reset_chat(chat, chat_input, chat_busy, chat_scope);
            scroll_to_seg.set(seg_id);
            let _ = engine.db_tx.send(DbCmd::Load(sid));
        }
    };

    // Persist an inline transcript edit, then reload the open session.
    let on_edit_segment = {
        let engine = engine.clone();
        move |(segment_id, text): (i64, String)| {
            let _ = engine.db_tx.send(DbCmd::EditSegment { segment_id, text });
            if let View::Session(sid) = &*view.peek() {
                let _ = engine.db_tx.send(DbCmd::Load(sid.clone()));
            }
        }
    };

    // Replay one transcript line from its retained WAV (None = stop playback).
    let on_play_segment = {
        let engine = engine.clone();
        move |req: Option<(String, i64, u64, u64)>| {
            let cmd = match req {
                Some((wav, segment_id, start_ms, end_ms)) => PlayCmd::Play {
                    segment_id,
                    wav: PathBuf::from(wav),
                    start_ms,
                    end_ms,
                },
                None => PlayCmd::Stop,
            };
            let _ = engine.play_tx.send(cmd);
        }
    };

    // Open a saved session: reset the per-session panels and load it (same
    // body the session row's onclick had inline). Captures only Clone values
    // (Engine + Copy signals), so the closure is cloned for each consumer —
    // an EventHandler consumes its closure on construction.
    let on_open_session = {
        let engine = engine.clone();
        move |id: String| {
            // Close timeline if it was open for a different session.
            close_timeline(&engine, timeline_open, timeline_lanes, timeline_pos);
            // Phase 46: close stats panel and clear cached stats on session switch.
            stats_open.set(false);
            session_stats.set(None);
            view.set(View::Session(id.clone()));
            last_export.set(None);
            summary.set(None);
            compressed.set(None);
            summarizing.set(false);
            compressing.set(false);
            diarizing.set(false);
            retranscribing.set(false);
            diar_speakers.set(String::new());
            audio_files.set(AudioFiles::default());
            me_speaker.set(None);
            let _ = engine.play_tx.send(PlayCmd::Stop);
            reset_chat(chat, chat_input, chat_busy, chat_scope);
            let _ = engine.db_tx.send(DbCmd::Load(id));
        }
    };
    // Second binding of the same closure for the Speakers view.
    let on_open_from_speakers = on_open_session.clone();

    // Back to the live view; drop any panels left from a saved session viewed
    // mid-recording (the sidebar's pinned "Current recording" row).
    let on_show_live = {
        let engine = engine.clone();
        move |_: ()| {
            close_timeline(&engine, timeline_open, timeline_lanes, timeline_pos);
            // Phase 46: close stats panel when leaving session view.
            stats_open.set(false);
            session_stats.set(None);
            view.set(View::Live);
            summary.set(None);
            compressed.set(None);
            speaker_names.write().clear();
        }
    };

    rsx! {
        style { dangerous_inner_html: CSS }
        div { class: "app",
            // User theme overrides land here as CSS custom properties — the
            // whole token system re-derives from them (Settings → Theme).
            style: "{theme_style(&settings.read())}",
            // Splitter drag: track the pointer anywhere in the app while the
            // handle is held; persist the final width when released.
            onmousemove: move |e| {
                if *dragging_split.peek() {
                    // Subtract the icon rail's width — the sidebar starts to its right.
                    let x = (e.client_coordinates().x as u32).saturating_sub(RAIL_W);
                    sidebar_w.set(x.clamp(160, 480));
                }
            },
            onmouseup: move |_| {
                if *dragging_split.peek() {
                    dragging_split.set(false);
                    let mut s = settings.peek().clone();
                    s.sidebar_width = *sidebar_w.peek();
                    let _ = s.save();
                    settings.set(s);
                }
            },
            // Pointer left the window mid-drag: end the drag there.
            onmouseleave: move |_| {
                if *dragging_split.peek() {
                    dragging_split.set(false);
                    let mut s = settings.peek().clone();
                    s.sidebar_width = *sidebar_w.peek();
                    let _ = s.save();
                    settings.set(s);
                }
            },
            // ---- Icon rail: global nav (top) + utilities (bottom) ----
            IconRail {
                view,
                jobs,
                show_jobs,
                on_open_overview,
                on_open_search,
                on_open_speakers,
                on_toggle_settings,
            }
            // ---- Sidebar: session history + Record ----
            SessionsSidebar {
                sidebar_w,
                sessions,
                live_session_id,
                session_filter,
                session_badges,
                view,
                editing,
                edit_text,
                confirm_delete,
                selected_sessions,
                select_anchor,
                confirm_bulk_delete,
                rec_secs,
                recording,
                discord_button,
                recording_discord: *recording_discord.read(),
                st: st.clone(),
                status_text: status_text.clone(),
                tint_badges,
                mic_in_capture,
                system_in_capture,
                mic_muted,
                sys_muted,
                settings,
                engine: engine.clone(),
                on_open_session,
                on_show_live,
                on_record,
                on_mute,
                on_mute_system,
            }

            // ---- Sidebar / main divider (drag to resize) ----
            div {
                class: if *dragging_split.read() { "splitter active" } else { "splitter" },
                onmousedown: move |e| {
                    e.prevent_default();
                    dragging_split.set(true);
                },
            }

            // ---- Main column ----
            main { class: "main",
                header { class: "topbar",
                    div { class: "status",
                        span { class: if recording { "dot rec" } else { "dot" } }
                        span { "{status_text}" }
                        if matches!(st, Status::Recording) {
                            span { class: "rec-timer", "{fmt_dur(rec_secs())}" }
                        }
                    }
                }

                // Level meters. Pass the signals (not their values) so frequent
                // level ticks re-render only the meters, never the whole App.
                div { class: "meters",
                    Meter { label: "Me".to_string(), level: me_level, kind: "me".to_string() }
                    Meter { label: "Others".to_string(), level: others_level, kind: "others".to_string() }
                }

                if let Some(n) = notice.read().clone() {
                    div { class: "notice",
                        span { "{n}" }
                        button { class: "notice-x", onclick: move |_| notice.set(None), {icon("close")} }
                    }
                }

                // Export bar (only when viewing a saved session)
                if let View::Session(id) = &*view.read() {
                    SessionToolbar {
                        id: id.clone(),
                        engine: engine.clone(),
                        settings,
                        notice,
                        sessions,
                        summary,
                        compressed,
                        audio_files,
                        last_export,
                        segments,
                        speaker_names,
                        summarizing,
                        compressing,
                        diarizing,
                        retranscribing,
                        diar_speakers,
                        diarize_est_secs,
                        confirm_retranscribe,
                        show_export_menu,
                        show_generate_menu,
                        find_open,
                        timeline_open,
                        timeline_has_audio: {
                            let af = audio_files.read();
                            af.me.is_some() || af.others.is_some() || !af.speakers.is_empty()
                        },
                        on_open_timeline: {
                            let id2 = id.clone();
                            let engine2 = engine.clone();
                            move |_| {
                                timeline_open.set(true);
                                timeline_lanes.write().clear();
                                let _ = engine2.db_tx.send(DbCmd::LoadTimeline(id2.clone()));
                            }
                        },
                        on_close_timeline: {
                            let engine3 = engine.clone();
                            move |_| {
                                close_timeline(&engine3, timeline_open, timeline_lanes, timeline_pos);
                            }
                        },
                        stats_open,
                        on_toggle_stats: {
                            let id2 = id.clone();
                            let engine2 = engine.clone();
                            move |_| {
                                let was_open = *stats_open.peek();
                                stats_open.set(!was_open);
                                if !was_open {
                                    // Opening: (re-)compute stats.
                                    let _ = engine2.db_tx.send(DbCmd::LoadStats(id2.clone()));
                                }
                            }
                        },
                    }
                }

                // Find button for the Live view (Session view has it in the toolbar).
                if *view.read() == View::Live {
                    div { class: "live-find-row",
                        button {
                            class: if *find_open.read() { "tbtn find-active" } else { "tbtn" },
                            title: "Find in live transcript",
                            onclick: move |_| {
                                let v = *find_open.peek();
                                find_open.set(!v);
                            },
                            {icon("search")}
                            "Find"
                        }
                    }
                }

                // Phase 40: find-in-session bar (shown below toolbar for both views).
                if matches!(&*view.read(), View::Session(_) | View::Live) {
                    FindBar {
                        find_open,
                        find_query,
                        find_active,
                        find_hits: find_hits_computed,
                        highlight: highlight_seg,
                    }
                }

                // AI summary (when present for the viewed session).
                SummaryPanel { view, summary, show_summary, notice, engine: engine.clone() }

                // Dense-prose compression (Phase 23). Machine-oriented, so the
                // body is collapsed by default; the user can expand or copy it.
                CompressedPanel { view, compressed, show_compressed, notice, engine: engine.clone() }

                // Host notes now live in a right-side drawer (see below), so they
                // sit beside the transcript during recording + review rather than
                // stacking on top.

                // Speaker legend (rename diarized speakers) — only for a saved
                // session that has speaker labels.
                if let View::Session(id) = &*view.read() {
                    SpeakerLegend { id: id.clone(), segments, speaker_names, me_speaker, engine: engine.clone() }
                }

                // Phase 46: conversation analytics stats card.
                if let View::Session(_) = &*view.read() {
                    if *stats_open.read() {
                        StatsPanel {
                            stats: session_stats,
                            segments,
                            speaker_names,
                            me_speaker,
                            on_close: move |_| stats_open.set(false),
                        }
                    }
                }

                // Transcript / results. Pass signals so the list subscribes and
                // re-renders itself; App is not re-rendered on each new segment.
                if *view.read() == View::Search {
                    SearchView {
                        results: search_results,
                        note_results,
                        sessions,
                        query: search_query,
                        mode: search_mode,
                        on_query,
                        on_open: on_open_result,
                        engine: engine.clone(),
                    }
                } else if *view.read() == View::Speakers {
                    {
                        let engine = engine.clone();
                        rsx! {
                            speakers::SpeakersView {
                                voiceprints,
                                settings,
                                engine,
                                on_open_session: on_open_from_speakers,
                            }
                        }
                    }
                } else {
                    div { class: "transcript",
                        if *view.read() == View::Overview {
                            {
                                let engine = engine.clone();
                                rsx! {
                                    overview::OverviewDocView {
                                        doc: overview_doc,
                                        doc_updated: overview_doc_updated,
                                        editing: overview_editing,
                                        draft: overview_draft,
                                        notice,
                                        engine,
                                        settings,
                                    }
                                }
                            }
                        } else if *view.read() == View::Live {
                            // Capture-only mode (Phase 25): no live text by design.
                            if recording && !settings.read().live_transcription {
                                div { class: "empty",
                                    if settings.read().auto_transcribe {
                                        "Recording (capture only) — the transcript will be generated when you stop. Live transcription can be turned back on in Settings → Transcription."
                                    } else {
                                        "Recording (capture only) — transcription is deferred. Press Re-transcribe on the session afterwards, or turn on 'Transcribe automatically after recording' in Settings → Transcription."
                                    }
                                }
                            } else {
                                // Live view: timeline panel is not shown for live sessions.
                                TranscriptView { segments: live_segments, speaker_names, highlight: highlight_seg, on_edit: on_edit_segment, audio: audio_files, me_speaker, playing: playing_seg, on_play: on_play_segment, find_hits: find_hits_computed, on_seek: None }
                            }
                        } else {
                            // Saved session: pass an on_seek handler when the timeline is open.
                            {
                                let on_seek_handler = if *timeline_open.read() {
                                    let engine_seek = engine.clone();
                                    // In-place seek: the play worker reuses the
                                    // current mix's track set (no-op + notice when
                                    // nothing is loaded yet).
                                    Some(EventHandler::new(move |ms: u64| {
                                        let _ = engine_seek
                                            .play_tx
                                            .send(PlayCmd::TimelineSeek { start_ms: ms });
                                    }))
                                } else {
                                    None
                                };
                                rsx! {
                                    TranscriptView { segments, speaker_names, highlight: highlight_seg, on_edit: on_edit_segment, audio: audio_files, me_speaker, playing: playing_seg, on_play: on_play_segment, find_hits: find_hits_computed, on_seek: on_seek_handler }
                                }
                            }
                        }
                    }
                }

                // Phase 42c: session timeline panel — only for saved sessions.
                {
                    let tl_sid = match &*view.read() {
                        View::Session(id) => Some(id.clone()),
                        _ => None,
                    };
                    let tl_open = *timeline_open.read();
                    if let (Some(sid), true) = (tl_sid, tl_open) {
                        let engine_tl = engine.clone();
                        rsx! {
                            timeline::TimelinePanel {
                                session_id: sid,
                                lanes: timeline_lanes,
                                pos: timeline_pos,
                                lane_enabled,
                                merged: timeline_merged,
                                on_close: move |_| {
                                    close_timeline(&engine_tl, timeline_open, timeline_lanes, timeline_pos);
                                },
                                audio: audio_files,
                                speaker_names,
                                me_speaker,
                                segments,
                                engine: engine.clone(),
                                highlight: highlight_seg,
                            }
                        }
                    } else {
                        rsx! {}
                    }
                }

                // Chat (Phase 23d): per-meeting in a session, cross-meeting in the
                // Overview. Shares one conversation signal (reset when the scope
                // changes), so only the visible panel is active.
                if let View::Session(id) = &*view.read() {
                    {
                        let id = id.clone();
                        let engine = engine.clone();
                        rsx! {
                            ChatPanel {
                                title: "Ask this meeting".to_string(),
                                placeholder: "Ask about this meeting — decisions, action items, who said what…".to_string(),
                                chat, input: chat_input, busy: chat_busy, show: show_chat,
                                on_send: move |_| submit_chat(&engine, ChatScope::Meeting(id.clone()), chat, chat_input, chat_scope, chat_busy),
                            }
                        }
                    }
                }
                if *view.read() == View::Overview {
                    {
                        let engine = engine.clone();
                        rsx! {
                            ChatPanel {
                                title: "Ask across your meetings".to_string(),
                                placeholder: "Ask across recent meetings — where's project X, what do I owe, open questions…".to_string(),
                                chat, input: chat_input, busy: chat_busy, show: show_chat,
                                on_send: move |_| submit_chat(&engine, ChatScope::CrossMeeting, chat, chat_input, chat_scope, chat_busy),
                            }
                        }
                    }
                }
            }
        }

        // ---- Background jobs panel ----
        if *show_jobs.read() && !jobs.read().is_empty() {
            JobsPanel { show_jobs, jobs, job_tick, diarize_est_secs, on_cancel: on_cancel_job }
        }

        // ---- Session notes drawer (right side) ----
        // Targets the viewed session, or the live one while recording (its row
        // exists from the start of capture, so notes save immediately).
        NotesDrawer { view, recording, live_session_id, show_notes, notes, notes_draft, engine: engine.clone() }

        // ---- Confirm-delete dialog ----
        ConfirmDeleteDialog { confirm_delete, view, segments, summary, compressed, engine: engine.clone() }

        // ---- Confirm-bulk-delete dialog (Phase 43f) ----
        ConfirmBulkDeleteDialog { confirm_bulk_delete, selected_sessions, view, segments, summary, compressed, engine: engine.clone() }

        // ---- Confirm-retranscribe dialog (Phase 25c) ----
        ConfirmRetranscribeDialog { confirm_retranscribe, settings, retranscribing, notice, engine: engine.clone() }

        // ---- Full-screen settings overlay ----
        if *show_settings.read() {
            SettingsOverlay {
                show_settings,
                settings,
                settings_tab,
                download_help,
                models,
                model_progress,
                remote_models,
                notice,
                devices: devices.clone(),
                engine: engine.clone(),
                show_wizard,
            }
        }

        // ---- First-run setup wizard (Phase 36b) ----
        if *show_wizard.read() {
            wizard::SetupWizard {
                settings,
                show_wizard,
                engine: engine.clone(),
                devices: devices.clone(),
                me_level,
                models,
                model_progress,
                notice,
            }
        }
    }
}

/// Left icon rail: global navigation (top) + utilities (bottom).
#[component]
fn IconRail(
    view: Signal<View>,
    jobs: Signal<Vec<JobView>>,
    mut show_jobs: Signal<bool>,
    on_open_overview: EventHandler<MouseEvent>,
    on_open_search: EventHandler<MouseEvent>,
    on_open_speakers: EventHandler<MouseEvent>,
    on_toggle_settings: EventHandler<MouseEvent>,
) -> Element {
    rsx! {
            nav { class: "rail",
                div { class: "rail-top",
                    div { class: "rail-brand", "Z" }
                    button {
                        class: if matches!(&*view.read(), View::Overview) { "rail-btn active" } else { "rail-btn" },
                        title: "Overview — a project-grouped rollup across recent meetings",
                        onclick: move |e| on_open_overview.call(e),
                        {icon("overview")}
                    }
                    button {
                        class: if matches!(&*view.read(), View::Search) { "rail-btn active" } else { "rail-btn" },
                        title: "Search across every meeting's transcript",
                        onclick: move |e| on_open_search.call(e),
                        {icon("search")}
                    }
                    if cfg!(feature = "voiceprints") {
                        button {
                            class: if matches!(&*view.read(), View::Speakers) { "rail-btn active" } else { "rail-btn" },
                            title: "Speakers — people Zord can recognize across meetings",
                            onclick: move |e| on_open_speakers.call(e),
                            {icon("people")}
                        }
                    }
                }
                div { class: "rail-bottom",
                    if !jobs.read().is_empty() {
                        button {
                            class: "rail-btn jobs",
                            title: "Background jobs",
                            onclick: move |_| { let v = *show_jobs.peek(); show_jobs.set(!v); },
                            span { class: "jobs-spin" }
                        }
                    }
                    button {
                        class: "rail-btn",
                        title: "Settings",
                        onclick: move |e| on_toggle_settings.call(e),
                        {icon("settings")}
                    }
                }
            }
    }
}

/// Sessions sidebar: title filter, the pinned "Current recording" row, the
/// saved-session list (open / rename / delete), and the Record + mute foot.
#[component]
fn SessionsSidebar(
    sidebar_w: Signal<u32>,
    sessions: Signal<Vec<Session>>,
    /// The in-progress recording's session id. Its row is the pinned
    /// "Current recording" entry — keep it OUT of the saved-session list
    /// until the recording fully ends (mid-recording refreshes like
    /// auto-titling or the compress sweep re-emit the list while it's live).
    live_session_id: Signal<Option<String>>,
    mut session_filter: Signal<String>,
    session_badges: Signal<std::collections::HashMap<String, (bool, bool, bool)>>,
    view: Signal<View>,
    mut editing: Signal<Option<String>>,
    mut edit_text: Signal<String>,
    mut confirm_delete: Signal<Option<String>>,
    /// Phase 43f: multi-session selection.
    mut selected_sessions: Signal<std::collections::HashSet<String>>,
    mut select_anchor: Signal<Option<String>>,
    mut confirm_bulk_delete: Signal<bool>,
    rec_secs: Signal<u64>,
    recording: bool,
    discord_button: bool,
    recording_discord: bool,
    st: Status,
    status_text: String,
    tint_badges: bool,
    mic_in_capture: bool,
    system_in_capture: bool,
    mic_muted: Signal<bool>,
    sys_muted: Signal<bool>,
    mut settings: Signal<Settings>,
    engine: Engine,
    on_open_session: EventHandler<String>,
    on_show_live: EventHandler<()>,
    on_record: EventHandler<bool>,
    on_mute: EventHandler<MouseEvent>,
    on_mute_system: EventHandler<MouseEvent>,
) -> Element {
    // Per-app capture picker (Phase 43g): mirrors the AudioInputSettings picker.
    // We populate lazily (on mount when mode == "app", and when the select is
    // opened) to avoid firing the Screen Recording permission prompt eagerly.
    let mut sidebar_apps = use_signal(Vec::<(String, String)>::new);
    let mut sidebar_apps_err = use_signal(|| None::<String>);
    let mut sidebar_refresh_apps = move || match zord_capture::list_capturable_apps() {
        Ok(list) => {
            sidebar_apps.set(list.into_iter().map(|a| (a.id, a.name)).collect());
            sidebar_apps_err.set(None);
        }
        Err(e) => sidebar_apps_err.set(Some(e.to_string())),
    };
    // Populate on mount when already in "app" mode (user set it in Settings earlier).
    use_effect(move || {
        if settings.read().capture_mode == "app" {
            sidebar_refresh_apps();
        }
    });

    rsx! {
            aside { class: "sidebar", style: "width: {sidebar_w}px;",
                div { class: "side-label", "Sessions" }
                if sessions.read().len() > 6 {
                    input {
                        class: "session-filter",
                        placeholder: "Filter by title…",
                        value: "{session_filter}",
                        oninput: move |e| {
                            session_filter.set(e.value());
                            // Clear selection when filter changes.
                            selected_sessions.write().clear();
                            select_anchor.set(None);
                        },
                    }
                }
                // Phase 43f: action bar when rows are selected.
                {
                    let sel_count = selected_sessions.read().len();
                    if sel_count > 0 {
                        let engine_bulk = engine.clone();
                        rsx! {
                            div { class: "session-select-bar",
                                span { "{sel_count} selected" }
                                button {
                                    class: "mbtn ghost",
                                    onclick: move |_| confirm_bulk_delete.set(true),
                                    {icon("trash")} "Delete"
                                }
                                button {
                                    class: "mbtn ghost",
                                    onclick: move |_| {
                                        selected_sessions.write().clear();
                                        select_anchor.set(None);
                                    },
                                    "Clear"
                                }
                                // Suppress unused-variable warning.
                                { let _ = &engine_bulk; rsx!{} }
                            }
                        }
                    } else {
                        rsx! {}
                    }
                }
                div { class: "session-list",
                    // While recording, a pinned entry to jump back to the live view
                    // (the in-progress session isn't in the saved list until it ends).
                    if recording {
                        div {
                            class: if matches!(&*view.read(), View::Live) { "session live-rec active" } else { "session live-rec" },
                            onclick: move |_| on_show_live.call(()),
                            div { class: "session-row",
                                div { class: "session-title",
                                    span { class: "rec-pip" }
                                    "Current recording"
                                }
                                div { class: "session-meta",
                                    if matches!(st, Status::Recording) { "{fmt_dur(rec_secs())}" } else { "{status_text}" }
                                }
                            }
                        }
                    }
                    {
                        // Filter by title, then tag the first row of each date group.
                        let q = session_filter.read().to_lowercase();
                        let now = now_ms();
                        let mut last_group: Option<&'static str> = None;
                        let live = live_session_id.read().clone();
                        let items: Vec<(Option<&'static str>, Session)> = sessions
                            .read()
                            .iter()
                            // The live recording is the pinned row above, not a
                            // saved session yet — it joins this list at Idle
                            // (mid-recording list refreshes must not leak it in).
                            .filter(|s| live.as_deref() != Some(s.id.as_str()))
                            .filter(|s| q.is_empty() || session_title(s).to_lowercase().contains(q.as_str()))
                            .cloned()
                            .map(|s| {
                                let g = date_group(s.started_at, now);
                                let hdr = if last_group != Some(g) { last_group = Some(g); Some(g) } else { None };
                                (hdr, s)
                            })
                            .collect();
                        // Flat ordered id list for range selection.
                        let ordered_ids: Vec<String> = items.iter().map(|(_, s)| s.id.clone()).collect();
                        let empty_msg = if sessions.read().is_empty() {
                            "No recordings yet."
                        } else {
                            "No matching sessions."
                        };
                        rsx! {
                            if items.is_empty() {
                                div { class: "empty", "{empty_msg}" }
                            }
                            for (group_hdr, s) in items {
                        {
                            let id = s.id.clone();
                            let is_selected = selected_sessions.read().contains(&id);
                            let active = matches!(&*view.read(), View::Session(v) if *v == id);
                            let is_editing = editing.read().as_deref() == Some(id.as_str());
                            let title = session_title(&s);
                            let meta = session_meta(&s);
                            let (b_sum, b_comp, b_spk) =
                                session_badges.read().get(&id).copied().unwrap_or((false, false, false));
                            let b_audio = s.audio_path.is_some();
                            let eng_save = engine.clone();
                            let (id_edit, id_save, id_del) =
                                (id.clone(), id.clone(), id.clone());
                            let title_edit = title.clone();
                            let ordered_ids_row = ordered_ids.clone();
                            let id_click = id.clone();
                            let row_class = match (active, is_selected) {
                                (true, true) => "session active selected",
                                (true, false) => "session active",
                                (false, true) => "session selected",
                                (false, false) => "session",
                            };
                            rsx! {
                                div { key: "{s.id}", class: "session-wrap",
                                if let Some(h) = group_hdr {
                                    div { class: "date-group", "{h}" }
                                }
                                div {
                                    class: row_class,
                                    if is_editing {
                                        input {
                                            class: "rename-input",
                                            value: "{edit_text}",
                                            autofocus: true,
                                            oninput: move |e| edit_text.set(e.value()),
                                            onkeydown: move |e| match e.key() {
                                                Key::Enter => {
                                                    let t = edit_text.peek().trim().to_string();
                                                    if !t.is_empty() {
                                                        let _ = eng_save.db_tx.send(DbCmd::Rename { id: id_save.clone(), title: t });
                                                    }
                                                    editing.set(None);
                                                }
                                                Key::Escape => editing.set(None),
                                                _ => {}
                                            },
                                        }
                                    } else {
                                        div { class: "session-row",
                                            onclick: move |e: MouseEvent| {
                                                let mods = e.modifiers();
                                                use dioxus::html::input_data::keyboard_types::Modifiers;
                                                if mods.contains(Modifiers::META) || mods.contains(Modifiers::CONTROL) {
                                                    // Cmd/Ctrl-click: toggle membership.
                                                    let already = selected_sessions.peek().contains(&id_click);
                                                    if already {
                                                        selected_sessions.write().remove(&id_click);
                                                    } else {
                                                        selected_sessions.write().insert(id_click.clone());
                                                    }
                                                    select_anchor.set(Some(id_click.clone()));
                                                } else if mods.contains(Modifiers::SHIFT) {
                                                    // Shift-click: extend range from anchor.
                                                    let anchor = select_anchor.peek().clone();
                                                    let range = if let Some(ref a) = anchor {
                                                        range_ids(&ordered_ids_row, a, &id_click)
                                                    } else {
                                                        vec![id_click.clone()]
                                                    };
                                                    for rid in range {
                                                        selected_sessions.write().insert(rid);
                                                    }
                                                    select_anchor.set(Some(id_click.clone()));
                                                } else if !selected_sessions.peek().is_empty() {
                                                    // Plain click with existing selection: clear and open.
                                                    selected_sessions.write().clear();
                                                    select_anchor.set(None);
                                                    on_open_session.call(id_click.clone());
                                                } else {
                                                    // Plain click, no selection.
                                                    on_open_session.call(id_click.clone());
                                                }
                                            },
                                            div { class: "session-title", "{title}" }
                                            div { class: "session-meta",
                                                span { "{meta}" }
                                                span { class: "badges",
                                                    if b_sum { span { class: if tint_badges { "badge tint-sum" } else { "badge" }, title: "Has summary", {icon("sparkles")} } }
                                                    if b_comp { span { class: if tint_badges { "badge tint-comp" } else { "badge" }, title: "Compressed", {icon("archive")} } }
                                                    if b_spk { span { class: if tint_badges { "badge tint-spk" } else { "badge" }, title: "Speakers identified", {icon("users")} } }
                                                    if b_audio { span { class: if tint_badges { "badge tint-audio" } else { "badge" }, title: "Audio kept", {icon("headphones")} } }
                                                }
                                            }
                                        }
                                        div { class: "session-actions",
                                            button {
                                                class: "row-btn",
                                                title: "Rename",
                                                onclick: move |_| {
                                                    edit_text.set(title_edit.clone());
                                                    editing.set(Some(id_edit.clone()));
                                                },
                                                {icon("pen")}
                                            }
                                            button {
                                                class: "row-btn",
                                                title: "Delete",
                                                onclick: move |_| confirm_delete.set(Some(id_del.clone())),
                                                {icon("trash")}
                                            }
                                        }
                                    }
                                }
                                }
                            }
                        }
                    }
                        }
                    }
                }
                // ---- Record: permanent primary at the sidebar foot ----
                div { class: "sidebar-foot",
                    // Per-app capture picker (Phase 43g): compact app selector
                    // visible when system audio is in the capture mode AND not
                    // currently recording. The choice is locked in at Record
                    // start, so hiding it while recording avoids confusion.
                    if !recording && system_in_capture && !recording_discord {
                        div {
                            class: "app-capture-row",
                            title: "Record one application's audio instead of the whole desktop",
                            select {
                                class: if settings.read().capture_app_id.is_empty() { "app-capture-select" } else { "app-capture-select picked" },
                                onmousedown: move |_| {
                                    // Refresh on open — catches newly launched apps
                                    // without requiring a separate Refresh button.
                                    sidebar_refresh_apps();
                                },
                                onchange: move |e: FormEvent| {
                                    let val = e.value();
                                    let mut s = settings.peek().clone();
                                    if val.is_empty() {
                                        // "All system audio" choice.
                                        s.capture_app_id = String::new();
                                        s.capture_app_name = String::new();
                                        // Restore the mode the user had before
                                        // switching into app capture ("system"-only
                                        // users must not be flipped to "both").
                                        if s.capture_mode == "app" {
                                            s.capture_mode = s.capture_mode_before_app.clone();
                                        }
                                    } else {
                                        s.capture_app_id = val.clone();
                                        s.capture_app_name = sidebar_apps
                                            .peek()
                                            .iter()
                                            .find(|(id, _)| *id == val)
                                            .map(|(_, name)| name.clone())
                                            .unwrap_or_default();
                                        // Remember the prior mode so "All system
                                        // audio" can restore it later.
                                        if s.capture_mode != "app" {
                                            s.capture_mode_before_app = s.capture_mode.clone();
                                        }
                                        s.capture_mode = "app".to_string();
                                    }
                                    let _ = s.save();
                                    settings.set(s);
                                },
                                // "All system audio" option — selected when
                                // capture_app_id is empty (i.e. default mode).
                                option {
                                    value: "",
                                    selected: settings.read().capture_app_id.is_empty(),
                                    "Capture: All system audio"
                                }
                                // Keep the saved choice visible even when that app
                                // is not currently running.
                                if !settings.read().capture_app_id.is_empty()
                                    && !sidebar_apps.read().iter().any(|(id, _)| *id == settings.read().capture_app_id)
                                {
                                    option {
                                        value: "{settings.read().capture_app_id}",
                                        selected: true,
                                        "Capture: {settings.read().capture_app_name} (not running)"
                                    }
                                }
                                for (id, name) in sidebar_apps.read().iter() {
                                    option {
                                        value: "{id}",
                                        selected: settings.read().capture_app_id == *id,
                                        "Capture: {name}"
                                    }
                                }
                            }
                            if let Some(e) = sidebar_apps_err.read().clone() {
                                span { class: "app-capture-err", title: "{e}", "⚠" }
                            }
                        }
                    }
                    if recording && system_in_capture && !recording_discord {
                        button {
                            class: if *sys_muted.read() { "record muted" } else { "record mute" },
                            title: if *sys_muted.read() { "Desktop audio muted — click to unmute" } else { "Mute desktop / system audio" },
                            onclick: move |e| on_mute_system.call(e),
                            {icon(if *sys_muted.read() { "speaker-off" } else { "speaker" })}
                            if *sys_muted.read() { "Unmute desktop" } else { "Mute desktop" }
                        }
                    }
                    if recording && mic_in_capture && !recording_discord {
                        button {
                            class: if *mic_muted.read() { "record muted" } else { "record mute" },
                            title: if *mic_muted.read() { "Mic muted — click to unmute" } else { "Mute your microphone" },
                            onclick: move |e| on_mute.call(e),
                            {icon(if *mic_muted.read() { "mic-off" } else { "mic" })}
                            if *mic_muted.read() { "Unmute mic" } else { "Mute mic" }
                        }
                    }
                    if !recording && discord_button {
                        button {
                            class: "record discord",
                            title: "Record the Discord voice channel you're in (via your bot)",
                            onclick: move |_| on_record.call(true),
                            {icon("headphones")}
                            "Record Discord"
                        }
                    }
                    button {
                        class: if recording { "record stop" } else { "record" },
                        onclick: move |_| on_record.call(false),
                        {icon(if recording { "stop" } else { "record" })}
                        if recording { "Stop" } else { "Record" }
                    }
                }
            }
    }
}

/// Toolbar above a saved session's transcript: the Generate ▾ menu
/// (Summarize / Compress / Identify speakers / Re-transcribe) and the output
/// cluster (Copy, Export ▾, Reveal/Open the last export).
#[component]
fn SessionToolbar(
    id: String,
    engine: Engine,
    settings: Signal<Settings>,
    notice: Signal<Option<String>>,
    sessions: Signal<Vec<Session>>,
    summary: Signal<Option<String>>,
    compressed: Signal<Option<String>>,
    audio_files: Signal<AudioFiles>,
    last_export: Signal<Option<String>>,
    segments: Signal<Vec<Segment>>,
    speaker_names: Signal<std::collections::HashMap<i32, String>>,
    summarizing: Signal<bool>,
    compressing: Signal<bool>,
    diarizing: Signal<bool>,
    retranscribing: Signal<bool>,
    diar_speakers: Signal<String>,
    diarize_est_secs: Signal<Option<u64>>,
    confirm_retranscribe: Signal<Option<String>>,
    show_export_menu: Signal<bool>,
    show_generate_menu: Signal<bool>,
    find_open: Signal<bool>,
    /// Phase 42c: whether the timeline panel is open.
    timeline_open: Signal<bool>,
    /// Whether any audio files exist for timeline (controls enabling the button).
    timeline_has_audio: bool,
    /// Open the timeline panel (fires DbCmd::LoadTimeline).
    on_open_timeline: EventHandler<MouseEvent>,
    /// Close the timeline panel — routes through `close_timeline` in MainApp
    /// so playback stops and lane/pos state clears (same path as the panel's ×).
    on_close_timeline: EventHandler<()>,
    /// Phase 46: whether the stats card is open.
    stats_open: Signal<bool>,
    /// Phase 46: toggle the stats card; fires DbCmd::LoadStats when opening.
    on_toggle_stats: EventHandler<()>,
) -> Element {
    let sid = id.clone();
    let eng_sum = engine.clone();
    let eng_comp = engine.clone();
    let sid_comp = id.clone();
    let eng_diar = engine.clone();
    let sid_diar = id.clone();
    let sid_rt = id.clone();
    let eng_fold = engine.clone();
    let sid_fold = id.clone();
    let eng_maudio = engine.clone();
    let sid_maudio = id.clone();
    let mk = move |fmt: Format| {
        let id = id.clone();
        let engine = engine.clone();
        move |_| {
            let _ = engine.db_tx.send(DbCmd::Export {
                id: id.clone(),
                format: fmt,
            });
            show_export_menu.set(false);
        }
    };
    // Contextual state for the Generate menu rows.
    let has_summary = summary.read().is_some();
    let has_compressed = compressed.read().is_some();
    let has_others_audio = audio_files.read().others.is_some();
    let has_any_audio = {
        let af = audio_files.read();
        af.me.is_some() || af.others.is_some() || !af.speakers.is_empty()
    };
    let gen_busy = summarizing() || compressing() || diarizing() || retranscribing();
    rsx! {
        div { class: "toolbar",
            // --- Generate ▾ : stable surface, contents adapt ---
            div { class: "gen-dd",
                button {
                    class: if gen_busy { "tbtn busy" } else { "tbtn" },
                    title: "Run AI / speaker actions on this meeting",
                    onclick: move |_| { let v = *show_generate_menu.peek(); show_generate_menu.set(!v); },
                    {icon("sparkles")}
                    if gen_busy { "Working… ▾" } else { "Generate ▾" }
                }
                if *show_generate_menu.read() {
                    div { class: "dd-backdrop", onclick: move |_| show_generate_menu.set(false) }
                    div { class: "gen-menu",
                        // Items run top-to-bottom in PIPELINE order — the order
                        // you'd actually use them: transcript first, then who
                        // said it, then the AI digests, then the rollup.
                        // Re-transcribe — needs kept audio.
                        button {
                            class: "gen-item",
                            disabled: retranscribing() || !has_any_audio,
                            title: if has_any_audio { "Regenerate the transcript from kept audio with the re-transcription model" } else { "Needs kept audio" },
                            onclick: move |_| {
                                show_generate_menu.set(false);
                                if *retranscribing.peek() || !has_any_audio { return; }
                                confirm_retranscribe.set(Some(sid_rt.clone()));
                            },
                            span { {icon("refresh")}  { if retranscribing() { "Re-transcribing…" } else { "Re-transcribe" } } }
                            if !has_any_audio { span { class: "gen-state", "no audio" } }
                        }
                        // Identify speakers (+ expected-count input) — needs the Others track.
                        div { class: "gen-item-row",
                            button {
                                class: "gen-item grow",
                                disabled: diarizing() || !has_others_audio,
                                title: if has_others_audio { "Group the 'Others' channel into individual speakers" } else { "Needs the kept 'Others' audio" },
                                onclick: move |_| {
                                    show_generate_menu.set(false);
                                    if *diarizing.peek() { return; }
                                    diarizing.set(true);
                                    // Rough ETA: diarization runs ~6x faster than real time on CPU.
                                    let est = sessions.peek().iter()
                                        .find(|s| s.id == sid_diar)
                                        .and_then(|s| s.ended_at.map(|e| e.saturating_sub(s.started_at) / 1000))
                                        .map(|secs| (secs / 6).max(15));
                                    diarize_est_secs.set(est);
                                    notice.set(Some("Identifying speakers… (first run downloads the speaker model)".to_string()));
                                    let n = diar_speakers.peek().trim().parse::<u32>().unwrap_or(0);
                                    let _ = eng_diar.db_tx.send(DbCmd::Diarize { id: sid_diar.clone(), num_speakers: n });
                                },
                                span { {icon("users")}  { if diarizing() { "Identifying…" } else { "Identify speakers" } } }
                                if !has_others_audio { span { class: "gen-state", "no audio" } }
                            }
                            input {
                                class: "spk-count",
                                r#type: "number",
                                min: "0",
                                placeholder: "auto",
                                title: "How many people spoke (blank = auto-detect). Remembered per session.",
                                value: "{diar_speakers}",
                                oninput: move |e| diar_speakers.set(e.value()),
                            }
                        }
                        // Summarize
                        button {
                            class: "gen-item",
                            disabled: summarizing(),
                            onclick: move |_| {
                                show_generate_menu.set(false);
                                if *summarizing.peek() { return; }
                                summarizing.set(true);
                                notice.set(Some(if settings.peek().llm_backend == "external" {
                                    "Summarizing via the external LLM server…".to_string()
                                } else {
                                    "Summarizing… (first run downloads the model)".to_string()
                                }));
                                let _ = eng_sum.summ_tx.send(SummCmd::Summarize(sid.clone()));
                            },
                            span { {icon("sparkles")}  { if has_summary { "Re-summarize" } else { "Summarize" } } }
                            if summarizing() { span { class: "gen-state", "running…" } }
                            else if has_summary { span { class: "gen-state ok", {icon("check")} } }
                        }
                        // Compress
                        button {
                            class: "gen-item",
                            title: "Condense this meeting line-by-line into its token-minimal form (feeds the Overview)",
                            disabled: compressing(),
                            onclick: move |_| {
                                show_generate_menu.set(false);
                                if *compressing.peek() { return; }
                                compressing.set(true);
                                notice.set(Some(if settings.peek().llm_backend == "external" {
                                    "Compressing via the external LLM server…".to_string()
                                } else {
                                    "Compressing… (first run downloads the model)".to_string()
                                }));
                                let _ = eng_comp.summ_tx.send(SummCmd::Compress(sid_comp.clone()));
                            },
                            span { {icon("archive")}  { if has_compressed { "Re-compress" } else { "Compress" } } }
                            if compressing() { span { class: "gen-state", "running…" } }
                            else if has_compressed { span { class: "gen-state ok", {icon("check")} } }
                        }
                        // Update Overview — fold THIS meeting into the living
                        // document (also the manual fix-up after a range
                        // re-transcribe, which leaves the auto fold stale).
                        button {
                            class: "gen-item",
                            title: "Fold this meeting into the living Overview document",
                            onclick: move |_| {
                                show_generate_menu.set(false);
                                notice.set(Some("Updating the Overview from this meeting…".to_string()));
                                let _ = eng_fold.summ_tx.send(SummCmd::UpdateOverviewDoc {
                                    session: Some(sid_fold.clone()),
                                });
                            },
                            span { {icon("overview")}  "Update Overview" }
                        }
                    }
                }
            }

            // --- Output cluster (right) ---
            div { class: "export-spacer" }
            // Phase 46: stats card toggle button.
            button {
                class: if *stats_open.read() { "tbtn find-active" } else { "tbtn" },
                title: "Show meeting analytics (talk time, WPM, interruptions, silence ratio…)",
                onclick: move |_| on_toggle_stats.call(()),
                {icon("overview")}
                "Stats"
            }
            // Phase 42c: timeline toggle button.
            button {
                class: if *timeline_open.read() { "tbtn find-active" } else { "tbtn" },
                title: if timeline_has_audio { "Show session timeline" } else { "No audio files — keep audio to enable the timeline" },
                disabled: !timeline_has_audio,
                onclick: move |e| {
                    if *timeline_open.peek() {
                        // Same close path as the panel's × — stops playback too.
                        on_close_timeline.call(());
                    } else {
                        on_open_timeline.call(e);
                    }
                },
                {icon("waveform")}
                "Timeline"
            }
            button {
                class: if *find_open.read() { "tbtn find-active" } else { "tbtn" },
                title: "Find in transcript (search within this session)",
                onclick: move |_| {
                    let v = *find_open.peek();
                    find_open.set(!v);
                },
                {icon("search")}
                "Find"
            }
            button {
                class: "tbtn",
                title: "Copy the transcript to the clipboard",
                onclick: move |_| {
                    let names = speaker_names.peek().clone();
                    let text = segments
                        .peek()
                        .iter()
                        .map(|s| format!("[{} {}] {}", fmt_ts(s.t_start_ms), s.speaker_label(&names), s.text))
                        .collect::<Vec<_>>()
                        .join("\n");
                    osutil::copy_to_clipboard(&text);
                    notice.set(Some("Transcript copied to clipboard".to_string()));
                },
                {icon("copy")}
                "Copy"
            }
            div { class: "export-dd",
                button {
                    class: "tbtn",
                    title: "Export this transcript to a file",
                    onclick: move |_| { let v = *show_export_menu.peek(); show_export_menu.set(!v); },
                    {icon("export")}
                    "Export ▾"
                }
                if *show_export_menu.read() {
                    div { class: "dd-backdrop", onclick: move |_| show_export_menu.set(false) }
                    div { class: "export-menu",
                        button { class: "export-menu-item", onclick: mk(Format::Markdown), "Markdown (.md)" }
                        button { class: "export-menu-item", onclick: mk(Format::Srt), "Subtitles (.srt)" }
                        button { class: "export-menu-item", onclick: mk(Format::Json), "JSON (.json)" }
                        if has_any_audio {
                            button {
                                class: "export-menu-item",
                                title: "Mix every kept track into a single WAV",
                                onclick: move |_| {
                                    let _ = eng_maudio.db_tx.send(DbCmd::ExportAudio(sid_maudio.clone()));
                                    show_export_menu.set(false);
                                },
                                "Merged audio (.wav)"
                            }
                        }
                    }
                }
            }
            if let Some(path) = last_export.read().clone() {
                button {
                    class: "tbtn ghost",
                    onclick: {
                        let p = path.clone();
                        move |_| osutil::reveal_in_file_manager(&p)
                    },
                    {icon("folder")} "Reveal"
                }
                button {
                    class: "tbtn ghost",
                    onclick: move |_| osutil::open_in_editor(&path),
                    {icon("external")} "Open"
                }
            }
        }
    }
}

/// Phase 40: find-in-session bar. Shown when `find_open` is true, dismissed by
/// Esc or the × button. Enter cycles forward, Shift-Enter cycles back. The
/// active hit is scrolled into view by setting `highlight`, which reuses the
/// existing scroll-to-segment mechanism.
#[component]
fn FindBar(
    find_open: Signal<bool>,
    find_query: Signal<String>,
    find_active: Signal<usize>,
    find_hits: Signal<Vec<i64>>,
    highlight: Signal<Option<i64>>,
) -> Element {
    if !*find_open.read() {
        return rsx! {};
    }
    let hits = find_hits.read().clone();
    let total = hits.len();
    let active = (*find_active.read()).min(total.saturating_sub(1));
    let count_label = if total == 0 {
        "no matches".to_string()
    } else {
        format!("{} of {}", active + 1, total)
    };

    // Scroll the active hit into view by reusing the highlight signal.
    let mut scroll_to_active = move |idx: usize| {
        let h = find_hits.read();
        if let Some(&id) = h.get(idx) {
            highlight.set(Some(id));
            let _ = document::eval(&format!(
                "requestAnimationFrame(()=>{{const e=document.getElementById('seg-{id}');if(e){{e.scrollIntoView({{block:'center',behavior:'smooth'}});}}}})"
            ));
        }
    };

    // nav_step: +1 = forward, -1 = backward. Used from both onclick and onkeydown.
    let mut nav = move |forward: bool| {
        let t = find_hits.read().len();
        if t == 0 {
            return;
        }
        let cur = *find_active.peek();
        let next = if forward {
            (cur + 1) % t
        } else {
            if cur == 0 {
                t - 1
            } else {
                cur - 1
            }
        };
        find_active.set(next);
        scroll_to_active(next);
    };

    rsx! {
        div { class: "find-bar",
            {icon("search")}
            input {
                class: "find-input",
                r#type: "text",
                placeholder: "Find in transcript…",
                value: "{find_query}",
                autofocus: true,
                oninput: move |e| find_query.set(e.value()),
                onkeydown: move |e| match e.key() {
                    Key::Enter => {
                        nav(!e.modifiers().shift());
                    }
                    Key::Escape => {
                        find_open.set(false);
                        find_query.set(String::new());
                        highlight.set(None);
                    }
                    _ => {}
                },
            }
            span { class: "find-count", "{count_label}" }
            button {
                class: "find-nav",
                title: "Previous match (Shift+Enter)",
                disabled: total == 0,
                onclick: move |_| nav(false),
                "▲"
            }
            button {
                class: "find-nav",
                title: "Next match (Enter)",
                disabled: total == 0,
                onclick: move |_| nav(true),
                "▼"
            }
            button {
                class: "find-close",
                title: "Close find bar (Esc)",
                onclick: move |_| {
                    find_open.set(false);
                    find_query.set(String::new());
                    highlight.set(None);
                },
                {icon("close")}
            }
        }
    }
}

/// Collapsible AI-summary panel for the viewed (or live) session.
#[component]
fn SummaryPanel(
    view: Signal<View>,
    summary: Signal<Option<String>>,
    show_summary: Signal<bool>,
    notice: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    rsx! {
                if matches!(&*view.read(), View::Session(_) | View::Live) {
                    if let Some(text) = summary.read().clone() {
                        {
                            let text_copy = text.clone();
                            let open = *show_summary.read();
                            // Deleting only makes sense for a saved session.
                            let del_id = match &*view.read() {
                                View::Session(id) => Some(id.clone()),
                                _ => None,
                            };
                            let can_del = del_id.is_some();
                            let did = del_id.unwrap_or_default();
                            let eng_del = engine.clone();
                            rsx! {
                                div { class: "summary",
                                    div { class: "summary-head",
                                        button {
                                            class: "panel-toggle",
                                            onclick: move |_| { let v = *show_summary.peek(); show_summary.set(!v); },
                                            span { class: "chev", if open { "▾" } else { "▸" } }
                                            span { "Summary" }
                                        }
                                        button {
                                            class: "row-btn",
                                            onclick: move |_| {
                                                osutil::copy_to_clipboard(&text_copy);
                                                notice.set(Some("Summary copied to clipboard".to_string()));
                                            },
                                            "Copy"
                                        }
                                        if can_del {
                                            button {
                                                class: "row-btn",
                                                title: "Delete this summary (the transcript is kept)",
                                                onclick: move |_| {
                                                    let _ = eng_del.db_tx.send(DbCmd::ClearSummary(did.clone()));
                                                    notice.set(Some("Summary deleted".to_string()));
                                                },
                                                {icon("trash")}
                                            }
                                        }
                                    }
                                    if open {
                                        div { class: "summary-body", "{text}" }
                                    }
                                }
                            }
                        }
                    }
                }
    }
}

/// Collapsible dense-prose compression panel (Phase 23) for the viewed (or
/// live) session — machine-oriented, so the body starts collapsed.
#[component]
fn CompressedPanel(
    view: Signal<View>,
    compressed: Signal<Option<String>>,
    show_compressed: Signal<bool>,
    notice: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    rsx! {
                if matches!(&*view.read(), View::Session(_) | View::Live) {
                    if let Some(text) = compressed.read().clone() {
                        {
                            let text_copy = text.clone();
                            let expanded = *show_compressed.read();
                            // Deleting only makes sense for a saved session.
                            let del_id = match &*view.read() {
                                View::Session(id) => Some(id.clone()),
                                _ => None,
                            };
                            let can_del = del_id.is_some();
                            let did = del_id.unwrap_or_default();
                            let eng_del = engine.clone();
                            rsx! {
                                div { class: "summary compressed",
                                    div { class: "summary-head",
                                        button {
                                            class: "panel-toggle",
                                            onclick: move |_| {
                                                let v = *show_compressed.peek();
                                                show_compressed.set(!v);
                                            },
                                            span { class: "chev", if expanded { "▾" } else { "▸" } }
                                            span { "Compressed (dense)" }
                                        }
                                        button {
                                            class: "row-btn",
                                            onclick: move |_| {
                                                osutil::copy_to_clipboard(&text_copy);
                                                notice.set(Some("Compressed text copied to clipboard".to_string()));
                                            },
                                            "Copy"
                                        }
                                        if can_del {
                                            button {
                                                class: "row-btn",
                                                title: "Delete this compression (the transcript is kept)",
                                                onclick: move |_| {
                                                    let _ = eng_del.db_tx.send(DbCmd::ClearCompressed(did.clone()));
                                                    notice.set(Some("Compressed text deleted".to_string()));
                                                },
                                                {icon("trash")}
                                            }
                                        }
                                    }
                                    if expanded {
                                        div { class: "summary-body", "{text}" }
                                    }
                                }
                            }
                        }
                    }
                }
    }
}

// ---------------------------------------------------------------------------
// Phase 46 — Conversation analytics stats card
// ---------------------------------------------------------------------------

/// Format milliseconds as `H:MM:SS` or `M:SS`.
fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// One-liner standout insight: pick the most interesting metric to surface.
/// Pure-fn heuristic — no LLM.
fn standout_insight(
    stats: &SessionStats,
    names: &std::collections::HashMap<i32, String>,
) -> String {
    // Rule 1: if a single speaker has > 60% talk share, call it out.
    if let Some(top) = stats.speakers.first() {
        if top.talk_share > 0.60 {
            let pct = (top.talk_share * 100.0).round() as u32;
            let label = match &top.key[..] {
                "me" => "You".to_string(),
                "others" => "Others".to_string(),
                key => {
                    // spk-N → resolve display name or "Speaker N".
                    if let Some(n) = key.strip_prefix("spk-").and_then(|s| s.parse::<i32>().ok()) {
                        names
                            .get(&n)
                            .cloned()
                            .unwrap_or_else(|| format!("Speaker {}", n + 1))
                    } else {
                        key.to_string()
                    }
                }
            };
            return format!("{label} spoke {pct}% of this meeting.");
        }
    }
    // Rule 2: if silence ratio is high (> 40%), note it.
    if stats.silence_ratio > 0.40 {
        let pct = (stats.silence_ratio * 100.0).round() as u32;
        return format!("{pct}% of this meeting was silence.");
    }
    // Default: no standout — empty string → not rendered.
    String::new()
}

/// Phase 46 — Conversation analytics card, toggled by the Stats toolbar button.
/// Shown only for `View::Session`; closing it is via the × button or session switch.
#[component]
fn StatsPanel(
    stats: Signal<Option<SessionStats>>,
    segments: Signal<Vec<Segment>>,
    speaker_names: Signal<std::collections::HashMap<i32, String>>,
    me_speaker: Signal<Option<i32>>,
    on_close: EventHandler<()>,
) -> Element {
    let names = speaker_names.read().clone();
    let me_spk = *me_speaker.read();
    let has_transcript = !segments.read().is_empty();

    rsx! {
        div { class: "stats-card",
            div { class: "stats-header",
                span { class: "stats-title", {icon("overview")} " Meeting DNA" }
                button {
                    class: "tl-close",
                    title: "Close stats",
                    onclick: move |_| on_close.call(()),
                    {icon("close")}
                }
            }

            if !has_transcript {
                div { class: "stats-empty", "Transcribe first to see analytics." }
            } else if let Some(s) = stats.read().clone() {
                {
                    // ── header row ──────────────────────────────────────────
                    let meeting_s = s.meeting_ms / 1000;
                    let speech_pct = (s.speech_ms.min(s.meeting_ms) * 100)
                        .checked_div(s.meeting_ms)
                        .unwrap_or(0) as u32;
                    let insight = standout_insight(&s, &names);
                    rsx! {
                        div { class: "stats-meta",
                            span { class: "stats-meta-item",
                                {fmt_dur(meeting_s)} " · " {speech_pct.to_string()} "% speech"
                            }
                        }
                        if !insight.is_empty() {
                            div { class: "stats-insight", "{insight}" }
                        }
                        // ── per-speaker rows ─────────────────────────────────
                        div { class: "stats-speakers",
                            for spk in s.speakers.iter() {
                                {
                                    // Resolve display label (same rules as speaker_label).
                                    let label = match &spk.key[..] {
                                        "me" => "Me".to_string(),
                                        "others" => "Others".to_string(),
                                        key => {
                                            if let Some(n) = key.strip_prefix("spk-")
                                                .and_then(|s| s.parse::<i32>().ok())
                                            {
                                                names.get(&n).cloned()
                                                    .unwrap_or_else(|| format!("Speaker {}", n + 1))
                                            } else {
                                                key.to_string()
                                            }
                                        }
                                    };
                                    // Speaker row accent class (same palette as transcript).
                                    let accent_class = match &spk.key[..] {
                                        "me" => "spk-me".to_string(),
                                        key => {
                                            if let Some(n) = key.strip_prefix("spk-")
                                                .and_then(|s| s.parse::<i32>().ok())
                                            {
                                                // Check if this spk-N is the me_speaker.
                                                if me_spk == Some(n) {
                                                    "spk-me".to_string()
                                                } else {
                                                    format!("spk-{n}")
                                                }
                                            } else {
                                                "spk-others".to_string()
                                            }
                                        }
                                    };
                                    let share_pct = (spk.talk_share * 100.0).round() as u32;
                                    let talk_min = spk.talk_ms / 60_000;
                                    let talk_sec = (spk.talk_ms % 60_000) / 1000;
                                    let wpm = spk.wpm.round() as u32;
                                    let longest = fmt_ms(spk.longest_monologue_ms);
                                    let interruptions = spk.interruptions_made;
                                    let questions = spk.questions;
                                    rsx! {
                                        div { class: "stats-spk-row {accent_class}",
                                            div { class: "stats-spk-name", "{label}" }
                                            div { class: "stats-spk-bar-wrap",
                                                div {
                                                    class: "stats-spk-bar",
                                                    style: "width: {share_pct}%",
                                                }
                                            }
                                            div { class: "stats-spk-detail",
                                                span { "{share_pct}% · " }
                                                span { "{talk_min}:{talk_sec:02} · " }
                                                span { "{wpm} wpm · " }
                                                span { "{questions} Q · " }
                                                span { "longest {longest} · " }
                                                span { "{interruptions} interruptions" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            } else {
                div { class: "stats-empty", "Loading…" }
            }
        }
    }
}

/// Speaker legend for a saved session with diarized speakers: one rename
/// input per speaker index.
#[component]
fn SpeakerLegend(
    id: String,
    segments: Signal<Vec<Segment>>,
    speaker_names: Signal<std::collections::HashMap<i32, String>>,
    me_speaker: Signal<Option<i32>>,
    engine: Engine,
) -> Element {
    rsx! {
        {
                        let mut spk: Vec<i32> =
                            segments.read().iter().filter_map(|s| s.speaker).collect();
                        spk.sort_unstable();
                        spk.dedup();
                        if spk.is_empty() {
                            rsx! {}
                        } else {
                            rsx! {
                                div { class: "speaker-legend",
                                    span { class: "legend-label", "Speakers:" }
                                    for idx in spk {
                                        {
                                            let val = speaker_names.read().get(&idx).cloned().unwrap_or_default();
                                            let engine = engine.clone();
                                            let id = id.clone();
                                            // The app user's own chip carries the Me
                                            // accent stripe (integration sessions tag
                                            // which index is them).
                                            let me = me_speaker.read().is_some_and(|m| m == idx);
                                            let cls = if me {
                                                "speaker-name spk-me".to_string()
                                            } else {
                                                format!("speaker-name spk-{idx}")
                                            };
                                            rsx! {
                                                input {
                                                    key: "{idx}",
                                                    class: "{cls}",
                                                    title: if me { "You" } else { "" },
                                                    value: "{val}",
                                                    placeholder: "Speaker {idx + 1}",
                                                    onchange: move |e: FormEvent| {
                                                        let _ = engine.db_tx.send(DbCmd::RenameSpeaker {
                                                            id: id.clone(),
                                                            speaker: idx,
                                                            name: e.value(),
                                                        });
                                                    },
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
        }
    }
}

/// Confirm-bulk-delete dialog for Phase 43f batch session delete.
#[component]
fn ConfirmBulkDeleteDialog(
    confirm_bulk_delete: Signal<bool>,
    mut selected_sessions: Signal<std::collections::HashSet<String>>,
    view: Signal<View>,
    segments: Signal<Vec<Segment>>,
    summary: Signal<Option<String>>,
    compressed: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    rsx! {
        if *confirm_bulk_delete.read() {
            {
                let engine = engine.clone();
                let count = selected_sessions.read().len();
                let s = if count == 1 { "" } else { "s" };
                rsx! {
                    div { class: "overlay",
                        div { class: "confirm-card",
                            h2 { "Delete {count} session{s}?" }
                            p { class: "field-note",
                                "Delete {count} session{s} and their audio \u{2014} cannot be undone."
                            }
                            div { class: "confirm-actions",
                                button {
                                    class: "mbtn ghost",
                                    onclick: move |_| confirm_bulk_delete.set(false),
                                    "Cancel"
                                }
                                button {
                                    class: "mbtn danger",
                                    onclick: move |_| {
                                        let ids: Vec<String> =
                                            selected_sessions.peek().iter().cloned().collect();
                                        // If the open session is in the batch, reset view.
                                        if ids.iter().any(|id| {
                                            matches!(&*view.peek(), View::Session(v) if v == id)
                                        }) {
                                            view.set(View::Live);
                                            segments.write().clear();
                                            summary.set(None);
                                            compressed.set(None);
                                        }
                                        let _ = engine.db_tx.send(DbCmd::DeleteSessions(ids));
                                        selected_sessions.write().clear();
                                        confirm_bulk_delete.set(false);
                                    },
                                    "Delete"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Right-side session-notes drawer (tab + textarea). Targets the viewed
/// session, or the live one while recording.
#[component]
fn NotesDrawer(
    view: Signal<View>,
    recording: bool,
    live_session_id: Signal<Option<String>>,
    show_notes: Signal<bool>,
    notes: Signal<Option<String>>,
    notes_draft: Signal<String>,
    engine: Engine,
) -> Element {
    rsx! {
        {
            let notes_target = match &*view.read() {
                View::Session(id) => Some(id.clone()),
                _ if recording => live_session_id.read().clone(),
                _ => None,
            };
            notes_target.map(|target| {
                let open = *show_notes.read();
                let has_notes = notes.read().is_some();
                let eng = engine.clone();
                let save_id = target.clone();
                rsx! {
                    button {
                        class: if open { "notes-tab open" } else { "notes-tab" },
                        title: "Session notes — links, action items, reminders",
                        onclick: move |_| { let v = *show_notes.peek(); show_notes.set(!v); },
                        {icon("file-text")}
                        span { class: "notes-tab-label", "Notes" }
                        if has_notes && !open { span { class: "notes-dot" } }
                    }
                    div { class: if open { "notes-drawer open" } else { "notes-drawer" },
                        div { class: "notes-drawer-head",
                            span { class: "notes-drawer-title", {icon("file-text")} span { "Notes" } }
                            button { class: "close-btn", onclick: move |_| show_notes.set(false), {icon("close")} }
                        }
                        textarea {
                            class: "notes-drawer-input",
                            placeholder: "Links, action items, reminders to revisit later… (searchable; your AI summary and \"ask this meeting\" chat can see these)",
                            value: "{notes_draft}",
                            oninput: move |e| notes_draft.set(e.value()),
                            onfocusout: move |_| {
                                let text = notes_draft.peek().clone();
                                let _ = eng.db_tx.send(DbCmd::SetNotes { id: save_id.clone(), notes: text.clone() });
                                notes.set((!text.trim().is_empty()).then_some(text));
                            },
                        }
                        div { class: "notes-drawer-foot", "Saved to this session · searchable · visible to your AI" }
                    }
                }
            })
        }
    }
}

/// Confirm-delete dialog for a session (set `confirm_delete` to show it).
#[component]
fn ConfirmDeleteDialog(
    confirm_delete: Signal<Option<String>>,
    view: Signal<View>,
    segments: Signal<Vec<Segment>>,
    summary: Signal<Option<String>>,
    compressed: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    rsx! {
        if let Some(did) = confirm_delete.read().clone() {
            {
                let engine = engine.clone();
                rsx! {
                    div { class: "overlay",
                        div { class: "confirm-card",
                            h2 { "Delete session?" }
                            p { class: "field-note", "This permanently removes the recording's transcript and summary. This can't be undone." }
                            div { class: "confirm-actions",
                                button { class: "mbtn ghost", onclick: move |_| confirm_delete.set(None), "Cancel" }
                                button {
                                    class: "mbtn danger",
                                    onclick: move |_| {
                                        let _ = engine.db_tx.send(DbCmd::DeleteSession(did.clone()));
                                        if matches!(&*view.peek(), View::Session(v) if *v == did) {
                                            view.set(View::Live);
                                            segments.write().clear();
                                            summary.set(None);
                                            compressed.set(None);
                                        }
                                        confirm_delete.set(None);
                                    },
                                    "Delete"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Confirm-re-transcribe dialog (Phase 25c; set `confirm_retranscribe`).
#[component]
fn ConfirmRetranscribeDialog(
    confirm_retranscribe: Signal<Option<String>>,
    settings: Signal<Settings>,
    retranscribing: Signal<bool>,
    notice: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    rsx! {
        if let Some(rid) = confirm_retranscribe.read().clone() {
            {
                let engine = engine.clone();
                let model = settings.peek().retranscribe_model.clone();
                rsx! {
                    div { class: "overlay",
                        div { class: "confirm-card",
                            h2 { "Re-transcribe this session?" }
                            p { class: "field-note",
                                "The transcript is regenerated from the kept audio with {model} — any manual line edits are lost. Speaker labels are re-derived afterwards. Summaries aren't touched (regenerate them if the text changes a lot)."
                            }
                            div { class: "confirm-actions",
                                button { class: "mbtn ghost", onclick: move |_| confirm_retranscribe.set(None), "Cancel" }
                                button {
                                    class: "mbtn",
                                    onclick: move |_| {
                                        retranscribing.set(true);
                                        notice.set(Some("Re-transcribing… (runs in the background)".to_string()));
                                        let _ = engine.db_tx.send(DbCmd::Retranscribe(rid.clone()));
                                        confirm_retranscribe.set(None);
                                    },
                                    "Re-transcribe"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Full-screen settings overlay: tab nav on the left, one pane per tab. The
/// heavyweight inline panes (Transcription / AI / Speakers) are their own
/// components below; the others were already separate components.
#[component]
fn SettingsOverlay(
    mut show_settings: Signal<bool>,
    settings: Signal<Settings>,
    mut settings_tab: Signal<String>,
    download_help: Signal<Option<String>>,
    models: Signal<Vec<ModelInfo>>,
    model_progress: Signal<Option<(String, u8)>>,
    remote_models: Signal<Vec<String>>,
    notice: Signal<Option<String>>,
    devices: Vec<String>,
    engine: Engine,
    show_wizard: Signal<bool>,
) -> Element {
    let current = settings.read().model.clone();
    let progress = model_progress.read().clone();
    rsx! {
                    div { class: "overlay",
                        div { class: "overlay-card",
                            div { class: "overlay-head",
                                h2 { "Settings" }
                                button { class: "close-btn", onclick: move |_| show_settings.set(false), {icon("close")} }
                            }
                            div { class: "overlay-body",
                                // Manual-download fallback when an in-app fetch fails
                                // (e.g. behind a corporate proxy). Show the direct
                                // URL(s) + a jump to the models folder.
                                DownloadHelp { download_help, models, notice }
                                div { class: "settings-layout",
                                div { class: "settings-nav",
                                    button { class: if *settings_tab.read() == "transcription" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("transcription".into()), "Transcription" }
                                    button { class: if *settings_tab.read() == "ai" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("ai".into()), "AI" }
                                    button { class: if *settings_tab.read() == "speakers" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("speakers".into()), "Speakers" }
                                    button { class: if *settings_tab.read() == "recording" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("recording".into()), "Recording" }
                                    button { class: if *settings_tab.read() == "integrations" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("integrations".into()), "Integrations" }
                                    button { class: if *settings_tab.read() == "files" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("files".into()), "Files" }
                                    button { class: if *settings_tab.read() == "security" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("security".into()), "Security" }
                                    button { class: if *settings_tab.read() == "theme" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("theme".into()), "Theme" }
                                    button { class: if *settings_tab.read() == "about" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("about".into()), "About" }
                                }
                                div { class: "settings-pane",
                                if *settings_tab.read() == "theme" {
                                ThemeSettings { settings }
                                }
                                if *settings_tab.read() == "transcription" {
                                TranscriptionSettings {
                                    settings,
                                    models,
                                    current: current.clone(),
                                    progress: progress.clone(),
                                    engine: engine.clone(),
                                }
                                }
                                if *settings_tab.read() == "recording" {
                                AudioInputSettings { settings, devices: devices.clone() }
                                LevelSettings { settings }
                                RetentionSettings { settings }
                                }
                                if *settings_tab.read() == "ai" {
                                AiSettings {
                                    settings,
                                    models,
                                    progress: progress.clone(),
                                    remote_models,
                                    notice,
                                    engine: engine.clone(),
                                }
                                }
                                if *settings_tab.read() == "speakers" {
                                SpeakersSettings {
                                    settings,
                                    models,
                                    progress: progress.clone(),
                                    engine: engine.clone(),
                                }
                                }
                                if *settings_tab.read() == "security" {
                                EncryptionSettings { settings, notice }
                                }
                                if *settings_tab.read() == "integrations" {
                                IntegrationsSettings { settings, notice }
                                }
                                if *settings_tab.read() == "files" {
                                FilesSettings { settings, notice, engine: engine.clone() }
                                }
                                if *settings_tab.read() == "about" {
                                AboutSettings { settings, notice, show_settings, show_wizard, engine: engine.clone() }
                                }
                                } // settings-pane
                                } // settings-layout
                            }
                        }
                    }
    }
}

/// Manual-download fallback shown when an in-app model fetch fails (e.g.
/// behind a corporate proxy): the direct URL(s) + a jump to the models folder.
#[component]
fn DownloadHelp(
    download_help: Signal<Option<String>>,
    models: Signal<Vec<ModelInfo>>,
    notice: Signal<Option<String>>,
) -> Element {
    rsx! {
                                if let Some(failed) = download_help.read().clone() {
                                    {
                                        let urls = models.read().iter()
                                            .find(|m| m.name == failed)
                                            .map(|m| m.urls.clone())
                                            .unwrap_or_default();
                                        rsx! {
                                            div { class: "dl-help",
                                                div { class: "dl-help-head",
                                                    span { class: "dl-help-title", {icon("alert")} span { "Couldn't download \"{failed}\"" } }
                                                    button { class: "notice-x", onclick: move |_| download_help.set(None), {icon("close")} }
                                                }
                                                p { class: "field-note", "Often a proxy / network block. Fetch it in your browser (which uses your proxy), then drop it in the models folder. If HuggingFace is blocked, use the modelscope.cn link below. Archives (.tar.bz2) must be extracted there first." }
                                                for u in urls.iter() {
                                                    {
                                                        let u_copy = u.clone();
                                                        let u_open = u.clone();
                                                        rsx! {
                                                            div { class: "dl-help-url",
                                                                code { class: "dl-url", "{u}" }
                                                                button {
                                                                    class: "mbtn ghost",
                                                                    onclick: move |_| { osutil::copy_to_clipboard(&u_copy); notice.set(Some("Download URL copied".to_string())); },
                                                                    "Copy"
                                                                }
                                                                button {
                                                                    class: "mbtn ghost",
                                                                    onclick: move |_| osutil::open_in_browser(&u_open),
                                                                    "Open in browser"
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                div { class: "btn-row",
                                                    button {
                                                        class: "mbtn",
                                                        onclick: move |_| {
                                                            if let Ok(d) = zord_config::models_dir() {
                                                                osutil::open_folder(&d.display().to_string());
                                                            }
                                                        },
                                                        {icon("folder")} "Open models folder"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
    }
}

/// Settings → Transcription: live/auto toggles + the transcription model list
/// (Live/Re role chips, download / delete / progress).
#[component]
fn TranscriptionSettings(
    settings: Signal<Settings>,
    models: Signal<Vec<ModelInfo>>,
    current: String,
    progress: Option<(String, u8)>,
    engine: Engine,
) -> Element {
    rsx! {
                                section { class: "settings-section",
                                    h3 { "Transcription" }
                                    p { class: "field-note", "One model catalog, two jobs: the Live model transcribes while you record (small = fewer CPU spikes); the Re model runs afterwards — Re-transcribe and automatic post-passes — where bigger is usually worth it. Models download on first use." }
                                    div { class: "field-row",
                                        label { class: "field-label", "Live transcription (transcribe while recording)" }
                                        button {
                                            class: if settings.read().live_transcription { "toggle on" } else { "toggle" },
                                            onclick: move |_| {
                                                let mut s = settings.peek().clone();
                                                s.live_transcription = !s.live_transcription;
                                                let _ = s.save();
                                                settings.set(s);
                                            },
                                            if settings.read().live_transcription { "On" } else { "Off" }
                                        }
                                    }
                                    p { class: "field-note", "Off = capture-only recording: meters and audio only — no CPU spikes or model RAM during the meeting (good for low-power machines)." }
                                    div { class: "field-row",
                                        label { class: "field-label", "Transcribe automatically after recording" }
                                        button {
                                            class: if settings.read().auto_transcribe { "toggle on" } else { "toggle" },
                                            onclick: move |_| {
                                                let mut s = settings.peek().clone();
                                                s.auto_transcribe = !s.auto_transcribe;
                                                let _ = s.save();
                                                settings.set(s);
                                            },
                                            if settings.read().auto_transcribe { "On" } else { "Off" }
                                        }
                                    }
                                    p { class: "field-note", "Runs the Re model when you stop. With live transcription on, this upgrades the live transcript; with live off, this is when the transcript is generated (otherwise it waits for Re-transcribe on the session)." }
                                    div { class: "field-row",
                                        label { class: "field-label", "Parallel transcription workers" }
                                        select {
                                            value: "{settings.read().transcribe_workers}",
                                            onchange: move |e| {
                                                let mut s = settings.peek().clone();
                                                s.transcribe_workers = e.value().parse().unwrap_or(1).clamp(1, 4);
                                                let _ = s.save();
                                                settings.set(s);
                                            },
                                            option { value: "1", selected: settings.read().transcribe_workers == 1, "1 (default — sequential)" }
                                            option { value: "2", selected: settings.read().transcribe_workers == 2, "2" }
                                            option { value: "3", selected: settings.read().transcribe_workers == 3, "3" }
                                            option { value: "4", selected: settings.read().transcribe_workers == 4, "4" }
                                        }
                                    }
                                    p { class: "field-note", "How many tracks transcribe at once after a recording — helps multi-speaker Discord sessions; 1 per available track, more memory per worker." }
                                    for m in models.read().iter().filter(|m| m.kind == "transcription") {
                                        {
                                            let name = m.name.clone();
                                            let is_live = name == current;
                                            let is_re = name == settings.read().retranscribe_model;
                                            let dl = match &progress {
                                                Some((n, p)) if *n == name => Some(*p),
                                                _ => None,
                                            };
                                            let eng_dl = engine.clone();
                                            let eng_del = engine.clone();
                                            let (n_live, n_re, n_dl, n_del) =
                                                (name.clone(), name.clone(), name.clone(), name.clone());
                                            rsx! {
                                                div { key: "{name}", class: if is_live || is_re { "model-row sel" } else { "model-row" },
                                                    div { class: "model-main",
                                                        div { class: "model-name", "{m.name}" }
                                                        div { class: "model-desc", "{m.description} · {m.size}" }
                                                    }
                                                    div { class: "model-actions",
                                                        // Role chips: two radio groups across the rows.
                                                        button {
                                                            class: if is_live { "chip on" } else { "chip" },
                                                            title: "Use this model for live transcription",
                                                            onclick: move |_| {
                                                                let mut s = settings.peek().clone();
                                                                s.model = n_live.clone();
                                                                let _ = s.save();
                                                                settings.set(s);
                                                            },
                                                            "Live"
                                                        }
                                                        button {
                                                            class: if is_re { "chip on" } else { "chip" },
                                                            title: "Use this model for Re-transcribe and automatic post-passes",
                                                            onclick: move |_| {
                                                                let mut s = settings.peek().clone();
                                                                s.retranscribe_model = n_re.clone();
                                                                let _ = s.save();
                                                                settings.set(s);
                                                            },
                                                            "Re"
                                                        }
                                                        if m.downloaded {
                                                            button {
                                                                class: "mbtn ghost",
                                                                disabled: is_live || is_re,
                                                                onclick: move |_| { let _ = eng_del.model_tx.send(ModelCmd::Delete(n_del.clone())); },
                                                                "Delete"
                                                            }
                                                        } else if let Some(p) = dl {
                                                            div { class: "dl-prog",
                                                                div { class: "dl-bar", style: "width: {p}%" }
                                                                span { class: "dl-txt", "Downloading… {p}%" }
                                                            }
                                                        } else {
                                                            button {
                                                                class: "mbtn",
                                                                onclick: move |_| { let _ = eng_dl.model_tx.send(ModelCmd::Download(n_dl.clone())); },
                                                                "Download"
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
    }
}

/// Settings → AI: LLM backend choice, external-server config, the summary
/// model list, auto-title and context-window options.
#[component]
fn AiSettings(
    settings: Signal<Settings>,
    models: Signal<Vec<ModelInfo>>,
    progress: Option<(String, u8)>,
    remote_models: Signal<Vec<String>>,
    notice: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    rsx! {
                                section { class: "settings-section",
                                    h3 { "AI" }
                                    {
                                        // Which LLM backends this binary carries.
                                        let local_avail = cfg!(feature = "llm-local");
                                        let remote_avail = cfg!(feature = "llm-remote");
                                        let summary_models: Vec<ModelInfo> = models
                                            .read()
                                            .iter()
                                            .filter(|m| m.kind == "summary")
                                            .cloned()
                                            .collect();
                                        let cur_sum = settings.read().summary_model.clone();
                                        if !local_avail && !remote_avail {
                                            rsx! {
                                                p { class: "field-note", "Build with `--features llm-local` (built-in models) and/or `--features llm-remote` (external servers) to enable the AI features — summaries, compression, Overview, chat and auto-titles." }
                                            }
                                        } else {
                                            // The setting decides only when both backends are
                                            // compiled in; otherwise the available one is used.
                                            let external = remote_avail
                                                && (settings.read().llm_backend == "external" || !local_avail);
                                            let eng_remote = engine.clone();
                                            rsx! {
                                                p { class: "field-note", "One LLM drives all AI features: summaries, compression, the Overview, chat and auto-titles." }
                                                if local_avail && remote_avail {
                                                div { class: "field",
                                                    label { "LLM backend" }
                                                    select {
                                                        onchange: move |e: FormEvent| {
                                                            let mut s = settings.peek().clone();
                                                            s.llm_backend = e.value();
                                                            let _ = s.save();
                                                            settings.set(s);
                                                        },
                                                        option { value: "local", selected: !external, "Built-in (local model)" }
                                                        option { value: "external", selected: external, "External server (OpenAI-compatible)" }
                                                    }
                                                }
                                                }
                                                if external {
                                                    div { class: "field",
                                                        label { "Server URL" }
                                                        input {
                                                            r#type: "text", class: "days",
                                                            placeholder: "http://localhost:1234",
                                                            value: "{settings.read().llm_base_url}",
                                                            oninput: move |e: FormEvent| {
                                                                let mut s = settings.peek().clone();
                                                                s.llm_base_url = e.value();
                                                                let _ = s.save();
                                                                settings.set(s);
                                                            },
                                                        }
                                                    }
                                                    div { class: "field",
                                                        label { "API key (optional — most local servers need none)" }
                                                        input {
                                                            r#type: "password", class: "days",
                                                            value: "{settings.read().llm_api_key}",
                                                            oninput: move |e: FormEvent| {
                                                                let mut s = settings.peek().clone();
                                                                s.llm_api_key = e.value();
                                                                let _ = s.save();
                                                                settings.set(s);
                                                            },
                                                        }
                                                    }
                                                    div { class: "field",
                                                        label { "Model" }
                                                        div { class: "remote-model-row",
                                                            select {
                                                                onchange: move |e: FormEvent| {
                                                                    let mut s = settings.peek().clone();
                                                                    s.llm_model = e.value();
                                                                    let _ = s.save();
                                                                    settings.set(s);
                                                                },
                                                                // Keep the saved choice visible before/without a fetch.
                                                                {
                                                                    let cur = settings.read().llm_model.clone();
                                                                    let listed = remote_models.read().clone();
                                                                    let show_cur = !cur.trim().is_empty() && !listed.contains(&cur);
                                                                    rsx! {
                                                                        if show_cur {
                                                                            option { value: "{cur}", selected: true, "{cur}" }
                                                                        }
                                                                        if listed.is_empty() && cur.trim().is_empty() {
                                                                            option { value: "", selected: true, disabled: true, "— test the connection to list models —" }
                                                                        }
                                                                        for m in listed {
                                                                            option { value: "{m}", selected: m == settings.read().llm_model, "{m}" }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            button {
                                                                class: "mbtn",
                                                                title: "Contact the server's /v1/models — verifies the connection and fills the model list",
                                                                onclick: move |_| {
                                                                    notice.set(Some("Contacting the LLM server…".to_string()));
                                                                    let _ = eng_remote.model_tx.send(ModelCmd::ListRemoteLlm);
                                                                },
                                                                "Test connection"
                                                            }
                                                        }
                                                    }
                                                    p { class: "field-note", "Works with anything OpenAI-compatible: LM Studio (http://localhost:1234), Ollama (http://localhost:11434), llama-server, vLLM, Jan… Summaries, compression, Overview, chat and auto-titles all use this server. Note: transcripts are sent to it, so point Zord only at a server you trust — there is no fallback to the local model." }
                                                } else {
                                                p { class: "field-note", "Download and pick the summary model. Bigger = better notes but slower. No HuggingFace access? Drop any GGUF into the models folder (Files & folders, below) and it appears here as a custom model." }
                                                for m in summary_models.iter() {
                                                    {
                                                        let name = m.name.clone();
                                                        let selected = name == cur_sum;
                                                        let dl = match &progress {
                                                            Some((n, p)) if *n == name => Some(*p),
                                                            _ => None,
                                                        };
                                                        let eng_dl = engine.clone();
                                                        let eng_del = engine.clone();
                                                        let (n_sel, n_dl, n_del) = (name.clone(), name.clone(), name.clone());
                                                        rsx! {
                                                            div { key: "{name}", class: if selected { "model-row sel" } else { "model-row" },
                                                                div { class: "model-main",
                                                                    div { class: "model-name", "{m.name}" }
                                                                    div { class: "model-desc", "{m.description} · {m.size}" }
                                                                }
                                                                div { class: "model-actions",
                                                                    if m.downloaded {
                                                                        button {
                                                                            class: "mbtn",
                                                                            disabled: selected,
                                                                            onclick: move |_| {
                                                                                let mut s = settings.peek().clone();
                                                                                s.summary_model = n_sel.clone();
                                                                                let _ = s.save();
                                                                                settings.set(s);
                                                                            },
                                                                            if selected { "Selected" } else { "Select" }
                                                                        }
                                                                        button {
                                                                            class: "mbtn ghost",
                                                                            disabled: selected,
                                                                            onclick: move |_| { let _ = eng_del.model_tx.send(ModelCmd::Delete(n_del.clone())); },
                                                                            "Delete"
                                                                        }
                                                                    } else if let Some(p) = dl {
                                                                        div { class: "dl-prog",
                                                                            div { class: "dl-bar", style: "width: {p}%" }
                                                                            span { class: "dl-txt", "Downloading… {p}%" }
                                                                        }
                                                                    } else {
                                                                        button {
                                                                            class: "mbtn",
                                                                            onclick: move |_| { let _ = eng_dl.model_tx.send(ModelCmd::Download(n_dl.clone())); },
                                                                            "Download"
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                }
                                                div { class: "field-row",
                                                    label { class: "field-label", "Auto-generate a meeting title after summarizing" }
                                                    button {
                                                        class: if settings.read().auto_title { "toggle on" } else { "toggle" },
                                                        onclick: move |_| {
                                                            let mut s = settings.peek().clone();
                                                            s.auto_title = !s.auto_title;
                                                            let _ = s.save();
                                                            settings.set(s);
                                                        },
                                                        if settings.read().auto_title { "On" } else { "Off" }
                                                    }
                                                }
                                                p { class: "field-note", "Names each recording from its summary (never overwrites a title you set yourself), so sessions are easy to find later." }
                                                div { class: "field",
                                                    label { "Compression context window (tokens)" }
                                                    input {
                                                        r#type: "number", min: "8192", max: "131072", step: "8192", class: "days",
                                                        value: "{settings.read().compress_ctx}",
                                                        oninput: move |e: FormEvent| {
                                                            if let Ok(v) = e.value().trim().parse::<u32>() {
                                                                let mut s = settings.peek().clone();
                                                                s.compress_ctx = v.clamp(8192, 131072);
                                                                let _ = s.save();
                                                                settings.set(s);
                                                            }
                                                        },
                                                    }
                                                }
                                                p { class: "field-note", "Used by Compress to ingest a whole meeting without truncation. 16K fits ~an hour; larger needs more RAM + CPU time. On a 16 GB laptop a 3B model handles 64K comfortably; 7B is tight beyond 32K." }
                                                // Overview auto-update toggle + Re-compress all (Phase 39d).
                                                div { class: "field-row",
                                                    label { class: "field-label", "Update the Overview automatically after each recording is transcribed" }
                                                    button {
                                                        class: if settings.read().overview_auto { "toggle on" } else { "toggle" },
                                                        onclick: move |_| {
                                                            let mut s = settings.peek().clone();
                                                            s.overview_auto = !s.overview_auto;
                                                            let _ = s.save();
                                                            settings.set(s);
                                                        },
                                                        if settings.read().overview_auto { "On" } else { "Off" }
                                                    }
                                                }
                                                p { class: "field-note", "Folds each finished, transcribed meeting into the living Overview document automatically. Requires compression to run first (also automatic). Disable to update the Overview manually from the Overview view." }
                                                {
                                                    let engine_rc = engine.clone();
                                                    let mut confirm_recompress = use_signal(|| false);
                                                    rsx! {
                                                        if confirm_recompress() {
                                                            div { class: "field-row",
                                                                span { class: "field-note", "Re-runs the local AI over every saved transcript to rebuild condensed versions — can take a long time on a large library." }
                                                                button {
                                                                    class: "mbtn danger",
                                                                    onclick: move |_| {
                                                                        confirm_recompress.set(false);
                                                                        let _ = engine_rc.summ_tx.send(SummCmd::RecompressAll);
                                                                    },
                                                                    "Re-compress all sessions"
                                                                }
                                                                button {
                                                                    class: "mbtn ghost",
                                                                    onclick: move |_| confirm_recompress.set(false),
                                                                    "Cancel"
                                                                }
                                                            }
                                                        } else {
                                                            div { class: "btn-row",
                                                                button {
                                                                    class: "mbtn ghost",
                                                                    onclick: move |_| confirm_recompress.set(true),
                                                                    "Re-compress all sessions…"
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                SummaryPromptSettings { settings }
                                                // Phase 45: semantic search index management.
                                                if cfg!(feature = "semantic") {
                                                    {
                                                        let engine_sem = engine.clone();
                                                        rsx! {
                                                            div { class: "settings-subsection",
                                                                h4 { "Semantic search" }
                                                                p { class: "field-note",
                                                                    "Builds a local embedding index (BGE-small-en-v1.5, ~24 MB) so the Search view can find passages by meaning, not just keywords. The model downloads on first run and stays on device. Brute-force cosine scoring — fast for libraries up to ~100 k transcript chunks."
                                                                }
                                                                div { class: "btn-row",
                                                                    button {
                                                                        class: "mbtn ghost",
                                                                        onclick: move |_| {
                                                                            let _ = engine_sem.embed_tx.send(EmbedCmd::BackfillAll);
                                                                        },
                                                                        "Build semantic index"
                                                                    }
                                                                }
                                                                p { class: "field-note", "Downloads the model on first run. Indexing runs in the background; you can use the app normally. Re-run after adding new sessions." }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
    }
}

/// Settings → Speakers: diarization embedding-model list, segmentation model,
/// clustering threshold, and the auto/live toggles.
#[component]
fn SpeakersSettings(
    settings: Signal<Settings>,
    models: Signal<Vec<ModelInfo>>,
    progress: Option<(String, u8)>,
    engine: Engine,
) -> Element {
    rsx! {
                                section { class: "settings-section",
                                    h3 { "Speakers" }
                                    {
                                        let diar_models: Vec<ModelInfo> = models
                                            .read()
                                            .iter()
                                            .filter(|m| m.kind == "diarization")
                                            .cloned()
                                            .collect();
                                        let cur_diar = settings.read().diarize_embedding_model.clone();
                                        if diar_models.is_empty() {
                                            rsx! {
                                                p { class: "field-note", "Build with `--features diarization` to label individual speakers in the 'Others' channel." }
                                            }
                                        } else {
                                            rsx! {
                                                p { class: "field-note", "Groups the 'Others' channel into individual speakers. Runs automatically after recording (and on demand). Bigger models = better accuracy, slower." }
                                                for m in diar_models.iter() {
                                                    {
                                                        let name = m.name.clone();
                                                        let selected = name == cur_diar;
                                                        let dl = match &progress {
                                                            Some((n, p)) if *n == name => Some(*p),
                                                            _ => None,
                                                        };
                                                        let eng_dl = engine.clone();
                                                        let eng_del = engine.clone();
                                                        let (n_sel, n_dl, n_del) = (name.clone(), name.clone(), name.clone());
                                                        rsx! {
                                                            div { key: "{name}", class: if selected { "model-row sel" } else { "model-row" },
                                                                div { class: "model-main",
                                                                    div { class: "model-name", "{m.name}" }
                                                                    div { class: "model-desc", "{m.description} · {m.size}" }
                                                                }
                                                                div { class: "model-actions",
                                                                    if m.downloaded {
                                                                        button {
                                                                            class: "mbtn",
                                                                            disabled: selected,
                                                                            onclick: move |_| {
                                                                                let mut s = settings.peek().clone();
                                                                                s.diarize_embedding_model = n_sel.clone();
                                                                                let _ = s.save();
                                                                                settings.set(s);
                                                                            },
                                                                            if selected { "Selected" } else { "Select" }
                                                                        }
                                                                        button {
                                                                            class: "mbtn ghost",
                                                                            disabled: selected,
                                                                            onclick: move |_| { let _ = eng_del.model_tx.send(ModelCmd::Delete(n_del.clone())); },
                                                                            "Delete"
                                                                        }
                                                                    } else if let Some(p) = dl {
                                                                        div { class: "dl-prog",
                                                                            div { class: "dl-bar", style: "width: {p}%" }
                                                                            span { class: "dl-txt", "Downloading… {p}%" }
                                                                        }
                                                                    } else {
                                                                        button {
                                                                            class: "mbtn",
                                                                            onclick: move |_| { let _ = eng_dl.model_tx.send(ModelCmd::Download(n_dl.clone())); },
                                                                            "Download"
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                div { class: "field",
                                                    label { "Segmentation model" }
                                                    select {
                                                        onchange: move |e: FormEvent| {
                                                            let mut s = settings.peek().clone();
                                                            s.diarize_segmentation_model = e.value();
                                                            let _ = s.save();
                                                            settings.set(s);
                                                        },
                                                        for sm in seg_model_options() {
                                                            option {
                                                                value: "{sm.0}",
                                                                selected: settings.read().diarize_segmentation_model == sm.0,
                                                                "{sm.1}"
                                                            }
                                                        }
                                                    }
                                                }
                                                p { class: "field-note", "How speech is split into speaker turns. Rev's Reverb models are fine-tuned on ~26k hours of real meetings — noticeably better turn detection, but licensed for non-commercial use. Downloads on first use; re-run Identify speakers to apply." }
                                                div { class: "field",
                                                    label { "Clustering threshold (auto mode): {settings.read().diarize_threshold:.2}" }
                                                    input {
                                                        r#type: "number", min: "0.1", max: "0.95", step: "0.05", class: "days",
                                                        value: "{settings.read().diarize_threshold:.2}",
                                                        oninput: move |e: FormEvent| {
                                                            if let Ok(v) = e.value().trim().parse::<f32>() {
                                                                let mut s = settings.peek().clone();
                                                                s.diarize_threshold = v.clamp(0.1, 0.95);
                                                                let _ = s.save();
                                                                settings.set(s);
                                                            }
                                                        },
                                                    }
                                                }
                                                p { class: "field-note", "Only used when the speaker count (set next to 'Identify speakers' on a session) is auto. Lower = split into more speakers; higher = merge into fewer. Default 0.50." }
                                                div { class: "field-row",
                                                    label { class: "field-label", "Identify speakers automatically after recording" }
                                                    button {
                                                        class: if settings.read().diarize_auto { "toggle on" } else { "toggle" },
                                                        onclick: move |_| {
                                                            let mut s = settings.peek().clone();
                                                            s.diarize_auto = !s.diarize_auto;
                                                            let _ = s.save();
                                                            settings.set(s);
                                                        },
                                                        if settings.read().diarize_auto { "On" } else { "Off" }
                                                    }
                                                }
                                                div { class: "field-row",
                                                    label { class: "field-label", "Show provisional speaker labels live while recording" }
                                                    button {
                                                        class: if settings.read().diarize_live { "toggle on" } else { "toggle" },
                                                        onclick: move |_| {
                                                            let mut s = settings.peek().clone();
                                                            s.diarize_live = !s.diarize_live;
                                                            let _ = s.save();
                                                            settings.set(s);
                                                        },
                                                        if settings.read().diarize_live { "On" } else { "Off" }
                                                    }
                                                }
                                                p { class: "field-note", "Live labels are rough and get replaced by the accurate pass at stop. Leave off on lighter hardware. Re-running Identify speakers later (with a different model) uses the kept audio — see Recording & retention." }
                                            }
                                        }
                                    }
                                }
                                // Phase 38d: voice-identification settings block.
                                speakers::VoiceprintSettings { settings, engine: engine.clone() }
    }
}

/// Summary style preset + editable system prompt. Uses only zord-config, so it
/// compiles regardless of the `summaries` feature (rendered next to the summary
/// model list, which is the part that needs the feature).
#[component]
fn SummaryPromptSettings(settings: Signal<Settings>) -> Element {
    let s = settings.read().clone();
    let effective = s.effective_summary_prompt();
    rsx! {
        div { class: "field",
            label { "Style preset" }
            select {
                onchange: move |e: FormEvent| {
                    let mut s = settings.peek().clone();
                    s.summary_preset = e.value();
                    s.summary_prompt = None; // switch to the preset's prompt
                    let _ = s.save();
                    settings.set(s);
                },
                for (id, label, _) in zord_config::summary_presets().iter() {
                    option { value: "{id}", selected: s.summary_preset == *id, "{label}" }
                }
            }
        }
        div { class: "field",
            label { "System prompt" }
            textarea {
                class: "prompt-edit",
                rows: "5",
                value: "{effective}",
                oninput: move |e: FormEvent| {
                    let mut s = settings.peek().clone();
                    s.summary_prompt = Some(e.value());
                    settings.set(s); // saved on blur to avoid per-keystroke writes
                },
                onfocusout: move |_| { let _ = settings.peek().save(); },
            }
            button {
                class: "mbtn ghost",
                onclick: move |_| {
                    let mut s = settings.peek().clone();
                    s.summary_prompt = None;
                    let _ = s.save();
                    settings.set(s);
                },
                "Reset to preset"
            }
        }
    }
}

/// Curated accent presets: (name, hex). Cyan first = the built-in default.
const ACCENT_PRESETS: [(&str, &str); 6] = [
    ("Cyan", "#4cc2ff"),
    ("Blurple", "#5865f2"),
    ("Coral", "#ff7059"),
    ("Green", "#3ecf8e"),
    ("Violet", "#a78bfa"),
    ("Amber", "#ffb454"),
];

/// One row of theme control: a label, preset swatches, and a hex input.
/// `current` empty = the built-in default (its swatch shows as selected).
#[component]
fn ColorRow(
    label: String,
    default_hex: String,
    current: String,
    presets: Vec<(&'static str, &'static str)>,
    on_pick: EventHandler<String>,
) -> Element {
    let effective = if current.is_empty() {
        default_hex.clone()
    } else {
        current.clone()
    };
    rsx! {
        div { class: "field-row",
            label { class: "field-label", "{label}" }
            div { class: "swatch-row",
                for (name, hex) in presets {
                    button {
                        key: "{hex}",
                        class: if effective.eq_ignore_ascii_case(hex) { "swatch on" } else { "swatch" },
                        style: "background: {hex};",
                        title: "{name}",
                        onclick: move |_| on_pick.call(hex.to_string()),
                    }
                }
                input {
                    class: "swatch-hex",
                    placeholder: "{default_hex}",
                    value: "{current}",
                    oninput: move |e: FormEvent| {
                        let v = e.value().trim().to_string();
                        if v.is_empty() || zord_config::is_valid_hex_color(&v) {
                            on_pick.call(v);
                        }
                    },
                }
            }
        }
    }
}

/// Settings → Theme: accent / Me / Others colors (presets + custom hex,
/// applied live via the root's custom properties), badge tint, and reset.
#[component]
fn ThemeSettings(settings: Signal<Settings>) -> Element {
    let mut set = move |apply: fn(&mut Settings, String), v: String| {
        let mut s = settings.peek().clone();
        apply(&mut s, v);
        let _ = s.save();
        settings.set(s);
    };
    rsx! {
        section { class: "settings-section",
            h3 { "Theme" }
            p { class: "field-note",
                "Colors apply instantly. The hex fields accept any #rrggbb; text on your color stays readable automatically. Record stays red — that one means something."
            }
            ColorRow {
                label: "Accent".to_string(),
                default_hex: "#4cc2ff".to_string(),
                current: settings.read().theme_accent.clone(),
                presets: ACCENT_PRESETS.to_vec(),
                on_pick: move |v| set(|s, v| s.theme_accent = v, v),
            }
            ColorRow {
                label: "Me (your channel)".to_string(),
                default_hex: "#4cc2ff".to_string(),
                current: settings.read().theme_me.clone(),
                presets: ACCENT_PRESETS.to_vec(),
                on_pick: move |v| set(|s, v| s.theme_me = v, v),
            }
            ColorRow {
                label: "Others (their channel)".to_string(),
                default_hex: "#ffb454".to_string(),
                current: settings.read().theme_others.clone(),
                presets: ACCENT_PRESETS.to_vec(),
                on_pick: move |v| set(|s, v| s.theme_others = v, v),
            }
            div { class: "field-row",
                label { class: "field-label", "Session badges: tint by meaning" }
                button {
                    class: if settings.read().badge_tint { "toggle on" } else { "toggle" },
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.badge_tint = !s.badge_tint;
                        let _ = s.save();
                        settings.set(s);
                    },
                    if settings.read().badge_tint { "Tinted" } else { "Mono" }
                }
            }
            p { class: "field-note", "The summary / compressed / speakers badges in the sidebar are color-coded by meaning (cyan / amber / green) so you can read a session at a glance. Turn off for a calmer, monochrome look." }
            div { class: "field",
                button {
                    class: "mbtn ghost",
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.theme_accent = String::new();
                        s.theme_me = String::new();
                        s.theme_others = String::new();
                        let _ = s.save();
                        settings.set(s);
                    },
                    "Reset colors to defaults"
                }
            }
        }
    }
}

/// Settings → About: a one-line local-only blurb, version/channel, updates,
/// setup-wizard re-run, and the diagnostic bundle export (Phase 43c).
#[component]
fn AboutSettings(
    settings: Signal<Settings>,
    mut notice: Signal<Option<String>>,
    mut show_settings: Signal<bool>,
    mut show_wizard: Signal<bool>,
    engine: Engine,
) -> Element {
    // Update controls exist only in `self-update` builds (the GitHub channel);
    // store builds neither check nor install — the store does.
    let update_ui: Option<Element> = {
        #[cfg(feature = "self-update")]
        {
            Some(rsx! {
                UpdateSettings { settings, notice }
            })
        }
        #[cfg(not(feature = "self-update"))]
        {
            let _ = (&settings, &notice);
            None
        }
    };
    rsx! {
        section { class: "settings-section",
            h3 { "About" }
            p { class: "field-note", "Zord · 100% local. Recordings, transcripts, and models stay on this device — nothing is uploaded." }
            p { class: "field-note",
                "Version {env!(\"CARGO_PKG_VERSION\")} · {zord_core::DIST_CHANNEL} channel"
            }
            div { class: "field",
                button {
                    class: "mbtn ghost",
                    onclick: move |_| {
                        show_settings.set(false);
                        show_wizard.set(true);
                    },
                    "Run setup again"
                }
            }
            // --- Diagnostic bundle (Phase 43c) ---
            div { class: "subhead", "Bug reporting" }
            div { class: "btn-row",
                button {
                    class: "mbtn",
                    title: "Write a zip you can attach to a bug report",
                    onclick: move |_| {
                        let _ = engine.db_tx.send(DbCmd::ExportDiagnostics);
                        notice.set(Some("Building diagnostic bundle…".to_string()));
                    },
                    {icon("archive")} "Export diagnostic bundle"
                }
            }
            p { class: "field-note", "No transcripts or audio included; secrets (bot token, API keys) are redacted. The zip lands in your Exports folder." }
            {update_ui}
        }
    }
}

/// Settings → About update controls (Phase 34, `self-update` builds): the
/// launch-check toggle, a manual check, and — on Windows, where the portable
/// EXE can be swapped in place — one-click install.
#[cfg(feature = "self-update")]
#[component]
fn UpdateSettings(settings: Signal<Settings>, mut notice: Signal<Option<String>>) -> Element {
    let mut found = use_signal(|| None::<update::UpdateInfo>);
    let mut busy = use_signal(|| false);
    if !update::channel_self_updates() {
        return rsx! {
            p { class: "field-note", "This build is distributed through a store — updates arrive through the store." }
        };
    }
    rsx! {
        div { class: "field-row",
            label { class: "field-label", "Check for updates at launch" }
            button {
                class: if settings.read().check_updates { "toggle on" } else { "toggle" },
                onclick: move |_| {
                    let mut s = settings.peek().clone();
                    s.check_updates = !s.check_updates;
                    let _ = s.save();
                    settings.set(s);
                },
                if settings.read().check_updates { "On" } else { "Off" }
            }
        }
        div { class: "field",
            button {
                class: "mbtn",
                disabled: *busy.read(),
                onclick: move |_| {
                    busy.set(true);
                    spawn(async move {
                        let res = tokio::task::spawn_blocking(update::check).await;
                        busy.set(false);
                        match res {
                            Ok(Ok(Some(info))) => {
                                notice.set(Some(format!("Update available: v{}", info.version)));
                                found.set(Some(info));
                            }
                            Ok(Ok(None)) => {
                                notice.set(Some(format!("You're up to date (v{}).", update::CURRENT_VERSION)));
                                found.set(None);
                            }
                            Ok(Err(e)) => notice.set(Some(e.to_string())),
                            Err(_) => notice.set(Some("update check failed".into())),
                        }
                    });
                },
                if *busy.read() { "Checking…" } else { "Check for updates" }
            }
        }
        if let Some(info) = found.read().clone() {
            div { class: "field",
                if let Some(url) = info.asset_url.clone() {
                    button {
                        class: "mbtn",
                        disabled: *busy.read(),
                        onclick: move |_| {
                            busy.set(true);
                            let url = url.clone();
                            spawn(async move {
                                let res = tokio::task::spawn_blocking(move || {
                                    update::download_and_install(&url)
                                })
                                .await;
                                busy.set(false);
                                match res {
                                    Ok(Ok(())) => notice.set(Some(
                                        "Update installed — restart Zord to finish.".into(),
                                    )),
                                    Ok(Err(e)) => notice.set(Some(format!("update failed: {e}"))),
                                    Err(_) => notice.set(Some("update failed".into())),
                                }
                            });
                        },
                        "Download & install v{info.version}"
                    }
                } else {
                    button {
                        class: "mbtn ghost",
                        onclick: move |_| {
                            if let Some(i) = found.peek().as_ref() {
                                let _ = open::that(&i.page_url);
                            }
                        },
                        "Open download page (v{info.version})"
                    }
                }
            }
        }
    }
}

/// Settings → Integrations (Phase 30d): the Discord bot credentials, inline
/// how-to help, a Test-connection probe, and the one-click invite link. The
/// recording itself is started by switching Capture (Recording tab) to
/// "Discord" in a `--features discord` build.
#[component]
fn IntegrationsSettings(settings: Signal<Settings>, notice: Signal<Option<String>>) -> Element {
    // View Channels (1024) + Send Messages (2048) + Connect (1048576): enough
    // to find you, join, and post the recording announcement — nothing more.
    const INVITE_PERMISSIONS: u64 = 1024 + 2048 + 1_048_576;
    let discord_built = cfg!(feature = "discord");
    rsx! {
        section { class: "settings-section",
            h3 { "Discord" }
            if !discord_built {
                p { class: "field-note",
                    "⚠ This build doesn't include the Discord engine — install a release build (or build with --features discord) to record calls. Settings entered here are kept."
                }
            }
            p { class: "field-note",
                "Bring your own bot: create an application at discord.com/developers/applications, add a Bot, and paste its token below. When you record, the bot joins your voice channel as a visible participant and captures one track per speaker — real names, no diarization. Everything stays on this machine."
            }
            div { class: "field-row",
                label { class: "field-label", "Show the Record Discord button" }
                button {
                    class: if settings.read().discord_record_button { "toggle on" } else { "toggle" },
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.discord_record_button = !s.discord_record_button;
                        let _ = s.save();
                        settings.set(s);
                    },
                    if settings.read().discord_record_button { "On" } else { "Off" }
                }
            }
            p { class: "field-note",
                "The sidebar button appears once a bot token and user ID are saved."
            }
            div { class: "field",
                label { "Bot token" }
                input {
                    r#type: "password", class: "days",
                    value: "{settings.read().discord_bot_token}",
                    oninput: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        s.discord_bot_token = e.value().trim().to_string();
                        let _ = s.save();
                        settings.set(s);
                    },
                }
            }
            div { class: "field",
                label { "Your Discord user ID (who the bot follows into voice)" }
                input {
                    class: "days",
                    placeholder: "e.g. 268473310986240001",
                    value: "{settings.read().discord_user_id}",
                    oninput: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        s.discord_user_id = e.value().trim().to_string();
                        let _ = s.save();
                        settings.set(s);
                    },
                }
            }
            p { class: "field-note",
                "To find it: Discord → Settings → Advanced → turn on Developer Mode, then right-click your own name and pick \"Copy User ID\". No server or channel to configure — the bot finds whichever voice channel you're in."
            }
            div { class: "field-row",
                label { class: "field-label", "Announce recording in the channel" }
                button {
                    class: if settings.read().discord_announce { "toggle on" } else { "toggle" },
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.discord_announce = !s.discord_announce;
                        let _ = s.save();
                        settings.set(s);
                    },
                    if settings.read().discord_announce { "On" } else { "Off" }
                }
            }
            p { class: "field-note",
                "The bot posts a \"recording started\" message in the voice channel's text chat when it joins — the consent signal Discord's developer policy expects."
            }
            div { class: "field",
                button {
                    class: "mbtn",
                    onclick: move |_| {
                        let token = settings.peek().discord_bot_token.clone();
                        if token.is_empty() {
                            notice.set(Some("Paste a bot token first.".into()));
                            return;
                        }
                        match zord_net::discord_bot_app(&token, std::time::Duration::from_secs(8)) {
                            Ok((_, name)) => notice.set(Some(format!("Connected — bot \"{name}\" is valid ✓"))),
                            Err(e) => notice.set(Some(format!("Discord rejected the token: {e}"))),
                        }
                    },
                    "Test connection"
                }
                button {
                    class: "mbtn ghost",
                    onclick: move |_| {
                        let token = settings.peek().discord_bot_token.clone();
                        if token.is_empty() {
                            notice.set(Some("Paste a bot token first.".into()));
                            return;
                        }
                        match zord_net::discord_bot_app(&token, std::time::Duration::from_secs(8)) {
                            Ok((id, _)) => {
                                let url = format!(
                                    "https://discord.com/oauth2/authorize?client_id={id}&scope=bot&permissions={INVITE_PERMISSIONS}"
                                );
                                if open::that(&url).is_ok() {
                                    notice.set(Some("Opened the invite page — pick your server and approve.".into()));
                                } else {
                                    notice.set(Some(format!("Couldn't open a browser — visit: {url}")));
                                }
                            }
                            Err(e) => notice.set(Some(format!("Couldn't read the bot's application id: {e}"))),
                        }
                    },
                    "Invite bot to a server…"
                }
            }
            p { class: "field-note",
                "To record a call: set Capture (Recording tab) to \"Discord\", join a voice channel in a server the bot is in, and press Record. The bot follows you in — and leaves when you do."
            }
        }
        section { class: "settings-section",
            h3 { "Microsoft Teams · Zoom" }
            p { class: "field-note", "Planned. Desktop/system capture with diarization covers them today." }
        }
    }
}

#[cfg(feature = "encryption")]
#[component]
fn EncryptionSettings(settings: Signal<Settings>, notice: Signal<Option<String>>) -> Element {
    let mut p1 = use_signal(String::new);
    let mut p2 = use_signal(String::new);
    let s = settings.read().clone();
    rsx! {
        section { class: "settings-section",
            h3 { "Encryption (at rest)" }
            if s.encrypted || s.encrypt_pending {
                p { class: "field-note",
                    if s.encrypt_pending { "Encryption will be applied next launch." } else { "Database encryption is ON." }
                }
                button {
                    class: "mbtn ghost",
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.decrypt_pending = true;
                        let _ = s.save();
                        settings.set(s);
                        notice.set(Some("Encryption will be removed on next launch — restart Zord.".to_string()));
                    },
                    "Disable encryption"
                }
            } else {
                p { class: "field-note", "Encrypt the local database with a passphrase (kept in your OS keychain). Applied on next launch." }
                input { r#type: "password", class: "search", placeholder: "Passphrase", value: "{p1}", oninput: move |e| p1.set(e.value()) }
                input { r#type: "password", class: "search", placeholder: "Confirm passphrase", value: "{p2}", oninput: move |e| p2.set(e.value()) }
                button {
                    class: "mbtn",
                    onclick: move |_| {
                        let (a, b) = (p1.peek().clone(), p2.peek().clone());
                        if a.is_empty() {
                            notice.set(Some("Passphrase must not be empty.".to_string()));
                            return;
                        }
                        if a != b {
                            notice.set(Some("Passphrases do not match.".to_string()));
                            return;
                        }
                        if zord_config::keychain::store(&a).is_err() {
                            notice.set(Some("Could not access the OS keychain.".to_string()));
                            return;
                        }
                        let mut s = settings.peek().clone();
                        s.encrypt_pending = true;
                        let _ = s.save();
                        settings.set(s);
                        p1.set(String::new());
                        p2.set(String::new());
                        notice.set(Some("Encryption will be applied on next launch — restart Zord.".to_string()));
                    },
                    "Enable encryption"
                }
            }
        }
    }
}

#[cfg(not(feature = "encryption"))]
#[component]
fn EncryptionSettings(settings: Signal<Settings>, notice: Signal<Option<String>>) -> Element {
    let _ = (settings, notice);
    rsx! {
        section { class: "settings-section",
            h3 { "Encryption (at rest)" }
            p { class: "field-note", "Build with `--features encryption` to enable SQLCipher database encryption." }
        }
    }
}

#[component]
fn Meter(label: String, level: Signal<f32>, kind: String) -> Element {
    // `level` is already a gained, smoothed RMS (0..1) from the engine; just
    // map to a percentage. Reading the signal here means only this component
    // re-renders on level changes.
    let pct = (level() * 100.0).clamp(0.0, 100.0);
    rsx! {
        div { class: "meter",
            span { class: "meter-label", "{label}" }
            div { class: "meter-track",
                div { class: "meter-fill {kind}", style: "width: {pct}%" }
            }
        }
    }
}

#[component]
fn TranscriptView(
    segments: Signal<Vec<Segment>>,
    speaker_names: Signal<std::collections::HashMap<i32, String>>,
    highlight: Signal<Option<i64>>,
    on_edit: EventHandler<(i64, String)>,
    /// Retained WAV paths that exist on disk for this session.
    audio: Signal<AudioFiles>,
    /// The app user's own speaker index (integration sessions) — their lines
    /// style as "me" even though they're a regular speaker track.
    me_speaker: Signal<Option<i32>>,
    /// The line (db id) currently playing back.
    playing: Signal<Option<i64>>,
    /// Replay a line: `Some((wav, segment_id, start_ms, end_ms))`, or `None` to stop.
    on_play: EventHandler<Option<(String, i64, u64, u64)>>,
    /// Phase 40: find-in-session hit set. The active hit is in `highlight`
    /// (reuses the scroll mechanism); this set drives the soft background on
    /// all other hits. Empty when the find bar is closed.
    find_hits: Signal<Vec<i64>>,
    /// Phase 42c: when the timeline panel is open, clicking a timestamp seeks
    /// playback to that line.  `None` = timeline closed, timestamp is plain.
    on_seek: Option<EventHandler<u64>>,
) -> Element {
    let mut editing = use_signal(|| Option::<i64>::None);
    let mut buf = use_signal(String::new);
    let segs = segments.read();
    let names = speaker_names.read();
    let hl = *highlight.read();
    let fhits = find_hits.read();
    let timeline_active = on_seek.is_some();
    if segs.is_empty() {
        return rsx! { div { class: "empty", "Press Record, or pick a session." } };
    }
    rsx! {
        for seg in segs.iter() {
            {
                let sid = seg.id;
                let is_editing = sid.is_some() && *editing.read() == sid;
                let text = seg.text.clone();
                let text_for_edit = text.clone();
                let who = seg.speaker_label(&names);
                // Replay is offered only when this channel's audio file exists.
                // For integration sessions the Others channel is split into
                // per-participant spk-N tracks; prefer the per-speaker file
                // when present and fall back to others.wav so normal diarized
                // sessions (speaker indices but a single others.wav) keep their
                // replay buttons.
                let wav = {
                    let af = audio.read();
                    match (seg.source, seg.speaker) {
                        (Source::Me, _) => af.me.clone(),
                        (Source::Others, Some(idx)) => {
                            af.speakers.get(&idx).cloned().or_else(|| af.others.clone())
                        }
                        (Source::Others, None) => af.others.clone(),
                    }
                };
                let can_play = sid.is_some() && wav.is_some();
                let wav_play = wav.unwrap_or_default();
                let is_playing = sid.is_some() && *playing.read() == sid;
                let (t0, t1) = (seg.t_start_ms, seg.t_end_ms);
                // DOM anchor + flash highlight so a search result can jump here.
                let dom_id = sid.map(|i| format!("seg-{i}")).unwrap_or_default();
                // `hit` = cross-session search-result flash.  Find-in-session:
                // active hit reuses the same flash class; other hits get a softer
                // `find-hit` background so all matches are visible at once.
                let hit = if sid.is_some() && sid == hl {
                    " hit"
                } else if sid.is_some() && fhits.contains(&sid.unwrap()) {
                    " find-hit"
                } else {
                    ""
                };
                rsx! {
                    div {
                        key: "{seg.source.as_str()}-{seg.t_start_ms}",
                        id: "{dom_id}",
                        class: "line {line_class_for(seg, *me_speaker.read())}{hit}",
                        // Timestamp: plain when timeline is closed; clickable seek
                        // when timeline is open (Phase 42c).
                        if timeline_active {
                            span {
                                class: "ts tl-jump",
                                title: "Seek timeline to this line",
                                onclick: {
                                    let handler = on_seek;
                                    move |_| {
                                        if let Some(ref h) = handler {
                                            h.call(t0);
                                        }
                                    }
                                },
                                "{fmt_ts(t0)}"
                            }
                        } else {
                            span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
                        }
                        span { class: "who", "{who}" }
                        if is_editing {
                            input {
                                class: "line-edit",
                                value: "{buf}",
                                autofocus: true,
                                oninput: move |e| buf.set(e.value()),
                                onkeydown: move |e| match e.key() {
                                    Key::Enter => {
                                        if let Some(id) = sid {
                                            on_edit.call((id, buf.peek().clone()));
                                        }
                                        editing.set(None);
                                    }
                                    Key::Escape => editing.set(None),
                                    _ => {}
                                },
                            }
                        } else {
                            span {
                                class: "text",
                                title: if sid.is_some() { "Double-click to edit" } else { "" },
                                ondoubleclick: move |_| {
                                    if sid.is_some() {
                                        buf.set(text_for_edit.clone());
                                        editing.set(sid);
                                    }
                                },
                                "{text}"
                            }
                        }
                        if can_play {
                            button {
                                class: if is_playing { "play-btn on" } else { "play-btn" },
                                title: if is_playing { "Stop playback" } else { "Replay this line's audio" },
                                onclick: move |_| {
                                    if is_playing {
                                        on_play.call(None);
                                    } else {
                                        on_play.call(Some((wav_play.clone(), sid.unwrap_or_default(), t0, t1)));
                                    }
                                },
                                {icon(if is_playing { "stop" } else { "play" })}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Dedicated full search view: a query box on top, and matches grouped under the
/// meeting they came from — meetings newest-first, lines within each in
/// chronological order. Clicking a meeting opens it; clicking a line opens it and
/// jumps to that line.
#[component]
fn SearchView(
    results: Signal<Vec<(String, Segment)>>,
    note_results: Signal<Vec<(String, String)>>,
    sessions: Signal<Vec<Session>>,
    query: Signal<String>,
    mut mode: Signal<String>,
    on_query: EventHandler<String>,
    on_open: EventHandler<(String, Option<i64>)>,
    engine: Engine,
) -> Element {
    use std::collections::HashMap;
    let sess = sessions.read();
    let started: HashMap<String, u64> = sess.iter().map(|s| (s.id.clone(), s.started_at)).collect();
    let titles: HashMap<String, String> = sess
        .iter()
        .map(|s| (s.id.clone(), session_title(s)))
        .collect();
    let note_hits: Vec<(String, String)> = note_results.read().clone();

    let is_semantic = cfg!(feature = "semantic") && *mode.read() == "semantic";

    // Group hits by session, sort lines chronologically, sessions newest-first.
    let mut groups: HashMap<String, Vec<Segment>> = HashMap::new();
    for (sid, seg) in results.read().iter() {
        groups.entry(sid.clone()).or_default().push(seg.clone());
    }
    let mut ordered: Vec<(String, Vec<Segment>)> = groups.into_iter().collect();
    for (_, segs) in ordered.iter_mut() {
        segs.sort_by_key(|s| s.t_start_ms);
    }
    ordered.sort_by_key(|(sid, _)| std::cmp::Reverse(started.get(sid).copied().unwrap_or(0)));

    let q_empty = query.read().trim().is_empty();
    let total: usize = ordered.iter().map(|(_, s)| s.len()).sum();
    let no_hits = ordered.is_empty() && note_hits.is_empty();

    rsx! {
        div { class: "search-view",
            div { class: "search-top-row",
                input {
                    class: "search-input-big",
                    r#type: "text",
                    placeholder: if is_semantic { "Meaning-based search…" } else { "Search all transcripts…" },
                    autofocus: true,
                    value: "{query}",
                    oninput: move |e| on_query.call(e.value()),
                }
                // Mode toggle — only visible in `semantic` builds.
                if cfg!(feature = "semantic") {
                    div { class: "search-mode-toggle",
                        button {
                            class: if !is_semantic { "smode active" } else { "smode" },
                            onclick: move |_| {
                                mode.set("keyword".into());
                                // Re-run the current query in the new mode.
                                let q = query.peek().clone();
                                on_query.call(q);
                            },
                            "Keyword"
                        }
                        button {
                            class: if is_semantic { "smode active" } else { "smode" },
                            onclick: move |_| {
                                mode.set("semantic".into());
                                let q = query.peek().clone();
                                on_query.call(q);
                            },
                            "Semantic"
                        }
                    }
                }
            }
            if !q_empty {
                div { class: "search-count",
                    if is_semantic {
                        "Top {total} semantic match(es) across {ordered.len()} meeting(s) · ranked by meaning"
                    } else {
                        "{total} transcript match(es) across {ordered.len()} meeting(s)"
                        if !note_hits.is_empty() { " · {note_hits.len()} in notes" }
                    }
                }
            }
            div { class: "search-results",
                if q_empty {
                    if is_semantic {
                        div { class: "empty", "Search by meaning — finds relevant passages even without exact keyword matches." }
                    } else {
                        div { class: "empty", "Search every meeting's transcript and your session notes." }
                    }
                } else if no_hits {
                    if is_semantic && cfg!(feature = "semantic") {
                        // Empty semantic results — check if the index is incomplete.
                        {
                            let engine_hint = engine.clone();
                            rsx! {
                                div { class: "empty",
                                    "No semantic matches found."
                                    br {}
                                    span { class: "field-note",
                                        "If you haven't built the semantic index yet, go to Settings → AI → \"Build semantic index\"."
                                    }
                                    div { class: "btn-row",
                                        button {
                                            class: "mbtn ghost",
                                            onclick: move |_| {
                                                let _ = engine_hint.embed_tx.send(EmbedCmd::BackfillAll);
                                            },
                                            "Build semantic index now"
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        div { class: "empty", "No matches." }
                    }
                } else {
                    // Host-note matches first (high-signal: links / action items).
                    for (sid, note) in note_hits {
                        {
                            let title = titles.get(&sid).cloned().unwrap_or_else(|| short_id(&sid));
                            let when = started.get(&sid).copied().map(relative_time).unwrap_or_default();
                            let sid_open = sid.clone();
                            rsx! {
                                div { class: "search-group note-group",
                                    div {
                                        class: "search-group-head",
                                        title: "Open this meeting's notes",
                                        onclick: move |_| on_open.call((sid_open.clone(), None)),
                                        span { class: "sg-title", span { class: "sg-badge", "Notes" } " {title}" }
                                        span { class: "sg-meta", "{when}" }
                                    }
                                    div { class: "note-hit", "{note}" }
                                }
                            }
                        }
                    }
                    for (sid, segs) in ordered {
                        {
                            let title = titles.get(&sid).cloned().unwrap_or_else(|| short_id(&sid));
                            let when = started.get(&sid).copied().map(relative_time).unwrap_or_default();
                            let count = segs.len();
                            let sid_head = sid.clone();
                            rsx! {
                                div { class: "search-group",
                                    div {
                                        class: "search-group-head",
                                        title: "Open this meeting",
                                        onclick: move |_| on_open.call((sid_head.clone(), None)),
                                        span { class: "sg-title", "{title}" }
                                        span { class: "sg-meta", "{count} match · {when}" }
                                    }
                                    for seg in segs {
                                        {
                                            let sid_line = sid.clone();
                                            let seg_id = seg.id;
                                            rsx! {
                                                div {
                                                    key: "{seg.t_start_ms}",
                                                    class: "search-hit {line_class(&seg)}",
                                                    title: "Jump to this line",
                                                    onclick: move |_| on_open.call((sid_line.clone(), seg_id)),
                                                    span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
                                                    span { class: "who", "{quick_speaker_label(&seg)}" }
                                                    span { class: "text", "{seg.text}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A grounded chat panel (Phase 23d): a scrolling Q&A log + an input row. The
/// conversation lives in the parent's signals; `on_send` submits the input with
/// the right scope.
#[component]
fn ChatPanel(
    title: String,
    placeholder: String,
    chat: Signal<Vec<(bool, String)>>,
    input: Signal<String>,
    busy: Signal<bool>,
    show: Signal<bool>,
    on_send: EventHandler<()>,
) -> Element {
    let is_busy = busy();
    let open = show();
    rsx! {
        div { class: "chat",
            div { class: "chat-title-row",
                button {
                    class: "panel-toggle",
                    onclick: move |_| { let v = *show.peek(); show.set(!v); },
                    span { class: "chev", if open { "▾" } else { "▸" } }
                    {icon("chat")}
                    span { "{title}" }
                }
            }
            if open {
                div { class: "chat-log",
                    if chat.read().is_empty() && !is_busy {
                        div { class: "chat-hint", "{placeholder}" }
                    }
                    for (i, (is_user, text)) in chat.read().iter().enumerate() {
                        div {
                            key: "{i}",
                            class: if *is_user { "chat-msg user" } else { "chat-msg bot" },
                            span { class: "chat-text", "{text}" }
                        }
                    }
                    if is_busy {
                        div { class: "chat-msg bot", span { class: "chat-text dim", "Thinking…" } }
                    }
                }
                div { class: "chat-input-row",
                    input {
                        class: "chat-input",
                        placeholder: "Ask a question…",
                        value: "{input}",
                        disabled: is_busy,
                        oninput: move |e| input.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                on_send.call(());
                            }
                        },
                    }
                    button {
                        class: "mbtn",
                        disabled: is_busy,
                        onclick: move |_| on_send.call(()),
                        "Ask"
                    }
                }
            }
        }
    }
}

/// Submit the chat input as a new user turn for `scope` and dispatch it.
fn submit_chat(
    engine: &Engine,
    scope: ChatScope,
    mut chat: Signal<Vec<(bool, String)>>,
    mut input: Signal<String>,
    mut chat_scope: Signal<Option<ChatScope>>,
    mut busy: Signal<bool>,
) {
    let q = input.peek().trim().to_string();
    if q.is_empty() || *busy.peek() {
        return;
    }
    input.set(String::new());
    chat.write().push((true, q));
    chat_scope.set(Some(scope.clone()));
    busy.set(true);
    let turns = chat.peek().clone();
    let _ = engine.summ_tx.send(SummCmd::Chat { scope, turns });
}

/// Clear the active conversation (called when the chat scope changes).
fn reset_chat(
    mut chat: Signal<Vec<(bool, String)>>,
    mut input: Signal<String>,
    mut busy: Signal<bool>,
    mut scope: Signal<Option<ChatScope>>,
) {
    chat.write().clear();
    input.set(String::new());
    busy.set(false);
    scope.set(None);
}

/// Settings → Files & folders: jump-to-disk shortcuts, grouped — folders to
/// open, individual files to reveal, and log helpers — kept distinct so the
/// folders aren't mixed in with the database/config files.
#[component]
fn FilesSettings(
    settings: Signal<Settings>,
    mut notice: Signal<Option<String>>,
    engine: Engine,
) -> Element {
    let engine_kb = engine.clone();
    rsx! {
        section { class: "settings-section",
            h3 { "Files & folders" }
            p { class: "field-note", "Jump to Zord's files on disk — handy for dropping in a manually-downloaded model, or grabbing logs when something fails." }

            div { class: "subhead", "Storage" }
            div { class: "btn-row",
                button {
                    class: "mbtn",
                    title: "Convert every kept WAV recording to Opus now, regardless of age",
                    onclick: move |_| {
                        let _ = engine.db_tx.send(DbCmd::CompressAudio { ignore_age: true });
                        notice.set(Some(
                            "Compressing kept recordings in the background — progress in the jobs panel.".to_string(),
                        ));
                    },
                    "Compress all kept recordings now"
                }
            }
            p { class: "field-note", "Frees ~96% of the space at the quality set under Recording & retention. Replay, re-transcribe, and export keep working on compressed recordings." }

            // --- Folders (open in the file manager) ---
            div { class: "subhead", "Folders" }
            div { class: "btn-row",
                button {
                    class: "mbtn",
                    title: "Downloaded transcription / summary / speaker models",
                    onclick: move |_| {
                        if let Ok(d) = zord_config::models_dir() {
                            osutil::open_folder(&d.display().to_string());
                        }
                    },
                    {icon("folder")} "Models"
                }
                button {
                    class: "mbtn",
                    title: "Database, recordings, and exports (the storage root)",
                    onclick: move |_| {
                        if let Ok(d) = settings.peek().storage_dir() {
                            osutil::open_folder(&d.display().to_string());
                        }
                    },
                    {icon("folder")} "Data"
                }
                button {
                    class: "mbtn",
                    title: "Retained per-channel recordings (.wav)",
                    onclick: move |_| {
                        if let Ok(d) = settings.peek().audio_dir() {
                            osutil::open_folder(&d.display().to_string());
                        }
                    },
                    {icon("folder")} "Recordings"
                }
                button {
                    class: "mbtn",
                    title: "Transcripts exported to Markdown / SRT / JSON",
                    onclick: move |_| {
                        if let Ok(d) = settings.peek().exports_dir() {
                            osutil::open_folder(&d.display().to_string());
                        }
                    },
                    {icon("folder")} "Exports"
                }
                button {
                    class: "mbtn",
                    title: "Application logs",
                    onclick: move |_| {
                        if let Ok(d) = zord_config::logs_dir() {
                            osutil::open_folder(&d.display().to_string());
                        }
                    },
                    {icon("folder")} "Logs"
                }
            }

            // --- Knowledge-base export (Phase 44) ---
            div { class: "subhead", "Knowledge-base export" }
            p { class: "field-note",
                "One-way mirror: Zord overwrites files in this folder. \
                 Point your notes app (Obsidian, Logseq) at it. \
                 Edits inside the folder will be overwritten on the next update. \
                 Exported files contain your full meeting transcripts — choose \
                 a location you trust."
            }
            div { class: "settings-row",
                label { class: "settings-label", "Knowledge-base folder" }
                div { class: "settings-control",
                    input {
                        r#type: "text",
                        class: "text-input",
                        placeholder: "Folder path (empty = off)",
                        value: settings.read().kb_export_dir.clone(),
                        onchange: move |e: FormEvent| {
                            let val = e.value().trim().to_string();
                            let mut s = settings.peek().clone();
                            s.kb_export_dir = val;
                            let _ = s.save();
                            settings.set(s);
                        },
                    }
                    button {
                        class: "mbtn",
                        title: "Mirror all sessions and the overview to the configured folder now",
                        onclick: move |_| {
                            if settings.peek().kb_export_dir.trim().is_empty() {
                                notice.set(Some(
                                    "Set a knowledge-base folder path first.".to_string(),
                                ));
                            } else {
                                let _ = engine_kb.db_tx.send(DbCmd::KbExportAll);
                                notice.set(Some(
                                    "Exporting knowledge base in the background — progress in the jobs panel.".to_string(),
                                ));
                            }
                        },
                        "Export everything now"
                    }
                }
            }

            // --- Individual files (reveal in the file manager) ---
            div { class: "subhead", "Files" }
            div { class: "btn-row",
                button {
                    class: "mbtn ghost",
                    title: "settings (config.json)",
                    onclick: move |_| {
                        if let Ok(p) = zord_config::config_path() {
                            osutil::reveal_in_file_manager(&p.display().to_string());
                        }
                    },
                    {icon("file-text")} "Config"
                }
                button {
                    class: "mbtn ghost",
                    title: "the SQLite database (sessions + transcripts)",
                    onclick: move |_| {
                        if let Ok(p) = settings.peek().db_path() {
                            osutil::reveal_in_file_manager(&p.display().to_string());
                        }
                    },
                    {icon("database")} "Database"
                }
            }

            // --- Logs (read / share for bug reports) ---
            div { class: "subhead", "Logs" }
            div { class: "btn-row",
                button {
                    class: "mbtn ghost",
                    onclick: move |_| {
                        match zord_config::logs_dir().map(|d| d.join("zord.log")) {
                            Ok(p) if p.exists() => osutil::open_in_editor(&p.display().to_string()),
                            _ => notice.set(Some("No log file yet — it appears after the next launch.".to_string())),
                        }
                    },
                    {icon("external")} "Open log"
                }
                button {
                    class: "mbtn ghost",
                    title: "Copy the most recent log lines to share in a bug report",
                    onclick: move |_| {
                        let log = zord_config::logs_dir().map(|d| d.join("zord.log"));
                        match log.and_then(|p| std::fs::read_to_string(p).map_err(Into::into)) {
                            Ok(txt) => {
                                let lines: Vec<&str> = txt.lines().collect();
                                let start = lines.len().saturating_sub(200);
                                osutil::copy_to_clipboard(&lines[start..].join("\n"));
                                notice.set(Some(format!("Copied last {} log lines to clipboard", lines.len() - start)));
                            }
                            Err(_) => notice.set(Some("No log file to copy yet.".to_string())),
                        }
                    },
                    {icon("copy")} "Copy recent log"
                }
            }
        }
    }
}

/// Settings → Recording: per-channel capture level — Off / Auto-level / Manual
/// gain. Applies to transcription input, the saved recording, and the meters;
/// a soft limiter prevents clipping (Phase 26).
#[component]
fn LevelSettings(mut settings: Signal<Settings>) -> Element {
    rsx! {
        section { class: "settings-section",
            h3 { "Audio levels" }
            p { class: "field-note", "Adjust each channel's volume into transcription, the saved recording, and the meters. Auto-level evens out a level that varies meeting-to-meeting; Manual applies a fixed gain. A soft limiter prevents clipping either way." }

            div { class: "subhead", "Microphone (Me)" }
            div { class: "field-row",
                label { class: "field-label", "Level" }
                select {
                    onchange: move |e: FormEvent| { let mut s = settings.peek().clone(); s.mic_level_mode = e.value(); let _ = s.save(); settings.set(s); },
                    option { value: "off", selected: settings.read().mic_level_mode == "off", "Off" }
                    option { value: "auto", selected: settings.read().mic_level_mode == "auto", "Auto-level" }
                    option { value: "manual", selected: settings.read().mic_level_mode == "manual", "Manual gain" }
                }
            }
            if settings.read().mic_level_mode == "manual" {
                div { class: "field-row",
                    label { class: "field-label", "Gain: {settings.read().mic_gain_db:.0} dB" }
                    input {
                        r#type: "range", min: "-24", max: "24", step: "1", class: "slider",
                        value: "{settings.read().mic_gain_db}",
                        oninput: move |e: FormEvent| {
                            if let Ok(v) = e.value().parse::<f32>() {
                                let mut s = settings.peek().clone();
                                s.mic_gain_db = v;
                                let _ = s.save();
                                settings.set(s);
                            }
                        },
                    }
                }
            }

            div { class: "subhead", "Desktop / system (Others)" }
            div { class: "field-row",
                label { class: "field-label", "Level" }
                select {
                    onchange: move |e: FormEvent| { let mut s = settings.peek().clone(); s.others_level_mode = e.value(); let _ = s.save(); settings.set(s); },
                    option { value: "off", selected: settings.read().others_level_mode == "off", "Off" }
                    option { value: "auto", selected: settings.read().others_level_mode == "auto", "Auto-level" }
                    option { value: "manual", selected: settings.read().others_level_mode == "manual", "Manual gain" }
                }
            }
            if settings.read().others_level_mode == "manual" {
                div { class: "field-row",
                    label { class: "field-label", "Gain: {settings.read().others_gain_db:.0} dB" }
                    input {
                        r#type: "range", min: "-24", max: "24", step: "1", class: "slider",
                        value: "{settings.read().others_gain_db}",
                        oninput: move |e: FormEvent| {
                            if let Ok(v) = e.value().parse::<f32>() {
                                let mut s = settings.peek().clone();
                                s.others_gain_db = v;
                                let _ = s.save();
                                settings.set(s);
                            }
                        },
                    }
                }
            }
        }
    }
}

/// Settings → Recording & retention: keep-audio toggle + auto-delete window.
#[component]
fn RetentionSettings(mut settings: Signal<Settings>) -> Element {
    rsx! {
        section { class: "settings-section",
            h3 { "Recording & retention" }
            div { class: "field-row",
                label { class: "field-label", "Keep audio after transcription" }
                button {
                    class: if settings.read().keep_audio { "toggle on" } else { "toggle" },
                    onclick: move |_| {
                        let mut s = settings.peek().clone();
                        s.keep_audio = !s.keep_audio;
                        let _ = s.save();
                        settings.set(s);
                    },
                    if settings.read().keep_audio { "On" } else { "Off" }
                }
            }
            div { class: "field",
                label { "Auto-delete kept audio after (days)" }
                input {
                    r#type: "number", min: "0", class: "days", placeholder: "never",
                    value: settings.read().auto_delete_days.map(|n| n.to_string()).unwrap_or_default(),
                    oninput: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        s.auto_delete_days = e.value().trim().parse::<u32>().ok().filter(|n| *n > 0);
                        let _ = s.save();
                        settings.set(s);
                    },
                }
            }
            div { class: "field",
                label { "Compress kept audio after (days)" }
                input {
                    r#type: "number", min: "0", class: "days", placeholder: "never",
                    value: settings.read().compress_after_days.map(|n| n.to_string()).unwrap_or_default(),
                    oninput: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        let v = e.value();
                        s.compress_after_days = if v.trim().is_empty() {
                            None
                        } else {
                            v.trim().parse::<u32>().ok()
                        };
                        let _ = s.save();
                        settings.set(s);
                    },
                }
            }
            div { class: "field",
                label { "Compression quality" }
                select {
                    onchange: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        s.compress_quality = e.value();
                        let _ = s.save();
                        settings.set(s);
                    },
                    option { value: "space", selected: settings.read().compress_quality == "space", "Space saver (24 kbps · ~11 MB/hour)" }
                    option { value: "standard", selected: settings.read().compress_quality == "standard", "Standard (32 kbps · ~14 MB/hour)" }
                    option { value: "high", selected: settings.read().compress_quality == "high", "High (48 kbps · ~21 MB/hour)" }
                }
            }
            p { class: "field-note",
                "Recordings older than this are converted from WAV (~350 MB/hour per track) to Opus — replay, re-transcribe, speaker identification, and export all keep working. 0 = compress as soon as a recording ends; blank = never."
            }
        }
    }
}

/// Settings → Audio input: microphone device + capture-mode pickers.
#[component]
fn AudioInputSettings(mut settings: Signal<Settings>, devices: Vec<String>) -> Element {
    // Per-app capture picker (Phase 31): (id, name) of running apps. Populated
    // when the user switches to "app" mode or presses Refresh — never eagerly,
    // since enumerating apps triggers the Screen Recording prompt on macOS.
    let mut apps = use_signal(Vec::<(String, String)>::new);
    let mut apps_err = use_signal(|| None::<String>);
    let mut refresh_apps = move || match zord_capture::list_capturable_apps() {
        Ok(list) => {
            apps.set(list.into_iter().map(|a| (a.id, a.name)).collect());
            apps_err.set(None);
        }
        Err(e) => apps_err.set(Some(e.to_string())),
    };
    rsx! {
        section { class: "settings-section",
            h3 { "Audio input" }
            div { class: "field",
                label { "Microphone" }
                select {
                    onchange: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        let v = e.value();
                        s.input_device = if v == "__default__" { None } else { Some(v) };
                        let _ = s.save();
                        settings.set(s);
                    },
                    option { value: "__default__", selected: settings.read().input_device.is_none(), "System default" }
                    for d in devices.iter() {
                        option { value: "{d}", selected: settings.read().input_device.as_deref() == Some(d.as_str()), "{d}" }
                    }
                }
            }
            div { class: "field",
                label { "Capture" }
                select {
                    onchange: move |e: FormEvent| {
                        let mut s = settings.peek().clone();
                        // Entering "app" mode: remember the prior mode so the
                        // sidebar's "All system audio" choice can restore it.
                        if e.value() == "app" && s.capture_mode != "app" {
                            s.capture_mode_before_app = s.capture_mode.clone();
                        }
                        s.capture_mode = e.value();
                        let _ = s.save();
                        settings.set(s);
                        if settings.peek().capture_mode == "app" {
                            refresh_apps();
                        }
                    },
                    option { value: "both", selected: settings.read().capture_mode == "both", "Microphone + system audio" }
                    option { value: "mic", selected: settings.read().capture_mode == "mic", "Microphone only (Me)" }
                    option { value: "system", selected: settings.read().capture_mode == "system", "System audio only (Others)" }
                    option { value: "app", selected: settings.read().capture_mode == "app", "Microphone + one app's audio" }
                }
            }
            if settings.read().capture_mode == "app" {
                div { class: "field",
                    label { "App to capture" }
                    select {
                        onchange: move |e: FormEvent| {
                            let mut s = settings.peek().clone();
                            s.capture_app_id = e.value();
                            s.capture_app_name = apps
                                .peek()
                                .iter()
                                .find(|(id, _)| *id == s.capture_app_id)
                                .map(|(_, name)| name.clone())
                                .unwrap_or_default();
                            let _ = s.save();
                            settings.set(s);
                        },
                        if settings.read().capture_app_id.is_empty() {
                            option { value: "", selected: true, disabled: true, "Choose an app…" }
                        } else if !apps.read().iter().any(|(id, _)| *id == settings.read().capture_app_id) {
                            // Keep the saved choice visible even when that app
                            // isn't running right now.
                            option {
                                value: "{settings.read().capture_app_id}",
                                selected: true,
                                "{settings.read().capture_app_name} (not running)"
                            }
                        }
                        for (id, name) in apps.read().iter() {
                            option { value: "{id}", selected: settings.read().capture_app_id == *id, "{name}" }
                        }
                    }
                    button { class: "mbtn ghost", onclick: move |_| refresh_apps(), "Refresh apps" }
                }
                if let Some(e) = apps_err.read().clone() {
                    p { class: "field-note", "⚠ Couldn't list apps: {e}" }
                }
                p { class: "field-note",
                    "Records your microphone plus only this app's audio (music and notifications stay out). The app must be running when you press Record. Speakers in its audio are identified by diarization, as with system capture."
                }
            }
        }
    }
}

/// The background-jobs board: one row per running cancellable job, with elapsed
/// time and a cancel (✕) button. Driven by the engine's job registry, so it's
/// independent of the viewed session. Visibility is gated at the call site.
#[component]
fn JobsPanel(
    mut show_jobs: Signal<bool>,
    jobs: Signal<Vec<JobView>>,
    job_tick: Signal<u64>,
    diarize_est_secs: Signal<Option<u64>>,
    on_cancel: EventHandler<String>,
) -> Element {
    let _ = job_tick.read(); // re-render each second for elapsed timers
    let now = now_ms();
    let est = *diarize_est_secs.read();
    let rows = jobs.read().clone();
    rsx! {
        div { class: "jobs-overlay", onclick: move |_| show_jobs.set(false),
            div {
                class: "jobs-card",
                onclick: move |e| e.stop_propagation(),
                div { class: "jobs-head",
                    span { "Background jobs" }
                    button { class: "close-btn", onclick: move |_| show_jobs.set(false), {icon("close")} }
                }
                for job in rows {
                    {
                        let elapsed = now.saturating_sub(job.started_at) / 1000;
                        let detail = job_detail(&job.kind, est, elapsed);
                        let id = job.id.clone();
                        rsx! {
                            div { key: "{job.id}", class: "job-row",
                                span { class: "job-icon", {icon(job_icon(&job.kind))} }
                                div { class: "job-main",
                                    div { class: "job-title", "{job.label}" }
                                    div { class: "job-detail", "{detail}" }
                                }
                                span { class: "job-time", "{fmt_dur(elapsed)}" }
                                button {
                                    class: "job-cancel",
                                    title: "Cancel this job",
                                    onclick: move |_| on_cancel.call(id.clone()),
                                    {icon("close")}
                                }
                            }
                        }
                    }
                }
                div { class: "jobs-foot", "Cancel stops a job at its next safe point; in-progress local LLM work may finish in the background." }
            }
        }
    }
}

/// Topbar text for the engine status.
fn status_label(st: &Status) -> String {
    match st {
        Status::Idle => "Idle".to_string(),
        Status::PreparingModel => "Preparing model…".to_string(),
        Status::Downloading(p) => format!("Downloading model… {p}%"),
        Status::Recording => "Recording".to_string(),
        Status::Error(e) => format!("Error: {e}"),
    }
}

/// Registry icon for a background-job kind.
fn job_icon(kind: &str) -> &'static str {
    match kind {
        "summarize" => "sparkles",
        "compress" => "archive",
        "overview" => "overview",
        "diarize" => "users",
        "retranscribe" => "refresh",
        _ => "sparkles",
    }
}

/// Per-job detail line (ETA for diarization; a short hint otherwise).
fn job_detail(kind: &str, est: Option<u64>, elapsed: u64) -> String {
    match kind {
        "diarize" => match est {
            Some(e) => format!("~{} left (estimate)", fmt_dur(e.saturating_sub(elapsed))),
            None => "processing audio…".to_string(),
        },
        "retranscribe" => "transcribing the kept audio…".to_string(),
        "overview" => "compressing + synthesizing…".to_string(),
        "summarize" => "generating notes…".to_string(),
        "compress" => "condensing…".to_string(),
        _ => "running…".to_string(),
    }
}

/// Map a capture-mode string to `(record_mic, record_system)` flags.
fn capture_sources(mode: &str) -> (bool, bool) {
    match mode {
        "mic" => (true, false),
        "system" => (false, true),
        _ => (true, true),
    }
}

fn source_class(s: Source) -> &'static str {
    match s {
        Source::Me => "me",
        Source::Others => "others",
    }
}

/// CSS class for a transcript line: the source, plus a rotating per-speaker
/// class (`spk-0`..`spk-7`) so diarized speakers get distinct accent colors.
fn line_class(seg: &Segment) -> String {
    match (seg.source, seg.speaker) {
        (Source::Others, Some(idx)) => format!("others spk-{}", idx.rem_euclid(8)),
        (s, _) => source_class(s).to_string(),
    }
}

/// [`line_class`], but the app user's own speaker track styles as "me":
/// integration sessions record everyone as a uniform spk-N track and tag which
/// index is the user, so identity drives the color, not the channel.
fn line_class_for(seg: &Segment, me_speaker: Option<i32>) -> String {
    match (seg.source, seg.speaker, me_speaker) {
        (Source::Others, Some(idx), Some(me)) if idx == me => "me".to_string(),
        _ => line_class(seg),
    }
}

/// Speaker label without a custom-name map (used in cross-session search where
/// names aren't loaded): "Speaker N" for diarized "Others", else the source.
fn quick_speaker_label(seg: &Segment) -> String {
    match (seg.source, seg.speaker) {
        (Source::Others, Some(idx)) => format!("Speaker {}", idx + 1),
        (s, _) => s.label().to_string(),
    }
}

fn fmt_ts(ms: u64) -> String {
    let total_s = ms / 1000;
    format!("{:02}:{:02}", total_s / 60, total_s % 60)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn session_title(s: &Session) -> String {
    match s.title.as_ref().filter(|t| !t.trim().is_empty()) {
        Some(t) => t.clone(),
        // The time lives on the meta line, so the fallback is just a label.
        None => "Recording".to_string(),
    }
}

/// Sidebar date bucket for grouping sessions (UTC-day based).
fn date_group(started_at: u64, now: u64) -> &'static str {
    let day = (started_at / 86_400_000) as i64;
    let today = (now / 86_400_000) as i64;
    match today - day {
        d if d <= 0 => "Today",
        1 => "Yesterday",
        d if d < 7 => "This week",
        d if d < 30 => "This month",
        _ => "Older",
    }
}

/// "just now" / "5m ago" / "2h ago" / "3d ago".
fn relative_time(ms: u64) -> String {
    let secs = now_ms().saturating_sub(ms) / 1000;
    if secs < 60 {
        "just now".to_string()
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

/// Segmentation-model choices for the Speakers settings: (id, label).
/// Empty without the diarization feature (the section isn't shown then).
fn seg_model_options() -> Vec<(&'static str, &'static str)> {
    #[cfg(feature = "diarization")]
    {
        zord_diarize::SegmentationModel::ALL
            .iter()
            .map(|m| (m.name(), m.label()))
            .collect()
    }
    #[cfg(not(feature = "diarization"))]
    {
        Vec::new()
    }
}

/// "Jun 4, 2026" in local time.
fn fmt_date(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|d| d.format("%b %-d, %Y").to_string())
        .unwrap_or_default()
}

/// Sidebar second line: date + how long ago + duration when known.
fn session_meta(s: &Session) -> String {
    let mut meta = format!(
        "{} · {}",
        fmt_date(s.started_at),
        relative_time(s.started_at)
    );
    if let Some(secs) = s.ended_at.map(|e| e.saturating_sub(s.started_at) / 1000) {
        if secs > 0 {
            meta.push_str(&format!(" · {}", fmt_dur(secs)));
        }
    }
    meta
}

fn fmt_dur(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}

/// Compute the contiguous range of session ids between `anchor` and `clicked`
/// in the given ordered slice.  Both endpoints are inclusive.  If either id is
/// absent the result is empty.  Works regardless of which endpoint comes first
/// in `items_in_order` (Shift-click in either direction).
fn range_ids(items_in_order: &[String], anchor: &str, clicked: &str) -> Vec<String> {
    let pos_a = items_in_order.iter().position(|id| id == anchor);
    let pos_b = items_in_order.iter().position(|id| id == clicked);
    match (pos_a, pos_b) {
        (Some(a), Some(b)) => {
            let lo = a.min(b);
            let hi = a.max(b);
            items_in_order[lo..=hi].to_vec()
        }
        _ => vec![],
    }
}

#[cfg(test)]
mod range_ids_tests {
    use super::range_ids;

    #[test]
    fn range_ids_forward() {
        let items: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let got = range_ids(&items, "a", "c");
        assert_eq!(got, vec!["a", "b", "c"]);
    }

    #[test]
    fn range_ids_reversed() {
        let items: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        let got = range_ids(&items, "c", "a");
        assert_eq!(got, vec!["a", "b", "c"]);
    }

    #[test]
    fn range_ids_same() {
        let items: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let got = range_ids(&items, "b", "b");
        assert_eq!(got, vec!["b"]);
    }

    #[test]
    fn range_ids_anchor_missing() {
        let items: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let got = range_ids(&items, "z", "b");
        assert!(got.is_empty());
    }
}

/// Black-or-white text for a `#rrggbb` background, by relative luminance —
/// keeps any user-picked theme color readable. Garbage input gets the
/// default dark ink (matches the built-in cyan's pairing).
fn readable_fg(hex: &str) -> &'static str {
    if !zord_config::is_valid_hex_color(hex) {
        return "#06222f";
    }
    let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0) as f32 / 255.0;
    let lin = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = 0.2126 * lin(v(1)) + 0.7152 * lin(v(3)) + 0.0722 * lin(v(5));
    if lum > 0.35 {
        "#06222f"
    } else {
        "#ffffff"
    }
}

/// Phase 42c: close the timeline panel, stop playback, and clear lane/pos state.
/// Called when leaving a session or switching sessions.
fn close_timeline(
    engine: &Engine,
    mut timeline_open: Signal<bool>,
    mut timeline_lanes: Signal<Vec<TimelineLane>>,
    mut timeline_pos: Signal<Option<u64>>,
) {
    if *timeline_open.peek() {
        timeline_open.set(false);
        let _ = engine.play_tx.send(PlayCmd::Stop);
    }
    timeline_lanes.write().clear();
    timeline_pos.set(None);
}

/// Returns the db ids of transcript segments whose text contains `query`
/// (case-insensitive substring match). Segments without a db id are skipped.
/// Empty query returns an empty vec. Order follows transcript order.
fn find_hits(segments: &[Segment], query: &str) -> Vec<i64> {
    if query.is_empty() {
        return Vec::new();
    }
    let q = query.to_lowercase();
    segments
        .iter()
        .filter_map(|s| s.id.filter(|_| s.text.to_lowercase().contains(&q)))
        .collect()
}

/// CSS custom-property overrides for the `.app` root from the theme settings.
/// Only valid `#rrggbb` values are emitted (inputs are also validated at the
/// settings layer; this is the second gate before a style attribute).
fn theme_style(s: &Settings) -> String {
    let mut css = String::new();
    for (var, value) in [
        ("--accent", &s.theme_accent),
        ("--me", &s.theme_me),
        ("--others", &s.theme_others),
    ] {
        if zord_config::is_valid_hex_color(value) {
            css.push_str(&format!(
                "{var}: {value}; {var}-fg: {}; ",
                readable_fg(value)
            ));
        }
    }
    css.trim_end().to_string()
}

#[cfg(test)]
mod find_tests {
    use super::*;
    use zord_core::{Segment, Source};

    fn seg(id: Option<i64>, text: &str) -> Segment {
        Segment {
            id,
            source: Source::Me,
            t_start_ms: 0,
            t_end_ms: 1000,
            text: text.into(),
            words: vec![],
            speaker: None,
        }
    }

    #[test]
    fn empty_query_returns_empty() {
        let segs = vec![seg(Some(1), "hello world")];
        assert!(find_hits(&segs, "").is_empty());
    }

    #[test]
    fn case_insensitive_match() {
        let segs = vec![seg(Some(1), "Hello World"), seg(Some(2), "foo bar")];
        assert_eq!(find_hits(&segs, "hello"), vec![1]);
        assert_eq!(find_hits(&segs, "WORLD"), vec![1]);
        assert_eq!(find_hits(&segs, "FOO"), vec![2]);
    }

    #[test]
    fn no_match_returns_empty() {
        let segs = vec![seg(Some(1), "hello"), seg(Some(2), "world")];
        assert!(find_hits(&segs, "zzz").is_empty());
    }

    #[test]
    fn segments_without_id_are_skipped() {
        let segs = vec![seg(None, "hello"), seg(Some(2), "hello")];
        assert_eq!(find_hits(&segs, "hello"), vec![2]);
    }

    #[test]
    fn order_follows_transcript_order() {
        let segs = vec![
            seg(Some(10), "apple"),
            seg(Some(5), "pineapple"),
            seg(Some(7), "mango"),
        ];
        // "apple" matches id 10 and 5 — order follows slice order
        let hits = find_hits(&segs, "apple");
        assert_eq!(hits, vec![10, 5]);
    }

    #[test]
    fn multiple_hits_across_segments() {
        let segs = vec![
            seg(Some(1), "the quick brown fox"),
            seg(Some(2), "jumped over the lazy dog"),
            seg(Some(3), "the end"),
        ];
        let hits = find_hits(&segs, "the");
        assert_eq!(hits, vec![1, 2, 3]);
    }
}

#[cfg(test)]
mod theme_tests {
    use super::*;

    #[test]
    fn readable_fg_picks_contrast() {
        assert_eq!(readable_fg("#ffffff"), "#06222f"); // light bg → dark text
        assert_eq!(readable_fg("#4cc2ff"), "#06222f"); // cyan is light
        assert_eq!(readable_fg("#1a1a2e"), "#ffffff"); // dark bg → white text
        assert_eq!(readable_fg("#5865f2"), "#ffffff"); // blurple is dark enough
        assert_eq!(readable_fg("not-a-color"), "#06222f"); // garbage → default
    }

    #[test]
    fn theme_style_only_emits_valid_overrides() {
        let mut s = zord_config::Settings::default();
        assert_eq!(theme_style(&s), "");
        s.theme_accent = "#5865f2".into();
        let css = theme_style(&s);
        assert!(css.contains("--accent: #5865f2;"));
        assert!(css.contains("--accent-fg: #ffffff;"));
        s.theme_me = "junk".into(); // invalid → ignored
        assert!(!theme_style(&s).contains("--me:"));
        s.theme_others = "#3ecf8e".into();
        assert!(theme_style(&s).contains("--others: #3ecf8e;"));
    }
}
