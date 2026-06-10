//! Microphone capture via cpal. Downmixes to mono and emits `f32` frames at
//! the device's native rate. The `Stream` is not `Send` on macOS, so the
//! returned `Microphone` must stay on the thread that created it.

use crate::{AudioSource, FrameSink};
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};

pub struct Microphone {
    #[allow(dead_code)] // held only for its Drop (stops capture)
    stream: cpal::Stream,
    sample_rate: u32,
}

/// Names of available input (microphone) devices.
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devs) => devs
            .filter_map(|d| d.description().ok().map(|desc| desc.name().to_string()))
            .collect(),
        Err(_) => Vec::new(),
    }
}

impl Microphone {
    /// Start the default input device.
    pub fn start(sink: FrameSink) -> Result<Self> {
        Self::start_with(sink, None)
    }

    /// Start a specific input device by name, falling back to the default if
    /// `name` is `None` or no match is found.
    pub fn start_with(sink: FrameSink, name: Option<&str>) -> Result<Self> {
        let device = resolve_input_device(name)?;
        let supported = device
            .default_input_config()
            .context("querying default input config")?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels as usize;
        let sample_rate = config.sample_rate;

        tracing::info!(
            sample_rate,
            channels,
            ?sample_format,
            "microphone capture starting"
        );

        let stream = match sample_format {
            cpal::SampleFormat::F32 => build::<f32>(&device, &config, channels, sink)?,
            cpal::SampleFormat::I16 => build::<i16>(&device, &config, channels, sink)?,
            cpal::SampleFormat::U16 => build::<u16>(&device, &config, channels, sink)?,
            other => bail!("unsupported sample format: {other:?}"),
        };
        stream.play()?;
        Ok(Self {
            stream,
            sample_rate,
        })
    }
}

impl AudioSource for Microphone {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
}

/// Select the requested input device, falling back to the default.
fn resolve_input_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let device = match name {
        Some(want) => host
            .input_devices()
            .ok()
            .and_then(|mut devs| {
                devs.find(|d| {
                    d.description()
                        .map(|desc| desc.name() == want)
                        .unwrap_or(false)
                })
            })
            .or_else(|| host.default_input_device())
            .context("no input (microphone) device")?,
        None => host
            .default_input_device()
            .context("no default input (microphone) device")?,
    };
    Ok(device)
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    sink: FrameSink,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        *config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mono = downmix_to_mono(data, channels);
            let _ = sink.send(mono);
        },
        |e| tracing::error!("microphone stream error: {e}"),
        None,
    )?;
    Ok(stream)
}

fn downmix_to_mono<T>(data: &[T], channels: usize) -> Vec<f32>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    if channels <= 1 {
        return data.iter().map(|&s| f32::from_sample(s)).collect();
    }
    data.chunks(channels)
        .map(|frame| {
            let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
            sum / frame.len() as f32
        })
        .collect()
}
