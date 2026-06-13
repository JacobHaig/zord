//! Speakers view, consent dialog, voice-identification settings UI, and
//! Phase 48 person profile detail pane.
//!
//! Phase 38d — guarded at the component level with `cfg!(feature = "voiceprints")`
//! runtime checks; the view is only reachable when the feature is compiled in.

use dioxus::prelude::*;
use zord_config::Settings;
use zord_store::VoiceprintInfo;

use crate::{engine::DbCmd, icon, profile::ProfileData, Engine};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Unix time in epoch seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "Jun 4, 2026" from an epoch-SECONDS timestamp. Distinct from main.rs's
/// `fmt_date`, which takes MILLISECONDS — `VoiceprintInfo::updated_at` is in
/// seconds, so don't deduplicate this into the ms helper.
fn fmt_date_secs(secs: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(secs as i64, 0)
        .single()
        .map(|d| d.format("%b %-d, %Y").to_string())
        .unwrap_or_default()
}

/// Canonical embedding-model name as the engine stores it on voiceprints: the
/// raw setting string round-trips through the same
/// `EmbeddingModel::parse_or_default(..).name()` the engine uses, so the
/// stale-model comparison is apples-to-apples (the raw setting can differ from
/// the canonical name on a default config).
#[cfg(feature = "voiceprints")]
fn canonical_model_name(setting: &str) -> String {
    zord_diarize::EmbeddingModel::parse_or_default(setting)
        .name()
        .to_string()
}
#[cfg(not(feature = "voiceprints"))]
fn canonical_model_name(setting: &str) -> String {
    setting.to_string()
}

// ── Consent dialog ────────────────────────────────────────────────────────────

/// Shown whenever voiceprint identification is enabled for the first time (or
/// re-shown from Settings). Writes consent + enables the setting on accept,
/// then closes itself via `show`.
#[component]
pub fn ConsentDialog(mut show: Signal<bool>, settings: Signal<Settings>) -> Element {
    rsx! {
        div { class: "overlay",
            div { class: "confirm-card consent-card",
                h2 { "Remember voices on this device" }
                ul { class: "consent-bullets",
                    li {
                        {icon("check")}
                        span {
                            "Stores voice patterns \u{2014} small numeric fingerprints, not recordings \u{2014} "
                            "for each person you name."
                        }
                    }
                    li {
                        {icon("check")}
                        span {
                            "Everything lives only on this computer. Zord has no server; "
                            "nothing is ever uploaded."
                        }
                    }
                    li {
                        {icon("alert")}
                        span {
                            "Voice patterns may be considered biometric data in some regions "
                            "(e.g. Illinois BIPA, GDPR Art. 9). You are responsible for any "
                            "applicable local rules."
                        }
                    }
                    li {
                        {icon("trash")}
                        span {
                            "Forget any person \u{2014} or everything \u{2014} anytime, instantly, "
                            "from the Speakers view."
                        }
                    }
                }
                div { class: "confirm-actions",
                    button {
                        class: "mbtn ghost",
                        onclick: move |_| show.set(false),
                        "Cancel"
                    }
                    button {
                        class: "mbtn",
                        onclick: move |_| {
                            let mut s = settings.peek().clone();
                            s.voiceprints_enabled = true;
                            s.voiceprints_consented_at = now_secs();
                            let _ = s.save();
                            settings.set(s);
                            show.set(false);
                        },
                        "I agree \u{2014} enable"
                    }
                }
            }
        }
    }
}

// ── Main Speakers view ────────────────────────────────────────────────────────

