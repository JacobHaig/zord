//! Driving a live integration session: consume its events, assign each
//! participant a stable 0-based **speaker index** (in join order), and hand the
//! engine each new participant's audio stream + identity. This is the reusable,
//! backend-agnostic glue between an [`Integration`] and the engine's per-speaker
//! tracks — kept here (not in the engine) so it's testable with [`FakeProvider`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;

use crate::integration::{AudioStream, Integration, IntegrationEvent};

/// Where a participant's audio goes on the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackRole {
    /// The followed user → the "Me" track (captured via the platform).
    Me,
    /// An "Others" participant with a stable 0-based speaker index.
    Speaker(i32),
}

/// Why a driven session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndReason {
    /// The provider signalled `Ended` (followed user left, disconnected, …).
    Provider(String),
    /// The caller set the stop flag (user pressed Stop).
    Stopped,
    /// The event channel closed without an `Ended` (provider thread gone).
    Disconnected,
}

/// Start `integration` and pump its events until it ends:
/// - each **new** participant (by [`crate::Participant::key`]) is assigned a
///   [`TrackRole`] — the followed user → [`TrackRole::Me`], everyone else the
///   next 0-based [`TrackRole::Speaker`] — and passed to `on_join(role, name,
///   sample_rate, audio)`; the engine spawns the matching track from `audio`;
/// - a rename is forwarded to `on_rename(role, name)` (engine updates
///   `speaker_names` for `Speaker` roles);
/// - returns when the provider ends, the caller raises `stop`, or the channel
///   closes.
///
/// A participant key that re-appears keeps its original role (a rejoin maps to
/// the same track); `on_join` is still called so the engine can attach the new
/// stream.
pub fn drive_session(
    integration: &mut dyn Integration,
    stop: &Arc<AtomicBool>,
    mut on_join: impl FnMut(TrackRole, String, u32, AudioStream),
    mut on_rename: impl FnMut(TrackRole, String),
) -> Result<EndReason> {
    let rx = integration.start()?;
    let mut roles: HashMap<String, TrackRole> = HashMap::new();
    let mut next_speaker = 0i32;

    loop {
        if stop.load(Ordering::Relaxed) {
            integration.stop();
            return Ok(EndReason::Stopped);
        }
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(IntegrationEvent::ParticipantJoined {
                participant,
                sample_rate,
                audio,
            }) => {
                let role = *roles.entry(participant.key).or_insert_with(|| {
                    if participant.is_me {
                        TrackRole::Me
                    } else {
                        let r = TrackRole::Speaker(next_speaker);
                        next_speaker += 1;
                        r
                    }
                });
                on_join(role, participant.name, sample_rate, audio);
            }
            Ok(IntegrationEvent::ParticipantRenamed { key, name }) => {
                if let Some(&role) = roles.get(&key) {
                    on_rename(role, name);
                }
            }
            Ok(IntegrationEvent::Ended { reason }) => {
                integration.stop();
                return Ok(EndReason::Provider(reason));
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(EndReason::Disconnected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeProvider;

    #[test]
    fn assigns_me_then_sequential_speakers_and_ends() {
        let mut fake = FakeProvider::new(3, 1); // participant 0 is "me"
        let stop = Arc::new(AtomicBool::new(false));
        let mut roles: Vec<TrackRole> = Vec::new();
        let mut streams = Vec::new();

        let reason = drive_session(
            &mut fake,
            &stop,
            |role, _name, sr, audio| {
                assert_eq!(sr, 48_000);
                roles.push(role);
                streams.push(audio); // keep alive so the provider isn't cut short
            },
            |_role, _name| {},
        )
        .unwrap();

        // The followed user → Me; the other two → Speaker(0), Speaker(1).
        assert_eq!(roles.len(), 3);
        assert_eq!(roles.iter().filter(|r| **r == TrackRole::Me).count(), 1);
        let mut speakers: Vec<i32> = roles
            .iter()
            .filter_map(|r| match r {
                TrackRole::Speaker(i) => Some(*i),
                TrackRole::Me => None,
            })
            .collect();
        speakers.sort();
        assert_eq!(speakers, vec![0, 1]);
        assert!(matches!(reason, EndReason::Provider(_)));
    }

    #[test]
    fn stop_flag_ends_the_session() {
        let mut fake = FakeProvider::new(2, 100); // long session
        let stop = Arc::new(AtomicBool::new(true)); // already stopped
        let reason = drive_session(&mut fake, &stop, |_, _, _, _| {}, |_, _| {}).unwrap();
        assert_eq!(reason, EndReason::Stopped);
    }
}
