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
    pub source: Source,
    /// Milliseconds from session start.
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
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
