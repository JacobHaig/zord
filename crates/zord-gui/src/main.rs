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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "zord=info,whisper_rs=warn".into()),
        )
        .init();
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
    // Session id currently being renamed (+ its edit buffer); pending delete.
    let mut editing = use_signal(|| Option::<String>::None);
    let mut edit_text = use_signal(String::new);
    let mut confirm_delete = use_signal(|| Option::<String>::None);
    let mut settings = use_signal(Settings::load);
    let mut show_settings = use_signal(|| false);
    let devices = use_hook(zord_capture::input_devices);
    let mut models = use_signal(Vec::<ModelInfo>::new);
    // (model name currently downloading, percent).
    let mut model_progress = use_signal(|| Option::<(String, u8)>::None);

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
                    Event::Summary(v) => summary.set(v),
                }
            }
        });
        let _ = engine.db_tx.send(DbCmd::ListSessions);
        let _ = engine.model_tx.send(ModelCmd::List);
        engine
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
                view.set(View::Live);
                let s = settings.peek().clone();
                let model = ModelId::parse(&s.model).unwrap_or(ModelId::LargeV3TurboQ5);
                let audio_dir = s.audio_dir().unwrap_or_else(|_| PathBuf::from("audio"));
                let _ = engine.rec_tx.send(RecorderCmd::Start {
                    model,
                    keep_audio: s.keep_audio,
                    input_device: s.input_device.clone(),
                    audio_dir,
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
                    div { class: "notice", "{n}" }
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
                        div { class: "summary",
                            div { class: "summary-head", "Summary" }
                            div { class: "summary-body", "{text}" }
                        }
                    }
                }

                // Transcript / results. Pass signals so the list subscribes and
                // re-renders itself; App is not re-rendered on each new segment.
                div { class: "transcript",
                    if *view.read() == View::Search {
                        SearchResultsView { results: search_results }
                    } else {
                        TranscriptView { segments }
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
                                section { class: "settings-section",
                                    h3 { "Transcription model" }
                                    p { class: "field-note", "Pick a downloaded model, or download another. Bigger = more accurate but slower; the quantized turbo is the best all-round." }
                                    for m in models.read().iter() {
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
                                    p { class: "field-note", "Desktop / system audio (the “Others” channel) is captured automatically." }
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

                                EncryptionSettings { settings, notice }

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
fn TranscriptView(segments: Signal<Vec<Segment>>) -> Element {
    let segs = segments.read();
    if segs.is_empty() {
        return rsx! { div { class: "empty", "Press Record, or pick a session." } };
    }
    rsx! {
        for seg in segs.iter() {
            div {
                key: "{seg.source.as_str()}-{seg.t_start_ms}",
                class: "line {source_class(seg.source)}",
                span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
                span { class: "who", "{seg.source.label()}" }
                span { class: "text", "{seg.text}" }
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
                class: "line {source_class(seg.source)}",
                span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
                span { class: "who", "{seg.source.label()}" }
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
