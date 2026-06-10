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
/// - each **new** participant (by [`crate::Participant::key`]) is assigned the
///   next 0-based speaker index and passed to `on_join(index, name, sample_rate,
///   audio)` — the engine spawns a per-speaker track from `audio`;
/// - a rename is forwarded to `on_rename(index, name)` (engine updates
///   `speaker_names`);
/// - returns when the provider ends, the caller raises `stop`, or the channel
///   closes.
///
/// A participant key that re-appears keeps its original index (a rejoin maps to
/// the same speaker); `on_join` is still called so the engine can attach the new
/// stream.
pub fn drive_session(
    integration: &mut dyn Integration,
    stop: &Arc<AtomicBool>,
    mut on_join: impl FnMut(i32, String, u32, AudioStream),
    mut on_rename: impl FnMut(i32, String),
) -> Result<EndReason> {
    let rx = integration.start()?;
    let mut index_of: HashMap<String, i32> = HashMap::new();

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
                let next = index_of.len() as i32;
                let index = *index_of.entry(participant.key).or_insert(next);
                on_join(index, participant.name, sample_rate, audio);
            }
            Ok(IntegrationEvent::ParticipantRenamed { key, name }) => {
                if let Some(&index) = index_of.get(&key) {
                    on_rename(index, name);
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
    fn assigns_sequential_indices_and_ends() {
        let mut fake = FakeProvider::new(3, 1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut joins: Vec<(i32, String)> = Vec::new();
        let mut streams = Vec::new();

        let reason = drive_session(
            &mut fake,
            &stop,
            |idx, name, sr, audio| {
                assert_eq!(sr, 48_000);
                joins.push((idx, name));
                streams.push(audio); // keep alive so the provider isn't cut short
            },
            |_idx, _name| {},
        )
        .unwrap();

        // Three participants, indices assigned 0,1,2 in join order.
        assert_eq!(joins.len(), 3);
        let mut indices: Vec<i32> = joins.iter().map(|(i, _)| *i).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2]);
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
