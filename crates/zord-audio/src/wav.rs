//! Minimal WAV writer for audio retention. Writes mono f32 at the caller's
//! sample rate (Phase 25d: the capture device's native rate — the single
//! stored track; models derive 16 kHz from it on the fly), converted to
//! 16-bit PCM (compact, universally playable).

use anyhow::{bail, Result};
use std::path::Path;

/// Reject WAV headers that would make downstream math misbehave on a crafted or
/// corrupt file: `sample_rate == 0` makes the resample ratio infinite (→ a huge
/// allocation), and a `bits_per_sample` outside the supported set can overflow
/// the `1 << (bits - 1)` scale shift. A bad file errors cleanly instead.
pub fn validate_wav_spec(spec: hound::WavSpec) -> Result<()> {
    if spec.sample_rate == 0 {
        bail!("invalid WAV: sample_rate is 0");
    }
    if !(1..=64).contains(&spec.bits_per_sample) {
        bail!(
            "invalid WAV: bits_per_sample {} out of range",
            spec.bits_per_sample
        );
    }
    Ok(())
}

pub struct WavWriter {
    inner: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl WavWriter {
    /// Create a mono 16-bit WAV at `sample_rate`.
    pub fn create(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Self {
            inner: hound::WavWriter::create(path, spec)?,
        })
    }

    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        for &s in samples {
            let clamped = s.clamp(-1.0, 1.0);
            self.inner
                .write_sample((clamped * i16::MAX as f32) as i16)?;
        }
        Ok(())
    }

    pub fn finalize(self) -> Result<()> {
        self.inner.finalize()?;
        Ok(())
    }
}

/// Crash recovery: fix a WAV whose header lengths don't match the file.
///
/// A hard stop (kill, power loss) skips `finalize()`, leaving the RIFF/data
/// length fields stale (hound writes them as 0 at create time and only fills
/// them in at the end) — so readers see an "empty" file even though the
/// samples are on disk. This walks the RIFF chunks, recomputes the `data`
/// length from the actual file size (clipped to whole frames), rewrites both
/// length fields, and trims any partial trailing frame. Returns `true` if the
/// file was modified. Non-WAV / too-short files are left untouched (`false`).
///
/// Safe against a concurrent live writer: repairing only edits the two length
/// fields, which the writer's own `finalize()` overwrites with its true counts.
pub fn repair_wav_header(path: impl AsRef<Path>) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)?;
    let file_len = f.metadata()?.len();
    if file_len < 44 {
        return Ok(false); // smaller than the smallest header+fmt+data layout
    }
    let mut hdr = [0u8; 12];
    f.read_exact(&mut hdr)?;
    if &hdr[0..4] != b"RIFF" || &hdr[8..12] != b"WAVE" {
        return Ok(false);
    }
    let mut pos: u64 = 12;
    let mut block_align: u64 = 1;
    loop {
        if pos + 8 > file_len {
            return Ok(false); // no data chunk found
        }
        f.seek(SeekFrom::Start(pos))?;
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch)?;
        let len = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        match &ch[0..4] {
            b"fmt " if len >= 16 => {
                let mut fmt = [0u8; 16];
                f.read_exact(&mut fmt)?;
                block_align = u16::from_le_bytes([fmt[12], fmt[13]]).max(1) as u64;
            }
            b"data" => {
                let avail = file_len.saturating_sub(pos + 8);
                let actual = avail - (avail % block_align);
                if len == actual {
                    return Ok(false);
                }
                f.seek(SeekFrom::Start(pos + 4))?;
                f.write_all(&(actual.min(u32::MAX as u64) as u32).to_le_bytes())?;
                let riff_len = (pos + actual).min(u32::MAX as u64) as u32; // file minus the 8-byte RIFF header
                f.seek(SeekFrom::Start(4))?;
                f.write_all(&riff_len.to_le_bytes())?;
                f.set_len(pos + 8 + actual)?; // drop a partial trailing frame
                return Ok(true);
            }
            _ => {}
        }
        pos += 8 + len + (len & 1); // chunks are 2-byte aligned
    }
}

