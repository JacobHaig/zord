//! Zord desktop GUI (Dioxus 0.7). Record mic + system audio, watch the
//! transcript stream in live, browse past sessions, and full-text search.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;
mod osutil;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use engine::{DbCmd, Engine, Event, RecorderCmd, Status};
use std::path::PathBuf;
use zord_config::Settings;
use zord_core::{Segment, Session, Source};
use zord_export::Format;
use zord_transcribe::ModelId;

const CSS: &str = include_str!("style.css");

/// Whisper models offered in the settings dropdown.
const MODELS: &[&str] = &["large-v3-turbo-q5_0", "large-v3-turbo", "small.en"];

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
    LaunchBuilder::desktop()
        .with_cfg(Config::new().with_window(window))
        .launch(App);
}

#[derive(Clone, PartialEq)]
enum View {
    Live,
    Session(String),
    Search,
}

#[component]
fn App() -> Element {
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
    let settings = use_signal(Settings::load);
    let mut show_settings = use_signal(|| false);
    let devices = use_hook(zord_capture::input_devices);

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
                    Event::Level { source, peak } => match source {
                        Source::Me => me_level.set(peak),
                        Source::Others => others_level.set(peak),
                    },
                    Event::Sessions(v) => sessions.set(v),
                    Event::SearchResults(v) => search_results.set(v),
                    Event::Transcript(v) => segments.set(v),
                    Event::Exported(p) => {
                        notice.set(Some(format!("Exported to {p}")));
                        last_export.set(Some(p));
                    }
                }
            }
        });
        let _ = engine.db_tx.send(DbCmd::ListSessions);
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

                if *show_settings.read() {
                    SettingsPanel { settings, devices: devices.clone() }
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
    }
}

#[component]
fn SettingsPanel(settings: Signal<Settings>, devices: Vec<String>) -> Element {
    let s = settings.read().clone();

    let set_model = move |e: FormEvent| {
        let mut s = settings.peek().clone();
        s.model = e.value();
        let _ = s.save();
        settings.set(s);
    };
    let toggle_keep = move |_| {
        let mut s = settings.peek().clone();
        s.keep_audio = !s.keep_audio;
        let _ = s.save();
        settings.set(s);
    };
    let set_days = move |e: FormEvent| {
        let mut s = settings.peek().clone();
        s.auto_delete_days = e.value().trim().parse::<u32>().ok().filter(|n| *n > 0);
        let _ = s.save();
        settings.set(s);
    };
    let set_device = move |e: FormEvent| {
        let mut s = settings.peek().clone();
        let v = e.value();
        s.input_device = if v == "__default__" { None } else { Some(v) };
        let _ = s.save();
        settings.set(s);
    };

    rsx! {
        div { class: "settings",
            div { class: "setting",
                label { "Model" }
                select { onchange: set_model,
                    for m in MODELS {
                        option { value: "{m}", selected: s.model == *m, "{m}" }
                    }
                }
            }
            div { class: "setting",
                label { "Microphone" }
                select { onchange: set_device,
                    option { value: "__default__", selected: s.input_device.is_none(), "System default" }
                    for d in devices.iter() {
                        option { value: "{d}", selected: s.input_device.as_deref() == Some(d.as_str()), "{d}" }
                    }
                }
            }
            div { class: "setting",
                label { "Keep audio" }
                button {
                    class: if s.keep_audio { "toggle on" } else { "toggle" },
                    onclick: toggle_keep,
                    if s.keep_audio { "On" } else { "Off" }
                }
            }
            div { class: "setting",
                label { "Auto-delete audio after (days)" }
                input {
                    r#type: "number",
                    min: "0",
                    class: "days",
                    placeholder: "never",
                    value: s.auto_delete_days.map(|n| n.to_string()).unwrap_or_default(),
                    oninput: set_days,
                }
            }
        }
    }
}

#[component]
fn Meter(label: String, level: Signal<f32>, kind: String) -> Element {
    // Map a peak amplitude (0..1) to a friendlier visual width. Reading the
    // signal here means only this component re-renders on level changes.
    let pct = (level().sqrt() * 100.0).clamp(0.0, 100.0);
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
