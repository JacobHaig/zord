//! Shared types for the Zord audio transcription app.
//!
//! These types are deliberately dependency-light so every other crate can
//! depend on `zord-core` without pulling in audio/ML/storage machinery.

use serde::{Deserialize, Serialize};

/// Sample rate that whisper.cpp requires for all input audio.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// Distribution channel baked in at build time via the `ZORD_CHANNEL` env var
/// (Phase 34): `github` (self-updating), `steam` / `msstore` / `macappstore`
/// (the store owns updates), or `dev` (local builds; behaves like `github` so
/// the update path is testable). Stores forbid self-updating binaries, so all
/// update machinery keys off this.
pub const DIST_CHANNEL: &str = match option_env!("ZORD_CHANNEL") {
    Some(c) => c,
    None => "dev",
};

/// Is `latest` a strictly newer version than `current`? Accepts `v`-prefixed
/// and bare dotted-numeric versions ("v0.3.1", "0.2.18"); non-numeric parts
/// end the comparison (treated as equal from there on).
pub fn is_newer_version(current: &str, latest: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches(['v', 'V'])
            .split('.')
            .map_while(|p| p.parse::<u64>().ok())
            .collect()
    }
    let (cur, new) = (parts(current), parts(latest));
    if new.is_empty() {
        return false; // unparseable tag — never nag
    }
    for i in 0..cur.len().max(new.len()) {
        let c = cur.get(i).copied().unwrap_or(0);
        let n = new.get(i).copied().unwrap_or(0);
        if n != c {
            return n > c;
        }
    }
    false
}

/// Which side of the conversation a segment came from.
///
/// We separate audio at the *capture* layer (microphone vs. system loopback)
/// rather than using ML speaker diarization, so the source is always known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// The local user's microphone ("Me").
    Me,
    /// Desktop / system loopback audio ("Others" — Teams, Zoom, browser, etc.).
    Others,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Me => "me",
            Source::Others => "others",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Me => "Me",
            Source::Others => "Others",
        }
    }
}

/// Native configuration reported by an audio capture device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A single word with its timing, relative to the start of the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub t_start_ms: u64,
    pub t_end_ms: u64,
}

/// A transcribed utterance segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Database row id, populated on read. `None` for freshly-built segments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    pub source: Source,
    /// Milliseconds from session start.
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
    /// Diarized speaker index within the "Others" channel (0-based), if known.
    /// `None` for "Me" or before diarization has run. Set by Phase 16 diarization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speaker: Option<i32>,
}

impl Segment {
    /// Human label for this segment's speaker: "Me" for the mic channel, or
    /// "Speaker N" (1-based) for a diarized "Others" segment, falling back to
    /// "Others" when no speaker has been assigned. `names` optionally maps a
    /// 0-based speaker index to a custom name (e.g. "Alex" — integration
    /// sessions fill these with platform usernames, the app user included).
    pub fn speaker_label(&self, names: &std::collections::HashMap<i32, String>) -> String {
        match self.source {
            Source::Me => "Me".to_string(),
            Source::Others => match self.speaker {
                Some(idx) => names
                    .get(&idx)
                    .cloned()
                    .unwrap_or_else(|| format!("Speaker {}", idx + 1)),
                None => "Others".to_string(),
            },
        }
    }
}

/// A sentiment "moment" on the session timeline (Phase 49): an audio-prosody
/// marker derived from on-device ONNX models, attributed to a speaker track.
///
/// Two flavours share this shape:
/// - **Audio events** (YAMNet): `kind` is one of `laughter`, `applause`,
///   `crying`, `cough`, `sneeze`, `cheering`. Near-unambiguous → always shown.
/// - **Emotion** (wav2vec2 SER): `kind` is `emotion:<label>` (e.g.
///   `emotion:happy`), emitted only when a strong non-neutral label persists
///   across several consecutive utterances (the conservative-rendering rule).
///
/// `t_ms` is milliseconds from session start; `speaker` is the diarized /
/// integration speaker index of the track it came from (`me` and `others` map
/// to fixed sentinels — see the engine producer). `score` is the model's
/// confidence (0..1) at the marked frame/utterance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Moment {
    /// Milliseconds from session start.
    pub t_ms: u64,
    /// Marker kind: an event class (`laughter`, `applause`, …) or
    /// `emotion:<label>`.
    pub kind: String,
    /// Speaker index of the source track. `me`/`others` tracks use the
    /// sentinels in [`Moment::SPEAKER_ME`] / [`Moment::SPEAKER_OTHERS`].
    pub speaker: i32,
    /// Model confidence at the marked frame/utterance (0..1).
    pub score: f32,
}

