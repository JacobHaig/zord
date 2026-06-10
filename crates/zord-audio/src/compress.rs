//! Opus compression for kept recordings (Phase 37).
//!
//! Aged session tracks shrink ~96% (WAV → Opus-in-Ogg at 24–48 kbps mono)
//! while staying playable AND consumable by every reader — the
//! [`read_audio_mono_f32`] / [`read_audio_mono_16k`] / [`read_audio_slice_ms`]
//! functions dispatch on extension, so replay, re-transcription, diarization,
//! and the merged export all work on compressed sessions.
//!
//! Container details (RFC 7845): mono 48 kHz, 20 ms frames (960 samples);
//! `pre_skip` = encoder lookahead, written in OpusHead and skipped on decode;
//! per-packet granule advances by 960 with the final granule end-trimmed to
//! `pre_skip + input_samples` so the zero-padded last frame doesn't lengthen
//! the file. Decoders stop at `final_granule - pre_skip`.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{bail, Context, Result};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use ogg::PacketReader;
use opus2::{Application, Bitrate, Channels, Decoder, Encoder};

use crate::wav::validate_wav_spec;
use crate::MonoResampler;

/// Opus's native rate — every compressed track is stored at 48 kHz mono.
const OPUS_RATE: u32 = 48_000;
/// 20 ms frames at 48 kHz.
const FRAME: usize = 960;
/// When slicing, seek this many samples early: up to ~2 s to resync exact
/// positions on a page boundary + the spec's 80 ms (3840-sample) pre-roll.
const SEEK_BACK: u64 = 2 * OPUS_RATE as u64 + 3_840;

/// Quality preset → bitrate (bits/s). Unknown presets get the standard rate.
pub fn opus_bitrate(quality: &str) -> i32 {
    match quality {
        "space" => 24_000,
        "high" => 48_000,
        _ => 32_000,
    }
}

/// Compress a mono/stereo WAV into Opus-in-Ogg at `bitrate`. Streams block
/// by block (an hour-long track never loads whole); the input is downmixed
/// and resampled to 48 kHz mono when needed. Writes `dst` in full before
/// returning — callers use a `.partial` path and rename after verifying.
pub fn compress_wav_to_opus(src: &Path, dst: &Path, bitrate: i32) -> Result<()> {
    let mut reader = hound::WavReader::open(src).context("open wav")?;
    let spec = reader.spec();
    validate_wav_spec(spec)?;
    let channels = spec.channels.max(1) as usize;
    let mut resampler = (spec.sample_rate != OPUS_RATE)
        .then(|| MonoResampler::to_rate(spec.sample_rate, spec.channels, OPUS_RATE))
        .transpose()?;

    let mut enc =
        Encoder::new(OPUS_RATE, Channels::Mono, Application::Voip).context("opus encoder")?;
    enc.set_bitrate(Bitrate::Bits(bitrate)).context("bitrate")?;
    let pre_skip = enc.get_lookahead().unwrap_or(312).max(0) as u16;

    let out = BufWriter::new(File::create(dst).context("create opus")?);
    let mut writer = PacketWriter::new(out);
    let serial: u32 = 0x5a4f5244; // "ZORD"

    // OpusHead — input_sample_rate records the original rate (informational).
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(1); // channel count
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&spec.sample_rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family
    writer
        .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
        .context("write OpusHead")?;

    let vendor = b"zord";
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    writer
        .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
        .context("write OpusTags")?;

    // Stream: WAV blocks → mono 48k → 960-sample frames → packets.
    let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
    let block_len = spec.sample_rate as usize * channels; // ~1 s interleaved
    let mut block: Vec<f32> = Vec::with_capacity(block_len);
    let mut pending: Vec<f32> = Vec::new(); // mono 48k awaiting a full frame
    let mut input_samples_48k: u64 = 0;
    let mut granule: u64 = 0;
    let mut packets: Vec<Vec<u8>> = Vec::new(); // small buffer; flushed per block

    let mut push_block = |block: &mut Vec<f32>,
                          pending: &mut Vec<f32>,
                          enc: &mut Encoder,
                          packets: &mut Vec<Vec<u8>>|
     -> Result<()> {
        if block.is_empty() {
            return Ok(());
        }
        let mono_48k = match resampler.as_mut() {
            Some(r) => r.process(block)?,
            None if channels > 1 => block
                .chunks(channels)
                .map(|f| f.iter().sum::<f32>() / f.len() as f32)
                .collect(),
            None => std::mem::take(block),
        };
        input_samples_48k += mono_48k.len() as u64;
        pending.extend_from_slice(&mono_48k);
        block.clear();
        while pending.len() >= FRAME {
            let frame: Vec<f32> = pending.drain(..FRAME).collect();
            packets.push(enc.encode_vec_float(&frame, 4000).context("encode")?);
        }
        Ok(())
    };

    macro_rules! flush_packets {
        () => {
            for p in packets.drain(..) {
                granule += FRAME as u64;
                writer
                    .write_packet(p, serial, PacketWriteEndInfo::NormalPacket, granule)
                    .context("write packet")?;
            }
        };
    }

    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                block.push(s?);
                if block.len() >= block_len {
                    push_block(&mut block, &mut pending, &mut enc, &mut packets)?;
                    flush_packets!();
                }
            }
        }
        hound::SampleFormat::Int => {
            for s in reader.samples::<i32>() {
                block.push(s? as f32 * scale);
                if block.len() >= block_len {
                    push_block(&mut block, &mut pending, &mut enc, &mut packets)?;
                    flush_packets!();
                }
            }
        }
    }
    push_block(&mut block, &mut pending, &mut enc, &mut packets)?;
    flush_packets!();

    // Final (possibly zero-padded) frame, end-trimmed via the granule.
    let mut last = std::mem::take(&mut pending);
    if !last.is_empty() || input_samples_48k == 0 {
        last.resize(FRAME, 0.0);
        let p = enc.encode_vec_float(&last, 4000).context("encode tail")?;
        let final_granule = pre_skip as u64 + input_samples_48k;
        writer
            .write_packet(p, serial, PacketWriteEndInfo::EndStream, final_granule)
            .context("write tail")?;
    } else {
        // Input ended exactly on a frame boundary: re-mark the stream end by
        // an empty-but-valid trailing frame carrying the trim granule.
        let silent = vec![0.0f32; FRAME];
        let p = enc.encode_vec_float(&silent, 4000).context("encode tail")?;
        let final_granule = pre_skip as u64 + input_samples_48k;
        writer
            .write_packet(p, serial, PacketWriteEndInfo::EndStream, final_granule)
            .context("write tail")?;
    }
    Ok(())
}

