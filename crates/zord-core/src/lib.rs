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
    /// 0-based speaker index to a custom name (e.g. "Alex").
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
}
