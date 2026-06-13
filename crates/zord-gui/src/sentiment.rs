//! Phase 49 — sentiment "moments": audio-prosody markers on the Timeline.
//!
//! Two Apache-2.0 ONNX models run on the **same `ort` (ONNX Runtime) the
//! `semantic` feature already ships** (no sherpa, no second runtime):
//!
//! - **YAMNet** (TF Models, Apache-2.0): an audio-event classifier over the 521
//!   AudioSet classes. We surface a handful of near-unambiguous classes
//!   (laughter, applause, crying, cough, sneeze, cheering) as *event* moments.
//! - **wav2vec2-base SER** (onnx-community export, Apache-2.0): a per-utterance
//!   speech-emotion classifier. We surface an *emotion* moment ONLY when a
//!   strong non-neutral label persists across several consecutive utterances
//!   (the conservative-rendering rule from docs/PLAN.md Phase 49).
//!
//! ## What is unit-tested vs. what needs a LIVE model run
//! Everything in the `pure` region below — frame→time math, event-frame
//! collapsing/debounce, emotion-persistence detection, waveform normalization,
//! moment dedup/ordering — is pure and covered by the `tests` module. The
//! actual ONNX inference (the `runtime` region, gated on `feature = "sentiment"`)
//! CANNOT be exercised in CI (the models won't download/run there) and is
//! flagged with `LIVE-TEST` comments where behaviour must be confirmed against
//! real model I/O.
//!
//! The producers that drive the worker live in `engine.rs`
//! (`AnalyzeCmd` + `sentiment_loop`).
//!
//! Most items here are consumed only by the `#[cfg(feature = "sentiment")]`
//! producer in `engine.rs`; the default build compiles the pure fns (for the
//! unit tests) but doesn't call them, so the whole module tolerates dead code.
#![allow(dead_code)]

use zord_core::Moment;

// ===========================================================================
// Constants — model class maps + thresholds (CITED)
// ===========================================================================

/// One YAMNet AudioSet class we surface as a moment: its class index, the
/// `Moment::kind` string we store, and a per-class confidence threshold.
///
/// **AudioSet indices are verified against the canonical class map**
/// `tensorflow/models` → `research/audioset/yamnet/yamnet_class_map.csv`
/// (rows quoted in the comment beside each entry). YAMNet emits a length-521
/// score vector per ~0.48 s frame; index N here selects the column.
pub struct EventClass {
    /// Column index into YAMNet's 521-wide per-frame score vector.
    pub index: usize,
    /// `Moment::kind` written to the store.
    pub kind: &'static str,
    /// Minimum per-frame score for this class to register a moment.
    pub threshold: f32,
}

/// The audio events we surface. Thresholds are deliberately conservative —
/// these are "always shown" markers, so a false positive is more costly than a
/// miss. Tune against real audio (LIVE-TEST).
///
/// Indices verified against yamnet_class_map.csv (master, June 2026):
///   13,/m/01j3sz,Laughter
///   62,/m/028ght,Applause
///   19,/m/0463cq4,"Crying, sobbing"
///   42,/m/01b_21,Cough
///   44,/m/01hsr_,Sneeze
///   61,/m/053hz1,Cheering
pub const EVENT_CLASSES: &[EventClass] = &[
    EventClass {
        index: 13,
        kind: "laughter",
        threshold: 0.5,
    },
    EventClass {
        index: 62,
        kind: "applause",
        threshold: 0.5,
    },
    EventClass {
        index: 19,
        kind: "crying",
        threshold: 0.6,
    },
    EventClass {
        index: 42,
        kind: "cough",
        threshold: 0.6,
    },
    EventClass {
        index: 44,
        kind: "sneeze",
        threshold: 0.6,
    },
    EventClass {
        index: 61,
        kind: "cheering",
        threshold: 0.5,
    },
];

/// wav2vec2-base SER emotion labels, in model output order.
///
/// **Verified** against onnx-community/wav2vec2-base-Speech_Emotion_Recognition-ONNX
/// `config.json` `id2label` (June 2026): 0=SAD 1=ANGRY 2=DISGUST 3=FEAR 4=HAPPY
/// 5=NEUTRAL. We lowercase the kind suffix; `neutral` is treated specially
/// (never emitted) by [`persistent_emotion`].
pub const EMOTION_LABELS: &[&str] = &["sad", "angry", "disgust", "fear", "happy", "neutral"];