impl Moment {
    /// Speaker sentinel for the local user's `me` track (no diarized index).
    pub const SPEAKER_ME: i32 = -1;
    /// Speaker sentinel for the undiarized `others` track.
    pub const SPEAKER_OTHERS: i32 = -2;

    /// Prefix that marks an emotion (vs. audio-event) moment.
    pub const EMOTION_PREFIX: &'static str = "emotion:";

    /// The emotion label if this is an `emotion:<label>` moment, else `None`.
    pub fn emotion_label(&self) -> Option<&str> {
        self.kind.strip_prefix(Self::EMOTION_PREFIX)
    }

    /// Whether this is an emotion moment (`emotion:*`) rather than an audio event.
    pub fn is_emotion(&self) -> bool {
        self.kind.starts_with(Self::EMOTION_PREFIX)
    }
}

/// A recording session (one "call" or capture run).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Unix epoch milliseconds.
    pub started_at: u64,
    pub ended_at: Option<u64>,
    pub title: Option<String>,
    /// Path to retained audio, if kept.
    pub audio_path: Option<String>,
    /// Which whisper model produced this transcript.
    pub model: String,
    /// When this session was folded into the living overview document
    /// (Phase 39), epoch ms. `None` = not folded yet (fold-all retries it).
    #[serde(default)]
    pub overview_folded_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Phase 26: the rolling project ledger.
//
// The Overview is a durable set of `Project`s, each holding a running list of
// `ProjectItem`s (action items / decisions / open questions). Each new meeting
// is folded in as a delta: items get added, transitioned, or marked done. These
// types are the shared shape the store, the merge engine, and the GUI agree on.
// ---------------------------------------------------------------------------

/// Lifecycle of a project in the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectStatus {
    /// Actively tracked (shown first in the Overview).
    Active,
    /// Wound down / parked; hidden by default but kept for history.
    Archived,
}

impl ProjectStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProjectStatus::Active => "active",
            ProjectStatus::Archived => "archived",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "archived" => ProjectStatus::Archived,
            _ => ProjectStatus::Active,
        }
    }
}

/// What kind of ledger entry a `ProjectItem` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    /// Something to be done (has an owner + a completion state).
    Action,
    /// An unresolved question raised in a meeting.
    Question,
    /// A decision the group made (kept as a record; usually `Done`).
    Decision,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Action => "action",
            ItemKind::Question => "question",
            ItemKind::Decision => "decision",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "question" => ItemKind::Question,
            "decision" => ItemKind::Decision,
            _ => ItemKind::Action,
        }
    }
}

/// Lifecycle of a `ProjectItem`. `Done` items are retained for history and
/// shown only on demand; the rest are "active".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Open,
    Blocked,
    Waiting,
    Done,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Open => "open",
            ItemStatus::Blocked => "blocked",
            ItemStatus::Waiting => "waiting",
            ItemStatus::Done => "done",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "blocked" => ItemStatus::Blocked,
            "waiting" => ItemStatus::Waiting,
            "done" => ItemStatus::Done,
            _ => ItemStatus::Open,
        }
    }
    /// `Done` is history; everything else is currently active.
    pub fn is_active(self) -> bool {
        self != ItemStatus::Done
    }
}

/// A tracked project in the Overview ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub status: ProjectStatus,
    /// Short running description of where the project stands (LLM-maintained,
    /// user-editable).
    pub description: Option<String>,
    /// Unix epoch ms.
    pub created_at: u64,
    pub updated_at: u64,
    /// When a meeting last touched this project (drives Overview ordering).
    pub last_activity_at: u64,
}

/// One entry under a project: an action item, open question, or decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectItem {
    pub id: String,
    pub project_id: String,
    pub kind: ItemKind,
    pub text: String,
    /// Who owns it (for actions), if attributed.
    pub owner: Option<String>,
    pub status: ItemStatus,
    /// Session that first introduced this item.
    pub created_session: Option<String>,
    /// Session that last changed it.
    pub updated_session: Option<String>,
    /// Session in which it was marked done (provenance for "why is this done?").
    pub completed_session: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    /// Hand-edited by the user — protected from being overwritten by later
    /// automatic folds (only an explicit rebuild-from-history clears it).
    pub manual: bool,
}

// ---------------------------------------------------------------------------
// Phase 46 — Conversation analytics ("Meeting DNA")
//
// Every metric is a pure function over the `Segment` slice Zord already has.
// No LLM, no external data — fast, exact, unit-tested.
// ---------------------------------------------------------------------------

