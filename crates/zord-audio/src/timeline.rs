//! Streaming per-track amplitude peak computation for the session timeline
//! (Phase 42a/42d).
//!
//! Processes each retained track file in **streaming blocks** — never slurping
//! a full hour into memory — and folds sample values into a fixed-size
//! [`PEAK_BUCKETS`] array. Phase 42d also accumulates per-bucket RMS so that a
//! `speech: Vec<bool>` flag vector can be derived with the same relative-floor
//! logic as `gather_speech` in `zord-diarize`. Works on both `.wav` and
//! `.opus` files.

use std::path::Path;

use anyhow::Result;

/// Number of amplitude buckets across the full duration of a track.
/// At one hour that is roughly one bucket per 2.4 s.
pub const PEAK_BUCKETS: usize = 1500;

/// Fold one decoded block into the running peaks **and** RMS accumulator
/// arrays.
///
/// `start_sample` is the zero-based index of the first sample in `block`
/// within the track. `total_samples` is the total frame count of the track
/// (from the file header or granule). Samples are mono `f32` in `[-1, 1]`;
/// peak per bucket is `max(|s|)` normalised to `[0, 1]`. The RMS accumulator
/// (`rms_sum`, `rms_count`) stores the squared-sample sum and sample count so
/// `sqrt(sum/count)` yields bucket RMS after the full pass.
///
/// This is a pure function with no I/O — easy to unit-test.
pub fn fold_peaks(block: &[f32], start_sample: u64, total_samples: u64, peaks: &mut [f32]) {
    if total_samples == 0 || peaks.is_empty() {
        return;
    }
    let n = peaks.len() as u64;
    for (i, &s) in block.iter().enumerate() {
        let pos = start_sample + i as u64;
        if pos >= total_samples {
            break;
        }
        let bucket = (pos * n / total_samples) as usize;
        let bucket = bucket.min(peaks.len() - 1);
        let abs = s.abs().min(1.0);
        if abs > peaks[bucket] {
            peaks[bucket] = abs;
        }
    }
}

/// Fold one decoded block into both the running peaks array **and** per-bucket
/// RMS accumulators (squared-sum + count). Call this instead of [`fold_peaks`]
/// when speech detection is needed; after the full pass call
/// [`speech_from_rms`] to derive the `Vec<bool>`.
pub fn fold_peaks_and_rms(
    block: &[f32],
    start_sample: u64,
    total_samples: u64,
    peaks: &mut [f32],
    rms_sum: &mut [f64],
    rms_count: &mut [u64],
) {
    if total_samples == 0 || peaks.is_empty() {
        return;
    }
    let n = peaks.len() as u64;
    for (i, &s) in block.iter().enumerate() {
        let pos = start_sample + i as u64;
        if pos >= total_samples {
            break;
        }
        let bucket = (pos * n / total_samples) as usize;
        let bucket = bucket.min(peaks.len() - 1);
        let abs = s.abs().min(1.0);
        if abs > peaks[bucket] {
            peaks[bucket] = abs;
        }
        rms_sum[bucket] += (s as f64) * (s as f64);
        rms_count[bucket] += 1;
    }
}

/// Convert per-bucket RMS accumulators into a `Vec<bool>` speech-activity
/// flag, mirroring `gather_speech`'s relative-floor logic:
/// bucket is speech when its RMS ≥ max(peak_rms * 0.1, 1e-4).
///
/// The result has the same length as `rms_sum`. Returns all-false for an
/// all-silent track.
pub fn speech_from_rms(rms_sum: &[f64], rms_count: &[u64]) -> Vec<bool> {
    let bucket_rms: Vec<f32> = rms_sum
        .iter()
        .zip(rms_count.iter())
        .map(|(&sum, &cnt)| {
            if cnt == 0 {
                0.0f32
            } else {
                (sum / cnt as f64).sqrt() as f32
            }
        })
        .collect();
    let peak_rms = bucket_rms.iter().cloned().fold(0.0f32, f32::max);
    let floor = (peak_rms * 0.1).max(1e-4);
    bucket_rms.iter().map(|&r| r >= floor).collect()
}

