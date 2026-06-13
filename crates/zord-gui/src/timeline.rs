//! Session timeline panel — Phase 42c/42d.
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
//! - [`untranscribed_buckets`] — pure function finding speech regions without
//!   any transcript coverage (unit-tested below).
//! - [`clipping_buckets`] — pure function finding amplitude-clipped buckets
//!   (unit-tested below).

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

// ── diagnostics pure functions ────────────────────────────────────────────────

/// Minimum run of consecutive speech-active buckets required to flag a region
/// as "untranscribed speech". Shorter runs (likely breath noise, plosives) are
/// suppressed. At ~2.4 s/bucket (1500 buckets/hour), 2 buckets ≈ ~5 s.
const MIN_SPEECH_RUN: usize = 2;

/// Build the `covered` boolean vector: bucket `k` is `true` when at least one
/// transcript segment of the right source covers it.
///
/// For the `me` lane only `Source::Me` segments count; for an `others` lane
/// any `Source::Others` segment counts; for a `spk-N` lane we require
/// `Source::Others` with `speaker == Some(N)`.
fn covered_buckets(
    segments: &[Segment],
    duration_ms: u64,
    n_buckets: usize,
    track: &str,
    speaker: Option<i32>,
) -> Vec<bool> {
    let mut out = vec![false; n_buckets];
    if duration_ms == 0 || n_buckets == 0 {
        return out;
    }
    let ms_per_bucket = duration_ms as f64 / n_buckets as f64;

    for seg in segments {
        // Filter by source/speaker for this lane.
        let matches = match track {
            "me" => seg.source == Source::Me,
            "others" => seg.source == Source::Others,
            _ => {
                // spk-N: Others + matching speaker index
                seg.source == Source::Others && seg.speaker == speaker
            }
        };
        if !matches {
            continue;
        }
        let start_b = (seg.t_start_ms as f64 / ms_per_bucket).floor() as usize;
        let end_b = ((seg.t_end_ms as f64 / ms_per_bucket).ceil() as usize).min(n_buckets);
        for slot in out.iter_mut().take(end_b).skip(start_b) {
            *slot = true;
        }
    }
    out
}

/// Find bucket indices that have speech energy but no transcript coverage,
/// subject to a minimum run-length guard ([`MIN_SPEECH_RUN`]) to suppress
/// breath-noise false positives. Returns the indices of flagged buckets.
///
/// `speech` — per-bucket speech activity flags from the peaks pass.
/// `covered` — per-bucket transcript coverage computed by [`covered_buckets`].
pub fn untranscribed_buckets(speech: &[bool], covered: &[bool]) -> Vec<usize> {
    assert_eq!(speech.len(), covered.len());
    let n = speech.len();
    let mut result = Vec::new();
    let mut i = 0;
    while i < n {
        if speech[i] && !covered[i] {
            // Start of an uncovered-speech run — measure its length.
            let run_start = i;
            while i < n && speech[i] && !covered[i] {
                i += 1;
            }
            let run_len = i - run_start;
            if run_len >= MIN_SPEECH_RUN {
                result.extend(run_start..run_start + run_len);
            }
        } else {
            i += 1;
        }
    }
    result
}

/// Amplitude-clipping threshold: peak ≥ this value flags the bucket as
/// potentially clipped.
const CLIP_THRESHOLD: f32 = 0.985;