/// Per-speaker analytics for one session.
///
/// Every field is `#[serde(default)]` so stored `session_stats` JSON rows
/// written by older builds keep deserializing after new fields are added
/// (Phase 48 reads these rows directly).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpeakerStats {
    /// Stable key used as a map key and by Phase 48 profiles.
    /// `"me"` for `Source::Me`; `"spk-N"` for `Source::Others + Some(N)`;
    /// `"others"` for un-diarized `Source::Others + None`.
    #[serde(default)]
    pub key: String,
    /// Raw speaker index (meaningful for spk-N rows; `None` for "me" and
    /// the un-diarized "others" bucket).
    #[serde(default)]
    pub speaker: Option<i32>,
    /// `true` when this row is the app user's perspective (either `Source::Me`
    /// or an integration session where `speaker == me_speaker`).
    #[serde(default)]
    pub is_me: bool,
    /// Total speaking time in milliseconds (sum of segment durations).
    #[serde(default)]
    pub talk_ms: u64,
    /// Fraction of total speech time: `talk_ms / total_talk_ms` (0.0 when no
    /// speech at all). Range [0, 1].
    #[serde(default)]
    pub talk_share: f32,
    /// Total word count (whitespace-split tokens in `Segment::text`).
    #[serde(default)]
    pub words: u32,
    /// Speaking rate in words per minute. `0.0` when `talk_ms` is zero.
    #[serde(default)]
    pub wpm: f32,
    /// Number of segments whose trimmed text ends with `'?'`.
    #[serde(default)]
    pub questions: u32,
    /// Total number of segments attributed to this speaker.
    #[serde(default)]
    pub lines: u32,
    /// Duration in ms of the longest unbroken speech run (same-speaker
    /// consecutive segments with inter-segment gaps < 2 000 ms are merged into
    /// one "monologue run" for this metric).
    #[serde(default)]
    pub longest_monologue_ms: u64,
    /// How many times this speaker started a segment while a *different*
    /// speaker's segment was still nominally in-progress (i.e. speaker B's
    /// `t_start_ms` strictly inside another speaker's `[t_start_ms, t_end_ms)`).
    #[serde(default)]
    pub interruptions_made: u32,
    /// Total milliseconds during which this speaker's segments overlapped with
    /// a different speaker's segments (attributed to both parties).
    #[serde(default)]
    pub talk_over_ms: u64,
}

/// Full per-session conversation metrics, computed purely from segments.
/// Serialized as JSON into `session_stats.json` in zord-store (Phase 46).
///
/// Every field is `#[serde(default)]` so stored rows from older builds keep
/// deserializing after the schema grows (Phase 48 reads them directly).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionStats {
    /// Per-speaker rows, sorted by `talk_share` descending before delivery.
    #[serde(default)]
    pub speakers: Vec<SpeakerStats>,
    /// Wall-clock meeting length in ms (`ended_at - started_at`). Zero when
    /// the session hasn't ended yet (compute is deferred until then).
    #[serde(default)]
    pub meeting_ms: u64,
    /// Union of all segment spans in ms, stored UNCLAMPED: it can exceed
    /// `meeting_ms` when segments outlast `ended_at` (clock skew, padded
    /// tails). Do NOT compute `meeting_ms - speech_ms` without saturating;
    /// use [`SessionStats::silence_ratio`] for the clamped silence fraction.
    #[serde(default)]
    pub speech_ms: u64,
    /// `1 - min(speech_ms, meeting_ms) / meeting_ms`; how quiet this meeting
    /// was (speech_ms is clamped here, unlike the raw field).
    /// Zero when `meeting_ms` is zero.
    #[serde(default)]
    pub silence_ratio: f32,
}

