//! Shared types for the Zord audio transcription app.
//!
//! These types are deliberately dependency-light so every other crate can
//! depend on `zord-core` without pulling in audio/ML/storage machinery.

use serde::{Deserialize, Serialize};

/// Sample rate that whisper.cpp requires for all input audio.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

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
