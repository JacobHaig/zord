//! Zord desktop GUI (Dioxus 0.7). Record mic + system audio, watch the
//! transcript stream in live, browse past sessions, and full-text search.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod osutil;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use engine::{DbCmd, Engine, Event, ModelCmd, ModelInfo, RecorderCmd, Status};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zord_config::Settings;
use zord_core::{Segment, Session, Source};
use zord_export::Format;
use zord_transcribe::ModelId;

const CSS: &str = include_str!("style.css");

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

#[derive(Clone, PartialEq)]
enum View {
    Live,
    Session(String),
    Search,
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
    let mut me_level = use_signal(|| 0.0f32);
    let mut others_level = use_signal(|| 0.0f32);
    let mut sessions = use_signal(Vec::<Session>::new);
    let mut search_results = use_signal(Vec::<(String, Segment)>::new);
    let mut view = use_signal(|| View::Live);
    // Path of the most recently exported file (drives the Reveal/Open buttons).
    let mut last_export = use_signal(|| Option::<String>::None);
    // Current session's AI summary, if any.
    let mut summary = use_signal(|| Option::<String>::None);
    // Custom names for diarized speakers in the viewed session (index → name).
    let mut speaker_names = use_signal(std::collections::HashMap::<i32, String>::new);
    // Session id currently being renamed (+ its edit buffer); pending delete.
    let mut editing = use_signal(|| Option::<String>::None);
    let mut edit_text = use_signal(String::new);
    let mut confirm_delete = use_signal(|| Option::<String>::None);
    // Seconds elapsed in the current recording (0 when idle).
    let mut rec_secs = use_signal(|| 0u64);
    let mut settings = use_signal(Settings::load);
    let mut show_settings = use_signal(|| false);
    let devices = use_hook(zord_capture::input_devices);
    let mut models = use_signal(Vec::<ModelInfo>::new);
    // (model name currently downloading, percent).
    let mut model_progress = use_signal(|| Option::<(String, u8)>::None);
    // Name of a model whose download failed → show the manual-fetch fallback.
    let mut download_help = use_signal(|| Option::<String>::None);

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
            while let Some(ev) = ev_rx.recv().await {
                match ev {
                    Event::Status(s) => status.set(s),
                    Event::Notice(n) => notice.set(Some(n)),
                    Event::Segment(seg) => {
                        if *view.peek() == View::Live {
                            segments.write().push(seg);
                        }
                    }
                    Event::Level { source, level } => match source {
                        Source::Me => me_level.set(level),
                        Source::Others => others_level.set(level),
                    },
                    Event::Sessions(v) => sessions.set(v),
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
                    Event::Summary(v) => summary.set(v),
                    Event::Speakers(v) => speaker_names.set(v),
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
        let _ = segments.read().len(); // subscribe to new segments
        if matches!(*status.peek(), Status::Recording) {
            let _ = document::eval(
                "const t=document.querySelector('.transcript'); if(t){t.scrollTop=t.scrollHeight;}",
            );
        }
    });

    let st = status.read().clone();
    let recording = matches!(
        st,
        Status::Recording | Status::PreparingModel | Status::Downloading(_)
    );
    let status_text = match &st {
        Status::Idle => "Idle".to_string(),
        Status::PreparingModel => "Preparing model…".to_string(),
        Status::Downloading(p) => format!("Downloading model… {p}%"),
        Status::Recording => "Recording".to_string(),
        Status::Error(e) => format!("Error: {e}"),
    };

    let on_record = {
        let engine = engine.clone();
        move |_| {
            if recording {
                let _ = engine.rec_tx.send(RecorderCmd::Stop);
                let _ = engine.db_tx.send(DbCmd::ListSessions);
            } else {
                segments.write().clear();
                notice.set(None);
                summary.set(None);
                speaker_names.write().clear();
                view.set(View::Live);
                let s = settings.peek().clone();
                let model = ModelId::parse(&s.model).unwrap_or(ModelId::LargeV3TurboQ5);
                let audio_dir = s.audio_dir().unwrap_or_else(|_| PathBuf::from("audio"));
                let (record_mic, record_system) = match s.capture_mode.as_str() {
                    "mic" => (true, false),
                    "system" => (false, true),
                    _ => (true, true),
                };
                let _ = engine.rec_tx.send(RecorderCmd::Start {
                    model,
                    keep_audio: s.keep_audio,
                    input_device: s.input_device.clone(),
                    audio_dir,
                    record_mic,
                    record_system,
                });
            }
        }
    };