/// The label index that means "no salient emotion" — never emitted as a moment.
pub const NEUTRAL_INDEX: usize = 5;

/// YAMNet frame hop in milliseconds. The TF Hub model produces one frame of
/// scores every 0.48 s (STFT hop), with a 0.96 s window. We attribute each
/// frame's events to its hop-start time. (Source: TF Hub YAMNet model card —
/// "0.96 s window, 0.48 s hop".) Confirm the exact hop of the int8 ONNX export
/// against its output frame count (LIVE-TEST).
pub const YAMNET_HOP_MS: u64 = 480;

/// Max gap (ms) between two same-kind event frames that still collapses into a
/// single moment. Two YAMNet hops (~0.96 s) of quiet ends a run. Pure-fn input.
pub const EVENT_COLLAPSE_MAX_GAP_MS: u64 = 1_000;

/// How many consecutive utterances must carry the same strong non-neutral
/// emotion before we emit an emotion moment (the conservative-rendering N).
pub const EMOTION_PERSIST_N: usize = 3;

/// Minimum per-utterance emotion confidence for it to count toward a run.
pub const EMOTION_MIN_SCORE: f32 = 0.6;

/// Cap on per-utterance audio length fed to wav2vec2 (ms). SER models are
/// trained on short clips; a long monologue is windowed to its first 30 s so
/// one utterance can't dominate memory/latency. Documented choice (LIVE-TEST
/// whether a longer or centered window classifies better).
pub const SER_UTTERANCE_CAP_MS: u64 = 30_000;

// ===========================================================================
// Pure functions (UNIT-TESTED) — no model, no I/O
// ===========================================================================

/// Time (ms from track start) of YAMNet frame `frame_idx` given the hop.
/// Frame 0 starts at 0; frame k at `k * hop`.
#[inline]
pub fn frame_to_t_ms(frame_idx: usize, hop_ms: u64) -> u64 {
    frame_idx as u64 * hop_ms
}