/// Pull-based 48 kHz mono block reader over an Opus-in-Ogg file. Pre-skip is
/// consumed internally; output stops at the end-trim granule.
pub struct OpusBlocks {
    reader: PacketReader<BufReader<File>>,
    decoder: Decoder,
    pre_skip_left: u64,
    emitted: u64,
    total: Option<u64>,
    headers_done: bool,
}

impl OpusBlocks {
    pub fn open(path: &Path) -> Result<Self> {
        let total = final_granule(path)?;
        let file = BufReader::new(File::open(path).context("open opus")?);
        let mut me = Self {
            reader: PacketReader::new(file),
            decoder: Decoder::new(OPUS_RATE, Channels::Mono).context("opus decoder")?,
            pre_skip_left: 0,
            emitted: 0,
            total: None,
            headers_done: false,
        };
        let pre_skip = me.read_headers()?;
        me.pre_skip_left = pre_skip as u64;
        me.total = total.map(|g| g.saturating_sub(pre_skip as u64));
        Ok(me)
    }

    /// Always 48 kHz (Opus's native decode rate).
    pub fn sample_rate(&self) -> u32 {
        OPUS_RATE
    }

    /// Decoded length in samples (from the end-trim granule), when the file
    /// carries one.
    pub fn total_samples(&self) -> Option<u64> {
        self.total
    }

    fn read_headers(&mut self) -> Result<u16> {
        let head = self
            .reader
            .read_packet()
            .context("read OpusHead")?
            .context("empty opus file")?;
        if head.data.len() < 19 || &head.data[..8] != b"OpusHead" {
            bail!("not an Opus stream (missing OpusHead)");
        }
        let pre_skip = u16::from_le_bytes([head.data[10], head.data[11]]);
        let _tags = self
            .reader
            .read_packet()
            .context("read OpusTags")?
            .context("truncated opus file")?;
        self.headers_done = true;
        Ok(pre_skip)
    }

