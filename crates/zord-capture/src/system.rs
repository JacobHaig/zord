//! System / desktop audio capture ("Others").
//!
//! macOS: ScreenCaptureKit, which can tap all system output audio. Requires the
//! user to grant **Screen Recording** permission (a TCC prompt the first time).
//! Other platforms return an error for now (Windows WASAPI loopback is a later
//! phase).

use crate::AudioSource;

#[cfg(target_os = "macos")]
pub use macos::SystemAudio;

#[cfg(not(target_os = "macos"))]
pub use other::SystemAudio;

#[cfg(target_os = "macos")]
mod macos {
    use crate::{bytes_as_f32, AudioSource, FrameSink};
    use anyhow::{Context, Result};
    use screencapturekit::prelude::*;
    use std::sync::Mutex;

    /// System sample rate we request from ScreenCaptureKit.
    const SYSTEM_SR: i32 = 48_000;

    pub struct SystemAudio {
        stream: SCStream,
        sample_rate: u32,
    }

    impl SystemAudio {
        pub fn start(sink: FrameSink) -> Result<Self> {
            // Touching shareable content is what triggers / requires the
            // Screen Recording permission.
            let content = SCShareableContent::get().context(
                "could not access screen content — grant Screen Recording permission \
                 in System Settings > Privacy & Security, then retry",
            )?;
            let display = content
                .displays()
                .into_iter()
                .next()
                .context("no display available to attach the audio stream to")?;

            let filter = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[])
                .build();

            // Audio-only: tiny video frames (we register no Screen handler).
            let config = SCStreamConfiguration::new()
                .with_width(2)
                .with_height(2)
                .with_captures_audio(true)
                .with_sample_rate(SYSTEM_SR)
                .with_channel_count(2)
                .with_excludes_current_process_audio(true);

            let mut stream = SCStream::new(&filter, &config);
            stream.add_output_handler(AudioOut { sink: Mutex::new(sink) }, SCStreamOutputType::Audio);
            stream
                .start_capture()
                .context("failed to start system audio capture")?;

            tracing::info!(sample_rate = SYSTEM_SR, "system audio capture started");
            Ok(Self {
                stream,
                sample_rate: SYSTEM_SR as u32,
            })
        }
    }

    impl AudioSource for SystemAudio {
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }

    impl Drop for SystemAudio {
        fn drop(&mut self) {
            let _ = self.stream.stop_capture();
        }
    }

    /// Receives audio sample buffers from ScreenCaptureKit on its dispatch
    /// queue, downmixes to mono, and forwards to the pipeline. `SCStreamOutputTrait`
    /// requires `Send + Sync`, hence the `Mutex` around the (`!Sync`) sender.
    struct AudioOut {
        sink: Mutex<FrameSink>,
    }

    impl SCStreamOutputTrait for AudioOut {
        fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
            if of_type != SCStreamOutputType::Audio {
                return;
            }
            let Some(list) = sample.audio_buffer_list() else {
                return;
            };
            let mono = buffers_to_mono(&list);
            if !mono.is_empty() {
                if let Ok(tx) = self.sink.lock() {
                    let _ = tx.send(mono);
                }
            }
        }
    }

    /// Collapse an `AudioBufferList` (planar = one buffer per channel, or a
    /// single interleaved buffer) into a mono `f32` vector.
    fn buffers_to_mono(list: &screencapturekit::cm::AudioBufferList) -> Vec<f32> {
        let channels: Vec<Vec<f32>> = list
            .iter()
            .map(|buf| (bytes_as_f32(buf.data()), buf.number_channels.max(1) as usize))
            .map(|(samples, nch)| {
                if nch <= 1 {
                    samples
                } else {
                    // Interleaved within a single buffer.
                    samples
                        .chunks(nch)
                        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
                        .collect()
                }
            })
            .collect();

        match channels.len() {
            0 => Vec::new(),
            1 => channels.into_iter().next().unwrap(),
            n => {
                // Planar: average channel buffers element-wise.
                let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
                (0..len)
                    .map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / n as f32)
                    .collect()
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod other {
    use crate::{AudioSource, FrameSink};
    use anyhow::{bail, Result};

    pub struct SystemAudio;

    impl SystemAudio {
        pub fn start(_sink: FrameSink) -> Result<Self> {
            bail!("system audio capture is not yet implemented on this platform")
        }
    }

    impl AudioSource for SystemAudio {
        fn sample_rate(&self) -> u32 {
            0
        }
    }
}

// Re-assert the trait is in scope for both cfg branches' impls.
#[allow(unused_imports)]
use AudioSource as _;
