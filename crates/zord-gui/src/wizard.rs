//! First-run guided setup (Phase 36b): a fully-skippable overlay that tunes
//! defaults from the user's intent, lets them *hear* their microphone work,
//! walks the platform permission where it matters, pre-downloads the
//! transcription model, and configures Discord — then gets out of the way.
//! Re-runnable from Settings → About. Spec:
//! `docs/superpowers/specs/2026-06-10-setup-wizard-design.md`.

use dioxus::prelude::*;
use zord_config::Settings;

use crate::engine::{Engine, ModelCmd, ModelInfo, RecorderCmd};
use crate::{icon, IntegrationsSettings, Meter};

/// Tune defaults from the intent answers (pure; the discord flag only routes
/// a wizard step, it configures nothing by itself — and the model choice
/// belongs to the model step's picker, not here).
pub fn apply_intents(
    s: &mut Settings,
    meetings: bool,
    discord: bool,
    voice: bool,
    low_power: bool,
) {
    let _ = discord;
    if meetings {
        s.capture_mode = "both".to_string();
    } else if voice {
        s.capture_mode = "mic".to_string();
    }
    if low_power {
        s.live_transcription = false;
    }
}

/// The transcription model the wizard recommends. Parakeet first when the
/// build carries it — fast and accurate even on CPU; otherwise the hardware
/// answer picks between the turbo default and a small model.
fn recommended_model(low_power: bool) -> &'static str {
    if cfg!(feature = "parakeet") {
        "parakeet-tdt-0.6b-v3"
    } else if low_power {
        "small.en"
    } else {
        "large-v3-turbo-q5_0"
    }
}

