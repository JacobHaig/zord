//! Zord desktop GUI (Dioxus 0.7). Record mic + system audio, watch the
//! transcript stream in live, browse past sessions, and full-text search.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod osutil;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use engine::{
    ChatScope, DbCmd, Engine, Event, ItemView, LedgerView, ModelCmd, ModelInfo, OverviewData,
    PlayCmd, ProjectView, RecorderCmd, Status, SummCmd,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zord_config::Settings;
use zord_core::{Segment, Session, Source};
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
        // AI / speaker actions
        "sparkles" => "<path d='M12 3l1.7 5.3a2 2 0 0 0 1.3 1.3L20.3 11l-5.3 1.7a2 2 0 0 0-1.3 1.3L12 19.3l-1.7-5.3a2 2 0 0 0-1.3-1.3L3.7 11l5.3-1.7a2 2 0 0 0 1.3-1.3z'/>",
        "archive" => "<rect x='3' y='4' width='18' height='4' rx='1'/><path d='M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8'/><line x1='10' y1='12' x2='14' y2='12'/>",
        "users" => "<circle cx='9' cy='8' r='3.5'/><path d='M3 20v-1a5 5 0 0 1 5-5h2a5 5 0 0 1 5 5v1'/><path d='M16 5.5a3.5 3.5 0 0 1 0 6.8'/><path d='M21 20v-1a5 5 0 0 0-3.5-4.8'/>",
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
                .with(tracing_subscriber::fmt::layer().with_ansi(false).with_writer(writer))
                .init();
            tracing::info!(path = %dir.join("zord.log").display(), "file logging enabled");
            Some(guard)
        }
        Err(e) => {
            tracing_subscriber::registry().with(filter).with(stderr_layer).init();
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
}

/// A manual edit to the project ledger (Phase 26e). Carried by a single
/// `EventHandler<LedgerAction>` so the ledger components stay decoupled from the
/// `Engine` (which isn't `PartialEq` and so can't be a component prop); the App
/// translates each into the matching [`DbCmd`].
#[derive(Clone, PartialEq)]
enum LedgerAction {
    RenameProject { id: String, name: String },
    SetDescription { id: String, description: String },
    Archive { id: String, archived: bool },
    DeleteProject(String),
    EditItem { id: String, text: String, owner: String },
    SetItemStatus { id: String, status: String },
    MoveItem { item_id: String, project_id: String },
    DeleteItem(String),
    AddItem { project_id: String, kind: String, text: String, owner: String },
}

// ---------------------------------------------------------------------------
// Encryption gate: runs before the main app so the DB key is set (and any
// pending migration applied) before the engine opens any connection.
// ---------------------------------------------------------------------------