/// A single YAMNet frame that crossed an event threshold: `(frame_idx, kind,
/// score)`. Produced by the runtime; consumed by [`collapse_events`].
pub type EventHit = (usize, &'static str, f32);

/// Collapse consecutive same-kind event frames into ONE moment each (debounce).
///
/// Input: per-frame hits, in ascending frame order (the runtime appends them
/// in frame order). For each maximal run of the SAME kind whose frames are no
/// more than `max_gap_ms` apart in time, emit a single moment at the run's
/// FIRST frame time, carrying the run's PEAK score. Different kinds never merge;
/// a gap larger than `max_gap_ms` starts a new moment for the same kind.
///
/// `speaker` is the source track's index (stamped by the caller).
pub fn collapse_events(
    hits: &[EventHit],
    hop_ms: u64,
    max_gap_ms: u64,
    speaker: i32,
) -> Vec<Moment> {
    let mut out: Vec<Moment> = Vec::new();
    // Track the open run per kind: (start_t_ms, last_t_ms, peak_score).
    // We process in frame order; a hit either extends the matching open run
    // (same kind, within gap) or closes it and starts a new one.
    use std::collections::HashMap;
    let mut open: HashMap<&'static str, (u64, u64, f32)> = HashMap::new();

    // Helper: flush an open run into `out`.
    fn flush(out: &mut Vec<Moment>, kind: &'static str, start: u64, peak: f32, speaker: i32) {
        out.push(Moment {
            t_ms: start,
            kind: kind.to_string(),
            speaker,
            score: peak,
        });
    }

    for &(frame_idx, kind, score) in hits {
        let t = frame_to_t_ms(frame_idx, hop_ms);
        match open.get_mut(kind) {
            Some((_start, last, peak)) if t.saturating_sub(*last) <= max_gap_ms => {
                *last = t;
                if score > *peak {
                    *peak = score;
                }
            }
            Some((start, _last, peak)) => {
                // Gap too large — close the run, open a fresh one.
                flush(&mut out, kind, *start, *peak, speaker);
                open.insert(kind, (t, t, score));
            }
            None => {
                open.insert(kind, (t, t, score));
            }
        }
    }
    for (kind, (start, _last, peak)) in open {
        flush(&mut out, kind, start, peak, speaker);
    }
    // Deterministic order for the store + UI.
    sort_moments(&mut out);
    out
}

/// One classified utterance: `(t_ms, label_index, score)`. `label_index`
/// indexes [`EMOTION_LABELS`]; `t_ms` is the utterance start.
pub type EmotionUtterance = (u64, usize, f32);

/// Detect persistent non-neutral emotion across consecutive utterances.
///
/// Emits a moment ONLY where the SAME non-neutral label holds for `n` or more
/// consecutive utterances, each at or above `min_score`. The moment is placed
/// at the FIRST utterance of the run and carries the run's mean score. Neutral
/// (or sub-threshold) utterances break a run and never produce a moment — so an
/// isolated spike is suppressed and only a sustained mood is surfaced.
///
/// `utterances` must be in ascending `t_ms` order (the caller feeds the
/// speaker's segments in time order). `speaker` stamps the source track.
pub fn persistent_emotion(
    utterances: &[EmotionUtterance],
    n: usize,
    min_score: f32,
    speaker: i32,
) -> Vec<Moment> {
    let mut out = Vec::new();
    if n == 0 {
        return out;
    }
    let mut i = 0;
    while i < utterances.len() {
        let (t0, label, score) = utterances[i];
        // Neutral or weak → can't start a run.
        if label == NEUTRAL_INDEX || score < min_score {
            i += 1;
            continue;
        }
        // Extend the run while the same label stays strong.
        let mut j = i + 1;
        let mut sum = score;
        while j < utterances.len() {
            let (_t, l, s) = utterances[j];
            if l == label && s >= min_score {
                sum += s;
                j += 1;
            } else {
                break;
            }
        }
        let run_len = j - i;
        if run_len >= n {
            out.push(Moment {
                t_ms: t0,
                kind: format!("{}{}", Moment::EMOTION_PREFIX, EMOTION_LABELS[label]),
                speaker,
                score: sum / run_len as f32,
            });
        }
        // Continue scanning after this run (whether or not it qualified).
        i = j.max(i + 1);
    }
    sort_moments(&mut out);
    out
}

/// Zero-mean / unit-variance normalize a waveform in place-style (returns a new
/// vec), matching wav2vec2's expected feature normalization (the HF
/// `Wav2Vec2FeatureExtractor` with `do_normalize=true`). A silent / constant
/// signal (variance 0) is returned zero-centered without scaling (no divide by
/// zero). Population std (divide by N) is used, as the feature extractor does.
pub fn normalize_waveform(samples: &[f32]) -> Vec<f32> {
    let n = samples.len();
    if n == 0 {
        return Vec::new();
    }
    let mean = samples.iter().copied().sum::<f32>() / n as f32;
    let var = samples.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n as f32;
    let std = var.sqrt();
    if std <= f32::EPSILON {
        return samples.iter().map(|x| x - mean).collect();
    }
    samples.iter().map(|x| (x - mean) / std).collect()
}

/// Argmax of a logits/score slice → `(index, value)`. Returns `None` for an
/// empty slice. Ties resolve to the lowest index. Used to pick the SER label.
pub fn argmax(scores: &[f32]) -> Option<(usize, f32)> {
    let mut best: Option<(usize, f32)> = None;
    for (i, &v) in scores.iter().enumerate() {
        match best {
            Some((_, bv)) if v <= bv => {}
            _ => best = Some((i, v)),
        }
    }
    best
}

/// Numerically-stable softmax over a logits slice (so SER "score" is a
/// probability, comparable to the emotion threshold). Empty → empty.
pub fn softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|&x| (x - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum <= 0.0 {
        return vec![0.0; logits.len()];
    }
    exps.into_iter().map(|e| e / sum).collect()
}

/// Stable ordering for moments: by time, then kind, then speaker. Applied
/// before storing so the store + UI see a deterministic sequence.
pub fn sort_moments(moments: &mut [Moment]) {
    moments.sort_by(|a, b| {
        a.t_ms
            .cmp(&b.t_ms)
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.speaker.cmp(&b.speaker))
    });
}

