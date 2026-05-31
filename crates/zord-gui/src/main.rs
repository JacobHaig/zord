//! Zord desktop GUI (Dioxus 0.7). Record mic + system audio, watch the
//! transcript stream in live, browse past sessions, and full-text search.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod osutil;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use engine::{DbCmd, Engine, Event, ModelCmd, ModelInfo, RecorderCmd, Status};
use std::path::PathBuf;
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
                            let engine = engine.clone();
                            let active = matches!(&*view.read(), View::Session(v) if *v == id);
                            rsx! {
                                div {
                                    key: "{s.id}",
                                    class: if active { "session active" } else { "session" },
                                    onclick: move |_| {
                                        view.set(View::Session(id.clone()));
                                        last_export.set(None);
                                        let _ = engine.db_tx.send(DbCmd::Load(id.clone()));
                                    },
                                    div { class: "session-title", "{session_title(&s)}" }
                                    div { class: "session-meta", "{s.model}" }
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

fn session_title(s: &Session) -> String {
    s.title
        .clone()
        .unwrap_or_else(|| format!("Recording {}", s.started_at / 1000))
}

fn short_id(id: &str) -> String {
    id.chars().take(12).collect()
}
