//! System / desktop audio capture ("Others").
//!
//! - **macOS:** ScreenCaptureKit (system output tap). Needs Screen Recording
//!   permission.
//! - **Windows:** WASAPI loopback on the default render device (no virtual
//!   device needed). We use the `wasapi` crate's render-device + capture-
//!   direction combo, which sets `AUDCLNT_STREAMFLAGS_LOOPBACK`.
//! - **Other (Linux):** not implemented yet.

#[cfg(target_os = "macos")]
pub use macos::SystemAudio;

#[cfg(target_os = "windows")]
pub use windows_impl::SystemAudio;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
                let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
                (0..len)
                    .map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / n as f32)
                    .collect()
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use crate::{AudioSource, FrameSink};
    use anyhow::{bail, Context, Result};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread::{self, JoinHandle};
    use wasapi::{
        initialize_mta, Direction, DeviceEnumerator, SampleType, StreamMode, WaveFormat,
    };

    const SYSTEM_SR: u32 = 48_000;
    const CHANNELS: usize = 2;

    pub struct SystemAudio {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
        sample_rate: u32,
    }

    impl SystemAudio {
        pub fn start(sink: FrameSink) -> Result<Self> {
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = stop.clone();
            // The capture thread owns all COM/WASAPI objects (they are
            // apartment-bound and not Send). It reports setup success/failure
            // back over `ready` so `start` can surface errors synchronously.
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

            let handle = thread::spawn(move || {
                match capture_setup() {
                    Ok((audio_client, h_event, capture_client)) => {
                        let _ = ready_tx.send(Ok(()));
                        capture_loop(audio_client, h_event, capture_client, sink, stop_thread);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                    }
                }
            });

            match ready_rx.recv() {
                Ok(Ok(())) => {
                    tracing::info!(sample_rate = SYSTEM_SR, "system audio (WASAPI loopback) started");
                    Ok(Self {
                        stop,
                        handle: Some(handle),
                        sample_rate: SYSTEM_SR,
                    })
                }
                Ok(Err(e)) => bail!("WASAPI loopback init failed: {e}"),
                Err(_) => bail!("WASAPI capture thread exited during setup"),
            }
        }
    }

    impl AudioSource for SystemAudio {
        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }
    }

    impl Drop for SystemAudio {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    type ClientBundle = (wasapi::AudioClient, wasapi::Handle, wasapi::AudioCaptureClient);

    /// Initialize a loopback capture client on the default render device.
    fn capture_setup() -> Result<ClientBundle> {
        // COM apartment for this thread.
        let _ = initialize_mta();

        let enumerator = DeviceEnumerator::new().context("DeviceEnumerator::new")?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .context("no default render (output) device")?;
        let mut audio_client = device.get_iaudioclient().context("get_iaudioclient")?;

        // 32-bit float, stereo, 48 kHz; autoconvert makes WASAPI match the
        // device mix format to ours.
        let format = WaveFormat::new(32, 32, &SampleType::Float, SYSTEM_SR as usize, CHANNELS, None);
        let (_def, min_time) = audio_client.get_device_period().context("get_device_period")?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: min_time,
        };
        // Render device + Capture direction + Shared => loopback flag.
        audio_client
            .initialize_client(&format, &Direction::Capture, &mode)
            .context("initialize_client (loopback)")?;

        let h_event = audio_client.set_get_eventhandle().context("set_get_eventhandle")?;
        let capture_client = audio_client
            .get_audiocaptureclient()
            .context("get_audiocaptureclient")?;
        audio_client.start_stream().context("start_stream")?;
        Ok((audio_client, h_event, capture_client))
    }

    /// Pull loopback frames until stopped, downmix to mono, forward to the sink.
    fn capture_loop(
        audio_client: wasapi::AudioClient,
        h_event: wasapi::Handle,
        capture_client: wasapi::AudioCaptureClient,
        sink: FrameSink,
        stop: Arc<AtomicBool>,
    ) {
        let frame_bytes = CHANNELS * 4; // f32 per channel
        let mut queue: VecDeque<u8> = VecDeque::new();

        while !stop.load(Ordering::Relaxed) {
            // Short timeout so we re-check the stop flag during silence.
            if h_event.wait_for_event(200).is_err() {
                continue;
            }
            if capture_client.read_from_device_to_deque(&mut queue).is_err() {
                break;
            }
            let frames = queue.len() / frame_bytes;
            if frames == 0 {
                continue;
            }
            let mut mono = Vec::with_capacity(frames);
            for _ in 0..frames {
                let mut sum = 0.0f32;
                for _ in 0..CHANNELS {
                    let b = [
                        queue.pop_front().unwrap(),
                        queue.pop_front().unwrap(),
                        queue.pop_front().unwrap(),
                        queue.pop_front().unwrap(),
                    ];
                    sum += f32::from_le_bytes(b);
                }
                mono.push(sum / CHANNELS as f32);
            }
            if sink.send(mono).is_err() {
                break; // pipeline dropped the receiver
            }
        }
        let _ = audio_client.stop_stream();
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