/// Read the `[start_ms, end_ms)` span of a WAV as mono `f32` in `[-1, 1]`,
/// returning `(samples, sample_rate)`. Rate-agnostic (Phase 25d): offsets are
/// computed from the file's own header, so a wall-clock-aligned track maps
/// `ms → sample` exactly at any rate. Multi-channel files are downmixed; a
/// range past end-of-file just returns fewer samples. Used for per-line replay.
pub fn read_wav_slice_ms(
    path: impl AsRef<Path>,
    start_ms: u64,
    end_ms: u64,
) -> Result<(Vec<f32>, u32)> {
    let _ = repair_wav_header(&path); // crash recovery; no-op on healthy files
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    validate_wav_spec(spec)?;
    let rate = spec.sample_rate;
    let start_sample = (start_ms * rate as u64 / 1000) as u32;
    let len = (end_ms.saturating_sub(start_ms) * rate as u64 / 1000) as u32;
    let channels = spec.channels.max(1) as usize;
    reader.seek(start_sample)?;
    let want = len as usize * channels;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .take(want)
            .filter_map(|s| s.ok())
            .collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .take(want)
                .filter_map(|s| s.ok())
                .map(|s| s as f32 * scale)
                .collect()
        }
    };
    let mono = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok((mono, rate))
}

/// Read a WAV file (any rate/format) into **16 kHz** mono `f32` in `[-1, 1]`,
/// resampling on the fly in ~1 s blocks so a long device-rate recording never
/// gets slurped whole (Phase 25d). Used to feed the diarizer from the single
/// stored native-rate track. A 16 kHz file passes through untouched.
pub fn read_wav_mono_16k(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    let _ = repair_wav_header(&path); // crash recovery; no-op on healthy files
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    validate_wav_spec(spec)?;
    let channels = spec.channels.max(1) as usize;
    let mut resampler = crate::MonoResampler::new(spec.sample_rate, spec.channels)?;
    let block_len = spec.sample_rate as usize * channels; // ~1 s interleaved
    let mut block: Vec<f32> = Vec::with_capacity(block_len);
    // Rough capacity: output is 16 kHz mono.
    let mut out: Vec<f32> = Vec::new();

    let mut flush = |block: &mut Vec<f32>, out: &mut Vec<f32>| -> Result<()> {
        if !block.is_empty() {
            out.extend(resampler.process(block)?);
            block.clear();
        }
        Ok(())
    };
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in reader.samples::<f32>() {
                block.push(s?);
                if block.len() >= block_len {
                    flush(&mut block, &mut out)?;
                }
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            for s in reader.samples::<i32>() {
                block.push(s? as f32 * scale);
                if block.len() >= block_len {
                    flush(&mut block, &mut out)?;
                }
            }
        }
    }
    flush(&mut block, &mut out)?;
    Ok(out)
}

/// Read a WAV file (any int/float format) into mono `f32` samples in `[-1, 1]`
/// at its **native** rate. Multi-channel files are downmixed by averaging.
pub fn read_wav_mono_f32(path: impl AsRef<Path>) -> Result<Vec<f32>> {
    let _ = repair_wav_header(&path); // crash recovery; no-op on healthy files
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    validate_wav_spec(spec)?;
    let channels = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 * scale)
                .collect()
        }
    };
    if channels <= 1 {
        return Ok(interleaved);
    }
    Ok(interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect())
}

