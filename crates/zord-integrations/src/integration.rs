//! The integration seam: a platform that follows a user into a live session and
//! hands Zord **one identity-labeled audio stream per participant**.
//!
//! This is deliberately dependency-light (std + anyhow) and lives in the default
//! build — only concrete *implementations* (Discord via songbird, behind the
//! `discord` feature) pull heavy deps. The engine consumes this trait the same
//! way regardless of the backend, so a local bot today and a hosted bot later
//! plug into the identical seam.

use std::sync::mpsc::Receiver;

use anyhow::Result;

/// A participant in an integration session: a stable per-session key plus a
/// human display name (the platform's real name — not "Speaker N").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    /// Stable identifier within this session (e.g. a Discord user id as a
    /// string). The engine maps this to a 0-based speaker index in order of
    /// first appearance, and that index is what lands on stored segments.
    pub key: String,
    /// Display name shown in the transcript. May be refined later via
    /// [`IntegrationEvent::ParticipantRenamed`] (e.g. a Discord SSRC that only
    /// resolves to a username once the user first speaks).
    pub name: String,
    /// `true` for the **followed user** (the local operator). They record as a
    /// uniform speaker track like everyone else — captured *through the
    /// platform* (so Discord's noise suppression etc. apply), never a local
    /// mic — and this flag lets the engine tag their index for "me" styling.
    /// At most one participant should set this.
    pub is_me: bool,
}

/// Mono `f32` audio frames at a fixed sample rate — the same shape the capture
/// layer's `FrameSink` produces, so a participant stream feeds the existing
/// resample → VAD → transcribe pipeline unchanged. Sparse by nature: frames
/// arrive only while the participant transmits; the engine silence-pads the gaps
/// to the session clock (full session-aligned tracks, Phase 28).
pub type AudioStream = Receiver<Vec<f32>>;

/// Events emitted by a running integration session.
pub enum IntegrationEvent {
    /// A participant's audio is now flowing. The engine assigns the next speaker
    /// index, records `participant.name` in `speaker_names`, and spawns a
    /// per-speaker track from `audio` (mono `f32` @ `sample_rate`).
    ParticipantJoined {
        participant: Participant,
        sample_rate: u32,
        audio: AudioStream,
    },
    /// A late-resolved or changed display name for an already-joined participant
    /// (keyed by [`Participant::key`]). Lets identity-by-name catch up after
    /// identity-by-stream has already started (the Phase 27 mapping gap).
    ParticipantRenamed { key: String, name: String },
    /// A transient, user-facing status message from the provider (the engine
    /// maps it to `Event::Notice`). Used for things the user should see but that
    /// aren't a session end — e.g. the Discord late-joiner re-sync (Phase 50)
    /// surfacing the brief audio gap while the bot leaves and rejoins to re-key
    /// the DAVE group. Purely informational; does not affect the track layout.
    Notice(String),
    /// The session ended on the provider's side — the followed user left, the bot
    /// was disconnected, etc. The engine finalizes the recording. `error` marks
    /// ends the user must act on (join refused, bad token, gateway failure) so
    /// the GUI can surface them in the notice banner, vs. benign ends (the
    /// followed user simply left) that only need the log.
    Ended { reason: String, error: bool },
}

/// A platform integration that follows a user into a live session and yields one
/// identity-labeled audio stream per participant.
///
/// Discord is the first implementation (Phase 30, behind the `discord` feature);
/// [`crate::FakeProvider`] is a dependency-free stand-in used to validate the
/// engine/store/GUI paths end-to-end before any network code exists.
pub trait Integration: Send {
    /// Human label for diagnostics / UI (e.g. `"Discord"`).
    fn name(&self) -> &str;

    /// Connect, resolve the target session (follow-the-user), join it, and begin
    /// emitting events on the returned receiver. The integration owns its own
    /// threads/runtime; [`Integration::stop`] (or dropping the integration) tears
    /// the session down and closes the receiver.
    fn start(&mut self) -> Result<Receiver<IntegrationEvent>>;

    /// Leave the session and stop emitting events. Idempotent.
    fn stop(&mut self);
}