#[component]
pub fn SpeakersView(
    voiceprints: Signal<Vec<VoiceprintInfo>>,
    settings: Signal<Settings>,
    engine: Engine,
    on_open_session: EventHandler<String>,
    /// Phase 48: current profile detail pane (`None` = show card grid).
    profile: Signal<Option<ProfileData>>,
    /// Phase 48: `true` while the profile request is in flight.
    profile_loading: Signal<bool>,
) -> Element {
    let mut show_consent = use_signal(|| false);

    rsx! {
        div { class: "speakers-view",

            // Consent overlay (opened by Enable button or settings toggle).
            if *show_consent.read() {
                ConsentDialog { show: show_consent, settings }
            }

            if cfg!(feature = "voiceprints") {
                {
                    let enabled = settings.read().voiceprints_enabled;
                    let items = voiceprints.read().clone();

                    // ── Phase 48: profile detail pane (overrides card grid) ────
                    if let Some(p) = profile.read().clone() {
                        rsx! {
                            ProfilePane {
                                data: p,
                                on_back: move |_| profile.set(None),
                                on_open_session,
                            }
                        }
                    } else if *profile_loading.read() {
                        // Loading state while the db thread assembles the profile.
                        rsx! {
                            div { class: "profile-loading",
                                {icon("users")}
                                span { "Loading profile\u{2026}" }
                            }
                        }
                    } else if !enabled {
                        // ── State 1: disabled ──────────────────────────────
                        rsx! {
                            div { class: "speakers-hero",
                                div { class: "speakers-hero-icon", {icon("users")} }
                                h2 { "Remember who's speaking" }
                                p { class: "field-note",
                                    "Zord can remember voices \u{2014} stored only on this device \u{2014} "
                                    "and name people automatically in future meetings."
                                }
                                button {
                                    class: "mbtn",
                                    onclick: move |_| show_consent.set(true),
                                    {icon("check")} "Enable voice identification"
                                }
                            }
                        }
                    } else if items.is_empty() {
                        // ── State 2: enabled, empty ────────────────────────
                        rsx! {
                            div { class: "speakers-empty",
                                div { class: "speakers-hero-icon", {icon("users")} }
                                h2 { "No voices saved yet" }
                                p { class: "field-note",
                                    "Name a speaker on any transcript, or record a Discord call \u{2014} "
                                    "people you name are remembered here automatically."
                                }
                            }
                        }
                    } else {
                        // ── State 3: library ──────────────────────────────
                        let current_model =
                            canonical_model_name(&settings.read().diarize_embedding_model);

                        rsx! {
                            div { class: "speakers-list",
                                h2 { class: "speakers-title", "Voice library" }
                                p { class: "field-note speakers-subtitle",
                                    "{items.len()} person(s) remembered. Click a name to see their profile; use the action buttons to rename or forget."
                                }
                                {
                                    // Build the name list before consuming `items`.
                                    let all_names: Vec<(i64, String)> = items
                                        .iter()
                                        .map(|i| (i.id, i.name.clone()))
                                        .collect::<Vec<_>>();
                                    let all_names_owned = all_names.clone();
                                    items.into_iter().map(move |info| {
                                        let an = all_names_owned.clone();
                                        let cm = current_model.clone();
                                        let eng = engine.clone();
                                        let eng2 = eng.clone();
                                        rsx! {
                                            SpeakerCard {
                                                key: "{info.id}",
                                                info,
                                                all_names: an,
                                                current_model: cm,
                                                engine: eng,
                                                on_open_session,
                                                on_open_profile: move |vp_id: i64| {
                                                    profile_loading.set(true);
                                                    profile.set(None);
                                                    let _ = eng2.db_tx.send(DbCmd::LoadProfile(vp_id));
                                                },
                                            }
                                        }
                                    })
                                }
                            }
                        }
                    }
                }
            } else {
                div { class: "empty",
                    "Build with --features voiceprints to use voice identification."
                }
            }
        }
    }
}

// ── Per-person card ───────────────────────────────────────────────────────────