/// Compute [`SessionStats`] from a transcript slice, purely.
///
/// # Parameters
/// - `segments` — the full ordered session transcript (may be empty).
/// - `me_speaker` — which `Others+Some(N)` index is the app user in an
///   integration session (`None` for ordinary mic/desktop recordings).
/// - `started_at` / `ended_at` — session epoch-ms boundaries from `sessions`.
///   Pass `ended_at == started_at` for an in-progress session → zeroed stats.
///
/// # Guarantees
/// - Never panics on any input.
/// - Empty segments or `ended_at == started_at` → all zero stats.
/// - Speaker key rules (pinned here, tested below):
///   - `Source::Me`                         → `"me"`
///   - `Source::Others + Some(n)`           → `"spk-N"` (0-based N)
///   - `Source::Others + None`              → `"others"` (un-diarized bucket)
pub fn compute_stats(
    segments: &[Segment],
    me_speaker: Option<i32>,
    started_at: u64,
    ended_at: u64,
) -> SessionStats {
    let meeting_ms = ended_at.saturating_sub(started_at);

    // ── 1. Build per-speaker accumulators ─────────────────────────────────
    use std::collections::HashMap;
    #[derive(Default)]
    struct Acc {
        speaker: Option<i32>,
        is_me: bool,
        talk_ms: u64,
        words: u32,
        questions: u32,
        lines: u32,
        longest_monologue_ms: u64,
        interruptions_made: u32,
        talk_over_ms: u64,
    }
    let mut map: HashMap<String, Acc> = HashMap::new();

    for seg in segments {
        let key = speaker_key(seg.source, seg.speaker);
        let is_me = speaker_is_me(seg.source, seg.speaker, me_speaker);
        let dur = seg.t_end_ms.saturating_sub(seg.t_start_ms);
        let words = seg.text.split_whitespace().count() as u32;
        let question = seg.text.trim().ends_with('?');
        let e = map.entry(key.clone()).or_insert_with(|| Acc {
            speaker: seg.speaker.filter(|_| seg.source == Source::Others),
            is_me,
            ..Default::default()
        });
        e.talk_ms += dur;
        e.words += words;
        if question {
            e.questions += 1;
        }
        e.lines += 1;
    }

    // ── 2. Monologues ─────────────────────────────────────────────────────
    // Same-speaker consecutive segments with gap < 2 000 ms are merged.
    const MONO_GAP_MS: u64 = 2_000;
    {
        // Group segments by speaker key, maintaining original order.
        let mut by_key: HashMap<String, Vec<&Segment>> = HashMap::new();
        for seg in segments {
            by_key
                .entry(speaker_key(seg.source, seg.speaker))
                .or_default()
                .push(seg);
        }
        for (key, mut segs) in by_key {
            // Sort by start time (they're almost always already ordered).
            segs.sort_unstable_by_key(|s| s.t_start_ms);
            let mut run_start = 0u64;
            let mut run_end = 0u64;
            let mut longest = 0u64;
            for (i, seg) in segs.iter().enumerate() {
                if i == 0 {
                    run_start = seg.t_start_ms;
                    run_end = seg.t_end_ms;
                } else {
                    let gap = seg.t_start_ms.saturating_sub(run_end);
                    if gap < MONO_GAP_MS {
                        // Extend run (handle overlapping segs).
                        run_end = run_end.max(seg.t_end_ms);
                    } else {
                        let dur = run_end.saturating_sub(run_start);
                        if dur > longest {
                            longest = dur;
                        }
                        run_start = seg.t_start_ms;
                        run_end = seg.t_end_ms;
                    }
                }
            }
            // Final run.
            let dur = run_end.saturating_sub(run_start);
            if dur > longest {
                longest = dur;
            }
            if let Some(acc) = map.get_mut(&key) {
                acc.longest_monologue_ms = longest;
            }
        }
    }

    // ── 3. Interruptions + talk-over ──────────────────────────────────────
    // We need a sorted segment list to scan pairwise efficiently.
    let mut sorted: Vec<&Segment> = segments.iter().collect();
    sorted.sort_unstable_by_key(|s| s.t_start_ms);
    {
        // Build active-speaker windows: for each segment, track active concurrent
        // segments from other speakers using a sweep-line over the sorted list.
        // We iterate pairs (j, i) where j is a "candidate interrupter" and i is
        // an "active" segment that started before j.
        //
        // Interruption: segment j starts strictly inside segment i's interval
        // AND they belong to different speaker keys.
        //
        // Talk-over: pairwise overlap between different-speaker segments,
        // attributed to BOTH speakers.
        //
        // We use a simple O(n²) scan — meeting transcripts are small (<<10k segs).
        let n = sorted.len();
        for j in 0..n {
            let sj = sorted[j];
            let kj = speaker_key(sj.source, sj.speaker);
            // Scan backward over ALL earlier-starting segments. We cannot
            // `break` on the first one that ended before sj starts: the array
            // is sorted by START time, so a short early-ending segment can sit
            // between sj and a long still-active one (A=[0,20s), B=[5s,7s),
            // C=[10s,15s) — breaking at B would hide A from C). O(n²) is fine;
            // meeting transcripts are small (<<10k segs).
            let mut k = j;
            while k > 0 {
                k -= 1;
                let si = sorted[k];
                // si starts before sj; skip it if it ended before sj started —
                // but keep scanning (earlier segments may still be active).
                if si.t_end_ms <= sj.t_start_ms {
                    continue;
                }
                let ki = speaker_key(si.source, si.speaker);
                if ki == kj {
                    continue; // same speaker
                }
                // sj starts strictly inside si's interval → interruption by sj.
                if sj.t_start_ms > si.t_start_ms && sj.t_start_ms < si.t_end_ms {
                    if let Some(acc) = map.get_mut(&kj) {
                        acc.interruptions_made += 1;
                    }
                }
                // Overlap attribution.
                let ov = {
                    let lo = sj.t_start_ms.max(si.t_start_ms);
                    let hi = sj.t_end_ms.min(si.t_end_ms);
                    hi.saturating_sub(lo)
                };
                if ov > 0 {
                    if let Some(acc) = map.get_mut(&kj) {
                        acc.talk_over_ms += ov;
                    }
                    if let Some(acc) = map.get_mut(&ki) {
                        acc.talk_over_ms += ov;
                    }
                }
            }
        }
        // De-duplicate talk_over_ms: the backward loop above counts each
        // (j,i) pair once when visiting j. Since we attribute to BOTH speakers
        // in a single pass, we might double-count a pair when j later becomes
        // an "active" segment and a subsequent segment k overlaps it. However,
        // our backward scan only visits earlier segments (i < j), so each
        // overlap pair (i, j) is visited exactly once: when j is the outer
        // index and i is found backward. No double-counting.
    }

    // ── 4. Union speech (interval-union) ─────────────────────────────────
    let speech_ms = {
        let mut spans: Vec<(u64, u64)> = sorted
            .iter()
            .map(|s| (s.t_start_ms, s.t_end_ms))
            .filter(|(a, b)| b > a)
            .collect();
        spans.sort_unstable_by_key(|(a, _)| *a);
        let mut union_ms = 0u64;
        let mut cur_start = 0u64;
        let mut cur_end = 0u64;
        for (a, b) in spans {
            if a >= cur_end {
                union_ms += cur_end.saturating_sub(cur_start);
                cur_start = a;
                cur_end = b;
            } else {
                cur_end = cur_end.max(b);
            }
        }
        union_ms += cur_end.saturating_sub(cur_start);
        union_ms
    };

    // ── 5. talk_share + wpm ───────────────────────────────────────────────
    let total_talk_ms: u64 = map.values().map(|a| a.talk_ms).sum();

    // ── 6. Assemble output ────────────────────────────────────────────────
    let mut speaker_stats: Vec<SpeakerStats> = map
        .into_iter()
        .map(|(key, acc)| {
            let talk_share = if total_talk_ms > 0 {
                acc.talk_ms as f32 / total_talk_ms as f32
            } else {
                0.0
            };
            let wpm = if acc.talk_ms > 0 {
                acc.words as f32 / (acc.talk_ms as f32 / 60_000.0)
            } else {
                0.0
            };
            SpeakerStats {
                key,
                speaker: acc.speaker,
                is_me: acc.is_me,
                talk_ms: acc.talk_ms,
                talk_share,
                words: acc.words,
                wpm,
                questions: acc.questions,
                lines: acc.lines,
                longest_monologue_ms: acc.longest_monologue_ms,
                interruptions_made: acc.interruptions_made,
                talk_over_ms: acc.talk_over_ms,
            }
        })
        .collect();
    // Sort by talk_share descending for display.
    speaker_stats.sort_unstable_by(|a, b| {
        b.talk_share
            .partial_cmp(&a.talk_share)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let silence_ratio = if meeting_ms > 0 {
        1.0 - speech_ms.min(meeting_ms) as f32 / meeting_ms as f32
    } else {
        0.0
    };

    SessionStats {
        speakers: speaker_stats,
        meeting_ms,
        speech_ms,
        silence_ratio,
    }
}

/// Canonical speaker key for a segment (see [`compute_stats`] doc).
fn speaker_key(source: Source, speaker: Option<i32>) -> String {
    match (source, speaker) {
        (Source::Me, _) => "me".to_string(),
        (Source::Others, Some(n)) => format!("spk-{n}"),
        (Source::Others, None) => "others".to_string(),
    }
}

/// Whether a speaker slot is the app user's own perspective.
fn speaker_is_me(source: Source, speaker: Option<i32>, me_speaker: Option<i32>) -> bool {
    match source {
        Source::Me => true,
        Source::Others => me_speaker.is_some() && speaker == me_speaker,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn seg(source: Source, speaker: Option<i32>) -> Segment {
        Segment {
            id: None,
            source,
            t_start_ms: 0,
            t_end_ms: 1000,
            text: "hi".into(),
            words: Vec::new(),
            speaker,
        }
    }

    #[test]
    fn speaker_labels() {
        let mut names = HashMap::new();
        assert_eq!(seg(Source::Me, None).speaker_label(&names), "Me");
        assert_eq!(seg(Source::Others, None).speaker_label(&names), "Others");
        // Unnamed diarized speakers display 1-based.
        assert_eq!(
            seg(Source::Others, Some(0)).speaker_label(&names),
            "Speaker 1"
        );
        names.insert(0, "Alex".into());
        assert_eq!(seg(Source::Others, Some(0)).speaker_label(&names), "Alex");
        // A name for one index doesn't leak onto another.
        assert_eq!(
            seg(Source::Others, Some(1)).speaker_label(&names),
            "Speaker 2"
        );
    }

    #[test]
    fn enum_str_roundtrips() {
        for s in [ProjectStatus::Active, ProjectStatus::Archived] {
            assert_eq!(ProjectStatus::parse(s.as_str()), s);
        }
        for k in [ItemKind::Action, ItemKind::Question, ItemKind::Decision] {
            assert_eq!(ItemKind::parse(k.as_str()), k);
        }
        for st in [
            ItemStatus::Open,
            ItemStatus::Blocked,
            ItemStatus::Waiting,
            ItemStatus::Done,
        ] {
            assert_eq!(ItemStatus::parse(st.as_str()), st);
        }
        // Unknown strings fall back to the safe default instead of erroring.
        assert_eq!(ProjectStatus::parse("garbage"), ProjectStatus::Active);
        assert_eq!(ItemKind::parse("garbage"), ItemKind::Action);
        assert_eq!(ItemStatus::parse("garbage"), ItemStatus::Open);
    }

    #[test]
    fn only_done_is_inactive() {
        assert!(ItemStatus::Open.is_active());
        assert!(ItemStatus::Blocked.is_active());
        assert!(ItemStatus::Waiting.is_active());
        assert!(!ItemStatus::Done.is_active());
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer_version("0.2.18", "v0.2.19"));
        assert!(is_newer_version("v0.2.18", "0.3.0"));
        assert!(is_newer_version("0.2.18", "1.0.0"));
        assert!(is_newer_version("0.2", "0.2.1")); // longer = newer when equal so far
        assert!(!is_newer_version("0.2.18", "0.2.18"));
        assert!(!is_newer_version("0.2.18", "v0.2.17"));
        assert!(!is_newer_version("1.0.0", "0.9.9"));
        assert!(!is_newer_version("0.2.18", "not-a-version")); // never nag on junk
    }

    #[test]
    fn segment_serde_shape() {
        // Stored JSON must keep the lowercase source tags and omit empty fields
        // (the DB and exports both rely on this shape).
        let json = serde_json::to_string(&seg(Source::Others, None)).unwrap();
        assert!(json.contains("\"source\":\"others\""));
        assert!(!json.contains("words"));
        assert!(!json.contains("speaker"));
        let back: Segment = serde_json::from_str(&json).unwrap();
        assert_eq!(back, seg(Source::Others, None));
    }

    // ── Phase 46: compute_stats unit tests ───────────────────────────────────

    /// Build a segment with explicit timing + text.
    fn seg_t(source: Source, speaker: Option<i32>, start: u64, end: u64, text: &str) -> Segment {
        Segment {
            id: None,
            source,
            t_start_ms: start,
            t_end_ms: end,
            text: text.to_string(),
            words: Vec::new(),
            speaker,
        }
    }

    /// Test 1 — empty segments → zeroed stats, no panic.
    #[test]
    fn stats_empty_segments() {
        let stats = compute_stats(&[], None, 0, 60_000);
        assert!(stats.speakers.is_empty());
        assert_eq!(stats.meeting_ms, 60_000);
        assert_eq!(stats.speech_ms, 0);
        assert_eq!(stats.silence_ratio, 1.0);
    }

    /// Test 2 — ended == started → degenerate meeting, all zeros.
    #[test]
    fn stats_degenerate_meeting() {
        let segs = [seg_t(Source::Me, None, 0, 5_000, "hello")];
        let stats = compute_stats(&segs, None, 1_000, 1_000);
        assert_eq!(stats.meeting_ms, 0);
        assert_eq!(stats.silence_ratio, 0.0);
    }

    /// Test 3 — single speaker, basic talk_ms / words / wpm / silence_ratio.
    #[test]
    fn stats_single_speaker_basic() {
        // 60-second meeting; 10 seconds of speech (one segment).
        let segs = [seg_t(
            Source::Me,
            None,
            0,
            10_000,
            "one two three four five",
        )];
        let stats = compute_stats(&segs, None, 0, 60_000);
        assert_eq!(stats.meeting_ms, 60_000);
        assert_eq!(stats.speech_ms, 10_000);
        // silence_ratio = 1 - 10/60 ≈ 0.833
        let expected_silence = 1.0 - 10_000.0_f32 / 60_000.0;
        assert!((stats.silence_ratio - expected_silence).abs() < 1e-4);
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        assert_eq!(me.talk_ms, 10_000);
        assert_eq!(me.words, 5);
        // wpm = 5 / (10_000 / 60_000) = 5 / 0.1667 ≈ 30 wpm
        assert!((me.wpm - 30.0).abs() < 1.0);
        assert_eq!(me.talk_share, 1.0);
        assert!(me.is_me);
    }

    /// Test 4 — two speakers, talk_share sums to 1.0.
    #[test]
    fn stats_two_speaker_share_sums_to_one() {
        let segs = [
            seg_t(Source::Me, None, 0, 20_000, "a b c"),
            seg_t(Source::Others, Some(0), 30_000, 50_000, "d e f g"),
        ];
        let stats = compute_stats(&segs, None, 0, 60_000);
        let total_share: f32 = stats.speakers.iter().map(|s| s.talk_share).sum();
        assert!(
            (total_share - 1.0).abs() < 1e-5,
            "total share = {total_share}"
        );
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        let spk0 = stats.speakers.iter().find(|s| s.key == "spk-0").unwrap();
        // me: 20s / 40s total = 0.5
        assert!((me.talk_share - 0.5).abs() < 1e-5);
        // spk-0: 20s / 40s = 0.5
        assert!((spk0.talk_share - 0.5).abs() < 1e-5);
    }

    /// Test 5 — question counting: only lines ending with '?'.
    #[test]
    fn stats_questions() {
        let segs = [
            seg_t(Source::Me, None, 0, 2_000, "How are you?"),
            seg_t(Source::Me, None, 3_000, 5_000, "Fine thanks."),
            seg_t(Source::Me, None, 6_000, 8_000, "Really?"),
            seg_t(Source::Others, Some(0), 9_000, 11_000, "Yep."),
        ];
        let stats = compute_stats(&segs, None, 0, 15_000);
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        assert_eq!(me.questions, 2);
        let spk0 = stats.speakers.iter().find(|s| s.key == "spk-0").unwrap();
        assert_eq!(spk0.questions, 0);
    }

    /// Test 6 — monologue bridging: gaps < 2 s merge into one run.
    #[test]
    fn stats_monologue_bridging() {
        // Three segments; first two have a 1.5 s gap (< 2 s, so they bridge).
        // Third is 3 s gap from second → separate run.
        // Run 1: 0..5_000 + gap + 6_500..9_000 → wall-clock span = 9_000 ms
        // Run 2: 12_000..14_000 → 2_000 ms
        // Longest = 9_000 ms.
        let segs = [
            seg_t(Source::Me, None, 0, 5_000, "a b"),
            seg_t(Source::Me, None, 6_500, 9_000, "c d"),
            seg_t(Source::Me, None, 12_000, 14_000, "e"),
        ];
        let stats = compute_stats(&segs, None, 0, 20_000);
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        assert_eq!(me.longest_monologue_ms, 9_000);
    }

    /// Test 7 — interruption: speaker B starts inside speaker A's segment.
    #[test]
    fn stats_interruption() {
        // A: [0, 10_000); B starts at 5_000 (inside A) → B made 1 interruption.
        let segs = [
            seg_t(Source::Me, None, 0, 10_000, "a b c"),
            seg_t(Source::Others, Some(0), 5_000, 12_000, "d e"),
        ];
        let stats = compute_stats(&segs, None, 0, 15_000);
        let spk0 = stats.speakers.iter().find(|s| s.key == "spk-0").unwrap();
        assert_eq!(spk0.interruptions_made, 1);
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        assert_eq!(me.interruptions_made, 0);
    }

    /// Test 8 — overlap/talk-over attributed to BOTH speakers.
    #[test]
    fn stats_talk_over_bilateral() {
        // A: [0, 10_000); B: [4_000, 7_000) → overlap = 3_000 ms.
        // Both speakers should have 3_000 ms of talk_over_ms.
        let segs = [
            seg_t(Source::Me, None, 0, 10_000, "a"),
            seg_t(Source::Others, Some(0), 4_000, 7_000, "b"),
        ];
        let stats = compute_stats(&segs, None, 0, 15_000);
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        let spk0 = stats.speakers.iter().find(|s| s.key == "spk-0").unwrap();
        assert_eq!(me.talk_over_ms, 3_000);
        assert_eq!(spk0.talk_over_ms, 3_000);
    }

    /// Test 8b — REGRESSION: a short early-ending segment between a long
    /// segment and a later candidate must not hide the long one from the
    /// backward sweep (sorted-by-START + early-`break` bug).
    ///
    /// A = me      [0, 20_000)   — long, spans everything
    /// B = spk-0   [5_000, 7_000) — short, ends before C starts
    /// C = spk-1   [10_000, 15_000) — starts after B ended, inside A
    ///
    /// Expected: C interrupts A exactly once; A↔C overlap = 5_000 ms on both;
    /// B's only involvement is its own 2_000 ms overlap with A.
    #[test]
    fn stats_overlap_behind_short_segment() {
        let segs = [
            seg_t(Source::Me, None, 0, 20_000, "a"),
            seg_t(Source::Others, Some(0), 5_000, 7_000, "b"),
            seg_t(Source::Others, Some(1), 10_000, 15_000, "c"),
        ];
        let stats = compute_stats(&segs, None, 0, 25_000);
        let me = stats.speakers.iter().find(|s| s.key == "me").unwrap();
        let spk0 = stats.speakers.iter().find(|s| s.key == "spk-0").unwrap();
        let spk1 = stats.speakers.iter().find(|s| s.key == "spk-1").unwrap();
        // C started strictly inside A → one interruption made by C.
        assert_eq!(spk1.interruptions_made, 1);
        // B also started strictly inside A.
        assert_eq!(spk0.interruptions_made, 1);
        assert_eq!(me.interruptions_made, 0);
        // A↔C overlap (5_000) must be attributed to both, despite B sitting
        // between them in start order.
        assert_eq!(spk1.talk_over_ms, 5_000);
        // A: 2_000 (with B) + 5_000 (with C) = 7_000.
        assert_eq!(me.talk_over_ms, 7_000);
        // B: only its own 2_000 ms with A — uninvolved with C.
        assert_eq!(spk0.talk_over_ms, 2_000);
    }

    /// Test 9 — union speech: overlapping segments counted once.
    #[test]
    fn stats_union_speech_deduplicates() {
        // Me: [0, 5_000); Others: [3_000, 8_000) → union = [0, 8_000) = 8_000 ms.
        let segs = [
            seg_t(Source::Me, None, 0, 5_000, "x"),
            seg_t(Source::Others, Some(0), 3_000, 8_000, "y"),
        ];
        let stats = compute_stats(&segs, None, 0, 10_000);
        assert_eq!(stats.speech_ms, 8_000);
    }

    /// Test 10 — un-diarized Others lands in the "others" bucket.
    #[test]
    fn stats_undiarized_others_bucket() {
        let segs = [seg_t(Source::Others, None, 0, 5_000, "hello")];
        let stats = compute_stats(&segs, None, 0, 10_000);
        assert!(stats.speakers.iter().any(|s| s.key == "others"));
        assert!(stats.speakers.iter().all(|s| s.key != "spk-0"));
    }

    /// Test 11 — me_speaker integration: Others+Some(1) with me_speaker=Some(1)
    /// → is_me = true, key = "spk-1".
    #[test]
    fn stats_me_speaker_integration() {
        let segs = [
            seg_t(Source::Others, Some(1), 0, 5_000, "I said this"),
            seg_t(Source::Others, Some(0), 6_000, 9_000, "they said this"),
        ];
        let stats = compute_stats(&segs, Some(1), 0, 12_000);
        let spk1 = stats.speakers.iter().find(|s| s.key == "spk-1").unwrap();
        assert!(spk1.is_me);
        let spk0 = stats.speakers.iter().find(|s| s.key == "spk-0").unwrap();
        assert!(!spk0.is_me);
    }

    /// Test 12 — stats sorted by talk_share descending.
    #[test]
    fn stats_sorted_by_talk_share_desc() {
        let segs = [
            seg_t(Source::Others, Some(0), 0, 5_000, "a"), // 5s
            seg_t(Source::Me, None, 6_000, 20_000, "b"),   // 14s
            seg_t(Source::Others, Some(1), 21_000, 24_000, "c"), // 3s
        ];
        let stats = compute_stats(&segs, None, 0, 30_000);
        let shares: Vec<f32> = stats.speakers.iter().map(|s| s.talk_share).collect();
        for w in shares.windows(2) {
            assert!(w[0] >= w[1], "not sorted: {:?}", shares);
        }
    }

    /// Test 13 — stats are serde-roundtrippable (JSON cache in store).
    #[test]
    fn stats_serde_roundtrip() {
        let segs = [
            seg_t(Source::Me, None, 0, 10_000, "hello world"),
            seg_t(Source::Others, Some(0), 5_000, 12_000, "hi there"),
        ];
        let stats = compute_stats(&segs, None, 0, 15_000);
        let json = serde_json::to_string(&stats).unwrap();
        let back: SessionStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, back);
    }
}
