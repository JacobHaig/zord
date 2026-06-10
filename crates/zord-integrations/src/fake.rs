//! A dependency-free [`Integration`] used to validate the engine/store/GUI paths
//! before any network backend exists. It announces a fixed set of participants,
//! then emits **sparse** per-participant audio (real-time-paced tone bursts with
//! silent gaps, a distinct pitch per participant) so the engine's silence-padding
//! and per-speaker tracks can be exercised, and finally signals `Ended`.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::Result;

use crate::integration::{Integration, IntegrationEvent, Participant};

/// Built-in fake provider. `participants` distinct speakers, each with a steady
/// pitch; `bursts` talk/silence cycles before the session ends.
pub struct FakeProvider {
    participants: usize,
    bursts: u32,
    sample_rate: u32,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeProvider {
    /// `participants` speakers over a short (~`bursts` × 1 s) session at 48 kHz —
    /// matching Discord's decode rate so the fake mirrors the real source.
    pub fn new(participants: usize, bursts: u32) -> Self {
        Self {
            participants: participants.max(1),
            bursts: bursts.max(1),
            sample_rate: 48_000,
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
        }
    }
}

impl Default for FakeProvider {
    fn default() -> Self {
        Self::new(2, 3)
    }
}

/// One 20 ms mono frame of a sine at `freq`, continuing the phase via the running
/// absolute sample index `start` (so consecutive frames don't click).
fn tone_frame(freq: f32, sample_rate: u32, start: u64, len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| {
            let t = (start + i as u64) as f32 / sample_rate as f32;
            (TAU * freq * t).sin() * 0.3
        })
        .collect()
}

impl Integration for FakeProvider {
    fn name(&self) -> &str {
        "Fake"
    }

    fn start(&mut self) -> Result<Receiver<IntegrationEvent>> {
        let (ev_tx, ev_rx) = mpsc::channel();
        let stop = self.stop.clone();
        let (n, bursts, sr) = (self.participants, self.bursts, self.sample_rate);

        self.handle = Some(thread::spawn(move || {
            let frame = (sr / 50) as usize; // 20 ms
            let frames_per_phase = 25; // 500 ms of talk, then 500 ms of silence

            // Announce participants, each with its own audio channel + pitch.
            let mut chans = Vec::with_capacity(n);
            for i in 0..n {
                let (atx, arx) = mpsc::channel::<Vec<f32>>();
                let participant = Participant {
                    key: format!("fake-{i}"),
                    name: format!("Tester {}", i + 1),
                    // First participant stands in for the followed user ("Me").
                    is_me: i == 0,
                };
                if ev_tx
                    .send(IntegrationEvent::ParticipantJoined {
                        participant,
                        sample_rate: sr,
                        audio: arx,
                    })
                    .is_err()
                {
                    return; // receiver dropped — engine gone
                }
                chans.push((atx, 220.0 * (i as f32 + 1.0), 0u64));
            }

            // Sparse audio: each burst is talk (frames sent) then silence (no
            // frames — the engine pads to wall-clock). Real-time paced.
            'outer: for _ in 0..bursts {
                for _ in 0..frames_per_phase {
                    if stop.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    for (tx, freq, produced) in chans.iter_mut() {
                        let buf = tone_frame(*freq, sr, *produced, frame);
                        *produced += frame as u64;
                        if tx.send(buf).is_err() {
                            break 'outer;
                        }
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                // Silent gap — deliberately send nothing.
                for _ in 0..frames_per_phase {
                    if stop.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
            }

            chans.clear(); // drop the senders → close the per-participant streams
            let _ = ev_tx.send(IntegrationEvent::Ended {
                reason: "fake session complete".into(),
            });
        }));

        Ok(ev_rx)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announces_participants_then_streams_audio() {
        let mut fake = FakeProvider::new(2, 1);
        let rx = fake.start().unwrap();
        let timeout = Duration::from_secs(3);

        // Two distinct participants are announced, each with an audio stream.
        let mut joined = Vec::new();
        let mut streams = Vec::new();
        for _ in 0..2 {
            match rx.recv_timeout(timeout).unwrap() {
                IntegrationEvent::ParticipantJoined { participant, sample_rate, audio } => {
                    assert_eq!(sample_rate, 48_000);
                    joined.push(participant.key);
                    streams.push(audio);
                }
                other => panic!("expected ParticipantJoined, got something else: {}", label(&other)),
            }
        }
        joined.sort();
        assert_eq!(joined, vec!["fake-0".to_string(), "fake-1".to_string()]);

        // Each participant's stream delivers real (non-empty) audio frames.
        for s in &streams {
            let frame = s.recv_timeout(timeout).expect("a frame");
            assert!(!frame.is_empty());
            assert!(frame.iter().any(|&x| x.abs() > 0.0), "frame should carry signal");
        }

        // The session eventually ends.
        loop {
            match rx.recv_timeout(timeout).expect("more events before timeout") {
                IntegrationEvent::Ended { .. } => break,
                _ => continue,
            }
        }
    }

    fn label(e: &IntegrationEvent) -> &'static str {
        match e {
            IntegrationEvent::ParticipantJoined { .. } => "joined",
            IntegrationEvent::ParticipantRenamed { .. } => "renamed",
            IntegrationEvent::Ended { .. } => "ended",
        }
    }
}