/// Mix several **session-aligned** mono/stereo WAVs into one mono WAV (the
/// Phase 30e "merged audio" export). Every Zord track is anchored at the
/// session start and silence-padded (the storage invariant), so mixing is a
/// sample-wise sum at a common rate — the highest input rate; lower-rate
/// tracks are resampled up. Streams in ~1 s blocks so an hours-long session
/// never loads whole. Overlap is clamped rather than normalized: meeting
/// speech rarely overlaps, and keeping single-speaker level intact beats
/// guarding rare peaks.
pub fn mix_tracks(paths: &[std::path::PathBuf], out: &Path) -> Result<()> {
    anyhow::ensure!(!paths.is_empty(), "no audio tracks to mix");

    enum Src {
        Wav {
            reader: hound::WavReader<std::io::BufReader<std::fs::File>>,
            channels: usize,
            float: bool,
            scale: f32,
        },
        Opus(crate::compress::OpusBlocks),
    }

    struct Track {
        src: Src,
        rate: u32,
        resampler: Option<crate::MonoResampler>,
        /// Target-rate mono samples decoded but not yet mixed.
        carry: Vec<f32>,
        done: bool,
    }

    impl Track {
        /// Read up to `frames` native frames, downmixed to mono (native rate).
        fn read_native(&mut self, frames: usize) -> Result<Vec<f32>> {
            match &mut self.src {
                Src::Wav {
                    reader,
                    channels,
                    float,
                    scale,
                } => {
                    let want = frames * *channels;
                    let mut inter = Vec::with_capacity(want);
                    if *float {
                        for s in reader.samples::<f32>().take(want) {
                            inter.push(s?);
                        }
                    } else {
                        for s in reader.samples::<i32>().take(want) {
                            inter.push(s? as f32 * *scale);
                        }
                    }
                    if *channels <= 1 {
                        return Ok(inter);
                    }
                    Ok(inter
                        .chunks(*channels)
                        .map(|f| f.iter().sum::<f32>() / f.len() as f32)
                        .collect())
                }
                Src::Opus(blocks) => {
                    let mut mono = Vec::with_capacity(frames);
                    while mono.len() < frames {
                        match blocks.next_block()? {
                            Some(b) => mono.extend_from_slice(&b),
                            None => break,
                        }
                    }
                    Ok(mono)
                }
            }
        }

        /// Top `carry` up to ≥ `want` target-rate samples (or until the file
        /// ends — a sub-chunk resampler tail of a few ms is dropped).
        fn fill(&mut self, want: usize) -> Result<()> {
            while !self.done && self.carry.len() < want {
                let block = self.read_native(self.rate as usize)?; // ~1 s
                if block.is_empty() {
                    self.done = true;
                    break;
                }
                match self.resampler.as_mut() {
                    Some(r) => self.carry.extend(r.process(&block)?),
                    None => self.carry.extend(block),
                }
            }
            Ok(())
        }
    }

    let mut opened: Vec<(Src, u32)> = Vec::new();
    for p in paths {
        if p.extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("opus"))
        {
            let blocks = crate::compress::OpusBlocks::open(p)
                .map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
            let rate = blocks.sample_rate();
            opened.push((Src::Opus(blocks), rate));
        } else {
            let _ = repair_wav_header(p); // crash recovery, as in the readers
            let reader =
                hound::WavReader::open(p).map_err(|e| anyhow::anyhow!("{}: {e}", p.display()))?;
            let spec = reader.spec();
            validate_wav_spec(spec)?;
            opened.push((
                Src::Wav {
                    channels: spec.channels.max(1) as usize,
                    float: spec.sample_format == hound::SampleFormat::Float,
                    scale: 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32,
                    reader,
                },
                spec.sample_rate,
            ));
        }
    }
    let target_rate = opened.iter().map(|(_, r)| *r).max().unwrap_or(48_000);
    let mut tracks: Vec<Track> = opened
        .into_iter()
        .map(|(src, rate)| -> Result<Track> {
            Ok(Track {
                src,
                rate,
                resampler: (rate != target_rate)
                    .then(|| crate::MonoResampler::to_rate(rate, 1, target_rate))
                    .transpose()?,
                carry: Vec::new(),
                done: false,
            })
        })
        .collect::<Result<_>>()?;

    let mut writer = WavWriter::create(out, target_rate)?;
    let block = target_rate as usize; // mix 1 s at a time
    loop {
        let mut mix: Vec<f32> = Vec::new();
        for t in &mut tracks {
            t.fill(block)?;
            let take = t.carry.len().min(block);
            if mix.len() < take {
                mix.resize(take, 0.0);
            }
            for (m, s) in mix.iter_mut().zip(t.carry.drain(..take)) {
                *m += s; // WavWriter::write clamps to [-1, 1]
            }
        }
        if mix.is_empty() {
            break; // every track exhausted
        }
        writer.write(&mix)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a hard crash (finalize never ran): hound leaves the RIFF/data
    /// length fields as 0, so readers see an empty file. Repair must restore
    /// the lengths from the file size and make the samples readable again.
    #[test]
    fn repairs_unfinalized_wav() {
        let dir = std::env::temp_dir().join(format!("zord-wavfix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");

        let mut w = WavWriter::create(&path, 16_000).unwrap();
        w.write(&vec![0.25f32; 16_000]).unwrap(); // 1 s of audio
        w.finalize().unwrap();

        // Zero the RIFF + data length fields, as an unfinalized header has them.
        use std::io::{Seek, SeekFrom, Write};
        let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.seek(SeekFrom::Start(4)).unwrap();
        f.write_all(&0u32.to_le_bytes()).unwrap();
        f.seek(SeekFrom::Start(40)).unwrap(); // data length (44-byte canonical header)
        f.write_all(&0u32.to_le_bytes()).unwrap();
        drop(f);
        assert_eq!(hound::WavReader::open(&path).unwrap().len(), 0);

        assert!(repair_wav_header(&path).unwrap());
        let samples = read_wav_mono_f32(&path).unwrap();
        assert_eq!(samples.len(), 16_000);
        assert!((samples[0] - 0.25).abs() < 0.01);

        // Healthy file: a second pass changes nothing.
        assert!(!repair_wav_header(&path).unwrap());

        // Data past the header's count (crash after more samples were flushed):
        // whole frames are recovered, a partial trailing frame is trimmed.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&[0xAB, 0xCD, 0xEF]).unwrap(); // 1.5 frames
        drop(f);
        assert!(repair_wav_header(&path).unwrap());
        assert_eq!(read_wav_mono_f32(&path).unwrap().len(), 16_001);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Mixing session-aligned tracks: sum where they overlap, the longer
    /// track's tail passes through, output spans the longest input.
    #[test]
    fn mix_sums_aligned_tracks() {
        let dir = std::env::temp_dir().join(format!("zord-mix-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, out) = (dir.join("a.wav"), dir.join("b.wav"), dir.join("m.wav"));

        let mut w = WavWriter::create(&a, 16_000).unwrap();
        w.write(&vec![0.25f32; 1_600]).unwrap(); // 0.1 s
        w.finalize().unwrap();
        let mut w = WavWriter::create(&b, 16_000).unwrap();
        w.write(&vec![0.25f32; 3_200]).unwrap(); // 0.2 s
        w.finalize().unwrap();

        mix_tracks(&[a, b], &out).unwrap();
        let mixed = read_wav_mono_f32(&out).unwrap();
        assert_eq!(mixed.len(), 3_200);
        assert!((mixed[0] - 0.5).abs() < 0.02, "overlap sums: {}", mixed[0]);
        assert!(
            (mixed[2_000] - 0.25).abs() < 0.02,
            "tail passes: {}",
            mixed[2_000]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Non-WAV files must be left untouched.
    #[test]
    fn repair_ignores_non_wav() {
        let dir = std::env::temp_dir().join(format!("zord-wavfix2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not.wav");
        let body = vec![7u8; 100];
        std::fs::write(&path, &body).unwrap();
        assert!(!repair_wav_header(&path).unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