    /// Next decoded block (one packet's worth, ≤ 960 samples after trims).
    /// `Ok(None)` = end of stream.
    pub fn next_block(&mut self) -> Result<Option<Vec<f32>>> {
        loop {
            if let Some(total) = self.total {
                if self.emitted >= total {
                    return Ok(None);
                }
            }
            let Some(pkt) = self.reader.read_packet().context("read packet")? else {
                return Ok(None);
            };
            let mut pcm = vec![0.0f32; FRAME * 2];
            let n = self
                .decoder
                .decode_float(&pkt.data, &mut pcm, false)
                .context("decode")?;
            pcm.truncate(n);
            // Consume pre-skip.
            if self.pre_skip_left > 0 {
                let skip = (self.pre_skip_left as usize).min(pcm.len());
                pcm.drain(..skip);
                self.pre_skip_left -= skip as u64;
                if pcm.is_empty() {
                    continue;
                }
            }
            // End-trim.
            if let Some(total) = self.total {
                let left = (total - self.emitted) as usize;
                if pcm.len() > left {
                    pcm.truncate(left);
                }
            }
            self.emitted += pcm.len() as u64;
            if pcm.is_empty() {
                continue;
            }
            return Ok(Some(pcm));
        }
    }
}

/// The stream's final granule position (scan to the last page cheaply by
/// seeking near the end is overkill at our sizes — read pages forward).
fn final_granule(path: &Path) -> Result<Option<u64>> {
    let file = BufReader::new(File::open(path).context("open opus")?);
    let mut reader = PacketReader::new(file);
    let mut last: Option<u64> = None;
    while let Some(pkt) = reader.read_packet().context("scan packet")? {
        last = Some(pkt.absgp_page());
    }
    Ok(last)
}

// ---------------------------------------------------------------------------
// Format-dispatching readers: `.opus` → decode path, everything else → the
// existing WAV readers (which keep crash-repair).
// ---------------------------------------------------------------------------

fn is_opus(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("opus"))
}

/// Read any supported track into mono `f32` at its native rate.
pub fn read_audio_mono_f32(path: &Path) -> Result<(Vec<f32>, u32)> {
    if is_opus(path) {
        let mut blocks = OpusBlocks::open(path)?;
        let mut out = Vec::new();
        while let Some(b) = blocks.next_block()? {
            out.extend_from_slice(&b);
        }
        Ok((out, OPUS_RATE))
    } else {
        let rate = hound::WavReader::open(path)?.spec().sample_rate;
        Ok((crate::wav::read_wav_mono_f32(path)?, rate))
    }
}

/// Read any supported track into 16 kHz mono `f32` (model input).
pub fn read_audio_mono_16k(path: &Path) -> Result<Vec<f32>> {
    if is_opus(path) {
        let mut blocks = OpusBlocks::open(path)?;
        let mut resampler = MonoResampler::new(OPUS_RATE, 1)?;
        let mut out = Vec::new();
        while let Some(b) = blocks.next_block()? {
            out.extend(resampler.process(&b)?);
        }
        Ok(out)
    } else {
        crate::wav::read_wav_mono_16k(path)
    }
}