#[component]
fn SpeakerCard(
    info: VoiceprintInfo,
    /// All names in the library (for the Merge-into target select).
    all_names: Vec<(i64, String)>,
    current_model: String,
    engine: Engine,
    on_open_session: EventHandler<String>,
    /// Phase 48: called when the user clicks the name area to open the profile.
    on_open_profile: EventHandler<i64>,
) -> Element {
    let id = info.id;
    let mut editing = use_signal(|| false);
    let mut edit_text = use_signal(|| info.name.clone());
    let mut confirm_forget = use_signal(|| false);
    // Merge-into: None = picker closed; Some(name) = name chosen, confirm pending.
    let mut merge_target: Signal<Option<String>> = use_signal(|| None);

    // Unlink: Some((session_id, label)) = confirm pending.
    let mut confirm_unlink: Signal<Option<(String, String)>> = use_signal(|| None);

    // Stale-model indicator: samples were built with a different embedding model.
    let model_mismatch = info.model != current_model;

    rsx! {
        div { class: "speaker-card",

            // ── Confirm-forget dialog ──────────────────────────────────────
            if *confirm_forget.read() {
                {
                    let engine = engine.clone();
                    let name = info.name.clone();
                    rsx! {
                        div { class: "overlay",
                            div { class: "confirm-card",
                                h2 { "Forget this voice?" }
                                p { class: "field-note",
                                    "Permanently removes all voice samples for \"{name}\". "
                                    "Past transcript labels are kept; future auto-naming won't recognize them."
                                }
                                div { class: "confirm-actions",
                                    button {
                                        class: "mbtn ghost",
                                        onclick: move |_| confirm_forget.set(false),
                                        "Cancel"
                                    }
                                    button {
                                        class: "mbtn danger",
                                        onclick: move |_| {
                                            let _ = engine.db_tx.send(DbCmd::VoiceprintForget { id });
                                            confirm_forget.set(false);
                                        },
                                        "Forget voice"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Merge-into confirm dialog ──────────────────────────────────
            if let Some(target) = merge_target.read().clone() {
                {
                    let engine = engine.clone();
                    let source_name = info.name.clone();
                    let target_name = target.clone();
                    rsx! {
                        div { class: "overlay",
                            div { class: "confirm-card",
                                h2 { "Combine voices?" }
                                p { class: "field-note",
                                    "Combine \"{source_name}\"'s voice samples into \"{target_name}\" — "
                                    "\"{source_name}\" disappears from the library."
                                }
                                div { class: "confirm-actions",
                                    button {
                                        class: "mbtn ghost",
                                        onclick: move |_| merge_target.set(None),
                                        "Cancel"
                                    }
                                    button {
                                        class: "mbtn",
                                        onclick: move |_| {
                                            // Rename-merge: the store merges when
                                            // the target name already exists.
                                            let _ = engine.db_tx.send(
                                                DbCmd::VoiceprintRename {
                                                    id,
                                                    name: target_name.clone(),
                                                },
                                            );
                                            merge_target.set(None);
                                        },
                                        "Combine"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Unlink-session confirm dialog ──────────────────────────────
            if let Some((ref unlink_sid, ref unlink_label)) = *confirm_unlink.read() {
                {
                    let engine = engine.clone();
                    let sid_c = unlink_sid.clone();
                    let label_c = unlink_label.clone();
                    let name = info.name.clone();
                    rsx! {
                        div { class: "overlay",
                            div { class: "confirm-card",
                                h2 { "Remove this session link?" }
                                p { class: "field-note",
                                    "Unlinks \"{label_c}\" from {name}. "
                                    "The voice samples from that session will be removed so they no longer affect recognition. "
                                    "The transcript label is reset to \"Speaker N\"."
                                }
                                div { class: "confirm-actions",
                                    button {
                                        class: "mbtn ghost",
                                        onclick: move |_| confirm_unlink.set(None),
                                        "Cancel"
                                    }
                                    button {
                                        class: "mbtn danger",
                                        onclick: move |_| {
                                            let _ = engine.db_tx.send(
                                                DbCmd::VoiceprintUnlink {
                                                    voiceprint_id: id,
                                                    session_id: sid_c.clone(),
                                                },
                                            );
                                            confirm_unlink.set(None);
                                        },
                                        "Unlink session"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Card header: name + actions ────────────────────────────────
            div { class: "speaker-card-head",
                if *editing.read() {
                    {
                        let engine = engine.clone();
                        rsx! {
                            input {
                                class: "rename-input",
                                autofocus: true,
                                value: "{edit_text}",
                                oninput: move |e| edit_text.set(e.value()),
                                onkeydown: move |e| match e.key() {
                                    Key::Enter => {
                                        let t = edit_text.peek().trim().to_string();
                                        if !t.is_empty() {
                                            let _ = engine.db_tx.send(DbCmd::VoiceprintRename {
                                                id,
                                                name: t,
                                            });
                                        }
                                        editing.set(false);
                                    }
                                    Key::Escape => editing.set(false),
                                    _ => {}
                                },
                            }
                        }
                    }
                } else {
                    // Phase 48: clicking the name area opens the profile pane.
                    button {
                        class: "speaker-name speaker-name-btn",
                        title: "View profile for {info.name}",
                        onclick: move |_| on_open_profile.call(id),
                        "{info.name}"
                    }
                }
                div { class: "speaker-card-actions",
                    if !*editing.read() {
                        button {
                            class: "row-btn",
                            title: "Rename",
                            onclick: move |_| {
                                edit_text.set(info.name.clone());
                                editing.set(true);
                            },
                            {icon("pen")}
                        }
                    }
                    // Merge-into: only shown when there are other people.
                    if !*editing.read() && all_names.len() > 1 {
                        {
                            let other_names: Vec<String> = all_names
                                .iter()
                                .filter(|(oid, _)| *oid != id)
                                .map(|(_, n)| n.clone())
                                .collect();
                            rsx! {
                                select {
                                    class: "merge-select",
                                    title: "Merge into another person",
                                    // A placeholder option that triggers no action.
                                    onchange: move |e: FormEvent| {
                                        let val = e.value();
                                        if !val.is_empty() {
                                            merge_target.set(Some(val));
                                        }
                                    },
                                    option { value: "", "Merge into\u{2026}" }
                                    for name in other_names {
                                        option { value: "{name}", "{name}" }
                                    }
                                }
                            }
                        }
                    }
                    button {
                        class: "row-btn",
                        title: "Forget this voice",
                        onclick: move |_| confirm_forget.set(true),
                        {icon("trash")}
                    }
                }
            }

            // ── Meta line ─────────────────────────────────────────────────
            div { class: "speaker-meta",
                "{info.samples} voice sample(s) \u{b7} last updated {fmt_date_secs(info.updated_at)}"
                if model_mismatch {
                    span { class: "speaker-stale", " \u{b7} re-enroll needed (model changed)" }
                }
            }

            // ── Appearance chips ──────────────────────────────────────────
            if !info.appearances.is_empty() {
                div { class: "speaker-appearances",
                    span { class: "speaker-appearances-label", "Seen in:" }
                    for (sid, title, score) in info.appearances.iter().take(8).cloned() {
                        {
                            let label = if title.trim().is_empty() {
                                "Recording".to_string()
                            } else {
                                title.clone()
                            };
                            let chip_title = match score {
                                Some(s) => format!("auto-matched at {}% \u{2014} click to open, \u{d7} to unlink", (s * 100.0).round() as u32),
                                None => "named manually \u{2014} click to open, \u{d7} to unlink".to_string(),
                            };
                            let sid_open = sid.clone();
                            let sid_unlink = sid.clone();
                            let label_unlink = label.clone();
                            rsx! {
                                span {
                                    key: "{sid}",
                                    class: "speaker-chip-wrap",
                                    button {
                                        class: "speaker-chip",
                                        title: "{chip_title}",
                                        onclick: move |_| on_open_session.call(sid_open.clone()),
                                        "{label}"
                                    }
                                    button {
                                        class: "speaker-chip-unlink",
                                        title: "Wrong person — unlink this session",
                                        onclick: move |_| {
                                            confirm_unlink.set(Some((
                                                sid_unlink.clone(),
                                                label_unlink.clone(),
                                            )));
                                        },
                                        "\u{d7}"
                                    }
                                }
                            }
                        }
                    }
                    if info.appearances.len() > 8 {
                        span { class: "speaker-chip-more",
                            "+{info.appearances.len() - 8} more"
                        }
                    }
                }
            }
        }
    }
}

// ── Voice-identification settings block ──────────────────────────────────────
//
// Appended to `SpeakersSettings` in main.rs. Rendered only when
// `cfg!(feature = "voiceprints")`.

#[component]
pub fn VoiceprintSettings(settings: Signal<Settings>, engine: Engine) -> Element {
    let mut show_consent = use_signal(|| false);
    let mut confirm_forget_all = use_signal(|| false);

    rsx! {
        if cfg!(feature = "voiceprints") {
            {
                rsx! {
                    // Consent / forget-all overlays
                    if *show_consent.read() {
                        ConsentDialog { show: show_consent, settings }
                    }
                    if *confirm_forget_all.read() {
                        {
                            let engine = engine.clone();
                            rsx! {
                                div { class: "overlay",
                                    div { class: "confirm-card",
                                        h2 { "Forget all voices?" }
                                        p { class: "field-note",
                                            "Permanently removes every voice sample from the library. "
                                            "Past transcript labels are kept. This cannot be undone."
                                        }
                                        div { class: "confirm-actions",
                                            button {
                                                class: "mbtn ghost",
                                                onclick: move |_| confirm_forget_all.set(false),
                                                "Cancel"
                                            }
                                            button {
                                                class: "mbtn danger",
                                                onclick: move |_| {
                                                    let _ = engine.db_tx.send(DbCmd::VoiceprintForgetAll);
                                                    confirm_forget_all.set(false);
                                                },
                                                "Forget all voices"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    section { class: "settings-section",
                        h3 { "Voice identification" }
                        p { class: "field-note",
                            "Name a speaker on any transcript and Zord remembers their voice \u{2014} "
                            "samples are stored as small numeric fingerprints, never as recordings, "
                            "and never leave this device."
                        }

                        // Enable toggle
                        div { class: "field-row",
                            label { class: "field-label", "Enable voice identification" }
                            {
                                let consented = settings.read().voiceprints_consented_at;
                                let enabled = settings.read().voiceprints_enabled;
                                rsx! {
                                    button {
                                        class: if enabled { "toggle on" } else { "toggle" },
                                        onclick: move |_| {
                                            let cur = settings.peek().voiceprints_enabled;
                                            if cur {
                                                // Turning off: keep the library, just disable.
                                                let mut s = settings.peek().clone();
                                                s.voiceprints_enabled = false;
                                                let _ = s.save();
                                                settings.set(s);
                                            } else if consented == 0 {
                                                // Never consented: show dialog.
                                                show_consent.set(true);
                                            } else {
                                                // Re-enabling after prior consent.
                                                let mut s = settings.peek().clone();
                                                s.voiceprints_enabled = true;
                                                let _ = s.save();
                                                settings.set(s);
                                            }
                                        },
                                        if enabled { "On" } else { "Off" }
                                    }
                                }
                            }
                        }

                        // Match strictness (only shown when enabled).
                        if settings.read().voiceprints_enabled {
                            div { class: "field",
                                label { "Recognition sensitivity" }
                                select {
                                    onchange: move |e: FormEvent| {
                                        let mut s = settings.peek().clone();
                                        s.voiceprints_match = e.value();
                                        let _ = s.save();
                                        settings.set(s);
                                    },
                                    option {
                                        value: "strict",
                                        selected: settings.read().voiceprints_match == "strict",
                                        "Strict \u{2014} fewer wrong names"
                                    }
                                    option {
                                        value: "standard",
                                        selected: settings.read().voiceprints_match == "standard",
                                        "Standard \u{2014} balanced (default)"
                                    }
                                    option {
                                        value: "relaxed",
                                        selected: settings.read().voiceprints_match == "relaxed",
                                        "Relaxed \u{2014} names more readily"
                                    }
                                }
                            }

                            // Forget all voices.
                            div { class: "field-row",
                                label { class: "field-label", "Clear the voice library" }
                                button {
                                    class: "mbtn ghost",
                                    onclick: move |_| confirm_forget_all.set(true),
                                    {icon("trash")} "Forget all voices"
                                }
                            }
                        }

                        p { class: "field-note",
                            "Manage individual people in the Speakers view."
                        }
                    }
                }
            }
        }
    }
}

// ── Phase 48: person profile detail pane ────────────────────────────────────

/// "Jun 4, 2026" from an epoch-MILLISECONDS timestamp.
fn fmt_date_ms(ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(ms as i64)
        .single()
        .map(|d| d.format("%b %-d, %Y").to_string())
        .unwrap_or_default()
}

/// "32%" talk share string; 0 → "—" (stats unavailable).
fn fmt_talk_share(share: f32) -> String {
    if share <= 0.0 {
        "\u{2014}".to_string()
    } else {
        format!("{:.0}%", (share * 100.0).round())
    }
}

/// Profile detail pane rendered instead of the card grid when a name is clicked.
#[component]
fn ProfilePane(
    data: ProfileData,
    on_back: EventHandler<()>,
    on_open_session: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "profile-pane",

            // ── Header: back button + name ─────────────────────────────────
            div { class: "profile-header",
                button {
                    class: "mbtn ghost profile-back-btn",
                    onclick: move |_| on_back.call(()),
                    "\u{2190} All speakers"
                }
                div { class: "profile-title-row",
                    h2 { class: "profile-name", "{data.name}" }
                    div { class: "profile-meta",
                        if data.last_heard_ms > 0 {
                            span { "Last heard {fmt_date_ms(data.last_heard_ms)}" }
                        }
                        span { "{data.meetings.len()} meeting(s)" }
                    }
                }
            }

            // ── Meetings list ──────────────────────────────────────────────
            if !data.meetings.is_empty() {
                div { class: "profile-section",
                    h3 { class: "profile-section-title", "Meetings" }
                    div { class: "profile-meetings",
                        for m in data.meetings.iter().cloned() {
                            {
                                let label = if m.title.trim().is_empty() {
                                    "Recording".to_string()
                                } else {
                                    m.title.clone()
                                };
                                let talk_str = fmt_talk_share(m.talk_share);
                                let date_str = fmt_date_ms(m.started_at);
                                let sid = m.session_id.clone();
                                rsx! {
                                    button {
                                        key: "{m.session_id}",
                                        class: "profile-meeting-row",
                                        onclick: move |_| on_open_session.call(sid.clone()),
                                        span { class: "profile-meeting-title", "{label}" }
                                        span { class: "profile-meeting-meta",
                                            "{date_str} \u{b7} {talk_str}"
                                            if m.interruptions > 0 {
                                                " \u{b7} {m.interruptions} interruption(s)"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Open items ─────────────────────────────────────────────────
            div { class: "profile-section",
                h3 { class: "profile-section-title", "Open items" }
                if data.open_items.is_empty() {
                    p { class: "profile-empty-note", "Nothing assigned in the Overview." }
                } else {
                    ul { class: "profile-open-items",
                        for item in data.open_items.iter() {
                            li { key: "{item}", "{item}" }
                        }
                    }
                }
            }

            // ── Topics ─────────────────────────────────────────────────────
            if !data.topics.is_empty() {
                div { class: "profile-section",
                    h3 { class: "profile-section-title", "Topics" }
                    div { class: "profile-topics",
                        for topic in data.topics.iter() {
                            span { key: "{topic}", class: "profile-topic-chip", "{topic}" }
                        }
                    }
                }
            }
        }
    }
}