#[component]
pub fn SetupWizard(
    settings: Signal<Settings>,
    mut show_wizard: Signal<bool>,
    engine: Engine,
    devices: Vec<String>,
    me_level: Signal<f32>,
    models: Signal<Vec<ModelInfo>>,
    model_progress: Signal<Option<(String, u8)>>,
    notice: Signal<Option<String>>,
) -> Element {
    let mut step = use_signal(|| 0usize);
    // Intent selections — applied to settings when leaving the intent step.
    let mut want_meetings = use_signal(|| true);
    let mut want_discord = use_signal(|| false);
    let mut want_voice = use_signal(|| false);
    let mut low_power = use_signal(|| false);
    let mut testing_mic = use_signal(|| false);
    // The model picked on the model step. Empty = "the recommendation" (which
    // can shift with the low-power answer until the user touches the picker).
    let mut chosen_model = use_signal(String::new);

    // The step list adapts to the choices (recomputed every render).
    let mut steps: Vec<&'static str> = vec!["welcome", "intent", "mic"];
    if cfg!(target_os = "macos") && *want_meetings.read() {
        steps.push("sysaudio");
    }
    steps.push("model");
    if *want_discord.read() && cfg!(feature = "discord") {
        steps.push("discord");
    }
    steps.push("ready");
    let total = steps.len();
    let cur = (*step.read()).min(total - 1);
    let name = steps[cur];

    // Shared transition plumbing. Leaving the mic step (any direction, or
    // finishing) must stop a running test; finishing persists setup_complete.
    // These are FnMut closures cloned per consumer (Engine is Clone, not Copy).
    let stop_test_if_running = {
        let eng = engine.clone();
        move || {
            if *testing_mic.peek() {
                let _ = eng.rec_tx.send(RecorderCmd::MicTestStop);
                testing_mic.set(false);
            }
        }
    };
    let finish = {
        let mut stop = stop_test_if_running.clone();
        move || {
            stop();
            let mut s = settings.peek().clone();
            s.setup_complete = true;
            let _ = s.save();
            settings.set(s);
            show_wizard.set(false);
        }
    };
    // `apply` = whether the current step's choices take effect (Next) or are
    // left alone (Skip). Both advance.
    let advance = {
        let mut stop = stop_test_if_running.clone();
        let mut fin = finish.clone();
        move |apply: bool| {
            stop();
            match name {
                "intent" if apply => {
                    let mut s = settings.peek().clone();
                    apply_intents(
                        &mut s,
                        *want_meetings.peek(),
                        *want_discord.peek(),
                        *want_voice.peek(),
                        *low_power.peek(),
                    );
                    let _ = s.save();
                    settings.set(s);
                }
                "model" if apply => {
                    let chosen = chosen_model.peek().clone();
                    let mut s = settings.peek().clone();
                    s.model = if chosen.is_empty() {
                        recommended_model(*low_power.peek()).to_string()
                    } else {
                        chosen
                    };
                    let _ = s.save();
                    settings.set(s);
                }
                _ => {}
            }
            if cur + 1 >= total {
                fin();
            } else {
                step.set(cur + 1);
            }
        }
    };
    let back = {
        let mut stop = stop_test_if_running.clone();
        move || {
            stop();
            step.set(cur.saturating_sub(1));
        }
    };

    // The model step's effective selection: the user's pick, else the
    // recommendation (Parakeet-first when the build carries it).
    let sel_model = {
        let chosen = chosen_model.read().clone();
        if chosen.is_empty() {
            recommended_model(*low_power.read()).to_string()
        } else {
            chosen
        }
    };
    let sel_info = models.read().iter().find(|m| m.name == sel_model).cloned();
    let sel_progress = match &*model_progress.read() {
        Some((n, p)) if *n == sel_model => Some(*p),
        _ => None,
    };
    let transcription_models: Vec<ModelInfo> = models
        .read()
        .iter()
        .filter(|m| m.kind == "transcription")
        .cloned()
        .collect();
    let eng_test = engine.clone();
    let eng_dl = engine.clone();

    rsx! {
        div { class: "overlay",
            div { class: "wizard-card",
                if name == "welcome" {
                    div { class: "wizard-hero",
                        div { class: "wizard-brand", "Z" }
                        div {
                            div { class: "wizard-title", "Welcome to Zord" }
                            p { class: "wizard-sub", "Your conversations, transcribed — and they never leave this machine." }
                        }
                    }
                    div { class: "wizard-body",
                        ul { class: "wizard-summary",
                            li {
                                {icon("mic")}
                                "Hears both sides — your mic and this computer's audio, on one timeline."
                            }
                            li {
                                {icon("sparkles")}
                                "Summaries, action items, and \"what did we decide?\" answers — all on-device."
                            }
                            li {
                                {icon("search")}
                                "Every word searchable and exportable. No cloud. No account. No uploads."
                            }
                        }
                        p { class: "field-note", "A minute of setup and you're ready to record. Everything is skippable and lives in Settings afterwards — re-run this any time from Settings → About." }
                    }
                }
                if name == "intent" {
                    div { class: "wizard-title", "What will you record?" }
                    p { class: "wizard-sub", "Pick everything that applies — Zord tunes its defaults to match." }
                    div { class: "wizard-body",
                        div { class: "intent-cards",
                            button {
                                class: if *want_meetings.read() { "intent-card on" } else { "intent-card" },
                                onclick: move |_| { let v = *want_meetings.peek(); want_meetings.set(!v); },
                                {icon("speaker")}
                                span { "Mic + desktop audio" }
                                span { class: "intent-desc", "Plain recording: you and whatever this computer plays — Teams, Zoom, browser calls, videos. No bots involved." }
                            }
                            button {
                                class: if *want_discord.read() { "intent-card on" } else { "intent-card" },
                                onclick: move |_| { let v = *want_discord.peek(); want_discord.set(!v); },
                                {icon("headphones")}
                                span { "Discord calls" }
                                span { class: "intent-desc", "One clean track per speaker, real names, via your own bot." }
                            }
                            button {
                                class: if *want_voice.read() { "intent-card on" } else { "intent-card" },
                                onclick: move |_| { let v = *want_voice.peek(); want_voice.set(!v); },
                                {icon("mic")}
                                span { "Just my voice" }
                                span { class: "intent-desc", "Notes, dictation, drafts — microphone only." }
                            }
                        }
                        div { class: "field-row",
                            label { class: "field-label", "This is a low-powered machine (older laptop, no GPU)" }
                            button {
                                class: if *low_power.read() { "toggle on" } else { "toggle" },
                                onclick: move |_| { let v = *low_power.peek(); low_power.set(!v); },
                                if *low_power.read() { "Yes" } else { "No" }
                            }
                        }
                    }
                }
                if name == "mic" {
                    div { class: "wizard-title", "Your microphone" }
                    p { class: "wizard-sub", "Pick the mic you'll use, then test it — say something and watch the meter move." }
                    div { class: "wizard-body",
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
                        div { class: "meters",
                            Meter { label: "Mic".to_string(), level: me_level, kind: "me".to_string() }
                        }
                        button {
                            class: if *testing_mic.read() { "mbtn" } else { "wizard-primary" },
                            onclick: move |_| {
                                if *testing_mic.peek() {
                                    let _ = eng_test.rec_tx.send(RecorderCmd::MicTestStop);
                                    testing_mic.set(false);
                                } else {
                                    let _ = eng_test.rec_tx.send(RecorderCmd::MicTestStart {
                                        device: settings.peek().input_device.clone(),
                                    });
                                    testing_mic.set(true);
                                }
                            },
                            if *testing_mic.read() { "Stop test" } else { "Test microphone" }
                        }
                        p { class: "field-note", "Your OS may ask for microphone permission the first time — that's Zord asking to hear you, locally." }
                    }
                }
                if name == "sysaudio" {
                    div { class: "wizard-title", "Hearing the other side" }
                    p { class: "wizard-sub", "To record what others say in a call, macOS requires the Screen Recording permission (that's where system audio lives — Zord captures no video)." }
                    div { class: "wizard-body",
                        p { class: "field-note", "Enable Zord under Privacy & Security → Screen Recording, then relaunch Zord when you get a chance. Until then, recordings are mic-only with a gentle banner." }
                        button {
                            class: "wizard-primary",
                            onclick: move |_| {
                                let _ = open::that("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture");
                            },
                            "Open System Settings"
                        }
                    }
                }
                if name == "model" {
                    div { class: "wizard-title", "Your transcription model" }
                    p { class: "wizard-sub",
                        if cfg!(feature = "parakeet") {
                            "We recommend Parakeet — fast and accurate across 25 languages, even on CPU. Or pick any model; you can re-transcribe with a different one later."
                        } else if *low_power.read() {
                            "For this machine we recommend a small, fast model — you can re-transcribe anything with a bigger one later."
                        } else {
                            "We recommend the turbo model: near best-in-class accuracy at a fraction of the size."
                        }
                    }
                    div { class: "wizard-body",
                        div { class: "field",
                            label { "Model" }
                            select {
                                onchange: move |e: FormEvent| {
                                    let v = e.value();
                                    chosen_model.set(v.clone());
                                    let mut s = settings.peek().clone();
                                    s.model = v;
                                    let _ = s.save();
                                    settings.set(s);
                                },
                                for m in transcription_models.iter() {
                                    option { value: "{m.name}", selected: m.name == sel_model, "{m.name}" }
                                }
                            }
                        }
                        if let Some(m) = sel_info {
                            div { class: "model-row sel",
                                div { class: "model-main",
                                    div { class: "model-name", "{m.name}" }
                                    div { class: "model-desc", "{m.description} · {m.size}" }
                                }
                                div { class: "model-actions",
                                    if m.downloaded {
                                        span { class: "gen-state ok", {icon("check")} }
                                    } else if let Some(p) = sel_progress {
                                        div { class: "dl-prog",
                                            div { class: "dl-bar", style: "width: {p}%" }
                                            span { class: "dl-txt", "Downloading… {p}%" }
                                        }
                                    } else {
                                        button {
                                            class: "mbtn",
                                            onclick: {
                                                let name = m.name.clone();
                                                move |_| {
                                                    let mut s = settings.peek().clone();
                                                    s.model = name.clone();
                                                    let _ = s.save();
                                                    settings.set(s);
                                                    let _ = eng_dl.model_tx.send(ModelCmd::Download(name.clone()));
                                                }
                                            },
                                            "Download now"
                                        }
                                    }
                                }
                            }
                        }
                        p { class: "field-note", "Or skip — the model downloads automatically on your first recording. Models are cached locally and run fully offline." }
                    }
                }
                if name == "discord" {
                    div { class: "wizard-title", "Discord" }
                    p { class: "wizard-sub", "Bring your own bot and Zord records every speaker on their own track. This is the same panel as Settings → Integrations." }
                    div { class: "wizard-body wizard-scroll",
                        IntegrationsSettings { settings, notice }
                    }
                }
                if name == "ready" {
                    div { class: "wizard-title", "You're ready" }
                    p { class: "wizard-sub", "Here's how Zord is set up — all of it lives in Settings if you change your mind." }
                    div { class: "wizard-body",
                        ul { class: "wizard-summary",
                            li {
                                {icon("check")}
                                {match settings.read().capture_mode.as_str() {
                                    "mic" => "Capture: your microphone",
                                    "both" => "Capture: microphone + the other side of your calls",
                                    other => other,
                                }}
                            }
                            li { {icon("check")} "Model: {settings.read().model}" }
                            li {
                                {icon("check")}
                                if settings.read().live_transcription {
                                    "Live transcription while you record"
                                } else {
                                    "Transcription runs right after you stop (kind to this machine)"
                                }
                            }
                            if *want_discord.read() && cfg!(feature = "discord") {
                                li { {icon("check")} "Discord: press the blurple Record Discord button while in a voice channel" }
                            }
                        }
                        p { class: "field-note",
                            if *want_discord.read() && cfg!(feature = "discord") {
                                "Join a voice channel and press Record Discord — or press Record for a local session."
                            } else {
                                "Press Record whenever you're ready."
                            }
                        }
                    }
                }

                // ---- Footer: dots · Back · Skip · Next/Finish ----
                div { class: "wizard-foot",
                    div { class: "wizard-dots",
                        for i in 0..total {
                            span { key: "{i}", class: if i == cur { "wizard-dot on" } else { "wizard-dot" } }
                        }
                    }
                    if cur > 0 {
                        button { class: "mbtn ghost", onclick: { let mut b = back.clone(); move |_| b() }, "Back" }
                    }
                    if name == "welcome" {
                        button { class: "mbtn ghost", onclick: { let mut f = finish.clone(); move |_| f() }, "Skip setup" }
                        button { class: "wizard-primary", onclick: { let mut a = advance.clone(); move |_| a(true) }, "Set up Zord" }
                    } else if name == "ready" {
                        button { class: "wizard-primary", onclick: { let mut f = finish.clone(); move |_| f() }, "Finish" }
                    } else {
                        button { class: "mbtn ghost", onclick: { let mut a = advance.clone(); move |_| a(false) }, "Skip" }
                        button { class: "wizard-primary", onclick: { let mut a = advance.clone(); move |_| a(true) }, "Next" }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intents_tune_defaults() {
        // Meetings → both channels.
        let mut s = Settings::default();
        apply_intents(&mut s, true, false, false, false);
        assert_eq!(s.capture_mode, "both");
        assert!(s.live_transcription); // untouched

        // Voice-only → mic capture.
        let mut s = Settings::default();
        apply_intents(&mut s, false, false, true, false);
        assert_eq!(s.capture_mode, "mic");

        // Meetings wins over voice when both are picked.
        let mut s = Settings::default();
        apply_intents(&mut s, true, false, true, false);
        assert_eq!(s.capture_mode, "both");

        // Low power → deferred transcription; the model is the model step's
        // call, not intent's.
        let mut s = Settings::default();
        let model_before = s.model.clone();
        apply_intents(&mut s, true, true, false, true);
        assert!(!s.live_transcription);
        assert_eq!(s.model, model_before);

        // Discord alone changes no settings (it routes a step).
        let mut s = Settings::default();
        let before = s.clone();
        apply_intents(&mut s, false, true, false, false);
        assert_eq!(s, before);
    }

    #[test]
    fn model_recommendation_follows_build_and_hardware() {
        if cfg!(feature = "parakeet") {
            // Parakeet leads whenever the build carries it (CPU-friendly).
            assert_eq!(recommended_model(true), "parakeet-tdt-0.6b-v3");
            assert_eq!(recommended_model(false), "parakeet-tdt-0.6b-v3");
        } else {
            assert_eq!(recommended_model(true), "small.en");
            assert_eq!(recommended_model(false), "large-v3-turbo-q5_0");
        }
    }
}