/// Merge per-track moment vectors into one ordered, de-duplicated set. Two
/// moments are duplicates when they share the same `(t_ms, kind, speaker)` —
/// the higher score wins. (Within one track this is a no-op; across tracks it
/// guards against the same event leaking into overlapping mixes.)
pub fn merge_moments(parts: Vec<Vec<Moment>>) -> Vec<Moment> {
    use std::collections::HashMap;
    let mut by_key: HashMap<(u64, String, i32), f32> = HashMap::new();
    for part in parts {
        for m in part {
            let key = (m.t_ms, m.kind, m.speaker);
            by_key
                .entry(key)
                .and_modify(|s| {
                    if m.score > *s {
                        *s = m.score;
                    }
                })
                .or_insert(m.score);
        }
    }
    let mut out: Vec<Moment> = by_key
        .into_iter()
        .map(|((t_ms, kind, speaker), score)| Moment {
            t_ms,
            kind,
            speaker,
            score,
        })
        .collect();
    sort_moments(&mut out);
    out
}

/// Map a track suffix ("me", "others", "spk-N") to the `Moment::speaker`
/// value: the `me`/`others` sentinels, or the parsed spk index. Pure mapping
/// shared by the producer.
pub fn track_speaker(track: &str) -> i32 {
    match track {
        "me" => Moment::SPEAKER_ME,
        "others" => Moment::SPEAKER_OTHERS,
        other => other
            .strip_prefix("spk-")
            .and_then(|n| n.parse::<i32>().ok())
            .unwrap_or(Moment::SPEAKER_OTHERS),
    }
}

// ===========================================================================
// Model file layout + presence/ensure (download-on-demand, like Phase 45)
// ===========================================================================

/// Models-dir subfolder for the YAMNet ONNX.
pub const YAMNET_DIR: &str = "yamnet";
/// Models-dir subfolder for the wav2vec2 SER ONNX.
pub const SER_DIR: &str = "wav2vec2-ser";

/// YAMNet ONNX file (relative path under [`YAMNET_DIR`]).
pub const YAMNET_FILE: &str = "model.onnx";
/// wav2vec2 SER ONNX file (relative path under [`SER_DIR`]).
pub const SER_FILE: &str = "model.onnx";

// FLAG / TODO-VERIFY (download URLs):
//
// The wav2vec2 SER source is verified: onnx-community/
// wav2vec2-base-Speech_Emotion_Recognition-ONNX hosts `onnx/model.onnx` (and
// quantized variants) under Apache-2.0, with the id2label above.
//
// The YAMNet source named in the plan (STMicroelectronics/yamnet) does NOT host
// a plain waveform→521-class ONNX — that repo only carries an ESC-10
// transfer-learned variant that takes precomputed (64,96,1) mel patches and
// emits embeddings, NOT raw-waveform→AudioSet scores. So the YAMNet download
// URL is left as a TODO below and the runtime is written against the CANONICAL
// waveform-in/521-out YAMNet contract. A verified Apache-2.0 hosted .onnx of
// that exact model must be slotted in before the YAMNet path can run.
// (See the task report.)
#[cfg(feature = "sentiment")]
const SER_URL: &str =
    "https://huggingface.co/onnx-community/wav2vec2-base-Speech_Emotion_Recognition-ONNX/resolve/main/onnx/model.onnx";
// TODO verify URL — canonical waveform→521 YAMNet ONNX (Apache-2.0). Plan's
// STMicroelectronics/yamnet repo does not host this exact model (see above).
#[cfg(feature = "sentiment")]
const YAMNET_URL: &str = "";

/// True if the YAMNet model file is present in the models dir.
pub fn yamnet_present() -> bool {
    model_present(YAMNET_DIR, YAMNET_FILE)
}

/// True if the wav2vec2 SER model file is present in the models dir.
pub fn ser_present() -> bool {
    model_present(SER_DIR, SER_FILE)
}

/// True if both sentiment models are present (the worker's run gate).
pub fn models_present() -> bool {
    yamnet_present() && ser_present()
}

fn model_present(dir: &str, file: &str) -> bool {
    let Ok(root) = zord_config::models_dir() else {
        return false;
    };
    let p = root.join(dir).join(file);
    std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false)
}

// ===========================================================================
// Model runtime (ISOLATED, feature-gated, NOT CI-VERIFIABLE — flagged LIVE-TEST)
// ===========================================================================
//
// Everything below this line touches real ONNX inference and the network. It
// is `#[cfg(feature = "sentiment")]` so the default build never compiles `ort`.
// None of it can be exercised in CI (the models won't download/run there); each
// behaviour that must be checked against real model I/O is marked `LIVE-TEST`.
// The pure functions above are what the test suite verifies.
#[cfg(feature = "sentiment")]
pub use runtime::*;

