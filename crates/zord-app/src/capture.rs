//! Microphone capture via cpal. Delivers interleaved f32 buffers (whatever the
//! device's native rate/channels are) over a channel; the pipeline handles
//! resampling. The returned `Stream` must be kept alive to keep recording, and
//! dropped to stop (which closes the channel).

use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use std::sync::mpsc::Sender;
use zord_core::AudioConfig;

pub struct MicCapture {
    /// Held only for its `Drop` (dropping it stops capture); never read.
    #[allow(dead_code)]
    pub stream: cpal::Stream,
    pub config: AudioConfig,
}

pub fn start_mic(tx: Sender<Vec<f32>>) -> Result<MicCapture> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .context("no default input (microphone) device")?;
    let supported = device
        .default_input_config()
        .context("querying default input config")?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    let audio_config = AudioConfig {
        sample_rate: config.sample_rate.0,
        channels: config.channels,
    };
    tracing::info!(
        rate = audio_config.sample_rate,
        channels = audio_config.channels,
        ?sample_format,
        "microphone capture starting"
    );

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build::<f32>(&device, &config, tx)?,
        cpal::SampleFormat::I16 => build::<i16>(&device, &config, tx)?,
        cpal::SampleFormat::U16 => build::<u16>(&device, &config, tx)?,
        other => bail!("unsupported sample format: {other:?}"),
    };
    stream.play()?;
    Ok(MicCapture {
        stream,
        config: audio_config,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    tx: Sender<Vec<f32>>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let v: Vec<f32> = data.iter().map(|&s| f32::from_sample(s)).collect();
            let _ = tx.send(v);
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

fn err_fn(e: cpal::StreamError) {
    tracing::error!("audio stream error: {e}");
}
