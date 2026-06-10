//! System / desktop audio capture ("Others").
//!
//! - **macOS:** ScreenCaptureKit (system output tap). Needs Screen Recording
//!   permission.
//! - **Windows:** WASAPI loopback on the default render device (no virtual
//!   device needed). We use the `wasapi` crate's render-device + capture-
//!   direction combo, which sets `AUDCLNT_STREAMFLAGS_LOOPBACK`.
//! - **Other (Linux):** not implemented yet.

#[cfg(target_os = "macos")]
pub use macos::{list_capturable_apps, SystemAudio};

#[cfg(target_os = "windows")]
pub use windows_impl::{list_capturable_apps, SystemAudio};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use other::{list_capturable_apps, SystemAudio};

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
        /// Capture the whole system mix (every app).
        pub fn start(sink: FrameSink) -> Result<Self> {
            Self::start_filtered(sink, None)
        }

        /// Capture only the app with this bundle id (Phase 31). Fails with an
        /// actionable message when the app isn't running.
        pub fn start_app(sink: FrameSink, app_id: &str) -> Result<Self> {
            Self::start_filtered(sink, Some(app_id))
        }

        fn start_filtered(sink: FrameSink, app_id: Option<&str>) -> Result<Self> {
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

            // Whole mix, or scoped to one application (ScreenCaptureKit's
            // content filter applies to the audio as well as the video).
            let builder = SCContentFilter::create()
                .with_display(&display)
                .with_excluding_windows(&[]);
            let filter = match app_id {
                None => builder.build(),
                Some(id) => {
                    let app = content
                        .applications()
                        .into_iter()
                        .find(|a| a.bundle_identifier() == id)
                        .with_context(|| {
                            format!(
                                "the app to capture ({id}) isn't running — start it, then record"
                            )
                        })?;
                    builder.with_including_applications(&[&app], &[]).build()
                }
            };

            // Audio-only: tiny video frames (we register no Screen handler).
            let config = SCStreamConfiguration::new()
                .with_width(2)
                .with_height(2)
                .with_captures_audio(true)
                .with_sample_rate(SYSTEM_SR)
                .with_channel_count(2)
                .with_excludes_current_process_audio(true);

            let mut stream = SCStream::new(&filter, &config);
            stream.add_output_handler(
                AudioOut {
                    sink: Mutex::new(sink),
                },
                SCStreamOutputType::Audio,
            );
            stream
                .start_capture()
                .context("failed to start system audio capture")?;

            tracing::info!(
                sample_rate = SYSTEM_SR,
                app = app_id.unwrap_or("<all>"),
                "system audio capture started"
            );
            Ok(Self {
                stream,
                sample_rate: SYSTEM_SR as u32,
            })
        }
    }

    /// Running applications that per-app capture can target. Triggers the
    /// Screen Recording permission prompt on first use (same as recording).
    pub fn list_capturable_apps() -> Result<Vec<crate::CapturableApp>> {
        let content = SCShareableContent::get().context(
            "could not access screen content — grant Screen Recording permission \
             in System Settings > Privacy & Security, then retry",
        )?;
        let mut seen = std::collections::HashSet::new();
        let mut apps: Vec<crate::CapturableApp> = content
            .applications()
            .into_iter()
            .filter(|a| !a.application_name().is_empty())
            .filter(|a| seen.insert(a.bundle_identifier()))
            .map(|a| crate::CapturableApp {
                id: a.bundle_identifier(),
                name: a.application_name(),
                pid: a.process_id().max(0) as u32,
            })
            .collect();
        apps.sort_by_key(|a| a.name.to_lowercase());
        Ok(apps)
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
            .map(|buf| {
                (
                    bytes_as_f32(buf.data()),
                    buf.number_channels.max(1) as usize,
                )
            })
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
    use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

    const SYSTEM_SR: u32 = 48_000;
    const CHANNELS: usize = 2;

    pub struct SystemAudio {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
        sample_rate: u32,
    }

    impl SystemAudio {
        /// Capture the whole system mix (every app).
        pub fn start(sink: FrameSink) -> Result<Self> {
            Self::start_inner(sink, None)
        }

        /// Capture only one app (Phase 31): `app_id` is its executable name
        /// (e.g. "Discord.exe"), matched against apps with a live audio
        /// session. Fails with an actionable message when it isn't running.
        pub fn start_app(sink: FrameSink, app_id: &str) -> Result<Self> {
            let pid = enumerate_audio_apps()?
                .into_iter()
                .find(|a| a.id.eq_ignore_ascii_case(app_id))
                .map(|a| a.pid)
                .with_context(|| {
                    format!(
                        "the app to capture ({app_id}) isn't running (or hasn't played audio yet) — start it, then record"
                    )
                })?;
            Self::start_inner(sink, Some(pid))
        }

        fn start_inner(sink: FrameSink, target_pid: Option<u32>) -> Result<Self> {
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = stop.clone();
            // The capture thread owns all COM/WASAPI objects (they are
            // apartment-bound and not Send). It reports setup success/failure
            // back over `ready` so `start` can surface errors synchronously.
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();

            let handle = thread::spawn(move || match capture_setup(target_pid) {
                Ok((audio_client, h_event, capture_client)) => {
                    let _ = ready_tx.send(Ok(()));
                    capture_loop(audio_client, h_event, capture_client, sink, stop_thread);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            });

            match ready_rx.recv() {
                Ok(Ok(())) => {
                    tracing::info!(
                        sample_rate = SYSTEM_SR,
                        pid = target_pid.unwrap_or(0),
                        "system audio (WASAPI loopback) started"
                    );
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

    /// Apps with a live audio session on the default output device — the
    /// per-app capture picker. Runs the COM work on its own thread (the
    /// caller's apartment state is unknown).
    pub fn list_capturable_apps() -> Result<Vec<crate::CapturableApp>> {
        std::thread::spawn(enumerate_audio_apps)
            .join()
            .map_err(|_| anyhow::anyhow!("app enumeration thread panicked"))?
    }

    fn enumerate_audio_apps() -> Result<Vec<crate::CapturableApp>> {
        let _ = initialize_mta();
        let enumerator = DeviceEnumerator::new().context("DeviceEnumerator::new")?;
        let device = enumerator
            .get_default_device(&Direction::Render)
            .context("no default render (output) device")?;
        let manager = device
            .get_iaudiosessionmanager()
            .context("audio session manager")?;
        let sessions = manager
            .get_audiosessionenumerator()
            .context("audio session enumerator")?;
        let mut seen = std::collections::HashSet::new();
        let mut apps = Vec::new();
        for i in 0..sessions.get_count().unwrap_or(0) {
            let Ok(session) = sessions.get_session(i) else {
                continue;
            };
            let Ok(pid) = session.get_process_id() else {
                continue;
            };
            if pid == 0 {
                continue; // the system-sounds session
            }
            let Some(exe) = process_image_name(pid) else {
                continue;
            };
            if !seen.insert(exe.to_ascii_lowercase()) {
                continue;
            }
            apps.push(crate::CapturableApp {
                name: exe
                    .strip_suffix(".exe")
                    .or_else(|| exe.strip_suffix(".EXE"))
                    .unwrap_or(&exe)
                    .to_string(),
                id: exe,
                pid,
            });
        }
        apps.sort_by_key(|a| a.name.to_lowercase());
        Ok(apps)
    }

    /// Executable file name (e.g. "Discord.exe") for a PID.
    fn process_image_name(pid: u32) -> Option<String> {
        use windows::core::PWSTR;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            if !ok {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit(['\\', '/']).next().map(|s| s.to_string())
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

    type ClientBundle = (
        wasapi::AudioClient,
        wasapi::Handle,
        wasapi::AudioCaptureClient,
    );

    /// Initialize a loopback capture client: the default render device's whole
    /// mix, or — given a PID — that process's audio only (process-loopback,
    /// Windows 10 2004+; includes child processes, so multi-process apps like
    /// browsers and Discord are captured whole).
    fn capture_setup(target_pid: Option<u32>) -> Result<ClientBundle> {
        // COM apartment for this thread.
        let _ = initialize_mta();

        let mut audio_client = match target_pid {
            None => {
                let enumerator = DeviceEnumerator::new().context("DeviceEnumerator::new")?;
                let device = enumerator
                    .get_default_device(&Direction::Render)
                    .context("no default render (output) device")?;
                device.get_iaudioclient().context("get_iaudioclient")?
            }
            Some(pid) => wasapi::AudioClient::new_application_loopback_client(pid, true)
                .context("process-loopback client (needs Windows 10 2004+)")?,
        };

        // 32-bit float, stereo, 48 kHz; autoconvert makes WASAPI match the
        // device mix format to ours.
        let format = WaveFormat::new(
            32,
            32,
            &SampleType::Float,
            SYSTEM_SR as usize,
            CHANNELS,
            None,
        );
        let buffer_duration_hns = match target_pid {
            // get_device_period is unsupported in process-loopback mode.
            Some(_) => 200_000, // 20 ms
            None => {
                audio_client
                    .get_device_period()
                    .context("get_device_period")?
                    .1
            }
        };
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns,
        };
        // Render device + Capture direction + Shared => loopback flag.
        audio_client
            .initialize_client(&format, &Direction::Capture, &mode)
            .context("initialize_client (loopback)")?;

        let h_event = audio_client
            .set_get_eventhandle()
            .context("set_get_eventhandle")?;
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
            if capture_client
                .read_from_device_to_deque(&mut queue)
                .is_err()
            {
                break;
            }
            let frames = queue.len() / frame_bytes;
            if frames == 0 {
                continue;
            }
            let mono = drain_frames_to_mono(&mut queue, frames);
            if sink.send(mono).is_err() {
                break; // pipeline dropped the receiver
            }
        }
        let _ = audio_client.stop_stream();
    }

    /// Pop `frames` interleaved f32 frames off `queue` and downmix to mono.
    /// Clamped to the whole frames actually queued — a short queue yields fewer
    /// frames instead of panicking the capture thread.
    fn drain_frames_to_mono(queue: &mut VecDeque<u8>, frames: usize) -> Vec<f32> {
        let frames = frames.min(queue.len() / (CHANNELS * 4));
        let mut mono = Vec::with_capacity(frames);
        for _ in 0..frames {
            let mut sum = 0.0f32;
            for _ in 0..CHANNELS {
                let mut b = [0u8; 4];
                for byte in &mut b {
                    *byte = queue.pop_front().unwrap_or(0);
                }
                sum += f32::from_le_bytes(b);
            }
            mono.push(sum / CHANNELS as f32);
        }
        mono
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

        pub fn start_app(_sink: FrameSink, _app_id: &str) -> Result<Self> {
            bail!("per-app audio capture is not yet implemented on this platform")
        }
    }

    pub fn list_capturable_apps() -> Result<Vec<crate::CapturableApp>> {
        Ok(Vec::new())
    }

    impl AudioSource for SystemAudio {
        fn sample_rate(&self) -> u32 {
            0
        }
    }
}
