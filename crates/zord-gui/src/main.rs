//! Zord desktop GUI (Dioxus 0.7). Record mic + system audio, watch the
//! transcript stream in live, browse past sessions, and full-text search.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod engine;

use dioxus::desktop::{Config, LogicalSize, WindowBuilder};
use dioxus::prelude::*;
use engine::{DbCmd, Engine, Event, RecorderCmd, Status};
use std::path::PathBuf;
use zord_core::{Segment, Session, Source};
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
    LaunchBuilder::desktop()
        .with_cfg(Config::new().with_window(window))
        .launch(App);
}

/// Default DB path: alongside the model cache, under the app data dir.
fn db_path() -> PathBuf {
    match zord_transcribe::model_cache_dir() {
        Ok(models) => models
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(models)
            .join("zord.db"),
        Err(_) => PathBuf::from("zord.db"),
    }
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

    // Create the engine once and drain its events into signals.
    let engine = use_hook(|| {
        let (engine, mut ev_rx) = Engine::spawn(db_path());
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
                let _ = engine.rec_tx.send(RecorderCmd::Start {
                    model: ModelId::LargeV3TurboQ5,
                    keep_audio: false,
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
                    button {
                        class: if recording { "record stop" } else { "record" },
                        onclick: on_record,
                        if recording { "■ Stop" } else { "● Record" }
                    }
                }

                // Level meters
                div { class: "meters",
                    Meter { label: "Me".to_string(), level: me_level(), kind: "me".to_string() }
                    Meter { label: "Others".to_string(), level: others_level(), kind: "others".to_string() }
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

                // Transcript / results
                div { class: "transcript",
                    if *view.read() == View::Search {
                        SearchResultsView { results: search_results() }
                    } else {
                        TranscriptView { segments: segments() }
                    }
                }
            }
        }
    }
}

#[component]
fn Meter(label: String, level: f32, kind: String) -> Element {
    // Map a peak amplitude (0..1) to a friendlier visual width.
    let pct = (level.sqrt() * 100.0).clamp(0.0, 100.0);
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
fn TranscriptView(segments: Vec<Segment>) -> Element {
    if segments.is_empty() {
        return rsx! { div { class: "empty", "Press Record, or pick a session." } };
    }
    rsx! {
        for (i, seg) in segments.iter().enumerate() {
            div { key: "{i}", class: "line {source_class(seg.source)}",
                span { class: "ts", "{fmt_ts(seg.t_start_ms)}" }
                span { class: "who", "{seg.source.label()}" }
                span { class: "text", "{seg.text}" }
            }
        }
    }
}

#[component]
fn SearchResultsView(results: Vec<(String, Segment)>) -> Element {
    if results.is_empty() {
        return rsx! { div { class: "empty", "No matches." } };
    }
    rsx! {
        for (i, entry) in results.iter().enumerate() {
            div { key: "{i}", class: "line {source_class(entry.1.source)}",
                span { class: "ts", "{fmt_ts(entry.1.t_start_ms)}" }
                span { class: "who", "{entry.1.source.label()}" }
                span { class: "text", "{entry.1.text}" }
                span { class: "src", "{short_id(&entry.0)}" }
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