/// Read the `[start_ms, end_ms)` span of any supported track as mono `f32`
/// at its native rate. Opus slices seek by page granule (with pre-roll) so
/// replaying one line of an hour-long file stays snappy.
pub fn read_audio_slice_ms(path: &Path, start_ms: u64, end_ms: u64) -> Result<(Vec<f32>, u32)> {
    if !is_opus(path) {
        return crate::wav::read_wav_slice_ms(path, start_ms, end_ms);
    }
    let start = start_ms * OPUS_RATE as u64 / 1000;
    let end = end_ms.max(start_ms) * OPUS_RATE as u64 / 1000;

    // Re-open with a raw seekable reader for `seek_absgp`.
    let mut blocks = OpusBlocks::open(path)?;
    let pre_skip_total = blocks.pre_skip_left; // captured before any decode
    if start > SEEK_BACK + FRAME as u64 {
        // Page granules count from the stream start (pre-skip included).
        let target = start + pre_skip_total - SEEK_BACK;
        if blocks.reader.seek_absgp(None, target).is_ok() {
            // After a coarse seek we don't know the exact position until a
            // page boundary; resync on the first packet that ends a page.
            blocks.pre_skip_left = 0; // pre-skip is long gone at this depth
            let mut pos: Option<u64> = None; // absolute decoded position
            let mut out = Vec::new();
            loop {
                let Some(pkt) = blocks.reader.read_packet().context("read packet")? else {
                    break;
                };
                let mut pcm = vec![0.0f32; FRAME * 2];
                let n = blocks
                    .decoder
                    .decode_float(&pkt.data, &mut pcm, false)
                    .context("decode")?;
                pcm.truncate(n);
                if pos.is_none() {
                    if pkt.last_in_page() {
                        // Granule = samples (incl. pre-skip) through this
                        // packet → our absolute position after it.
                        pos = Some(pkt.absgp_page().saturating_sub(pre_skip_total));
                    }
                    continue; // pre-roll / position unknown — discard
                }
                let p = pos.unwrap();
                let pkt_start = p;
                let pkt_end = p + pcm.len() as u64;
                pos = Some(pkt_end);
                if pkt_end <= start {
                    continue;
                }
                let from = start.saturating_sub(pkt_start) as usize;
                let to = (end.saturating_sub(pkt_start) as usize).min(pcm.len());
                if from < to {
                    out.extend_from_slice(&pcm[from..to]);
                }
                if pkt_end >= end {
                    break;
                }
            }
            return Ok((out, OPUS_RATE));
        }
        // Seek unsupported/failed → fall through to the sequential path.
        blocks = OpusBlocks::open(path)?;
    }
    // Near the start (or no seek): decode forward, windowing as we go.
    let mut out = Vec::new();
    let mut pos: u64 = 0;
    while let Some(pcm) = blocks.next_block()? {
        let pkt_start = pos;
        let pkt_end = pos + pcm.len() as u64;
        pos = pkt_end;
        if pkt_end <= start {
            continue;
        }
        let from = start.saturating_sub(pkt_start) as usize;
        let to = (end.saturating_sub(pkt_start) as usize).min(pcm.len());
        if from < to {
            out.extend_from_slice(&pcm[from..to]);
        }
        if pkt_end >= end {
            break;
        }
    }
    Ok((out, OPUS_RATE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::WavWriter;

    /// 3 s at 44.1 kHz: silence · 440 Hz tone · silence.
    fn write_test_wav(path: &Path) -> usize {
        let rate = 44_100u32;
        let mut w = WavWriter::create(path, rate).unwrap();
        let mut samples = Vec::new();
        samples.extend(std::iter::repeat_n(0.0f32, rate as usize));
        samples.extend(
            (0..rate).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5),
        );
        samples.extend(std::iter::repeat_n(0.0f32, rate as usize));
        w.write(&samples).unwrap();
        w.finalize().unwrap();
        samples.len()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt()
    }

    #[test]
    fn roundtrip_preserves_duration_and_energy() {
        let dir = std::env::temp_dir().join(format!("zord-opus-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, opus) = (dir.join("t.wav"), dir.join("t.opus"));
        write_test_wav(&wav);

        compress_wav_to_opus(&wav, &opus, 32_000).unwrap();
        // Real compression happened (3s of 16-bit 44.1k ≈ 265 KB).
        assert!(std::fs::metadata(&opus).unwrap().len() < 40_000);

        let (decoded, rate) = read_audio_mono_f32(&opus).unwrap();
        assert_eq!(rate, 48_000);
        // Duration within one frame of 3 s at 48 kHz.
        let expect = 3 * 48_000usize;
        assert!(
            (decoded.len() as i64 - expect as i64).unsigned_abs() <= FRAME as u64,
            "decoded {} vs expected {expect}",
            decoded.len()
        );
        // Tone second is loud; outer seconds are (near-)silent.
        let third = decoded.len() / 3;
        let (a, b, c) = (
            rms(&decoded[..third]),
            rms(&decoded[third..2 * third]),
            rms(&decoded[2 * third..]),
        );
        assert!(b > 0.2, "tone rms {b}");
        assert!(
            b > a * 10.0 && b > c * 10.0,
            "silence rms {a}/{c}, tone {b}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slice_matches_window() {
        let dir = std::env::temp_dir().join(format!("zord-opus-slice-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (wav, opus) = (dir.join("t.wav"), dir.join("t.opus"));
        write_test_wav(&wav);
        compress_wav_to_opus(&wav, &opus, 32_000).unwrap();

        // 200 ms inside the tone second.
        let (slice, rate) = read_audio_slice_ms(&opus, 1_200, 1_400).unwrap();
        assert_eq!(rate, 48_000);
        let expect = (200 * 48_000 / 1000) as i64;
        assert!(
            (slice.len() as i64 - expect).unsigned_abs() <= FRAME as u64,
            "slice len {} vs {expect}",
            slice.len()
        );
        assert!(rms(&slice) > 0.2, "slice should be inside the tone");

        // 200 ms inside the leading silence.
        let (quiet, _) = read_audio_slice_ms(&opus, 300, 500).unwrap();
        assert!(rms(&quiet) < 0.05, "leading silence rms {}", rms(&quiet));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opus_bitrate_presets() {
        assert_eq!(opus_bitrate("space"), 24_000);
        assert_eq!(opus_bitrate("standard"), 32_000);
        assert_eq!(opus_bitrate("high"), 48_000);
        assert_eq!(opus_bitrate("garbage"), 32_000);
    }
}