#[cfg(feature = "sentiment")]
mod runtime {
    use super::*;
    use anyhow::{anyhow, Context, Result};
    use ndarray::Array;
    use ort::{session::Session, value::Value};
    use std::path::PathBuf;

    /// Resolve `<models_dir>/<dir>/<file>`, creating the subfolder.
    fn model_path(dir: &str, file: &str) -> Result<PathBuf> {
        let root = zord_config::models_dir().context("resolving models dir")?;
        let sub = root.join(dir);
        std::fs::create_dir_all(&sub).ok();
        Ok(sub.join(file))
    }

    /// Download the wav2vec2 SER model if absent. Returns Ok(true) if the file
    /// is present afterward. Never blocks the caller's decision-making — the
    /// worker calls this only on an explicit Backfill/run path.
    pub fn ensure_ser(progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<bool> {
        let dest = model_path(SER_DIR, SER_FILE)?;
        if super::ser_present() {
            return Ok(true);
        }
        zord_net::download_to_file(SER_URL, &dest, progress)
            .with_context(|| format!("downloading wav2vec2 SER model to {}", dest.display()))?;
        Ok(super::ser_present())
    }

    /// Download the YAMNet model if absent.
    ///
    /// FLAG: `YAMNET_URL` is intentionally empty — a verified Apache-2.0,
    /// waveform→521-class YAMNet ONNX could not be located (the plan's
    /// STMicroelectronics/yamnet repo hosts only a mel-patch ESC-10 transfer
    /// variant). Until a verified URL is slotted in, this returns Ok(false) so
    /// the worker cleanly skips the event pass instead of downloading the wrong
    /// model. See the task report.
    pub fn ensure_yamnet(progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<bool> {
        if super::yamnet_present() {
            return Ok(true);
        }
        if YAMNET_URL.is_empty() {
            // No verified source — do not guess.
            return Ok(false);
        }
        let dest = model_path(YAMNET_DIR, YAMNET_FILE)?;
        zord_net::download_to_file(YAMNET_URL, &dest, progress)
            .with_context(|| format!("downloading YAMNet model to {}", dest.display()))?;
        Ok(super::yamnet_present())
    }

    /// A loaded ONNX session plus its single input name (resolved at load so we
    /// don't hardcode it — different exports name the waveform input differently).
    pub struct OnnxModel {
        session: Session,
        input_name: String,
    }

    impl OnnxModel {
        /// Load an ONNX model from `<models_dir>/<dir>/<file>`.
        /// LIVE-TEST: confirms the file parses and the expected input exists.
        fn load(dir: &str, file: &str) -> Result<Self> {
            let path = model_path(dir, file)?;
            let session = Session::builder()
                .map_err(|e| anyhow!("ort session builder: {e}"))?
                .with_intra_threads(2)
                .map_err(|e| anyhow!("ort threads: {e}"))?
                .commit_from_file(&path)
                .map_err(|e| anyhow!("loading {}: {e}", path.display()))?;
            let input_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .ok_or_else(|| anyhow!("{}: model has no inputs", path.display()))?;
            Ok(Self {
                session,
                input_name,
            })
        }
    }

    /// YAMNet wrapper: 1-D 16 kHz mono waveform → per-frame AudioSet scores.
    pub struct Yamnet(OnnxModel);

    impl Yamnet {
        pub fn load() -> Result<Self> {
            Ok(Self(OnnxModel::load(YAMNET_DIR, YAMNET_FILE)?))
        }

        /// Run YAMNet over a whole 16 kHz mono track and return the event hits
        /// (frames crossing a class threshold) in frame order.
        ///
        /// LIVE-TEST: the canonical TF Hub YAMNet takes a 1-D `[N]` float32
        /// waveform and returns `scores [frames, 521]`. The exact input rank /
        /// output ordering of a given int8 ONNX export MUST be confirmed on a
        /// real model — if the export expects a batched `[1, N]` input or emits
        /// a transposed output, adjust the reshape below. We read the first
        /// output as a 2-D `[frames, 521]` f32 array.
        pub fn events(&mut self, waveform: &[f32]) -> Result<Vec<EventHit>> {
            if waveform.is_empty() {
                return Ok(Vec::new());
            }
            let input = Array::from_shape_vec((waveform.len(),), waveform.to_vec())
                .context("shaping YAMNet input")?;
            let inputs = ort::inputs![
                self.0.input_name.as_str() => Value::from_array(input)
                    .map_err(|e| anyhow!("yamnet input value: {e}"))?,
            ];
            let outputs = self
                .0
                .session
                .run(inputs)
                .map_err(|e| anyhow!("yamnet run: {e}"))?;
            // Scores: [frames, 521]. (LIVE-TEST: confirm output index 0 is the
            // per-frame scores, not embeddings/spectrogram.)
            let scores = outputs[0]
                .try_extract_array::<f32>()
                .map_err(|e| anyhow!("yamnet output extract: {e}"))?;
            let dims = scores.shape();
            if dims.len() != 2 {
                return Err(anyhow!(
                    "yamnet output rank {} (expected 2: [frames, classes])",
                    dims.len()
                ));
            }
            let (frames, classes) = (dims[0], dims[1]);
            let mut hits = Vec::new();
            for f in 0..frames {
                for ec in EVENT_CLASSES {
                    if ec.index >= classes {
                        continue; // defensive: wrong class count
                    }
                    let s = scores[[f, ec.index]];
                    if s >= ec.threshold {
                        hits.push((f, ec.kind, s));
                    }
                }
            }
            Ok(hits)
        }
    }

    /// wav2vec2 SER wrapper: normalized 16 kHz mono utterance → emotion logits.
    pub struct Ser(OnnxModel);

    impl Ser {
        pub fn load() -> Result<Self> {
            Ok(Self(OnnxModel::load(SER_DIR, SER_FILE)?))
        }

        /// Classify one utterance (raw 16 kHz mono samples). Returns
        /// `(label_index, probability)`. The waveform is zero-mean/unit-var
        /// normalized (the pure [`normalize_waveform`]) and fed as `[1, N]`.
        ///
        /// LIVE-TEST: wav2vec2 ONNX exports usually take a batched `[1, N]`
        /// `input_values` tensor and emit `logits [1, 6]`. Confirm against the
        /// real model; adjust the input rank / output index if the export
        /// differs. The id2label order is encoded in [`EMOTION_LABELS`].
        pub fn classify(&mut self, samples: &[f32]) -> Result<(usize, f32)> {
            if samples.is_empty() {
                return Ok((NEUTRAL_INDEX, 0.0));
            }
            let norm = normalize_waveform(samples);
            let input =
                Array::from_shape_vec((1, norm.len()), norm).context("shaping SER input")?;
            let inputs = ort::inputs![
                self.0.input_name.as_str() => Value::from_array(input)
                    .map_err(|e| anyhow!("ser input value: {e}"))?,
            ];
            let outputs = self
                .0
                .session
                .run(inputs)
                .map_err(|e| anyhow!("ser run: {e}"))?;
            let logits = outputs[0]
                .try_extract_array::<f32>()
                .map_err(|e| anyhow!("ser output extract: {e}"))?;
            // Flatten to the last-axis logits (handles [6] or [1,6]).
            let flat: Vec<f32> = logits.iter().copied().collect();
            let probs = softmax(&flat);
            match argmax(&probs) {
                Some((idx, p)) => Ok((idx, p)),
                None => Ok((NEUTRAL_INDEX, 0.0)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- frame_to_t_ms -----------------------------------------------------
    #[test]
    fn frame_time_math() {
        assert_eq!(frame_to_t_ms(0, YAMNET_HOP_MS), 0);
        assert_eq!(frame_to_t_ms(1, YAMNET_HOP_MS), 480);
        assert_eq!(frame_to_t_ms(10, 480), 4_800);
        assert_eq!(frame_to_t_ms(3, 100), 300);
    }

    // ---- collapse_events ---------------------------------------------------
    #[test]
    fn collapse_merges_consecutive_same_kind() {
        // Frames 0,1,2 all laughter within gap → ONE moment at t=0, peak score.
        let hits = vec![
            (0, "laughter", 0.6),
            (1, "laughter", 0.9),
            (2, "laughter", 0.7),
        ];
        let out = collapse_events(&hits, 480, EVENT_COLLAPSE_MAX_GAP_MS, 3);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].t_ms, 0);
        assert_eq!(out[0].kind, "laughter");
        assert!((out[0].score - 0.9).abs() < 1e-6, "peak score kept");
        assert_eq!(out[0].speaker, 3);
    }

    #[test]
    fn collapse_splits_on_large_gap() {
        // Two laughter bursts separated by a big frame gap → TWO moments.
        // hop=480ms; frame 0 then frame 10 (=4800ms) → gap 4800 > 1000.
        let hits = vec![(0, "laughter", 0.6), (10, "laughter", 0.8)];
        let out = collapse_events(&hits, 480, EVENT_COLLAPSE_MAX_GAP_MS, 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].t_ms, 0);
        assert_eq!(out[1].t_ms, 4_800);
    }

    #[test]
    fn collapse_keeps_distinct_kinds_separate() {
        // Two different kinds in the SAME frame must not merge; the kind
        // tiebreak (after equal t_ms) orders them alphabetically.
        let hits = vec![(0, "laughter", 0.6), (0, "applause", 0.7)];
        let out = collapse_events(&hits, 480, EVENT_COLLAPSE_MAX_GAP_MS, 0);
        assert_eq!(out.len(), 2);
        // sorted by t_ms (equal) then kind: "applause" < "laughter".
        assert_eq!(out[0].kind, "applause");
        assert_eq!(out[1].kind, "laughter");
    }

    #[test]
    fn collapse_empty() {
        assert!(collapse_events(&[], 480, 1_000, 0).is_empty());
    }

    // ---- persistent_emotion ------------------------------------------------
    #[test]
    fn isolated_spike_suppressed() {
        // happy(idx 4) once, surrounded by neutral → no moment (n=3).
        let utts = vec![
            (0, NEUTRAL_INDEX, 0.9),
            (1_000, 4, 0.9),
            (2_000, NEUTRAL_INDEX, 0.9),
        ];
        let out = persistent_emotion(&utts, EMOTION_PERSIST_N, EMOTION_MIN_SCORE, 1);
        assert!(out.is_empty(), "single spike must not emit");
    }

    #[test]
    fn three_in_a_row_emits() {
        let utts = vec![(0, 4, 0.7), (1_000, 4, 0.8), (2_000, 4, 0.9)];
        let out = persistent_emotion(&utts, 3, EMOTION_MIN_SCORE, 7);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "emotion:happy");
        assert_eq!(out[0].t_ms, 0, "placed at first utterance of run");
        assert_eq!(out[0].speaker, 7);
        // mean of 0.7,0.8,0.9 = 0.8
        assert!((out[0].score - 0.8).abs() < 1e-6);
    }

    #[test]
    fn neutral_never_emitted() {
        // A long neutral run must never produce a moment.
        let utts = vec![
            (0, NEUTRAL_INDEX, 0.99),
            (1_000, NEUTRAL_INDEX, 0.99),
            (2_000, NEUTRAL_INDEX, 0.99),
            (3_000, NEUTRAL_INDEX, 0.99),
        ];
        let out = persistent_emotion(&utts, 3, EMOTION_MIN_SCORE, 0);
        assert!(out.is_empty());
    }

    #[test]
    fn weak_scores_break_run() {
        // Strong, weak, strong, strong — the weak one breaks; no 3-in-a-row.
        let utts = vec![
            (0, 1, 0.9),
            (1_000, 1, 0.3), // below min_score → breaks
            (2_000, 1, 0.9),
            (3_000, 1, 0.9),
        ];
        let out = persistent_emotion(&utts, 3, EMOTION_MIN_SCORE, 0);
        // After the weak one, only 2 strong remain → still no emit.
        assert!(out.is_empty());
    }

    #[test]
    fn changing_label_breaks_run() {
        // angry, angry, happy, happy, happy — only happy run (3) qualifies.
        let utts = vec![
            (0, 1, 0.9),     // angry
            (1_000, 1, 0.9), // angry
            (2_000, 4, 0.9), // happy
            (3_000, 4, 0.9), // happy
            (4_000, 4, 0.9), // happy
        ];
        let out = persistent_emotion(&utts, 3, EMOTION_MIN_SCORE, 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, "emotion:happy");
        assert_eq!(out[0].t_ms, 2_000);
    }

    #[test]
    fn two_qualifying_runs() {
        // angry x3, neutral, happy x3 → two moments.
        let utts = vec![
            (0, 1, 0.9),
            (1_000, 1, 0.9),
            (2_000, 1, 0.9),
            (3_000, NEUTRAL_INDEX, 0.9),
            (4_000, 4, 0.9),
            (5_000, 4, 0.9),
            (6_000, 4, 0.9),
        ];
        let out = persistent_emotion(&utts, 3, EMOTION_MIN_SCORE, 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].kind, "emotion:angry");
        assert_eq!(out[1].kind, "emotion:happy");
    }

    // ---- normalize_waveform ------------------------------------------------
    #[test]
    fn normalize_zero_mean_unit_var() {
        // Known vector: [1,2,3,4,5] → mean 3, pop var 2, std sqrt(2).
        let out = normalize_waveform(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!(mean.abs() < 1e-6, "mean ~0");
        let var = out.iter().map(|x| x * x).sum::<f32>() / out.len() as f32;
        assert!((var - 1.0).abs() < 1e-5, "unit variance, got {var}");
        // Exact first element: (1-3)/sqrt(2) = -1.41421356
        assert!((out[0] - (-2.0 / 2.0_f32.sqrt())).abs() < 1e-5);
    }

    #[test]
    fn normalize_constant_signal_no_nan() {
        let out = normalize_waveform(&[2.0, 2.0, 2.0]);
        assert!(
            out.iter().all(|x| x.abs() < 1e-6),
            "constant → zero-centered"
        );
        assert!(out.iter().all(|x| x.is_finite()), "no NaN/inf on zero var");
    }

    #[test]
    fn normalize_empty() {
        assert!(normalize_waveform(&[]).is_empty());
    }

    // ---- argmax / softmax --------------------------------------------------
    #[test]
    fn argmax_basic() {
        assert_eq!(argmax(&[0.1, 0.9, 0.3]), Some((1, 0.9)));
        assert_eq!(argmax(&[]), None);
        // tie → lowest index
        assert_eq!(argmax(&[0.5, 0.5]), Some((0, 0.5)));
    }

    #[test]
    fn softmax_sums_to_one() {
        let p = softmax(&[1.0, 2.0, 3.0]);
        let s: f32 = p.iter().sum();
        assert!((s - 1.0).abs() < 1e-6);
        // monotonic: largest logit → largest prob
        assert!(p[2] > p[1] && p[1] > p[0]);
        assert!(softmax(&[]).is_empty());
    }

    // ---- merge / sort / track mapping --------------------------------------
    #[test]
    fn merge_dedups_keeps_higher_score() {
        let a = vec![Moment {
            t_ms: 100,
            kind: "laughter".into(),
            speaker: 0,
            score: 0.6,
        }];
        let b = vec![Moment {
            t_ms: 100,
            kind: "laughter".into(),
            speaker: 0,
            score: 0.9,
        }];
        let out = merge_moments(vec![a, b]);
        assert_eq!(out.len(), 1);
        assert!((out[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn merge_orders_by_time_then_kind() {
        let parts = vec![vec![
            Moment {
                t_ms: 200,
                kind: "applause".into(),
                speaker: 0,
                score: 0.8,
            },
            Moment {
                t_ms: 100,
                kind: "laughter".into(),
                speaker: 0,
                score: 0.8,
            },
            Moment {
                t_ms: 100,
                kind: "applause".into(),
                speaker: 0,
                score: 0.8,
            },
        ]];
        let out = merge_moments(parts);
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].t_ms, out[0].kind.as_str()), (100, "applause"));
        assert_eq!((out[1].t_ms, out[1].kind.as_str()), (100, "laughter"));
        assert_eq!((out[2].t_ms, out[2].kind.as_str()), (200, "applause"));
    }

    #[test]
    fn track_speaker_mapping() {
        assert_eq!(track_speaker("me"), Moment::SPEAKER_ME);
        assert_eq!(track_speaker("others"), Moment::SPEAKER_OTHERS);
        assert_eq!(track_speaker("spk-0"), 0);
        assert_eq!(track_speaker("spk-5"), 5);
        // Unparseable → others sentinel (defensive).
        assert_eq!(track_speaker("spk-x"), Moment::SPEAKER_OTHERS);
    }
}