/// Compute `PEAK_BUCKETS` normalized peak values **and** per-bucket speech
/// flags for the audio file at `path` (.wav or .opus), streaming block by
/// block.
///
/// Returns `(peaks, speech, duration_ms)`.
///
/// **Opus with `total_samples = None`**: our encoder always writes the final
/// granule, so `OpusBlocks::total_samples()` is `Some` for every Zord-written
/// file. If (pathologically) it is absent, we do a two-pass approach: first
/// pass counts total frames, second pass fills buckets. This is rare enough
/// that the overhead is acceptable.
pub fn compute_track_peaks(path: &Path) -> Result<(Vec<f32>, Vec<bool>, u64)> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());

    if ext.as_deref() == Some("opus") {
        compute_opus_peaks(path)
    } else {
        compute_wav_peaks(path)
    }
}

fn compute_wav_peaks(path: &Path) -> Result<(Vec<f32>, Vec<bool>, u64)> {
    use crate::wav::{repair_wav_header, validate_wav_spec};
    let _ = repair_wav_header(path);

    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    validate_wav_spec(spec)?;

    let rate = spec.sample_rate as u64;
    let channels = spec.channels.max(1) as usize;
    let total_mono = reader.duration() as u64; // mono frames in the file
    let duration_ms = total_mono * 1000 / rate.max(1);

    let mut peaks = vec![0.0f32; PEAK_BUCKETS];
    let mut rms_sum = vec![0.0f64; PEAK_BUCKETS];
    let mut rms_count = vec![0u64; PEAK_BUCKETS];

    // Re-open for streaming (hound's iterator is the only practical way).
    let _ = repair_wav_header(path);
    let mut reader = hound::WavReader::open(path)?;
    let spec2 = reader.spec();
    let scale = match spec2.sample_format {
        hound::SampleFormat::Float => 1.0_f32,
        hound::SampleFormat::Int => 1.0 / (1i64 << (spec2.bits_per_sample - 1)) as f32,
    };
    // Buffer about 1 s worth of interleaved samples per chunk.
    let chunk_frames = spec2.sample_rate as usize;
    let chunk_len = chunk_frames * channels;

    let mut buf: Vec<f32> = Vec::with_capacity(chunk_len);
    let mut mono_buf: Vec<f32> = Vec::with_capacity(chunk_frames);
    let mut frame_pos: u64 = 0;

    // Flush `buf` (interleaved) into peaks + rms accumulators, then clear it.
    let flush = |buf: &mut Vec<f32>,
                 mono_buf: &mut Vec<f32>,
                 frame_pos: &mut u64,
                 peaks: &mut Vec<f32>,
                 rms_sum: &mut Vec<f64>,
                 rms_count: &mut Vec<u64>| {
        mono_buf.clear();
        for frame in buf.chunks(channels) {
            let sum: f32 = frame.iter().sum();
            mono_buf.push(sum / channels as f32);
        }
        fold_peaks_and_rms(mono_buf, *frame_pos, total_mono, peaks, rms_sum, rms_count);
        *frame_pos += mono_buf.len() as u64;
        buf.clear();
    };

    match spec2.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                buf.push(s?);
                if buf.len() >= chunk_len {
                    flush(
                        &mut buf,
                        &mut mono_buf,
                        &mut frame_pos,
                        &mut peaks,
                        &mut rms_sum,
                        &mut rms_count,
                    );
                }
            }
        }
        hound::SampleFormat::Int => {
            for s in reader.samples::<i32>() {
                buf.push(s? as f32 * scale);
                if buf.len() >= chunk_len {
                    flush(
                        &mut buf,
                        &mut mono_buf,
                        &mut frame_pos,
                        &mut peaks,
                        &mut rms_sum,
                        &mut rms_count,
                    );
                }
            }
        }
    }
    if !buf.is_empty() {
        flush(
            &mut buf,
            &mut mono_buf,
            &mut frame_pos,
            &mut peaks,
            &mut rms_sum,
            &mut rms_count,
        );
    }

    let speech = speech_from_rms(&rms_sum, &rms_count);
    Ok((peaks, speech, duration_ms))
}