/// Find bucket indices where the peak amplitude is at or above the clipping
/// threshold. Returns the indices of clipped buckets.
pub fn clipping_buckets(peaks: &[f32]) -> Vec<usize> {
    peaks
        .iter()
        .enumerate()
        .filter(|(_, &p)| p >= CLIP_THRESHOLD)
        .map(|(i, _)| i)
        .collect()
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

// ── silence skip helper ───────────────────────────────────────────────────────

/// Given a playhead position in ms and the lanes currently enabled, find the
/// end of the current silent run (if any). Returns `Some(end_ms)` when the
/// playhead is in a silent region lasting at least `min_silence_ms`; `None`
/// when there's speech at the current position.
///
/// "Silent at position" = none of the enabled lanes have a speech-active
/// bucket at the bucket covering `pos_ms`.
pub fn silence_skip_target(
    lanes: &[TimelineLane],
    lane_enabled: &HashMap<String, bool>,
    pos_ms: u64,
    duration_ms: u64,
    min_silence_ms: u64,
) -> Option<u64> {
    if duration_ms == 0 {
        return None;
    }
    let n = zord_audio::PEAK_BUCKETS;
    let ms_per_bucket = duration_ms as f64 / n as f64;
    let cur_bucket = ((pos_ms as f64 / ms_per_bucket).floor() as usize).min(n - 1);

    // Check if ANY enabled lane has speech at the current bucket.
    let has_speech_at = |b: usize| -> bool {
        lanes
            .iter()
            .filter(|l| *lane_enabled.get(l.track.as_str()).unwrap_or(&true))
            .any(|l| l.speech.get(b).copied().unwrap_or(false))
    };

    // Not in a silent region → don't skip.
    if has_speech_at(cur_bucket) {
        return None;
    }

    // Find the end of this silent run.
    let mut end_b = cur_bucket;
    while end_b < n && !has_speech_at(end_b) {
        end_b += 1;
    }

    let run_end_ms = ((end_b as f64) * ms_per_bucket) as u64;
    let run_len_ms = run_end_ms.saturating_sub(pos_ms);
    if run_len_ms < min_silence_ms {
        return None; // too short to skip
    }
    Some(run_end_ms.min(duration_ms))
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
    /// Phase 47: `(t_ms, phrase)` bookmark list for the current session.
    pub bookmarks: Signal<Vec<(u64, String)>>,
    /// Phase 49: sentiment moments for the current session (event + emotion
    /// markers). Empty in non-`sentiment` builds (never populated).
    pub moments: Signal<Vec<zord_core::Moment>>,
}

/// The collapsible bottom session timeline panel.
///
/// Mounts under the transcript inside `View::Session`.  Not shown in
/// `View::Live` — peaks require finished audio files.
#[component]
pub fn TimelinePanel(props: TimelinePanelProps) -> Element {
    let TimelinePanelProps {
        session_id,
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
        bookmarks,
        moments,
    } = props;

    // Local state: are we playing or paused?
    let mut playing = use_signal(|| false);
    let mut paused = use_signal(|| false);
    // Drag-scrub: set while the mouse button is held on the graph area.
    let mut dragging = use_signal(|| false);
    // Shift-drag: is the user creating a range selection?
    let mut shift_dragging = use_signal(|| false);
    // Range selection: start/end in ms (None = no selection).
    let mut sel_start_ms: Signal<Option<u64>> = use_signal(|| None);
    let mut sel_end_ms: Signal<Option<u64>> = use_signal(|| None);
    // Measured CSS-pixel width of the graph container (set onmounted) — the
    // divisor that turns a click's element x into a 0..1 seek fraction.
    let mut container_w = use_signal(|| 1500.0_f64);
    // Current playback speed cycle: 1.0 → 1.5 → 2.0 → 1.0.
    let mut speed = use_signal(|| 1.0f32);
    // Silence skip toggle.
    let mut skip_silence = use_signal(|| false);
    // Loop-guard for silence skip: the target ms of the last skip we fired.
    // Don't re-fire the same seek or go backwards.
    let mut last_skip_target: Signal<Option<u64>> = use_signal(|| None);

    // Sync `playing` / `paused` from the pos tick: if pos becomes None while
    // we thought we were playing, the track ended.
    use_effect(move || {
        let p = *pos.read();
        if p.is_none() && *playing.peek() && !*paused.peek() {
            playing.set(false);
            // Clear last skip target when playback stops.
            last_skip_target.set(None);
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

    // Silence skip: GUI-driven — engine stays dumb; we just fire a seek.
    {
        let engine_skip = engine.clone();
        use_effect(move || {
            let Some(ms) = *pos.read() else { return };
            if !*playing.peek() || *paused.peek() {
                return;
            }
            if !*skip_silence.read() {
                return;
            }
            let lanes_v = lanes.read().clone();
            let le = lane_enabled.read().clone();
            let max_dur: u64 = lanes_v
                .iter()
                .filter(|l| *le.get(l.track.as_str()).unwrap_or(&true))
                .map(|l| l.duration_ms)
                .max()
                .unwrap_or(0);
            // Silence runs shorter than 2 s don't get skipped.
            let Some(target) = silence_skip_target(&lanes_v, &le, ms, max_dur, 2_000) else {
                return;
            };
            // Loop guard: only fire when the target is meaningfully ahead and
            // we haven't already fired this exact skip (avoids seek storms).
            let already_fired = (*last_skip_target.peek())
                .map(|t| t >= target)
                .unwrap_or(false);
            let advance = target.saturating_sub(ms);
            if advance < 500 || already_fired {
                return;
            }
            last_skip_target.set(Some(target));
            let _ = engine_skip
                .play_tx
                .send(PlayCmd::TimelineSeek { start_ms: target });
        });
    }

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
    let speed_v = *speed.read();
    let is_skip = *skip_silence.read();
    let sel_start_v = *sel_start_ms.read();
    let sel_end_v = *sel_end_ms.read();
    let sid = session_id.clone();

    // Max duration across enabled lanes.
    let max_dur: u64 = lanes_v
        .iter()
        .filter(|l| *lane_enabled.read().get(l.track.as_str()).unwrap_or(&true))
        .map(|l| l.duration_ms)
        .max()
        .unwrap_or(0);

    // Phase 47: snapshot bookmarks for rendering.
    let bkmarks_v = bookmarks.read().clone();

    // Phase 49: snapshot moments for the moments lane (only ever populated in
    // `sentiment` builds; an empty vec means the lane is not rendered).
    let moments_v = moments.read().clone();

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

    // ── diagnostics markers ───────────────────────────────────────────────────
    // Precompute per-lane untranscribed + clipping markers (cheap — pure fn).
    let has_any_gap = lanes_v.iter().any(|lane| {
        let cov = covered_buckets(
            &segs,
            lane.duration_ms,
            lane.peaks.len(),
            &lane.track,
            lane.speaker,
        );
        !untranscribed_buckets(&lane.speech, &cov).is_empty()
    });
    let has_any_clip = lanes_v
        .iter()
        .any(|lane| !clipping_buckets(&lane.peaks).is_empty());

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

    // ── selection overlay fractions ───────────────────────────────────────────
    let sel_frac: Option<(f64, f64)> = if let (Some(s), Some(e)) = (sel_start_v, sel_end_v) {
        if max_dur > 0 && e > s {
            Some((
                (s as f64 / max_dur as f64).clamp(0.0, 1.0),
                (e as f64 / max_dur as f64).clamp(0.0, 1.0),
            ))
        } else {
            None
        }
    } else {
        None
    };

    // ── play/pause button icon ────────────────────────────────────────────────
    let play_icon = if is_playing && !is_paused {
        "pause"
    } else {
        "play"
    };

    // ── speed label ───────────────────────────────────────────────────────────
    let speed_label = if (speed_v - 1.0).abs() < 0.01 {
        "1×"
    } else if (speed_v - 1.5).abs() < 0.01 {
        "1.5×"
    } else {
        "2×"
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

    // seek (click or drag on graph area) — also clears any selection that was
    // started by a plain (non-shift) drag.
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
            last_skip_target.set(None);
        }
    };

    // speed cycle: 1.0 → 1.5 → 2.0 → 1.0
    let on_speed = {
        let e = engine.clone();
        move |_: MouseEvent| {
            let next = if speed_v < 1.25 {
                1.5f32
            } else if speed_v < 1.75 {
                2.0f32
            } else {
                1.0f32
            };
            speed.set(next);
            let _ = e.play_tx.send(PlayCmd::TimelineSpeed(next));
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
                                            // If currently playing, restart with the updated lane
                                            // set. Read the live state — not a render-time snapshot.
                                            if *playing.peek() {
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
                // Speed cycle button (1× / 1.5× / 2×)
                button {
                    class: "tbtn tl-speedbtn",
                    title: "Cycle playback speed (affects pitch — rodio set_speed)",
                    onclick: on_speed,
                    "{speed_label}"
                }
                // Silence-skip toggle
                button {
                    class: if is_skip { "tbtn tl-skipbtn active" } else { "tbtn tl-skipbtn" },
                    title: "Skip silence: jump over silent regions > 2 s",
                    onclick: move |_| {
                        let cur = *skip_silence.peek();
                        skip_silence.set(!cur);
                    },
                    "Skip silence"
                }
                // Position label
                span { class: "tl-pos", "{pos_label}" }
                // Diagnostics legend (only shown when markers exist)
                if has_any_gap || has_any_clip {
                    span {
                        class: "tl-diag-legend",
                        title: "Diagnostic markers present in this timeline",
                        if has_any_gap { "▮ untranscribed speech" }
                        if has_any_gap && has_any_clip { " · " }
                        if has_any_clip { "▴ clipping" }
                    }
                }
                // Close
                button {
                    class: "tl-close",
                    title: "Close timeline",
                    onclick: move |_| on_close.call(()),
                    {icon("close")}
                }
            }

            // ── selection action chips ───────────────────────────────────────
            if let Some((s_frac, _e_frac)) = sel_frac {
                {
                    let chip_left_pct = (s_frac * 100.0).min(90.0);
                    let sid_export = sid.clone();
                    let sid_retranscribe = sid.clone();
                    let af_chip = af.clone();
                    let lanes_chip = lanes_v.clone();
                    let le_chip = lane_enabled.read().clone();
                    let e_export = engine.clone();
                    let e_retranscribe = engine.clone();
                    let sel_s = sel_start_v.unwrap_or(0);
                    let sel_e = sel_end_v.unwrap_or(0);
                    rsx! {
                        div {
                            class: "tl-sel-actions",
                            style: "left: {chip_left_pct:.1}%;",
                            button {
                                class: "tbtn tl-sel-btn",
                                title: "Export this range as a WAV clip to the exports folder",
                                onclick: move |_| {
                                    let paths = collect_enabled_paths(&lanes_chip, &af_chip, &le_chip);
                                    let _ = e_export.db_tx.send(crate::engine::DbCmd::ExportClip {
                                        id: sid_export.clone(),
                                        paths,
                                        start_ms: sel_s,
                                        end_ms: sel_e,
                                    });
                                },
                                "Export clip"
                            }
                            button {
                                class: "tbtn tl-sel-btn",
                                title: "Re-run transcription for this time range only",
                                onclick: move |_| {
                                    let _ = e_retranscribe.db_tx.send(crate::engine::DbCmd::RetranscribeRange {
                                        id: sid_retranscribe.clone(),
                                        start_ms: sel_s,
                                        end_ms: sel_e,
                                    });
                                },
                                "Re-transcribe"
                            }
                            button {
                                class: "tbtn tl-sel-btn tl-sel-dismiss",
                                title: "Clear selection",
                                onclick: move |_| {
                                    sel_start_ms.set(None);
                                    sel_end_ms.set(None);
                                },
                                "×"
                            }
                        }
                    }
                }
            }

            // ── waveform area ────────────────────────────────────────────────
            div {
                class: "tl-graphs",
                // Measure the graph container once on mount so click/drag x
                // (CSS pixels) maps to an accurate 0..1 fraction. Known
                // limitation: a window resize while the panel stays open isn't
                // re-measured until the panel is reopened.
                onmounted: move |data: Event<MountedData>| async move {
                    if let Ok(rect) = data.get_client_rect().await {
                        container_w.set(rect.size.width.max(1.0));
                    }
                },
                onmousedown: {
                    let mut seek = on_seek.clone();
                    move |e: MouseEvent| {
                        let x = e.element_coordinates().x;
                        let frac = x / *container_w.peek();
                        if e.modifiers().shift() {
                            // Shift-drag: start a range selection.
                            shift_dragging.set(true);
                            let ms = (frac.clamp(0.0, 1.0) * max_dur as f64) as u64;
                            sel_start_ms.set(Some(ms));
                            sel_end_ms.set(Some(ms));
                        } else {
                            dragging.set(true);
                            sel_start_ms.set(None);
                            sel_end_ms.set(None);
                            seek(frac);
                        }
                    }
                },
                onmousemove: {
                    let mut seek2 = on_seek.clone();
                    move |e: MouseEvent| {
                        let x = e.element_coordinates().x;
                        let frac = x / *container_w.peek();
                        if *shift_dragging.peek() {
                            let ms = (frac.clamp(0.0, 1.0) * max_dur as f64) as u64;
                            // Keep start ≤ end. Read sel_start once, drop the borrow,
                            // then mutate.
                            let maybe_s = *sel_start_ms.peek();
                            if let Some(s) = maybe_s {
                                if ms >= s {
                                    sel_end_ms.set(Some(ms));
                                } else {
                                    sel_end_ms.set(Some(s));
                                    sel_start_ms.set(Some(ms));
                                }
                            }
                        } else if *dragging.peek() {
                            seek2(frac);
                        }
                    }
                },
                onmouseup: move |_| {
                    dragging.set(false);
                    shift_dragging.set(false);
                },
                onmouseleave: move |_| {
                    dragging.set(false);
                    shift_dragging.set(false);
                },

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
                            // Selection overlay
                            if let Some((sf, ef)) = sel_frac {
                                {
                                    let sx = (sf * 1500.0) as i32;
                                    let sw = ((ef - sf) * 1500.0).max(1.0) as i32;
                                    rsx! {
                                        rect {
                                            class: "tl-sel-overlay",
                                            x: "{sx}", y: "0",
                                            width: "{sw}", height: "96",
                                            fill: "rgba(255,255,255,0.18)",
                                            stroke: "rgba(255,255,255,0.5)",
                                            stroke_width: "1",
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
                                let cov = covered_buckets(&segs, lane.duration_ms, lane.peaks.len(), &lane.track, lane.speaker);
                                let gap_buckets = untranscribed_buckets(&lane.speech, &cov);
                                let clip_buckets = clipping_buckets(&lane.peaks);
                                let n_buckets = lane.peaks.len();
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
                                                // Untranscribed-speech markers (top edge, class tl-gap)
                                                // Rendered as thin red ticks at y=0. SVG tooltip via
                                                // a nested <title> child is the correct SVG pattern
                                                // (the `title` HTML attribute would need a workaround
                                                // in Dioxus RSX; the SVG <title> child is used instead
                                                // by grouping in a <g> with aria-label).
                                                for b in gap_buckets.iter() {
                                                    {
                                                        let x = *b as i32 * 1500 / n_buckets as i32;
                                                        rsx! {
                                                            rect {
                                                                key: "gap-{b}",
                                                                class: "tl-gap",
                                                                x: "{x}", y: "0",
                                                                width: "2", height: "4",
                                                                fill: "var(--danger, #ff5555)",
                                                                stroke: "none",
                                                            }
                                                        }
                                                    }
                                                }
                                                // Clipping indicators (bottom edge, class tl-clip).
                                                // Small upward triangle: points at (x,48),(x+3,44),(x-3,44).
                                                for b in clip_buckets.iter() {
                                                    {
                                                        let x = *b as i32 * 1500 / n_buckets as i32;
                                                        let pts = format!("{x},48 {},44 {},44", x + 3, x - 3);
                                                        rsx! {
                                                            polygon {
                                                                key: "clip-{b}",
                                                                class: "tl-clip",
                                                                points: "{pts}",
                                                                fill: "var(--danger, #ff5555)",
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
                                                // Selection overlay
                                                if let Some((sf, ef)) = sel_frac {
                                                    {
                                                        let sx = (sf * 1500.0) as i32;
                                                        let sw = ((ef - sf) * 1500.0).max(1.0) as i32;
                                                        rsx! {
                                                            rect {
                                                                class: "tl-sel-overlay",
                                                                x: "{sx}", y: "0",
                                                                width: "{sw}", height: "48",
                                                                fill: "rgba(255,255,255,0.18)",
                                                                stroke: "rgba(255,255,255,0.5)",
                                                                stroke_width: "1",
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

            // ── Phase 47: bookmark tick lane ─────────────────────────────────
            // Only rendered when there are bookmarks; a thin row of orange
            // diamond ticks above a baseline.  Clicking a tick seeks playback
            // to that position (same as a waveform click).
            if !bkmarks_v.is_empty() && max_dur > 0 {
                {
                    let bkmarks_render = bkmarks_v.clone();
                    rsx! {
                        div {
                            class: "tl-bookmark-lane",
                            title: "Bookmark lane — click a marker to seek",
                            svg {
                                class: "tl-svg tl-svg-bookmarks",
                                view_box: "0 0 1500 16",
                                preserve_aspect_ratio: "none",
                                // baseline
                                line {
                                    x1: "0", y1: "15", x2: "1500", y2: "15",
                                    stroke: "var(--border, #3a3a4a)",
                                    stroke_width: "1",
                                }
                                for (t_ms, phrase) in bkmarks_render.iter() {
                                    {
                                        let t = *t_ms;
                                        let frac = t as f64 / max_dur as f64;
                                        let px = (frac.clamp(0.0, 1.0) * 1500.0) as i32;
                                        // Diamond: center (px, 8), half-width 4, half-height 6
                                        let pts = format!(
                                            "{px},2 {},8 {px},14 {},8",
                                            px + 5, px - 5
                                        );
                                        let tip = format!(
                                            "Bookmark — {} ({})",
                                            fmt_ms(t),
                                            phrase
                                        );
                                        let mut seek_bkm = on_seek.clone();
                                        rsx! {
                                            polygon {
                                                key: "bkm-{t}-{phrase}",
                                                class: "tl-bookmark-tick",
                                                points: "{pts}",
                                                fill: "#ffb454",
                                                stroke: "none",
                                                cursor: "pointer",
                                                onclick: move |e: MouseEvent| {
                                                    e.stop_propagation();
                                                    seek_bkm(t as f64 / max_dur as f64);
                                                },
                                                title { "{tip}" }
                                            }
                                        }
                                    }
                                }
                                // Playhead across the bookmark lane
                                if pos_v.is_some() {
                                    {
                                        let px = (playhead_frac * 1500.0) as i32;
                                        rsx! {
                                            line {
                                                x1: "{px}", y1: "0", x2: "{px}", y2: "16",
                                                stroke: "white",
                                                stroke_width: "1.5",
                                                stroke_opacity: "0.6",
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Phase 49: sentiment moments lane ─────────────────────────────
            // A thin row of emoji/colored ticks: warm amber for audio events
            // (laughter/applause/…), cool teal for persistent-emotion marks.
            // Clicking a tick seeks playback to that moment (same as a waveform
            // click). Only rendered when moments exist (i.e. `sentiment` builds
            // that have analysed the session).
            if !moments_v.is_empty() && max_dur > 0 {
                {
                    let moments_render = moments_v.clone();
                    rsx! {
                        div {
                            class: "tl-moment-lane",
                            title: "Moments — click a marker to seek",
                            svg {
                                class: "tl-svg tl-svg-moments",
                                view_box: "0 0 1500 18",
                                preserve_aspect_ratio: "none",
                                line {
                                    x1: "0", y1: "17", x2: "1500", y2: "17",
                                    stroke: "var(--border, #3a3a4a)",
                                    stroke_width: "1",
                                }
                                for (i, m) in moments_render.iter().enumerate() {
                                    {
                                        let t = m.t_ms;
                                        let frac = t as f64 / max_dur as f64;
                                        let px = frac.clamp(0.0, 1.0) * 1500.0;
                                        let (glyph, color, label) = moment_style(&m.kind);
                                        let tip = format!("{label} — {} ({:.0}%)", fmt_ms(t), m.score * 100.0);
                                        let mut seek_m = on_seek.clone();
                                        rsx! {
                                            text {
                                                key: "moment-{i}-{t}",
                                                class: "tl-moment-tick",
                                                x: "{px}",
                                                y: "13",
                                                font_size: "12",
                                                text_anchor: "middle",
                                                fill: "{color}",
                                                cursor: "pointer",
                                                onclick: move |e: MouseEvent| {
                                                    e.stop_propagation();
                                                    seek_m(t as f64 / max_dur as f64);
                                                },
                                                "{glyph}"
                                                title { "{tip}" }
                                            }
                                        }
                                    }
                                }
                                if pos_v.is_some() {
                                    {
                                        let px = (playhead_frac * 1500.0) as i32;
                                        rsx! {
                                            line {
                                                x1: "{px}", y1: "0", x2: "{px}", y2: "18",
                                                stroke: "white",
                                                stroke_width: "1.5",
                                                stroke_opacity: "0.6",
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

/// Phase 49: glyph + color + human label for a moment kind. Event kinds get a
/// distinct emoji + warm color; emotion kinds (`emotion:<label>`) share a
/// cooler color and a face glyph so the two flavours read differently in the
/// moments lane. Returns `(glyph, fill_color, human_label)`.
fn moment_style(kind: &str) -> (&'static str, &'static str, String) {
    if let Some(label) = kind.strip_prefix("emotion:") {
        // Emotion ticks: cool teal, a face glyph per label.
        let glyph = match label {
            "happy" => "🙂",
            "sad" => "🙁",
            "angry" => "😠",
            "fear" => "😨",
            "disgust" => "😖",
            _ => "💬",
        };
        return (glyph, "#4ec9b0", format!("Mood: {label}"));
    }
    // Audio-event ticks: warm amber, an emoji per event class.
    let (glyph, label) = match kind {
        "laughter" => ("😂", "Laughter"),
        "applause" => ("👏", "Applause"),
        "cheering" => ("🎉", "Cheering"),
        "crying" => ("😢", "Crying"),
        "cough" => ("🤧", "Cough"),
        "sneeze" => ("🤧", "Sneeze"),
        _ => ("•", "Event"),
    };
    (glyph, "#f4a259", label.to_string())
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

    // ── untranscribed_buckets tests ───────────────────────────────────────────

    #[test]
    fn untranscribed_all_covered() {
        let speech = vec![true; 10];
        let covered = vec![true; 10];
        assert!(untranscribed_buckets(&speech, &covered).is_empty());
    }

    #[test]
    fn untranscribed_no_speech() {
        let speech = vec![false; 10];
        let covered = vec![false; 10];
        assert!(untranscribed_buckets(&speech, &covered).is_empty());
    }

    #[test]
    fn untranscribed_run_too_short_suppressed() {
        // Single uncovered speech bucket — below MIN_SPEECH_RUN (2) → suppressed.
        let mut speech = vec![false; 10];
        let mut covered = vec![true; 10];
        speech[5] = true;
        covered[5] = false;
        assert!(
            untranscribed_buckets(&speech, &covered).is_empty(),
            "single-bucket run should be suppressed"
        );
    }

    #[test]
    fn untranscribed_run_long_enough_flagged() {
        // Two consecutive uncovered speech buckets → flagged (meets MIN_SPEECH_RUN).
        let mut speech = vec![false; 10];
        let mut covered = vec![true; 10];
        speech[3] = true;
        speech[4] = true;
        covered[3] = false;
        covered[4] = false;
        let out = untranscribed_buckets(&speech, &covered);
        assert_eq!(out, vec![3, 4]);
    }

    // ── clipping_buckets tests ────────────────────────────────────────────────

    #[test]
    fn clipping_none() {
        let peaks = vec![0.5; 10];
        assert!(clipping_buckets(&peaks).is_empty());
    }

    #[test]
    fn clipping_detects_above_threshold() {
        let mut peaks = vec![0.5f32; 10];
        peaks[2] = 0.99;
        peaks[7] = 0.985;
        let clips = clipping_buckets(&peaks);
        assert!(clips.contains(&2));
        assert!(clips.contains(&7));
        assert_eq!(clips.len(), 2);
    }

    #[test]
    fn clipping_below_threshold_not_flagged() {
        let mut peaks = vec![0.5f32; 10];
        peaks[4] = 0.984; // just below threshold
        assert!(clipping_buckets(&peaks).is_empty());
    }

    // ── delete_segments_in_range semantics (pure logic test) ─────────────────

    /// Verify the range-delete semantics: only segments whose t_start_ms is
    /// within [start, end) should be deleted. Segments that straddle the
    /// boundary (start before, end after) are left intact.
    #[test]
    fn range_delete_semantics_description() {
        // This is a documentation test — the actual SQL is tested in zord-store.
        // A segment starting AT start_ms IS deleted (inclusive lower bound).
        // A segment starting AT end_ms is NOT deleted (exclusive upper bound).
        let segments = [
            seg(Source::Me, None, 0, 5_000),       // starts before range → kept
            seg(Source::Me, None, 5_000, 8_000),   // starts AT range start → deleted
            seg(Source::Me, None, 7_000, 9_000),   // starts inside range → deleted
            seg(Source::Me, None, 10_000, 12_000), // starts AT range end → kept
            seg(Source::Me, None, 11_000, 14_000), // starts after range end → kept
        ];
        let (start, end) = (5_000u64, 10_000u64);
        let retained: Vec<_> = segments
            .iter()
            .filter(|s| !(s.t_start_ms >= start && s.t_start_ms < end))
            .collect();
        assert_eq!(
            retained.len(),
            3,
            "expected 3 segments retained, got {}",
            retained.len()
        );
    }
}
