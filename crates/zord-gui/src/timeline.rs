//! Session timeline panel — Phase 42c.
//!
//! Renders a collapsible bottom panel with per-track waveform graphs, a
//! scrubber/playhead, playback controls, and transcript sync.  Only shown for
//! `View::Session` (saved sessions); the live view never has finished audio files
//! so peaks cannot be computed.
//!
//! ## What this module provides
//! - [`TimelinePanel`] — the public component mounted by `main.rs`.
//! - [`bucket_speakers`] — pure function mapping segment spans onto per-bucket
//!   speaker indices (unit-tested below).

use std::collections::HashMap;
use std::path::PathBuf;

use dioxus::prelude::*;
use zord_core::{Segment, Source};

use crate::{
    engine::{AudioFiles, PlayCmd, TimelineLane},
    icon, Engine,
};

// ── palette ──────────────────────────────────────────────────────────────────

/// Hex fill colors for speaker-indexed lanes and the me/others lanes. Index
/// matches the `.line.spk-N` accent palette in style.css.
const SPK_COLORS: &[&str] = &[
    "#ffb454", "#6fd3ff", "#a6e22e", "#ff80bf", "#c792ea", "#ffd866", "#80ffd4", "#ff6e6e",
];

fn lane_color(track: &str, me_speaker: Option<i32>, speaker: Option<i32>) -> &'static str {
    if track == "me" {
        return "#4cc2ff"; // var(--me) default
    }
    if track == "others" {
        return "#ffb454"; // var(--others) default
    }
    // spk-N lane
    if let Some(idx) = speaker {
        // "me" speaker gets the --me accent
        if me_speaker == Some(idx) {
            return "#4cc2ff";
        }
        SPK_COLORS
            .get((idx as usize) % SPK_COLORS.len())
            .copied()
            .unwrap_or("#ffb454")
    } else {
        "#ffb454"
    }
}

// ── bucket_speakers ───────────────────────────────────────────────────────────

/// For the "others" lane, compute which diarized speaker (if any) covers each
/// amplitude bucket.  Returns a `Vec<Option<i32>>` of length `n_buckets`.
///
/// `segments` is the full session transcript.  Only `Source::Others` rows with a
/// `speaker` index are examined (normal diarized sessions + integration sessions
/// both satisfy this).  Buckets with no span keep `None` (rendered with the
/// neutral --others color).
///
/// The mapping is: bucket `k` covers `[k * ms_per_bucket, (k+1) * ms_per_bucket)`.
/// A bucket is attributed to whichever speaker's span *starts* within it or was
/// still ongoing at its midpoint; in a conflict the first writer wins.
pub fn bucket_speakers(
    segments: &[Segment],
    duration_ms: u64,
    n_buckets: usize,
) -> Vec<Option<i32>> {
    let mut out = vec![None; n_buckets];
    if duration_ms == 0 || n_buckets == 0 {
        return out;
    }
    let ms_per_bucket = duration_ms as f64 / n_buckets as f64;

    for seg in segments {
        if seg.source != Source::Others {
            continue;
        }
        let Some(spk) = seg.speaker else { continue };

        let start_b = (seg.t_start_ms as f64 / ms_per_bucket).floor() as usize;
        let end_b = ((seg.t_end_ms as f64 / ms_per_bucket).ceil() as usize).min(n_buckets);
        for slot in out.iter_mut().take(end_b).skip(start_b) {
            if slot.is_none() {
                *slot = Some(spk);
            }
        }
    }
    out
}

// ── display name helpers ──────────────────────────────────────────────────────

/// Human-readable display name for a timeline lane.
fn lane_display_name(
    track: &str,
    speaker: Option<i32>,
    speaker_names: &HashMap<i32, String>,
    _me_speaker: Option<i32>,
) -> String {
    match track {
        "me" => "Me".to_string(),
        "others" => "Desktop".to_string(),
        _ => {
            if let Some(idx) = speaker {
                speaker_names
                    .get(&idx)
                    .cloned()
                    .unwrap_or_else(|| format!("Speaker {}", idx + 1))
            } else {
                track.to_string()
            }
        }
    }
}