#[cfg(feature = "encryption")]
fn gate_db_path() -> PathBuf {
    Settings::load().db_path().unwrap_or_else(|_| PathBuf::from("zord.db"))
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
    let mut session_filter = use_signal(String::new);
    let mut search_results = use_signal(Vec::<(String, Segment)>::new);
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
    let mut show_summary = use_signal(|| true);
    let mut show_compressed = use_signal(|| false);
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
    // Retained WAV paths (me, others) that exist on disk for the viewed session.
    // A line only gets a replay button when its channel's file is present.
    let mut audio_files = use_signal(|| (Option::<String>::None, Option::<String>::None));
    // The transcript line (db id) currently playing back.
    let mut playing_seg = use_signal(|| Option::<i64>::None);
    // Cross-meeting Overview rollup (Phase 23c) + whether a synthesis is running.
    let mut overview = use_signal(|| Option::<OverviewData>::None);
    let mut overview_busy = use_signal(|| false);
    // Phase 26: the rolling project ledger (the Overview view is now this).
    let mut ledger = use_signal(|| Option::<LedgerView>::None);
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
    let mut editing = use_signal(|| Option::<String>::None);
    let mut edit_text = use_signal(String::new);
    let mut confirm_delete = use_signal(|| Option::<String>::None);
    // Session id awaiting Re-transcribe confirmation (Phase 25c).
    let mut confirm_retranscribe = use_signal(|| Option::<String>::None);
    // Seconds elapsed in the current recording (0 when idle).
    let mut rec_secs = use_signal(|| 0u64);
    // Whether the mic ("Me") / desktop ("Others") channels are muted during the
    // current recording.
    let mut mic_muted = use_signal(|| false);
    let mut sys_muted = use_signal(|| false);
    let mut settings = use_signal(Settings::load);
    let mut show_settings = use_signal(|| false);
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
    // Background-jobs indicator (Phase 24-ish polish): a live board of running
    // work. `job_starts` maps an active job key → start time (ms); reconciled from
    // the existing busy signals so no engine changes are needed. `job_tick` forces
    // the elapsed timers to re-render each second; `diarize_est_secs` is a rough
    // ETA for diarization scaled to the meeting length (captured at click time).
    let mut show_jobs = use_signal(|| false);
    // Whether the Export format dropdown is open.
    let mut show_export_menu = use_signal(|| false);
    // The contextual "Generate ▾" menu (Summarize/Compress/Identify/Re-transcribe).
    let mut show_generate_menu = use_signal(|| false);
    // Active tab in the settings overlay's left nav (Phase 3).
    let mut settings_tab = use_signal(|| "transcription".to_string());
    let mut job_starts = use_signal(std::collections::HashMap::<String, u64>::new);
    let mut job_tick = use_signal(|| 0u64);
    let mut diarize_est_secs = use_signal(|| Option::<u64>::None);

    // Create the engine once and drain its events into signals.
    let engine = use_hook(|| {
        let initial = settings.peek().clone();
        // Apply audio retention on startup.
        if let Ok(dir) = initial.audio_dir() {
            zord_config::apply_retention(&dir, initial.auto_delete_days);
        }
        let db = initial.db_path().unwrap_or_else(|_| PathBuf::from("zord.db"));
        let (engine, mut ev_rx) = Engine::spawn(db);
        spawn(async move {
            // Per-event application. `Level` is handled separately (coalesced
            // below) so a burst of meter updates can never starve control events
            // like `Status::Idle` (which is what flips the Stop button back).
            let mut apply = move |ev: Event| match ev {
                Event::Status(s) => status.set(s),
                Event::Notice(n) => notice.set(Some(n)),
                Event::Segment(seg) => {
                    // Always buffer the live stream so it's intact when you return
                    // to the Live view, even if you navigated away mid-recording.
                    live_segments.write().push(seg);
                }
                Event::Level { .. } => {} // handled via coalescing
                Event::Sessions(v) => sessions.set(v),
                Event::SessionBadges(b) => session_badges.set(b),
                Event::SearchResults(v) => search_results.set(v),
                Event::Transcript(v) => segments.set(v),
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
                Event::Overview(v) => {
                    overview.set(v);
                    overview_busy.set(false);
                }
                Event::Ledger(v) => {
                    ledger.set(Some(v));
                    overview_busy.set(false);
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
                Event::AudioFiles { me, others } => audio_files.set((me, others)),
                Event::Retranscribing => retranscribing.set(true),
                Event::Retranscribed => retranscribing.set(false),
                Event::Playing(v) => playing_seg.set(v),
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
                        Event::Level { source: Source::Me, level } => last_me = Some(level),
                        Event::Level { source: Source::Others, level } => last_others = Some(level),
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

    // Reconcile the background-jobs board from the live busy signals: insert a
    // start time when a job appears, drop it when it finishes.
    use_effect(move || {
        let mut active: Vec<&str> = Vec::new();
        if matches!(*status.read(), Status::Recording) {
            active.push("record");
        }
        if model_progress.read().is_some() {
            active.push("download");
        }
        if retranscribing() {
            active.push("transcribe");
        }
        if summarizing() {
            active.push("summarize");
        }
        if compressing() {
            active.push("compress");
        }
        if diarizing() {
            active.push("diarize");
        }
        if overview_busy() {
            active.push("overview");
        }
        if chat_busy() {
            active.push("chat");
        }
        let now = now_ms();
        let mut starts = job_starts.write();
        for k in &active {
            starts.entry((*k).to_string()).or_insert(now);
        }
        starts.retain(|k, _| active.iter().any(|a| *a == k));
    });

    // Tick once a second while any job is running so elapsed timers update.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            if !job_starts.peek().is_empty() {
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

    let on_record = {
        let engine = engine.clone();
        move |_| {
            if recording {
                tracing::info!("record button: Stop clicked");
                let _ = engine.rec_tx.send(RecorderCmd::Stop);
                let _ = engine.db_tx.send(DbCmd::ListSessions);
            } else {
                tracing::info!("record button: Record clicked");
                segments.write().clear();
                live_segments.write().clear();
                notice.set(None);
                summary.set(None);
                compressed.set(None);
                summarizing.set(false);
                compressing.set(false);
                // NOTE: diarization is a detached background job keyed by session
                // (cleared by Event::Diarized) — starting a recording must not
                // clear its in-progress indicator.
                retranscribing.set(false);
                audio_files.set((None, None));
                let _ = engine.play_tx.send(PlayCmd::Stop);
                reset_chat(chat, chat_input, chat_busy, chat_scope);
                speaker_names.write().clear();
                mic_muted.set(false);
                sys_muted.set(false);
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
                });
            }
        }
    };

    // Which channels the current capture mode includes (drives the mute buttons).
    let mic_in_capture = settings.read().capture_mode != "system";
    let system_in_capture = settings.read().capture_mode != "mic";
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

    // Open the Overview (Phase 26 ledger) view and load the stored ledger — no
    // LLM work on open; folding new meetings happens only on explicit Refresh.
    let on_open_overview = {
        let engine = engine.clone();
        move |_| {
            view.set(View::Overview);
            reset_chat(chat, chat_input, chat_busy, chat_scope);
            let _ = engine.db_tx.send(DbCmd::LoadLedger);
            // Also load any legacy rollup, shown as a fallback until the ledger
            // is first folded (graceful upgrade — Phase 26 migration).
            let _ = engine.db_tx.send(DbCmd::LoadOverview);
        }
    };

    // Fold not-yet-applied meetings into the ledger (heavy; background).
    let on_refresh_ledger = {
        let engine = engine.clone();
        move |_| {
            overview_busy.set(true);
            notice.set(Some(
                "Updating the project ledger — folding in new meetings. Runs in the background.".to_string(),
            ));
            let _ = engine.summ_tx.send(SummCmd::FoldOverview { rebuild: false });
        }
    };

    // Destructive: wipe the ledger and replay every meeting (loses manual edits).
    let on_rebuild_ledger = {
        let engine = engine.clone();
        move |_| {
            overview_busy.set(true);
            notice.set(Some(
                "Rebuilding the ledger from scratch — replaying every meeting. Runs in the background.".to_string(),
            ));
            let _ = engine.summ_tx.send(SummCmd::FoldOverview { rebuild: true });
        }
    };

    // Translate a ledger edit from any of the ledger components into a DbCmd
    // (each handler re-emits the updated ledger).
    let on_ledger_action = {
        let engine = engine.clone();
        move |a: LedgerAction| {
            let cmd = match a {
                LedgerAction::RenameProject { id, name } => DbCmd::RenameProject { id, name },
                LedgerAction::SetDescription { id, description } => {
                    DbCmd::SetProjectDescription { id, description }
                }
                LedgerAction::Archive { id, archived } => {
                    DbCmd::SetProjectArchived { id, archived }
                }
                LedgerAction::DeleteProject(id) => DbCmd::DeleteProject(id),
                LedgerAction::EditItem { id, text, owner } => DbCmd::EditItem { id, text, owner },
                LedgerAction::SetItemStatus { id, status } => DbCmd::SetItemStatus { id, status },
                LedgerAction::MoveItem { item_id, project_id } => {
                    DbCmd::MoveItem { item_id, project_id }
                }
                LedgerAction::DeleteItem(id) => DbCmd::DeleteItem(id),
                LedgerAction::AddItem { project_id, kind, text, owner } => {
                    DbCmd::AddItem { project_id, kind, text, owner }
                }
            };
            let _ = engine.db_tx.send(cmd);
        }
    };

    // Open the dedicated search view (does not clear the prior query/results).
    let on_open_search = move |_| view.set(View::Search);

    // Run a query from the search view's input.
    let on_query = {
        let engine = engine.clone();
        move |q: String| {
            search_query.set(q.clone());
            if q.trim().is_empty() {
                search_results.set(Vec::new());
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
            // diarization is a detached per-session background job — don't clear
            // its indicator just because we opened a different session.
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

    rsx! {
        style { dangerous_inner_html: CSS }
        div { class: "app",
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
            nav { class: "rail",
                div { class: "rail-top",
                    div { class: "rail-brand", "Z" }
                    button {
                        class: if matches!(&*view.read(), View::Overview) { "rail-btn active" } else { "rail-btn" },
                        title: "Overview — a project-grouped rollup across recent meetings",
                        onclick: on_open_overview,
                        {icon("overview")}
                    }
                    button {
                        class: if matches!(&*view.read(), View::Search) { "rail-btn active" } else { "rail-btn" },
                        title: "Search across every meeting's transcript",
                        onclick: on_open_search,
                        {icon("search")}
                    }
                }
                div { class: "rail-bottom",
                    if job_starts.read().len() > 0 {
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
                        onclick: on_toggle_settings,
                        {icon("settings")}
                    }
                }
            }
            // ---- Sidebar: session history + Record ----
            aside { class: "sidebar", style: "width: {sidebar_w}px;",
                div { class: "side-label", "Sessions" }
                if sessions.read().len() > 6 {
                    input {
                        class: "session-filter",
                        placeholder: "Filter by title…",
                        value: "{session_filter}",
                        oninput: move |e| session_filter.set(e.value()),
                    }
                }
                div { class: "session-list",
                    // While recording, a pinned entry to jump back to the live view
                    // (the in-progress session isn't in the saved list until it ends).
                    if recording {
                        div {
                            class: if matches!(&*view.read(), View::Live) { "session live-rec active" } else { "session live-rec" },
                            onclick: move |_| {
                                // Back to the live view; drop any panels left from a
                                // saved session viewed mid-recording.
                                view.set(View::Live);
                                summary.set(None);
                                compressed.set(None);
                                speaker_names.write().clear();
                            },
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
                        let items: Vec<(Option<&'static str>, Session)> = sessions
                            .read()
                            .iter()
                            .filter(|s| q.is_empty() || session_title(s).to_lowercase().contains(q.as_str()))
                            .cloned()
                            .map(|s| {
                                let g = date_group(s.started_at, now);
                                let hdr = if last_group != Some(g) { last_group = Some(g); Some(g) } else { None };
                                (hdr, s)
                            })
                            .collect();
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
                            let active = matches!(&*view.read(), View::Session(v) if *v == id);
                            let is_editing = editing.read().as_deref() == Some(id.as_str());
                            let title = session_title(&s);
                            let meta = session_meta(&s);
                            let (b_sum, b_comp, b_spk) =
                                session_badges.read().get(&id).copied().unwrap_or((false, false, false));
                            let b_audio = s.audio_path.is_some();
                            let eng_open = engine.clone();
                            let eng_save = engine.clone();
                            let (id_open, id_edit, id_save, id_del) =
                                (id.clone(), id.clone(), id.clone(), id.clone());
                            let title_edit = title.clone();
                            rsx! {
                                div { key: "{s.id}", class: "session-wrap",
                                if let Some(h) = group_hdr {
                                    div { class: "date-group", "{h}" }
                                }
                                div {
                                    class: if active { "session active" } else { "session" },
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
                                            onclick: move |_| {
                                                view.set(View::Session(id_open.clone()));
                                                last_export.set(None);
                                                summary.set(None);
                                                compressed.set(None);
                                                summarizing.set(false);
                                                compressing.set(false);
                                                // detached per-session job — see Event::Diarized
                                                retranscribing.set(false);
                                                diar_speakers.set(String::new());
                                                audio_files.set((None, None));
                                                let _ = eng_open.play_tx.send(PlayCmd::Stop);
                                                reset_chat(chat, chat_input, chat_busy, chat_scope);
                                                let _ = eng_open.db_tx.send(DbCmd::Load(id_open.clone()));
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
                    if recording && system_in_capture {
                        button {
                            class: if *sys_muted.read() { "record muted" } else { "record mute" },
                            title: if *sys_muted.read() { "Desktop audio muted — click to unmute" } else { "Mute desktop / system audio" },
                            onclick: on_mute_system,
                            {icon(if *sys_muted.read() { "speaker-off" } else { "speaker" })}
                            if *sys_muted.read() { "Unmute desktop" } else { "Mute desktop" }
                        }
                    }
                    if recording && mic_in_capture {
                        button {
                            class: if *mic_muted.read() { "record muted" } else { "record mute" },
                            title: if *mic_muted.read() { "Mic muted — click to unmute" } else { "Mute your microphone" },
                            onclick: on_mute,
                            {icon(if *mic_muted.read() { "mic-off" } else { "mic" })}
                            if *mic_muted.read() { "Unmute mic" } else { "Mute mic" }
                        }
                    }
                    button {
                        class: if recording { "record stop" } else { "record" },
                        onclick: on_record,
                        {icon(if recording { "stop" } else { "record" })}
                        if recording { "Stop" } else { "Record" }
                    }
                }
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
                    {
                        let id = id.clone();
                        let engine = engine.clone();
                        let sid = id.clone();
                        let eng_sum = engine.clone();
                        let eng_comp = engine.clone();
                        let sid_comp = id.clone();
                        let eng_diar = engine.clone();
                        let sid_diar = id.clone();
                        let sid_rt = id.clone();
                        let mk = move |fmt: Format| {
                            let id = id.clone();
                            let engine = engine.clone();
                            move |_| {
                                let _ = engine.db_tx.send(DbCmd::Export { id: id.clone(), format: fmt });
                                show_export_menu.set(false);
                            }
                        };
                        // Contextual state for the Generate menu rows.
                        let has_summary = summary.read().is_some();
                        let has_compressed = compressed.read().is_some();
                        let has_others_audio = audio_files.read().1.is_some();
                        let has_any_audio = audio_files.read().0.is_some() || audio_files.read().1.is_some();
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
                                                title: "Condense this meeting into token-minimal dense prose for cross-meeting synthesis",
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
                                        }
                                    }
                                }

                                // --- Output cluster (right) ---
                                div { class: "export-spacer" }
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
                }

                // AI summary (when present for the viewed session).
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

                // Dense-prose compression (Phase 23). Machine-oriented, so the
                // body is collapsed by default; the user can expand or copy it.
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

                // Speaker legend (rename diarized speakers) — only for a saved
                // session that has speaker labels.
                if let View::Session(id) = &*view.read() {
                    {
                        let id = id.clone();
                        let engine = engine.clone();
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
                                            rsx! {
                                                input {
                                                    key: "{idx}",
                                                    class: "speaker-name spk-{idx}",
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

                // Transcript / results. Pass signals so the list subscribes and
                // re-renders itself; App is not re-rendered on each new segment.
                if *view.read() == View::Search {
                    SearchView {
                        results: search_results,
                        sessions,
                        query: search_query,
                        on_query,
                        on_open: on_open_result,
                    }
                } else {
                    div { class: "transcript",
                        if *view.read() == View::Overview {
                            LedgerPanel {
                                ledger,
                                legacy: overview,
                                busy: overview_busy,
                                notice,
                                on_action: on_ledger_action,
                                on_refresh: on_refresh_ledger,
                                on_rebuild: on_rebuild_ledger,
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
                                TranscriptView { segments: live_segments, speaker_names, highlight: highlight_seg, on_edit: on_edit_segment, audio: audio_files, playing: playing_seg, on_play: on_play_segment }
                            }
                        } else {
                            TranscriptView { segments, speaker_names, highlight: highlight_seg, on_edit: on_edit_segment, audio: audio_files, playing: playing_seg, on_play: on_play_segment }
                        }
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
        if *show_jobs.read() && !job_starts.read().is_empty() {
            JobsPanel { show_jobs, job_starts, job_tick, model_progress, diarize_est_secs }
        }

        // ---- Confirm-delete dialog ----
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

        // ---- Confirm-retranscribe dialog (Phase 25c) ----
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

        // ---- Full-screen settings overlay ----
        if *show_settings.read() {
            {
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
                                div { class: "settings-layout",
                                div { class: "settings-nav",
                                    button { class: if *settings_tab.read() == "transcription" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("transcription".into()), "Transcription" }
                                    button { class: if *settings_tab.read() == "ai" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("ai".into()), "AI" }
                                    button { class: if *settings_tab.read() == "speakers" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("speakers".into()), "Speakers" }
                                    button { class: if *settings_tab.read() == "recording" { "stab active" } else { "stab" }, onclick: move |_| settings_tab.set("recording".into()), "Recording" }
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
                                if *settings_tab.read() == "recording" {
                                AudioInputSettings { settings, devices: devices.clone() }
                                LevelSettings { settings }
                                RetentionSettings { settings }
                                }
                                if *settings_tab.read() == "ai" {
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
                                                SummaryPromptSettings { settings }
                                            }
                                        }
                                    }
                                }

                                }
                                if *settings_tab.read() == "speakers" {
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

                                }
                                if *settings_tab.read() == "security" {
                                EncryptionSettings { settings, notice }
                                }
                                if *settings_tab.read() == "files" {
                                FilesSettings { settings, notice }
                                }
                                if *settings_tab.read() == "about" {
                                AboutSettings {}
                                }
                                } // settings-pane
                                } // settings-layout
                            }
                        }
                    }
                }
            }
        }
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

/// Settings → Theme: monochrome vs meaning-tinted session badges.
#[component]
fn ThemeSettings(settings: Signal<Settings>) -> Element {
    rsx! {
        section { class: "settings-section",
            h3 { "Theme" }
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
        }
    }
}

/// Settings → About: a one-line local-only blurb.
#[component]
fn AboutSettings() -> Element {
    rsx! {
        section { class: "settings-section",
            h3 { "About" }
            p { class: "field-note", "Zord · 100% local. Recordings, transcripts, and models stay on this device — nothing is uploaded." }
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
    /// Retained WAV paths (me, others) that exist on disk for this session.
    audio: Signal<(Option<String>, Option<String>)>,
    /// The line (db id) currently playing back.
    playing: Signal<Option<i64>>,
    /// Replay a line: `Some((wav, segment_id, start_ms, end_ms))`, or `None` to stop.
    on_play: EventHandler<Option<(String, i64, u64, u64)>>,
) -> Element {
    let mut editing = use_signal(|| Option::<i64>::None);
    let mut buf = use_signal(String::new);
    let segs = segments.read();
    let names = speaker_names.read();
    let hl = *highlight.read();
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
                // Replay is offered only when this channel's WAV exists on disk.
                let wav = match seg.source {
                    Source::Me => audio.read().0.clone(),
                    Source::Others => audio.read().1.clone(),
                };
                let can_play = sid.is_some() && wav.is_some();
                let wav_play = wav.unwrap_or_default();
                let is_playing = sid.is_some() && *playing.read() == sid;
                let (t0, t1) = (seg.t_start_ms, seg.t_end_ms);
                // DOM anchor + flash highlight so a search result can jump here.
                let dom_id = sid.map(|i| format!("seg-{i}")).unwrap_or_default();
                let hit = if sid.is_some() && sid == hl { " hit" } else { "" };
                rsx! {
                    div {
                        key: "{seg.source.as_str()}-{seg.t_start_ms}",
                        id: "{dom_id}",
                        class: "line {line_class(seg)}{hit}",
                        span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
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
    sessions: Signal<Vec<Session>>,
    query: Signal<String>,
    on_query: EventHandler<String>,
    on_open: EventHandler<(String, Option<i64>)>,
) -> Element {
    use std::collections::HashMap;
    let sess = sessions.read();
    let started: HashMap<String, u64> = sess.iter().map(|s| (s.id.clone(), s.started_at)).collect();
    let titles: HashMap<String, String> = sess.iter().map(|s| (s.id.clone(), session_title(s))).collect();

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

    rsx! {
        div { class: "search-view",
            input {
                class: "search-input-big",
                r#type: "text",
                placeholder: "Search all transcripts…",
                autofocus: true,
                value: "{query}",
                oninput: move |e| on_query.call(e.value()),
            }
            if !q_empty {
                div { class: "search-count",
                    "{total} match(es) across {ordered.len()} meeting(s)"
                }
            }
            div { class: "search-results",
                if q_empty {
                    div { class: "empty", "Type to search across every meeting's transcript." }
                } else if ordered.is_empty() {
                    div { class: "empty", "No matches." }
                } else {
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

/// The Overview view (Phase 26): the rolling project ledger. Lists projects
/// (active first), each expandable to its open items, completed history, and
/// edit controls. Folding new meetings is explicit (Refresh); a stored legacy
/// rollup is shown as a fallback until the ledger is first built.
#[component]
fn LedgerPanel(
    ledger: Signal<Option<LedgerView>>,
    legacy: Signal<Option<OverviewData>>,
    busy: Signal<bool>,
    notice: Signal<Option<String>>,
    on_action: EventHandler<LedgerAction>,
    on_refresh: EventHandler<()>,
    on_rebuild: EventHandler<()>,
) -> Element {
    let _ = notice; // reserved for future copy/export actions
    let data = ledger.read().clone();
    let is_busy = busy();
    let pending = data.as_ref().map(|l| l.pending).unwrap_or(0);
    let projects = data.map(|l| l.projects).unwrap_or_default();
    let targets: Vec<(String, String)> =
        projects.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
    let has_ledger = !projects.is_empty();
    let mut confirm_rebuild = use_signal(|| false);

    rsx! {
        div { class: "overview ledger",
            div { class: "overview-head",
                h2 { "Overview" }
                div { class: "overview-actions",
                    if pending > 0 && !is_busy {
                        span { class: "ledger-pending", "{pending} new meeting(s) to fold" }
                    }
                    button {
                        class: "mbtn",
                        disabled: is_busy,
                        onclick: move |_| on_refresh.call(()),
                        {icon("refresh")}
                        if is_busy { "Updating…" } else if has_ledger { "Refresh" } else { "Build ledger" }
                    }
                    if confirm_rebuild() {
                        span { class: "ledger-confirm",
                            "Rebuild from scratch? Discards manual edits."
                            button {
                                class: "mbtn danger",
                                disabled: is_busy,
                                onclick: move |_| { confirm_rebuild.set(false); on_rebuild.call(()); },
                                "Rebuild"
                            }
                            button { class: "mbtn ghost", onclick: move |_| confirm_rebuild.set(false), "Cancel" }
                        }
                    } else if has_ledger {
                        button {
                            class: "mbtn ghost",
                            disabled: is_busy,
                            onclick: move |_| confirm_rebuild.set(true),
                            "Rebuild…"
                        }
                    }
                }
            }
            if has_ledger {
                div { class: "ledger-body",
                    for p in projects {
                        ProjectCard { key: "{p.id}", project: p, targets: targets.clone(), on_action }
                    }
                }
            } else {
                {
                    let leg = legacy.read().clone().filter(|d| !d.text.trim().is_empty());
                    match leg {
                        Some(d) => {
                            let (preamble, sections) = split_sections(&d.text);
                            rsx! {
                                div { class: "ledger-legacy-note",
                                    "Showing your previous overview. Press Build ledger to fold your meetings into the new project ledger."
                                }
                                div { class: "overview-body",
                                    if !preamble.is_empty() { div { class: "overview-pre", "{preamble}" } }
                                    for (i, (heading, body)) in sections.into_iter().enumerate() {
                                        details { key: "{i}", class: "overview-sec", open: i == 0,
                                            summary { class: "overview-sec-head", "{heading}" }
                                            div { class: "overview-sec-body", "{body}" }
                                        }
                                    }
                                }
                            }
                        }
                        None => rsx! {
                            div { class: "empty",
                                if is_busy {
                                    "Building your project ledger — folding each meeting in. Runs in the background and can take a few minutes on CPU."
                                } else {
                                    "No project ledger yet. Press Build ledger to fold your meetings into a living, project-grouped view of action items, decisions, and open questions."
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One project in the ledger: a collapsible card with its open items, a
/// completed-history section, an add-item row, and inline edit/archive/delete.
#[component]
fn ProjectCard(
    project: ProjectView,
    targets: Vec<(String, String)>,
    on_action: EventHandler<LedgerAction>,
) -> Element {
    let archived = project.status == "archived";
    let active: Vec<ItemView> = project.active_items().cloned().collect();
    let done: Vec<ItemView> = project.done_items().cloned().collect();
    let done_count = done.len();

    let mut editing = use_signal(|| false);
    let mut name_draft = use_signal(String::new);
    let mut desc_draft = use_signal(String::new);
    let mut adding = use_signal(|| false);
    let mut new_text = use_signal(String::new);
    let mut new_owner = use_signal(String::new);
    let mut new_kind = use_signal(|| "action".to_string());

    rsx! {
        details { class: if archived { "ledger-project archived" } else { "ledger-project" }, open: !archived,
            summary { class: "ledger-project-head",
                span { class: "ledger-project-name", "{project.name}" }
                span { class: "ledger-project-meta",
                    "{active.len()} open · {done_count} done"
                    if archived { " · archived" }
                }
            }

            // Inline project rename / description editor.
            if editing() {
                div { class: "ledger-project-edit",
                    input {
                        class: "ledger-edit-text",
                        value: "{name_draft}",
                        oninput: move |e| name_draft.set(e.value()),
                    }
                    input {
                        class: "ledger-edit-text",
                        placeholder: "one-line state (optional)",
                        value: "{desc_draft}",
                        oninput: move |e| desc_draft.set(e.value()),
                    }
                    button {
                        class: "tbtn ghost",
                        title: "Save",
                        onclick: {
                            let id = project.id.clone();
                            move |_| {
                                let name = name_draft();
                                if !name.trim().is_empty() {
                                    on_action.call(LedgerAction::RenameProject { id: id.clone(), name });
                                }
                                on_action.call(LedgerAction::SetDescription { id: id.clone(), description: desc_draft() });
                                editing.set(false);
                            }
                        },
                        {icon("check")}
                    }
                    button { class: "tbtn ghost", title: "Cancel", onclick: move |_| editing.set(false), {icon("close")} }
                }
            } else if let Some(d) = project.description.clone() {
                if !d.trim().is_empty() {
                    div { class: "ledger-project-desc", "{d}" }
                }
            }

            // Open items.
            div { class: "ledger-items",
                for it in active {
                    ItemRow { key: "{it.id}", item: it, targets: targets.clone(), on_action }
                }
            }

            // Add a hand-written item.
            if adding() {
                div { class: "ledger-add",
                    select {
                        class: "ledger-kind-select",
                        value: "{new_kind}",
                        onchange: move |e| new_kind.set(e.value()),
                        option { value: "action", "Action" }
                        option { value: "question", "Question" }
                        option { value: "decision", "Decision" }
                    }
                    input {
                        class: "ledger-edit-text",
                        placeholder: "What needs tracking?",
                        value: "{new_text}",
                        oninput: move |e| new_text.set(e.value()),
                    }
                    input {
                        class: "ledger-edit-owner",
                        placeholder: "owner",
                        value: "{new_owner}",
                        oninput: move |e| new_owner.set(e.value()),
                    }
                    button {
                        class: "tbtn ghost",
                        title: "Add",
                        onclick: {
                            let id = project.id.clone();
                            move |_| {
                                let text = new_text();
                                if !text.trim().is_empty() {
                                    on_action.call(LedgerAction::AddItem {
                                        project_id: id.clone(),
                                        kind: new_kind(),
                                        text,
                                        owner: new_owner(),
                                    });
                                }
                                new_text.set(String::new());
                                new_owner.set(String::new());
                                adding.set(false);
                            }
                        },
                        {icon("check")}
                    }
                    button { class: "tbtn ghost", title: "Cancel", onclick: move |_| adding.set(false), {icon("close")} }
                }
            } else {
                button { class: "ledger-add-btn", onclick: move |_| adding.set(true),
                    {icon("plus")} "Add item"
                }
            }

            // Completed history.
            if done_count > 0 {
                details { class: "ledger-done",
                    summary { "Completed ({done_count})" }
                    for it in done {
                        ItemRow { key: "{it.id}", item: it, targets: targets.clone(), on_action }
                    }
                }
            }

            // Project-level controls.
            div { class: "ledger-project-tools",
                button {
                    class: "tbtn ghost",
                    title: "Edit name / state",
                    onclick: {
                        let n = project.name.clone();
                        let d = project.description.clone().unwrap_or_default();
                        move |_| { name_draft.set(n.clone()); desc_draft.set(d.clone()); editing.set(true); }
                    },
                    {icon("pen")}
                }
                button {
                    class: "tbtn ghost",
                    title: if archived { "Unarchive" } else { "Archive" },
                    onclick: {
                        let id = project.id.clone();
                        move |_| on_action.call(LedgerAction::Archive { id: id.clone(), archived: !archived })
                    },
                    {icon("archive")}
                }
                button {
                    class: "tbtn ghost danger",
                    title: "Delete project",
                    onclick: {
                        let id = project.id.clone();
                        move |_| on_action.call(LedgerAction::DeleteProject(id.clone()))
                    },
                    {icon("trash")}
                }
            }
        }
    }
}

/// A single ledger item row: kind badge, text, owner, provenance, and inline
/// controls (complete/reopen, edit, move, delete).
#[component]
fn ItemRow(
    item: ItemView,
    targets: Vec<(String, String)>,
    on_action: EventHandler<LedgerAction>,
) -> Element {
    let done = item.status == "done";
    let mut editing = use_signal(|| false);
    let mut text_draft = use_signal(String::new);
    let mut owner_draft = use_signal(String::new);

    rsx! {
        div { class: if done { "ledger-item done" } else { "ledger-item" },
            span { class: "ledger-kind k-{item.kind}", "{kind_label(&item.kind)}" }
            if editing() {
                input {
                    class: "ledger-edit-text",
                    value: "{text_draft}",
                    oninput: move |e| text_draft.set(e.value()),
                }
                input {
                    class: "ledger-edit-owner",
                    placeholder: "owner",
                    value: "{owner_draft}",
                    oninput: move |e| owner_draft.set(e.value()),
                }
                button {
                    class: "tbtn ghost",
                    title: "Save",
                    onclick: {
                        let id = item.id.clone();
                        move |_| {
                            on_action.call(LedgerAction::EditItem { id: id.clone(), text: text_draft(), owner: owner_draft() });
                            editing.set(false);
                        }
                    },
                    {icon("check")}
                }
                button { class: "tbtn ghost", title: "Cancel", onclick: move |_| editing.set(false), {icon("close")} }
            } else {
                span { class: "ledger-item-text", "{item.text}" }
                if let Some(o) = item.owner.clone() {
                    span { class: "ledger-owner", "{o}" }
                }
                if item.manual {
                    span { class: "ledger-manual", title: "hand-edited", {icon("pen")} }
                }
                div { class: "ledger-item-tools",
                    button {
                        class: "tbtn ghost",
                        title: if done { "Reopen" } else { "Mark done" },
                        onclick: {
                            let id = item.id.clone();
                            move |_| on_action.call(LedgerAction::SetItemStatus {
                                id: id.clone(),
                                status: if done { "open".to_string() } else { "done".to_string() },
                            })
                        },
                        {icon(if done { "refresh" } else { "check" })}
                    }
                    if targets.len() > 1 {
                        select {
                            class: "ledger-move",
                            title: "Move to project",
                            onchange: {
                                let id = item.id.clone();
                                move |e| {
                                    let pid = e.value();
                                    if !pid.is_empty() {
                                        on_action.call(LedgerAction::MoveItem { item_id: id.clone(), project_id: pid });
                                    }
                                }
                            },
                            option { value: "", "Move…" }
                            for (tid, tname) in targets.iter().cloned() {
                                option { value: "{tid}", "{tname}" }
                            }
                        }
                    }
                    button {
                        class: "tbtn ghost",
                        title: "Edit",
                        onclick: {
                            let t = item.text.clone();
                            let o = item.owner.clone().unwrap_or_default();
                            move |_| { text_draft.set(t.clone()); owner_draft.set(o.clone()); editing.set(true); }
                        },
                        {icon("pen")}
                    }
                    button {
                        class: "tbtn ghost danger",
                        title: "Delete",
                        onclick: {
                            let id = item.id.clone();
                            move |_| on_action.call(LedgerAction::DeleteItem(id.clone()))
                        },
                        {icon("trash")}
                    }
                }
            }
        }
    }
}

/// Short badge label for an item kind.
fn kind_label(kind: &str) -> &'static str {
    match kind {
        "question" => "Question",
        "decision" => "Decision",
        _ => "Action",
    }
}

/// Split overview Markdown into an optional preamble and `## `-headed sections,
/// so the UI can render each project as a collapsible block.
fn split_sections(md: &str) -> (String, Vec<(String, String)>) {
    let mut preamble = String::new();
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in md.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            if let Some(s) = cur.take() {
                sections.push(s);
            }
            cur = Some((h.trim().to_string(), String::new()));
        } else if let Some((_, body)) = cur.as_mut() {
            body.push_str(line);
            body.push('\n');
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    if let Some(s) = cur.take() {
        sections.push(s);
    }
    (preamble.trim().to_string(), sections.into_iter().map(|(h, b)| (h, b.trim().to_string())).collect())
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
fn FilesSettings(settings: Signal<Settings>, notice: Signal<Option<String>>) -> Element {
    rsx! {
        section { class: "settings-section",
            h3 { "Files & folders" }
            p { class: "field-note", "Jump to Zord's files on disk — handy for dropping in a manually-downloaded model, or grabbing logs when something fails." }

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
        }
    }
}

/// Settings → Audio input: microphone device + capture-mode pickers.
#[component]
fn AudioInputSettings(mut settings: Signal<Settings>, devices: Vec<String>) -> Element {
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
                        s.capture_mode = e.value();
                        let _ = s.save();
                        settings.set(s);
                    },
                    option { value: "both", selected: settings.read().capture_mode == "both", "Microphone + system audio" }
                    option { value: "mic", selected: settings.read().capture_mode == "mic", "Microphone only (Me)" }
                    option { value: "system", selected: settings.read().capture_mode == "system", "System audio only (Others)" }
                }
            }
        }
    }
}

/// The background-jobs board: one row per running job with elapsed time,
/// per-job detail, and optional progress. Visibility is gated at the call site.
#[component]
fn JobsPanel(
    mut show_jobs: Signal<bool>,
    job_starts: Signal<std::collections::HashMap<String, u64>>,
    job_tick: Signal<u64>,
    model_progress: Signal<Option<(String, u8)>>,
    diarize_est_secs: Signal<Option<u64>>,
) -> Element {
    let _ = job_tick.read(); // re-render each second for elapsed timers
    let now = now_ms();
    let mp = model_progress.read().clone();
    let est = *diarize_est_secs.read();
    let mut rows: Vec<(String, u64)> =
        job_starts.read().iter().map(|(k, v)| (k.clone(), *v)).collect();
    rows.sort_by_key(|(k, _)| job_order(k));
    rsx! {
        div { class: "jobs-overlay", onclick: move |_| show_jobs.set(false),
            div {
                class: "jobs-card",
                onclick: move |e| e.stop_propagation(),
                div { class: "jobs-head",
                    span { "Background jobs" }
                    button { class: "close-btn", onclick: move |_| show_jobs.set(false), {icon("close")} }
                }
                for (key, start) in rows {
                    {
                        let elapsed = now.saturating_sub(start) / 1000;
                        let (ic_name, title) = job_label(&key);
                        // Per-job detail, optional progress %, and ETA.
                        let (detail, pct): (String, Option<u8>) =
                            job_detail(key.as_str(), &mp, est, elapsed);
                        rsx! {
                            div { key: "{key}", class: "job-row",
                                span { class: "job-icon", {icon(ic_name)} }
                                div { class: "job-main",
                                    div { class: "job-title", "{title}" }
                                    div { class: "job-detail", "{detail}" }
                                    if let Some(p) = pct {
                                        div { class: "job-bar", div { class: "job-bar-fill", style: "width: {p}%" } }
                                    }
                                }
                                span { class: "job-time", "{fmt_dur(elapsed)}" }
                            }
                        }
                    }
                }
                div { class: "jobs-foot", "Local processing — times are estimates and vary with your hardware." }
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

/// Icon name (registry key) + label for a background-job key.
fn job_label(key: &str) -> (&'static str, &'static str) {
    match key {
        "record" => ("record", "Recording"),
        "transcribe" => ("refresh", "Transcribing"),
        "download" => ("download", "Downloading model"),
        "summarize" => ("sparkles", "Summarizing"),
        "compress" => ("archive", "Compressing"),
        "overview" => ("overview", "Building overview"),
        "chat" => ("chat", "Answering chat"),
        "diarize" => ("users", "Identifying speakers"),
        _ => ("", "Working"),
    }
}

/// Per-job detail line + optional progress % for a background-job row.
fn job_detail(
    key: &str,
    mp: &Option<(String, u8)>,
    est: Option<u64>,
    elapsed: u64,
) -> (String, Option<u8>) {
    match key {
        "download" => {
            let (name, p) = mp.clone().unwrap_or_default();
            let eta = if p > 0 && p < 100 {
                format!(" · ETA {}", fmt_dur(elapsed * (100 - p as u64) / p as u64))
            } else {
                String::new()
            };
            (format!("{name} · {p}%{eta}"), Some(p))
        }
        "diarize" => {
            let d = match est {
                Some(e) => format!("~{} left (estimate)", fmt_dur(e.saturating_sub(elapsed))),
                None => "processing audio…".to_string(),
            };
            (d, None)
        }
        "record" => ("capturing audio".to_string(), None),
        "transcribe" => ("transcribing the kept audio…".to_string(), None),
        "overview" => ("compressing + synthesizing".to_string(), None),
        _ => ("running…".to_string(), None),
    }
}

/// Stable display order for the jobs panel.
fn job_order(key: &str) -> u8 {
    match key {
        "record" => 0,
        "transcribe" => 1,
        "download" => 2,
        "summarize" => 3,
        "compress" => 4,
        "overview" => 5,
        "diarize" => 6,
        "chat" => 7,
        _ => 9,
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
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
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
    let mut meta = format!("{} · {}", fmt_date(s.started_at), relative_time(s.started_at));
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
