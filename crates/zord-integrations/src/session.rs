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
    /// `error` carries the provider's verdict: true = the user must act on it
    /// (join refused, bad token), false = a benign end of the call.
    Provider { reason: String, error: bool },
    /// The caller set the stop flag (user pressed Stop).
    Stopped,
    /// The event channel closed without an `Ended` (provider thread gone).
    Disconnected,
}

/// Start `integration` and pump its events until it ends:
/// - **every** new participant (by [`crate::Participant::key`]) — the followed
///   user included — is assigned the next 0-based speaker index in join order
///   and passed to `on_join(idx, name, is_me, sample_rate, audio)`; the engine
///   spawns a uniform `spk-N` track from `audio`. `is_me` tags which index is
///   the followed user (identity comes from the configured platform user ID) —
///   it changes labeling/styling only, never the track layout;
/// - a rename is forwarded to `on_rename(idx, name)` (engine updates
///   `speaker_names`);
/// - a provider notice is forwarded to `on_notice(msg)` (engine surfaces it as
///   `Event::Notice` — e.g. the Discord late-joiner re-sync banner, Phase 50);
/// - returns when the provider ends, the caller raises `stop`, or the channel
///   closes.
///
/// A participant key that re-appears keeps its original index (a rejoin maps to
/// the same track); `on_join` is still called so the engine can attach the new
/// stream. Phase 50: after a Discord leave+rejoin every participant re-announces
/// under their stable user-id key, so each `on_join` past the first for a given
/// idx hands the engine a *fresh* audio stream for an *existing* track — the
/// engine forwards it into that track's proc rather than spawning a duplicate.
pub fn drive_session(
    integration: &mut dyn Integration,
    stop: &Arc<AtomicBool>,
    mut on_join: impl FnMut(i32, String, bool, u32, AudioStream),
    mut on_rename: impl FnMut(i32, String),
    mut on_notice: impl FnMut(String),
) -> Result<EndReason> {
    let rx = integration.start()?;
    let mut indices: HashMap<String, i32> = HashMap::new();
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
                let idx = *indices.entry(participant.key).or_insert_with(|| {
                    let i = next_speaker;
                    next_speaker += 1;
                    i
                });
                on_join(idx, participant.name, participant.is_me, sample_rate, audio);
            }
            Ok(IntegrationEvent::ParticipantRenamed { key, name }) => {
                if let Some(&idx) = indices.get(&key) {
                    on_rename(idx, name);
                }
            }
            Ok(IntegrationEvent::Notice(msg)) => on_notice(msg),
            Ok(IntegrationEvent::Ended { reason, error }) => {
                integration.stop();
                return Ok(EndReason::Provider { reason, error });
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
    fn assigns_sequential_indices_with_one_me_tag_and_ends() {
        let mut fake = FakeProvider::new(3, 1); // one participant is "me"
        let stop = Arc::new(AtomicBool::new(false));
        let mut joined: Vec<(i32, bool)> = Vec::new();
        let mut streams = Vec::new();

        let reason = drive_session(
            &mut fake,
            &stop,
            |idx, _name, is_me, sr, audio| {
                assert_eq!(sr, 48_000);
                joined.push((idx, is_me));
                streams.push(audio); // keep alive so the provider isn't cut short
            },
            |_idx, _name| {},
            |_msg| {},
        )
        .unwrap();

        // Everyone — the followed user included — gets a sequential 0-based
        // index in join order; exactly one carries the is_me tag.
        assert_eq!(joined.len(), 3);
        let mut indices: Vec<i32> = joined.iter().map(|(i, _)| *i).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2]);
        assert_eq!(joined.iter().filter(|(_, me)| *me).count(), 1);
        assert!(matches!(reason, EndReason::Provider { error: false, .. }));
    }

    #[test]
    fn stop_flag_ends_the_session() {
        let mut fake = FakeProvider::new(2, 100); // long session
        let stop = Arc::new(AtomicBool::new(true)); // already stopped
        let reason =
            drive_session(&mut fake, &stop, |_, _, _, _, _| {}, |_, _| {}, |_| {}).unwrap();
        assert_eq!(reason, EndReason::Stopped);
    }
}