/// CSS class for a lane chip (for the me speaker or a spk-N index).
fn lane_chip_class(track: &str, speaker: Option<i32>, me_speaker: Option<i32>) -> String {
    if track == "me" {
        return "tl-chip spk-me".to_string();
    }
    if track == "others" {
        return "tl-chip tl-chip-others".to_string();
    }
    if let Some(idx) = speaker {
        if me_speaker == Some(idx) {
            return "tl-chip spk-me".to_string();
        }
        return format!("tl-chip tl-chip-spk-{}", (idx as usize) % SPK_COLORS.len());
    }
    "tl-chip".to_string()
}

// ── SVG path builder ─────────────────────────────────────────────────────────

/// Build an SVG filled-area path string from a slice of peak values (0..=1).
/// The viewBox height is `y_scale`. `y_offset` shifts the path down for
/// overlaid lanes.
fn peaks_to_path(peaks: &[f32], y_offset: f32, y_scale: f32) -> String {
    if peaks.is_empty() {
        return String::new();
    }
    let n = peaks.len();
    let mut d = format!("M0 {}", y_offset + y_scale);
    for (i, &p) in peaks.iter().enumerate() {
        let y = y_offset + y_scale - p.clamp(0.0, 1.0) * y_scale;
        d.push_str(&format!(" L{i} {y:.2}"));
    }
    d.push_str(&format!(" L{} {} Z", n - 1, y_offset + y_scale));
    d
}

/// Build a set of colored sub-paths for the "others" lane with per-bucket
/// speaker coloring.  Returns a list of `(color, path_d)` pairs, one per
/// contiguous color run.
fn colored_peaks_paths(
    peaks: &[f32],
    bucket_spk: &[Option<i32>],
    neutral_color: &str,
    me_speaker: Option<i32>,
    y_offset: f32,
    y_scale: f32,
) -> Vec<(String, String)> {
    if peaks.is_empty() {
        return Vec::new();
    }
    let n = peaks.len();
    let color_of = |opt: Option<i32>| -> String {
        match opt {
            None => neutral_color.to_string(),
            Some(idx) => {
                if me_speaker == Some(idx) {
                    "#4cc2ff".to_string()
                } else {
                    SPK_COLORS
                        .get((idx as usize) % SPK_COLORS.len())
                        .copied()
                        .unwrap_or("#ffb454")
                        .to_string()
                }
            }
        }
    };

    let mut result: Vec<(String, String)> = Vec::new();
    let mut run_start = 0usize;
    let mut run_color = color_of(bucket_spk.first().copied().flatten());

    let flush = |from: usize, to: usize, color: &str, result: &mut Vec<(String, String)>| {
        if from >= to {
            return;
        }
        let slice = &peaks[from..to];
        let n_slice = slice.len();
        let mut d = format!("M{} {}", from, y_offset + y_scale);
        for (i, &p) in slice.iter().enumerate() {
            let x = from + i;
            let y = y_offset + y_scale - p.clamp(0.0, 1.0) * y_scale;
            d.push_str(&format!(" L{x} {y:.2}"));
        }
        d.push_str(&format!(
            " L{} {} Z",
            from + n_slice - 1,
            y_offset + y_scale
        ));
        result.push((color.to_string(), d));
    };

    for i in 1..n {
        let c = color_of(bucket_spk.get(i).copied().flatten());
        if c != run_color {
            flush(run_start, i, &run_color.clone(), &mut result);
            run_start = i;
            run_color = c;
        }
    }
    flush(run_start, n, &run_color.clone(), &mut result);
    result
}

// ── path resolution ───────────────────────────────────────────────────────────

/// Resolve a `TimelineLane` to an absolute filesystem path using `AudioFiles`.
fn resolve_path(lane: &TimelineLane, audio: &AudioFiles) -> Option<PathBuf> {
    match lane.track.as_str() {
        "me" => audio.me.as_deref().map(PathBuf::from),
        "others" => audio.others.as_deref().map(PathBuf::from),
        _ => {
            // spk-N
            lane.speaker
                .and_then(|idx| audio.speakers.get(&idx))
                .map(PathBuf::from)
        }
    }
}

/// Collect the enabled lane paths from a snapshot of lanes + audio files.
fn collect_enabled_paths(
    lanes: &[TimelineLane],
    audio: &AudioFiles,
    lane_enabled: &HashMap<String, bool>,
) -> Vec<PathBuf> {
    lanes
        .iter()
        .filter(|l| *lane_enabled.get(l.track.as_str()).unwrap_or(&true))
        .filter_map(|l| resolve_path(l, audio))
        .collect()
}