    let on_search = {
        let engine = engine.clone();
        move |e: FormEvent| {
            let q = e.value();
            if q.trim().is_empty() {
                view.set(View::Live);
            } else {
                view.set(View::Search);
                let _ = engine.db_tx.send(DbCmd::Search(q));
            }
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

    rsx! {
        style { dangerous_inner_html: CSS }
        div { class: "app",
            // ---- Sidebar: session history ----
            aside { class: "sidebar",
                div { class: "brand", "ZORD" }
                div { class: "side-label", "Sessions" }
                div { class: "session-list",
                    if sessions.read().is_empty() {
                        div { class: "empty", "No recordings yet." }
                    }
                    for s in sessions.read().iter().cloned() {
                        {
                            let id = s.id.clone();
                            let active = matches!(&*view.read(), View::Session(v) if *v == id);
                            let is_editing = editing.read().as_deref() == Some(id.as_str());
                            let title = session_title(&s);
                            let meta = session_meta(&s);
                            let eng_open = engine.clone();
                            let eng_save = engine.clone();
                            let (id_open, id_edit, id_save, id_del) =
                                (id.clone(), id.clone(), id.clone(), id.clone());
                            let title_edit = title.clone();
                            rsx! {
                                div {
                                    key: "{s.id}",
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
                                                let _ = eng_open.db_tx.send(DbCmd::Load(id_open.clone()));
                                            },
                                            div { class: "session-title", "{title}" }
                                            div { class: "session-meta", "{meta}" }
                                        }
                                        div { class: "session-actions",
                                            button {
                                                class: "row-btn",
                                                title: "Rename",
                                                onclick: move |_| {
                                                    edit_text.set(title_edit.clone());
                                                    editing.set(Some(id_edit.clone()));
                                                },
                                                "✏"
                                            }
                                            button {
                                                class: "row-btn",
                                                title: "Delete",
                                                onclick: move |_| confirm_delete.set(Some(id_del.clone())),
                                                "🗑"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
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
                    div { class: "topbar-actions",
                        button {
                            class: "gear",
                            title: "Settings",
                            onclick: move |_| { let v = *show_settings.peek(); show_settings.set(!v); },
                            "⚙"
                        }
                        button {
                            class: if recording { "record stop" } else { "record" },
                            onclick: on_record,
                            if recording { "■ Stop" } else { "● Record" }
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
                        button { class: "notice-x", onclick: move |_| notice.set(None), "✕" }
                    }
                }

                // Search
                input {
                    class: "search",
                    r#type: "text",
                    placeholder: "Search all transcripts…",
                    oninput: on_search,
                }

                // Export bar (only when viewing a saved session)
                if let View::Session(id) = &*view.read() {
                    {
                        let id = id.clone();
                        let engine = engine.clone();
                        let sid = id.clone();
                        let eng_sum = engine.clone();
                        let eng_diar = engine.clone();
                        let sid_diar = id.clone();
                        let mk = move |fmt: Format| {
                            let id = id.clone();
                            let engine = engine.clone();
                            move |_| {
                                let _ = engine.db_tx.send(DbCmd::Export { id: id.clone(), format: fmt });
                            }
                        };
                        rsx! {
                            div { class: "export-bar",
                                span { class: "export-label", "Export:" }
                                button { class: "export-btn", onclick: mk(Format::Markdown), "Markdown" }
                                button { class: "export-btn", onclick: mk(Format::Srt), "SRT" }
                                button { class: "export-btn", onclick: mk(Format::Json), "JSON" }
                                span { class: "export-sep", "·" }
                                button {
                                    class: "export-btn",
                                    onclick: move |_| {
                                        notice.set(Some("Summarizing… (first run downloads the model)".to_string()));
                                        let _ = eng_sum.summ_tx.send(sid.clone());
                                    },
                                    "✨ Summarize"
                                }
                                button {
                                    class: "export-btn",
                                    title: "Group the 'Others' channel into individual speakers (needs retained audio)",
                                    onclick: move |_| {
                                        notice.set(Some("Identifying speakers… (first run downloads the speaker model)".to_string()));
                                        let _ = eng_diar.db_tx.send(DbCmd::Diarize(sid_diar.clone()));
                                    },
                                    "🗣 Identify speakers"
                                }
                                button {
                                    class: "export-btn",
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
                                    "Copy"
                                }
                                if let Some(path) = last_export.read().clone() {
                                    span { class: "export-sep", "·" }
                                    button {
                                        class: "export-btn ghost",
                                        onclick: {
                                            let p = path.clone();
                                            move |_| osutil::reveal_in_file_manager(&p)
                                        },
                                        "📂 Reveal"
                                    }
                                    button {
                                        class: "export-btn ghost",
                                        onclick: move |_| osutil::open_in_editor(&path),
                                        "📝 Open"
                                    }
                                }
                            }
                        }
                    }
                }

                // AI summary (when present for the viewed session).
                if *view.read() != View::Search {
                    if let Some(text) = summary.read().clone() {
                        {
                            let text_copy = text.clone();
                            rsx! {
                                div { class: "summary",
                                    div { class: "summary-head",
                                        span { "Summary" }
                                        button {
                                            class: "row-btn",
                                            onclick: move |_| {
                                                osutil::copy_to_clipboard(&text_copy);
                                                notice.set(Some("Summary copied to clipboard".to_string()));
                                            },
                                            "Copy"
                                        }
                                    }
                                    div { class: "summary-body", "{text}" }
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
                div { class: "transcript",
                    if *view.read() == View::Search {
                        SearchResultsView { results: search_results }
                    } else {
                        TranscriptView { segments, speaker_names, on_edit: on_edit_segment }
                    }
                }
            }
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
                                button { class: "close-btn", onclick: move |_| show_settings.set(false), "✕" }
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
                                                    span { "⚠ Couldn't download \"{failed}\"" }
                                                    button { class: "notice-x", onclick: move |_| download_help.set(None), "✕" }
                                                }
                                                p { class: "field-note", "Often a proxy / network block. Fetch it in your browser (which uses your proxy), then drop it in the models folder. Archives (.tar.bz2) must be extracted there first." }
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
                                                        "📁 Open models folder"
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                section { class: "settings-section",
                                    h3 { "Transcription model" }
                                    p { class: "field-note", "Pick a downloaded model, or download another. Bigger = more accurate but slower; the quantized turbo is the best all-round." }
                                    for m in models.read().iter().filter(|m| m.kind == "transcription") {
                                        {
                                            let name = m.name.clone();
                                            let selected = name == current;
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
                                                                    s.model = n_sel.clone();
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

                                section { class: "settings-section",
                                    h3 { "Recording & retention" }
                                    div { class: "field row",
                                        label { "Keep audio after transcription" }
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

                                section { class: "settings-section",
                                    h3 { "Summaries" }
                                    {
                                        let summary_models: Vec<ModelInfo> = models
                                            .read()
                                            .iter()
                                            .filter(|m| m.kind == "summary")
                                            .cloned()
                                            .collect();
                                        let cur_sum = settings.read().summary_model.clone();
                                        if summary_models.is_empty() {
                                            rsx! {
                                                p { class: "field-note", "Build with `--features summaries` to enable local AI summaries." }
                                            }
                                        } else {
                                            rsx! {
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
                                                SummaryPromptSettings { settings }
                                            }
                                        }
                                    }
                                }

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
                                                p { class: "field-note", "Live labels are rough and get replaced by the accurate pass at stop. Leave off on lighter hardware." }
                                            }
                                        }
                                    }
                                }

                                EncryptionSettings { settings, notice }

                                section { class: "settings-section",
                                    h3 { "Files & folders" }
                                    p { class: "field-note", "Jump to Zord's files on disk — handy for dropping in a manually-downloaded model, or grabbing logs when something fails." }
                                    div { class: "btn-row",
                                        button {
                                            class: "mbtn",
                                            title: "Downloaded transcription / summary / speaker models",
                                            onclick: move |_| {
                                                if let Ok(d) = zord_config::models_dir() {
                                                    osutil::open_folder(&d.display().to_string());
                                                }
                                            },
                                            "📁 Models"
                                        }
                                        button {
                                            class: "mbtn",
                                            title: "Database, recordings, and exports",
                                            onclick: move |_| {
                                                if let Ok(d) = settings.peek().storage_dir() {
                                                    osutil::open_folder(&d.display().to_string());
                                                }
                                            },
                                            "📁 Data"
                                        }
                                        button {
                                            class: "mbtn",
                                            onclick: move |_| {
                                                if let Ok(d) = zord_config::logs_dir() {
                                                    osutil::open_folder(&d.display().to_string());
                                                }
                                            },
                                            "📁 Logs"
                                        }
                                        button {
                                            class: "mbtn ghost",
                                            onclick: move |_| {
                                                if let Ok(p) = zord_config::config_path() {
                                                    osutil::reveal_in_file_manager(&p.display().to_string());
                                                }
                                            },
                                            "📄 Config"
                                        }
                                        button {
                                            class: "mbtn ghost",
                                            onclick: move |_| {
                                                if let Ok(p) = settings.peek().db_path() {
                                                    osutil::reveal_in_file_manager(&p.display().to_string());
                                                }
                                            },
                                            "📄 Database"
                                        }
                                    }
                                    div { class: "btn-row",
                                        button {
                                            class: "mbtn ghost",
                                            onclick: move |_| {
                                                match zord_config::logs_dir().map(|d| d.join("zord.log")) {
                                                    Ok(p) if p.exists() => osutil::open_in_editor(&p.display().to_string()),
                                                    _ => notice.set(Some("No log file yet — it appears after the next launch.".to_string())),
                                                }
                                            },
                                            "📝 Open log"
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
                                            "📋 Copy recent log"
                                        }
                                    }
                                }

                                section { class: "settings-section",
                                    h3 { "About" }
                                    p { class: "field-note", "Zord · 100% local. Recordings, transcripts, and models stay on this device — nothing is uploaded." }
                                }
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
    on_edit: EventHandler<(i64, String)>,
) -> Element {
    let mut editing = use_signal(|| Option::<i64>::None);
    let mut buf = use_signal(String::new);
    let segs = segments.read();
    let names = speaker_names.read();
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
                rsx! {
                    div {
                        key: "{seg.source.as_str()}-{seg.t_start_ms}",
                        class: "line {line_class(seg)}",
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
                    }
                }
            }
        }
    }
}

#[component]
fn SearchResultsView(results: Signal<Vec<(String, Segment)>>) -> Element {
    let hits = results.read();
    if hits.is_empty() {
        return rsx! { div { class: "empty", "No matches." } };
    }
    rsx! {
        for (sid, seg) in hits.iter() {
            div {
                key: "{sid}-{seg.t_start_ms}",
                class: "line {line_class(seg)}",
                span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
                span { class: "who", "{quick_speaker_label(seg)}" }
                span { class: "text", "{seg.text}" }
                span { class: "src", "{short_id(sid)}" }
            }
        }
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
        None => format!("Recording · {}", relative_time(s.started_at)),
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

/// Sidebar second line: model + recording duration when known.
fn session_meta(s: &Session) -> String {
    match s.ended_at.map(|e| e.saturating_sub(s.started_at) / 1000) {
        Some(secs) if secs > 0 => format!("{} · {}", s.model, fmt_dur(secs)),
        _ => s.model.clone(),
    }
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
