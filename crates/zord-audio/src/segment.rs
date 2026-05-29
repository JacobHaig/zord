//! Energy-based voice-activity segmentation.
//!
//! Splits a continuous 16 kHz mono stream into utterance-sized chunks on
//! silence boundaries, so we can transcribe incrementally (near-real-time)
//! instead of waiting for the whole recording. Each emitted segment carries
//! wall-clock-relative timing derived from the running sample position.
//!
//! This is intentionally a simple RMS gate with hangover. It's robust and
//! dependency-free; a Silero-VAD upgrade is tracked for a later phase.

use zord_core::WHISPER_SAMPLE_RATE;

const SR: usize = WHISPER_SAMPLE_RATE as usize;
/// Analysis window: 30 ms.
const WINDOW: usize = SR * 30 / 1000;

/// A speech chunk ready for transcription.
pub struct VadSegment {
    pub t_start_ms: u64,
    pub t_end_ms: u64,
    pub samples: Vec<f32>,
}

#[derive(Clone, Copy)]
pub struct SegmenterConfig {
    /// RMS above this counts as speech (f32 samples in [-1, 1]).
    pub speech_rms: f32,
    /// Close a segment after this much trailing silence.
    pub hangover_ms: u64,
    /// Force-close a segment that runs longer than this (bounds latency).
    pub max_segment_ms: u64,
    /// Drop segments shorter than this (spurious noise).
    pub min_segment_ms: u64,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            speech_rms: 0.012,
            hangover_ms: 700,
            max_segment_ms: 25_000,
            min_segment_ms: 250,
        }
    }
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    /// Samples not yet aligned to a full analysis window.
    leftover: Vec<f32>,
    /// Total samples consumed since start (for timestamps).
    pos: u64,
    in_speech: bool,
    seg_start_sample: u64,
    seg: Vec<f32>,
    trailing_silence_ms: u64,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig) -> Self {
        Self {
            cfg,
            leftover: Vec::new(),
            pos: 0,
            in_speech: false,
            seg_start_sample: 0,
            seg: Vec::new(),
            trailing_silence_ms: 0,
        }
    }

    fn ms_of(samples: u64) -> u64 {
        samples * 1000 / SR as u64
    }

    /// Push mono 16 kHz samples; returns any completed segments.
    pub fn push(&mut self, samples: &[f32]) -> Vec<VadSegment> {
        let mut out = Vec::new();
        self.leftover.extend_from_slice(samples);

        let window_ms = Self::ms_of(WINDOW as u64);
        let mut offset = 0;
        while self.leftover.len() - offset >= WINDOW {
            let win = &self.leftover[offset..offset + WINDOW];
            offset += WINDOW;

            let rms = rms(win);
            let is_speech = rms >= self.cfg.speech_rms;

            if is_speech {
                if !self.in_speech {
                    self.in_speech = true;
                    self.seg_start_sample = self.pos;
                    self.seg.clear();
                }
                self.trailing_silence_ms = 0;
                self.seg.extend_from_slice(win);
            } else if self.in_speech {
                // Keep trailing silence inside the segment until hangover trips.
                self.trailing_silence_ms += window_ms;
                self.seg.extend_from_slice(win);
                if self.trailing_silence_ms >= self.cfg.hangover_ms {
                    if let Some(s) = self.close_segment() {
                        out.push(s);
                    }
                }
            }

            self.pos += WINDOW as u64;

            // Force-close over-long segments.
            if self.in_speech {
                let len_ms = Self::ms_of(self.pos - self.seg_start_sample);
                if len_ms >= self.cfg.max_segment_ms {
                    if let Some(s) = self.close_segment() {
                        out.push(s);
                    }
                }
            }
        }
        self.leftover.drain(..offset);
        out
    }

    /// Close any in-flight segment (call at end of recording).
    pub fn flush(&mut self) -> Option<VadSegment> {
        if self.in_speech {
            self.close_segment()
        } else {
            None
        }
    }

    fn close_segment(&mut self) -> Option<VadSegment> {
        self.in_speech = false;
        self.trailing_silence_ms = 0;
        // Trim trailing silence we tacked on during hangover.
        let trim = (self.cfg.hangover_ms as usize * SR / 1000).min(self.seg.len());
        let kept = self.seg.len().saturating_sub(trim);
        let samples: Vec<f32> = self.seg.drain(..).take(kept).collect();

        let dur_ms = Self::ms_of(samples.len() as u64);
        if dur_ms < self.cfg.min_segment_ms {
            return None;
        }
        let t_start_ms = Self::ms_of(self.seg_start_sample);
        Some(VadSegment {
            t_start_ms,
            t_end_ms: t_start_ms + dur_ms,
            samples,
        })
    }
}

fn rms(win: &[f32]) -> f32 {
    if win.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = win.iter().map(|s| s * s).sum();
    (sum_sq / win.len() as f32).sqrt()
}