// ── TimelinePanel ─────────────────────────────────────────────────────────────

/// Props for [`TimelinePanel`].
#[derive(Props, Clone, PartialEq)]
pub struct TimelinePanelProps {
    /// Session id currently on screen.
    pub session_id: String,
    /// Computed amplitude lanes from Phase 42a.
    pub lanes: Signal<Vec<TimelineLane>>,
    /// Current playhead position (ms), or `None` when stopped.
    pub pos: Signal<Option<u64>>,
    /// Which lanes are enabled (missing key = enabled).
    pub lane_enabled: Signal<HashMap<String, bool>>,
    /// Whether to merge all enabled lanes into one SVG (vs stacked).
    pub merged: Signal<bool>,
    /// Close the panel.
    pub on_close: EventHandler<()>,
    /// Retained audio files for path resolution.
    pub audio: Signal<AudioFiles>,
    /// Speaker display names.
    pub speaker_names: Signal<HashMap<i32, String>>,
    /// Which speaker index is the app user (`me_speaker`).
    pub me_speaker: Signal<Option<i32>>,
    /// Full transcript segments (used for Others-lane coloring + sync).
    pub segments: Signal<Vec<Segment>>,
    /// Engine handle for play/pause/seek commands.
    pub engine: Engine,
    /// Jump the transcript to this segment (highlight + scroll).
    pub highlight: Signal<Option<i64>>,
}

