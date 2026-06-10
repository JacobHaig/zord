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
/// a wizard step, it configures nothing by itself).
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
        s.model = "small.en".to_string();
    }
}

/// The transcription model the wizard recommends for the hardware answer.
fn recommended_model(low_power: bool) -> &'static str {
    if low_power {
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
                    let mut s = settings.peek().clone();
                    s.model = recommended_model(*low_power.peek()).to_string();
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

    let rec_model = recommended_model(*low_power.read());
    let rec_info = models.read().iter().find(|m| m.name == rec_model).cloned();
    let rec_progress = match &*model_progress.read() {
        Some((n, p)) if n == rec_model => Some(*p),
        _ => None,
    };
    let eng_test = engine.clone();
    let eng_dl = engine.clone();

    rsx! {
        div { class: "overlay",
            div { class: "wizard-card",
                if name == "welcome" {
                    div { class: "wizard-title", "Welcome to Zord" }
                    p { class: "wizard-sub",
                        "Private meeting transcription that never leaves this machine — no cloud, no account, nothing uploaded. A minute of setup and you're ready to record."
                    }
                    div { class: "wizard-body",
                        p { class: "field-note", "Everything here is skippable and can be changed later in Settings. You can re-run this from Settings → About at any time." }
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
                                span { "Meetings on this computer" }
                                span { class: "intent-desc", "Teams, Zoom, browser calls — both sides, one timeline." }
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
                        if *low_power.read() {
                            "For this machine we recommend a small, fast model — you can re-transcribe anything with a bigger one later."
                        } else {
                            "We recommend the turbo model: near best-in-class accuracy at a fraction of the size."
                        }
                    }
                    div { class: "wizard-body",
                        if let Some(m) = rec_info {
                            div { class: "model-row sel",
                                div { class: "model-main",
                                    div { class: "model-name", "{m.name}" }
                                    div { class: "model-desc", "{m.description} · {m.size}" }
                                }
                                div { class: "model-actions",
                                    if m.downloaded {
                                        span { class: "gen-state ok", {icon("check")} }
                                    } else if let Some(p) = rec_progress {
                                        div { class: "dl-prog",
                                            div { class: "dl-bar", style: "width: {p}%" }
                                            span { class: "dl-txt", "Downloading… {p}%" }
                                        }
                                    } else {
                                        button {
                                            class: "mbtn",
                                            onclick: move |_| {
                                                let mut s = settings.peek().clone();
                                                s.model = rec_model.to_string();
                                                let _ = s.save();
                                                settings.set(s);
                                                let _ = eng_dl.model_tx.send(ModelCmd::Download(rec_model.to_string()));
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

        // Low power → deferred transcription + small model.
        let mut s = Settings::default();
        apply_intents(&mut s, true, true, false, true);
        assert!(!s.live_transcription);
        assert_eq!(s.model, "small.en");

        // Discord alone changes no settings (it routes a step).
        let mut s = Settings::default();
        let before = s.clone();
        apply_intents(&mut s, false, true, false, false);
        assert_eq!(s, before);
    }

    #[test]
    fn model_recommendation_follows_hardware() {
        assert_eq!(recommended_model(true), "small.en");
        assert_eq!(recommended_model(false), "large-v3-turbo-q5_0");
    }
}
