//! Render a session transcript to Markdown, SRT, or JSON. Pure functions — no
//! I/O — so they're trivial to use from the CLI, GUI, and web dashboard alike.

use serde::Serialize;
use std::collections::HashMap;
use zord_core::{Segment, Session};

/// Output formats supported by export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Markdown,
    Srt,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "md" | "markdown" => Some(Format::Markdown),
            "srt" => Some(Format::Srt),
            "json" => Some(Format::Json),
            _ => None,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Markdown => "md",
            Format::Srt => "srt",
            Format::Json => "json",
        }
    }
}

/// Render `segments` of `session` in the requested `format`. `names` maps
/// diarized speaker indices to custom names (pass an empty map for none).
pub fn render(
    session: &Session,
    segments: &[Segment],
    names: &HashMap<i32, String>,
    format: Format,
) -> String {
    match format {
        Format::Markdown => to_markdown(session, segments, names),
        Format::Srt => to_srt(segments, names),
        Format::Json => to_json(session, segments),
    }
}

/// Readable transcript: a heading plus one labelled, timestamped line each.
pub fn to_markdown(session: &Session, segments: &[Segment], names: &HashMap<i32, String>) -> String {
    let mut out = String::new();
    push_markdown_header(&mut out, session);
    for seg in segments {
        out.push_str(&format!(
            "**[{}] {}:** {}\n\n",
            clock(seg.t_start_ms),
            seg.speaker_label(names),
            seg.text.trim()
        ));
    }
    out
}

/// SubRip subtitles. Each segment becomes one cue, prefixed with the speaker.
pub fn to_srt(segments: &[Segment], names: &HashMap<i32, String>) -> String {
    let mut out = String::new();
    for (i, seg) in segments.iter().enumerate() {
        out.push_str(&format!("{}\n", i + 1));
        out.push_str(&format!(
            "{} --> {}\n",
            srt_ts(seg.t_start_ms),
            srt_ts(seg.t_end_ms.max(seg.t_start_ms + 1))
        ));
        out.push_str(&format!("{}: {}\n\n", seg.speaker_label(names), seg.text.trim()));
    }
    out
}

#[derive(Serialize)]
struct JsonExport<'a> {
    session: &'a Session,
    segments: &'a [Segment],
}

/// Full-fidelity JSON: session metadata + all segments (incl. word timings).
pub fn to_json(session: &Session, segments: &[Segment]) -> String {
    let export = JsonExport { session, segments };
    serde_json::to_string_pretty(&export).unwrap_or_else(|_| "{}".to_string())
}

/// Write the markdown title and metadata header for `session` to `out`.
fn push_markdown_header(out: &mut String, session: &Session) {
    let title = session.title.clone().unwrap_or_else(|| session.id.clone());
    out.push_str(&format!("# {title}\n\n"));
    out.push_str(&format!("- Model: `{}`\n", session.model));
    out.push_str(&format!("- Started: {} (unix ms)\n\n", session.started_at));
}

/// `mm:ss` for human-facing markdown.
fn clock(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// `HH:MM:SS,mmm` as required by SRT.
fn srt_ts(ms: u64) -> String {
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let millis = ms % 1000;
    format!("{h:02}:{m:02}:{s:02},{millis:03}")
}