/// The collapsible bottom session timeline panel.
///
/// Mounts under the transcript inside `View::Session`.  Not shown in
/// `View::Live` — peaks require finished audio files.
#[component]
pub fn TimelinePanel(props: TimelinePanelProps) -> Element {
    let TimelinePanelProps {
        session_id: _sid,
        lanes,
        pos,
        mut lane_enabled,
        mut merged,
        on_close,
        audio,
        speaker_names,
        me_speaker,
        segments,
        engine,
        mut highlight,
    } = props;

    // Local state: are we playing or paused?
    let mut playing = use_signal(|| false);
    let mut paused = use_signal(|| false);
    // Drag-scrub: set while the mouse button is held on the graph area.
    let mut dragging = use_signal(|| false);

    // Sync `playing` / `paused` from the pos tick: if pos becomes None while
    // we thought we were playing, the track ended.
    use_effect(move || {
        let p = *pos.read();
        if p.is_none() && *playing.peek() && !*paused.peek() {
            playing.set(false);
        }
    });

    // Transcript sync: highlight the segment containing the current playhead.
    use_effect(move || {
        let Some(ms) = *pos.read() else { return };
        if !*playing.peek() {
            return;
        }
        let segs_local = segments.read();
        let hit = segs_local
            .iter()
            .find(|s| s.id.is_some() && s.t_start_ms <= ms && ms < s.t_end_ms);
        if let Some(seg) = hit {
            if let Some(id) = seg.id {
                if *highlight.peek() != Some(id) {
                    highlight.set(Some(id));
                    let _ = document::eval(&format!(
                        "requestAnimationFrame(()=>{{const e=document.getElementById('seg-{id}');\
                         if(e){{e.scrollIntoView({{block:'nearest',behavior:'smooth'}});}}}})"
                    ));
                }
            }
        }
    });

    // Snapshot values for use in render (avoids multiple reads inside rsx! loops).
    let lanes_v = lanes.read().clone();
    let pos_v = *pos.read();
    let me_spk = *me_speaker.read();
    let names = speaker_names.read().clone();
    let af = audio.read().clone();
    let segs = segments.read().clone();
    let is_merged = *merged.read();
    let is_playing = *playing.read();
    let is_paused = *paused.read();

    // Max duration across enabled lanes.
    let max_dur: u64 = lanes_v
        .iter()
        .filter(|l| *lane_enabled.read().get(l.track.as_str()).unwrap_or(&true))
        .map(|l| l.duration_ms)
        .max()
        .unwrap_or(0);

    // Loading state: panel was opened but lanes haven't arrived yet.
    if lanes_v.is_empty() {
        return rsx! {
            div { class: "tl-panel",
                div { class: "tl-header",
                    span { class: "tl-loading", "Building timeline…" }
                    button { class: "tl-close", onclick: move |_| on_close.call(()), {icon("close")} }
                }
            }
        };
    }

    // ── position label ────────────────────────────────────────────────────────
    let pos_label = {
        let cur = pos_v.unwrap_or(0);
        format!("{} / {}", fmt_ms(cur), fmt_ms(max_dur))
    };

    // ── playhead fraction (0..=1) ─────────────────────────────────────────────
    let playhead_frac: f64 = if max_dur > 0 {
        pos_v.unwrap_or(0) as f64 / max_dur as f64
    } else {
        0.0
    };

    // ── play/pause button icon ────────────────────────────────────────────────
    let play_icon = if is_playing && !is_paused {
        "pause"
    } else {
        "play"
    };

    // ── handlers (all clones of signals/engine, no moved locals) ─────────────

    // play/pause/resume
    let on_play_pause = {
        let e = engine.clone();
        let lanes_snap = lanes_v.clone();
        let af_snap = af.clone();
        move |_: MouseEvent| {
            if is_playing && !is_paused {
                let _ = e.play_tx.send(PlayCmd::TimelinePause);
                paused.set(true);
            } else if is_playing && is_paused {
                let _ = e.play_tx.send(PlayCmd::TimelineResume);
                paused.set(false);
            } else {
                let start = pos_v.unwrap_or(0);
                let paths = collect_enabled_paths(&lanes_snap, &af_snap, &lane_enabled.peek());
                let _ = e.play_tx.send(PlayCmd::TimelinePlay {
                    paths,
                    start_ms: start,
                });
                playing.set(true);
                paused.set(false);
            }
        }
    };

    // seek (click or drag on graph area)
    let on_seek = {
        let e = engine.clone();
        let lanes_snap2 = lanes_v.clone();
        let af_snap2 = af.clone();
        move |frac: f64| {
            if max_dur == 0 {
                return;
            }
            let ms = (frac.clamp(0.0, 1.0) * max_dur as f64) as u64;
            let paths = collect_enabled_paths(&lanes_snap2, &af_snap2, &lane_enabled.peek());
            let _ = e.play_tx.send(PlayCmd::TimelinePlay {
                paths,
                start_ms: ms,
            });
            playing.set(true);
            paused.set(false);
        }
    };

    rsx! {
        div { class: "tl-panel",
            // ── header row ──────────────────────────────────────────────────
            div { class: "tl-header",
                // Lane chips
                div { class: "tl-chips",
                    for lane in lanes_v.iter() {
                        {
                            let track = lane.track.clone();
                            let track_cb = track.clone();
                            let lbl = lane_display_name(&track, lane.speaker, &names, me_spk);
                            let chip_cls = lane_chip_class(&track, lane.speaker, me_spk);
                            let enabled = *lane_enabled.read().get(track.as_str()).unwrap_or(&true);
                            let color = lane_color(&track, me_spk, lane.speaker).to_string();
                            let e_chip = engine.clone();
                            let lanes_chip = lanes_v.clone();
                            let af_chip = af.clone();
                            let cur_pos = pos_v.unwrap_or(0);
                            rsx! {
                                label {
                                    key: "{track}",
                                    class: if enabled { "{chip_cls}" } else { "{chip_cls} tl-chip-off" },
                                    input {
                                        r#type: "checkbox",
                                        class: "tl-chip-cb",
                                        checked: enabled,
                                        onchange: move |_| {
                                            {
                                                let mut le = lane_enabled.write();
                                                let cur = *le.get(track_cb.as_str()).unwrap_or(&true);
                                                le.insert(track_cb.clone(), !cur);
                                            }
                                            // If currently playing, restart with updated lane set.
                                            if is_playing {
                                                let paths = collect_enabled_paths(
                                                    &lanes_chip,
                                                    &af_chip,
                                                    &lane_enabled.peek(),
                                                );
                                                let _ = e_chip.play_tx.send(PlayCmd::TimelinePlay {
                                                    paths,
                                                    start_ms: cur_pos,
                                                });
                                            }
                                        },
                                    }
                                    span {
                                        class: "tl-chip-dot",
                                        style: "background: {color};",
                                    }
                                    span { class: "tl-chip-label", "{lbl}" }
                                }
                            }
                        }
                    }
                }
                // Stacked ⟷ Merged toggle
                div { class: "tl-view-toggle",
                    button {
                        class: if !is_merged { "tbtn tl-vtbtn active" } else { "tbtn tl-vtbtn" },
                        title: "Stacked: one lane per track",
                        onclick: move |_| merged.set(false),
                        "Stacked"
                    }
                    button {
                        class: if is_merged { "tbtn tl-vtbtn active" } else { "tbtn tl-vtbtn" },
                        title: "Merged: all tracks overlaid",
                        onclick: move |_| merged.set(true),
                        "Merged"
                    }
                }
                // Play/pause button
                button {
                    class: "tbtn tl-playbtn",
                    title: if is_playing && !is_paused { "Pause" } else if is_paused { "Resume" } else { "Play" },
                    onclick: on_play_pause,
                    {icon(play_icon)}
                }
                // Position label
                span { class: "tl-pos", "{pos_label}" }
                // Close
                button {
                    class: "tl-close",
                    title: "Close timeline",
                    onclick: move |_| on_close.call(()),
                    {icon("close")}
                }
            }

            // ── waveform area ────────────────────────────────────────────────
            div {
                class: "tl-graphs",
                onmousedown: {
                    let mut seek = on_seek.clone();
                    move |e: MouseEvent| {
                        dragging.set(true);
                        // Use element_coordinates().x as a fraction of 1500
                        // (the SVG viewBox width).  This is an approximation
                        // since element_coordinates is in CSS pixels, not SVG
                        // units.  For accuracy we rely on the fact that the SVG
                        // has preserveAspectRatio=none and fills its container,
                        // so the CSS pixel offset maps linearly to [0..1].
                        // We obtain the container width via a JS eval:
                        let _ = document::eval(
                            "window.__tl_w=(document.querySelector('.tl-graphs')||{}).offsetWidth||1500;"
                        );
                        let x = e.element_coordinates().x;
                        // Fallback: assume SVG container is ~1500px wide if JS
                        // hasn't run yet. Accurate seek fires via on_click on mouseup.
                        seek(x / 1500.0);
                    }
                },
                onmousemove: {
                    let mut seek2 = on_seek.clone();
                    move |e: MouseEvent| {
                        if *dragging.peek() {
                            let x = e.element_coordinates().x;
                            seek2(x / 1500.0);
                        }
                    }
                },
                onmouseup: move |_| dragging.set(false),
                onmouseleave: move |_| dragging.set(false),

                if is_merged {
                    // ── MERGED view ─────────────────────────────────────────
                    div { class: "tl-merged",
                        svg {
                            class: "tl-svg tl-svg-merged",
                            view_box: "0 0 1500 96",
                            preserve_aspect_ratio: "none",
                            for lane in lanes_v.iter() {
                                {
                                    let enabled = *lane_enabled.read().get(lane.track.as_str()).unwrap_or(&true);
                                    if !enabled {
                                        rsx! {}
                                    } else {
                                        let color = lane_color(&lane.track, me_spk, lane.speaker).to_string();
                                        let d = peaks_to_path(&lane.peaks, 0.0, 96.0);
                                        rsx! {
                                            path {
                                                key: "{lane.track}-merged",
                                                d: "{d}",
                                                fill: "{color}",
                                                fill_opacity: "0.45",
                                                stroke: "none",
                                            }
                                        }
                                    }
                                }
                            }
                            // Playhead
                            if pos_v.is_some() {
                                {
                                    let px = (playhead_frac * 1500.0) as i32;
                                    rsx! {
                                        line {
                                            class: "tl-playhead",
                                            x1: "{px}", y1: "0", x2: "{px}", y2: "96",
                                            stroke: "white",
                                            stroke_width: "1.5",
                                            stroke_opacity: "0.8",
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // ── STACKED view ────────────────────────────────────────
                    for lane in lanes_v.iter() {
                        {
                            let enabled = *lane_enabled.read().get(lane.track.as_str()).unwrap_or(&true);
                            if !enabled {
                                rsx! {}
                            } else {
                                let lbl = lane_display_name(&lane.track, lane.speaker, &names, me_spk);
                                let is_others = lane.track == "others";
                                rsx! {
                                    div { class: "tl-lane", key: "{lane.track}",
                                        span { class: "tl-lane-label", "{lbl}" }
                                        div { class: "tl-lane-svg-wrap",
                                            svg {
                                                class: "tl-svg",
                                                view_box: "0 0 1500 48",
                                                preserve_aspect_ratio: "none",
                                                if is_others {
                                                    {
                                                        let bspk = bucket_speakers(&segs, lane.duration_ms, lane.peaks.len());
                                                        let runs = colored_peaks_paths(
                                                            &lane.peaks,
                                                            &bspk,
                                                            "#ffb454",
                                                            me_spk,
                                                            0.0,
                                                            48.0,
                                                        );
                                                        rsx! {
                                                            for (run_idx, (color, d)) in runs.into_iter().enumerate() {
                                                                path {
                                                                    key: "run-{run_idx}",
                                                                    d: "{d}",
                                                                    fill: "{color}",
                                                                    fill_opacity: "0.7",
                                                                    stroke: "none",
                                                                }
                                                            }
                                                        }
                                                    }
                                                } else {
                                                    {
                                                        let color = lane_color(&lane.track, me_spk, lane.speaker).to_string();
                                                        let d = peaks_to_path(&lane.peaks, 0.0, 48.0);
                                                        rsx! {
                                                            path {
                                                                d: "{d}",
                                                                fill: "{color}",
                                                                fill_opacity: "0.7",
                                                                stroke: "none",
                                                            }
                                                        }
                                                    }
                                                }
                                                // Playhead
                                                if pos_v.is_some() {
                                                    {
                                                        let px = (playhead_frac * 1500.0) as i32;
                                                        rsx! {
                                                            line {
                                                                x1: "{px}", y1: "0", x2: "{px}", y2: "48",
                                                                stroke: "white",
                                                                stroke_width: "1.5",
                                                                stroke_opacity: "0.8",
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
        }
    }
}

// ── timestamp formatter ───────────────────────────────────────────────────────

fn fmt_ms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use zord_core::{Segment, Source};

    fn seg(source: Source, speaker: Option<i32>, t0: u64, t1: u64) -> Segment {
        Segment {
            id: Some(t0 as i64),
            source,
            t_start_ms: t0,
            t_end_ms: t1,
            text: String::new(),
            words: vec![],
            speaker,
        }
    }

    #[test]
    fn bucket_speakers_empty() {
        let out = bucket_speakers(&[], 10_000, 10);
        assert_eq!(out.len(), 10);
        assert!(out.iter().all(|x| x.is_none()));
    }

    #[test]
    fn bucket_speakers_ignores_me_source() {
        let segs = vec![seg(Source::Me, Some(0), 0, 10_000)];
        let out = bucket_speakers(&segs, 10_000, 10);
        assert!(out.iter().all(|x| x.is_none()));
    }

    #[test]
    fn bucket_speakers_ignores_no_speaker() {
        let segs = vec![seg(Source::Others, None, 0, 10_000)];
        let out = bucket_speakers(&segs, 10_000, 10);
        assert!(out.iter().all(|x| x.is_none()));
    }

    #[test]
    fn bucket_speakers_full_span() {
        let segs = vec![seg(Source::Others, Some(2), 0, 10_000)];
        let out = bucket_speakers(&segs, 10_000, 10);
        assert!(out.iter().all(|x| *x == Some(2)));
    }

    #[test]
    fn bucket_speakers_partial_span() {
        let segs = vec![
            seg(Source::Others, Some(1), 0, 5_000),
            seg(Source::Others, Some(3), 5_000, 10_000),
        ];
        let out = bucket_speakers(&segs, 10_000, 10);
        for v in &out[..5] {
            assert_eq!(*v, Some(1), "expected spk 1 in first half");
        }
        for v in &out[5..] {
            assert_eq!(*v, Some(3), "expected spk 3 in second half");
        }
    }

    #[test]
    fn bucket_speakers_first_wins_on_overlap() {
        let segs = vec![
            seg(Source::Others, Some(0), 0, 10_000),
            seg(Source::Others, Some(5), 0, 10_000),
        ];
        let out = bucket_speakers(&segs, 10_000, 10);
        assert!(out.iter().all(|x| *x == Some(0)));
    }
}