fn compute_opus_peaks(path: &Path) -> Result<(Vec<f32>, Vec<bool>, u64)> {
    use crate::compress::OpusBlocks;

    let mut blocks = OpusBlocks::open(path)?;
    let rate = blocks.sample_rate() as u64; // always 48 000

    if let Some(total) = blocks.total_samples() {
        // Fast path: total known → single pass.
        let duration_ms = total * 1000 / rate;
        let mut peaks = vec![0.0f32; PEAK_BUCKETS];
        let mut rms_sum = vec![0.0f64; PEAK_BUCKETS];
        let mut rms_count = vec![0u64; PEAK_BUCKETS];
        let mut pos: u64 = 0;
        while let Some(block) = blocks.next_block()? {
            fold_peaks_and_rms(&block, pos, total, &mut peaks, &mut rms_sum, &mut rms_count);
            pos += block.len() as u64;
        }
        let speech = speech_from_rms(&rms_sum, &rms_count);
        Ok((peaks, speech, duration_ms))
    } else {
        // Slow path (no final granule — shouldn't happen for Zord-written files):
        // first pass to count frames, second pass to fill buckets.
        tracing::warn!(
            path = %path.display(),
            "opus file missing final granule — falling back to two-pass peak scan"
        );
        let mut total: u64 = 0;
        while let Some(block) = blocks.next_block()? {
            total += block.len() as u64;
        }
        let duration_ms = total * 1000 / rate;
        if total == 0 {
            let speech = vec![false; PEAK_BUCKETS];
            return Ok((vec![0.0f32; PEAK_BUCKETS], speech, 0));
        }
        // Second pass.
        let mut blocks2 = OpusBlocks::open(path)?;
        let mut peaks = vec![0.0f32; PEAK_BUCKETS];
        let mut rms_sum = vec![0.0f64; PEAK_BUCKETS];
        let mut rms_count = vec![0u64; PEAK_BUCKETS];
        let mut pos: u64 = 0;
        while let Some(block) = blocks2.next_block()? {
            fold_peaks_and_rms(&block, pos, total, &mut peaks, &mut rms_sum, &mut rms_count);
            pos += block.len() as u64;
        }
        let speech = speech_from_rms(&rms_sum, &rms_count);
        Ok((peaks, speech, duration_ms))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::WavWriter;

    /// A 3-bucket signal: quiet · loud · quiet.
    /// The middle bucket's peak should be near the loud amplitude, the outer
    /// buckets should be near zero.
    #[test]
    fn fold_peaks_loud_middle_bucket() {
        let total: u64 = 300;
        let mut peaks = vec![0.0f32; 3];

        // Bucket 0: quiet (samples 0..100)
        let quiet: Vec<f32> = vec![0.01; 100];
        fold_peaks(&quiet, 0, total, &mut peaks);

        // Bucket 1: loud (samples 100..200)
        let loud: Vec<f32> = vec![0.8; 100];
        fold_peaks(&loud, 100, total, &mut peaks);

        // Bucket 2: quiet again (samples 200..300)
        let quiet2: Vec<f32> = vec![0.01; 100];
        fold_peaks(&quiet2, 200, total, &mut peaks);

        assert!(
            peaks[1] > 0.79,
            "middle bucket should be loud, got {}",
            peaks[1]
        );
        assert!(
            peaks[0] < 0.02,
            "first bucket should be quiet, got {}",
            peaks[0]
        );
        assert!(
            peaks[2] < 0.02,
            "last bucket should be quiet, got {}",
            peaks[2]
        );
    }

    /// Duration math: a 1 s track at 16 kHz should give duration_ms = 1000.
    #[test]
    fn duration_ms_math() {
        let dir = std::env::temp_dir().join(format!("zord-tl-dur-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dur.wav");

        let rate = 16_000u32;
        let mut w = WavWriter::create(&path, rate).unwrap();
        w.write(&vec![0.1f32; rate as usize]).unwrap(); // exactly 1 s
        w.finalize().unwrap();

        let (_peaks, _speech, dur_ms) = compute_track_peaks(&path).unwrap();
        assert_eq!(dur_ms, 1000, "duration_ms should be 1000, got {dur_ms}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A real WAV: loud second, quiet first and third thirds.
    /// The middle PEAK_BUCKETS/3 buckets should be louder than the edges.
    #[test]
    fn wav_peaks_loud_middle_third() {
        let dir = std::env::temp_dir().join(format!("zord-tl-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("loud_mid.wav");

        let rate = 16_000u32;
        let mut w = WavWriter::create(&path, rate).unwrap();
        // 3 s total: quiet · loud · quiet
        let mut samples = Vec::new();
        samples.extend(std::iter::repeat_n(0.01f32, rate as usize));
        samples.extend(std::iter::repeat_n(0.8f32, rate as usize));
        samples.extend(std::iter::repeat_n(0.01f32, rate as usize));
        w.write(&samples).unwrap();
        w.finalize().unwrap();

        let (peaks, _speech, dur_ms) = compute_track_peaks(&path).unwrap();
        assert_eq!(dur_ms, 3000);
        assert_eq!(peaks.len(), PEAK_BUCKETS);

        let third = PEAK_BUCKETS / 3;
        let mid_max = peaks[third..2 * third]
            .iter()
            .cloned()
            .fold(0.0f32, f32::max);
        let edge_max = peaks[..third]
            .iter()
            .chain(peaks[2 * third..].iter())
            .cloned()
            .fold(0.0f32, f32::max);
        assert!(mid_max > 0.7, "middle third should be loud, got {mid_max}");
        assert!(edge_max < 0.05, "edges should be quiet, got {edge_max}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Speech flags: silence · tone · silence → only middle buckets true.
    #[test]
    fn speech_flags_silence_tone_silence() {
        let dir = std::env::temp_dir().join(format!("zord-tl-spf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("speech_flags.wav");

        let rate = 16_000u32;
        let mut w = WavWriter::create(&path, rate).unwrap();
        // 3 s: silence · 440 Hz tone · silence
        let mut samples = Vec::new();
        samples.extend(std::iter::repeat_n(0.0f32, rate as usize));
        samples.extend(
            (0..rate).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5),
        );
        samples.extend(std::iter::repeat_n(0.0f32, rate as usize));
        w.write(&samples).unwrap();
        w.finalize().unwrap();

        let (_peaks, speech, _dur_ms) = compute_track_peaks(&path).unwrap();
        assert_eq!(speech.len(), PEAK_BUCKETS);

        let third = PEAK_BUCKETS / 3;
        let mid_true = speech[third..2 * third].iter().filter(|&&b| b).count();
        let edges_true = speech[..third]
            .iter()
            .chain(speech[2 * third..].iter())
            .filter(|&&b| b)
            .count();
        // The middle third should be mostly speech; edges mostly silent.
        assert!(
            mid_true > third / 2,
            "expected most middle buckets to be speech, got {mid_true}/{third}"
        );
        assert!(
            edges_true < third / 4,
            "expected few edge buckets to be speech, got {edges_true}/{third}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `speech_from_rms` pure-function unit test: all-silence → all-false;
    /// middle-loud → middle-true.
    #[test]
    fn speech_from_rms_unit() {
        // All silence → all false.
        let sum = vec![0.0f64; 5];
        let cnt = vec![100u64; 5];
        assert!(speech_from_rms(&sum, &cnt).iter().all(|&b| !b));

        // Middle bucket loud, outer quiet.
        let sum2 = vec![0.0001, 0.0001, 1.0, 0.0001, 0.0001];
        let cnt2 = vec![100u64; 5];
        let flags = speech_from_rms(&sum2, &cnt2);
        assert!(flags[2], "middle bucket should be speech");
        assert!(!flags[0], "first bucket should be silent");
        assert!(!flags[4], "last bucket should be silent");
    }
}
