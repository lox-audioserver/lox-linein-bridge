use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{HostId, SampleFormat, StreamConfig};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::warn;

const TARGET_RATE: u32 = 48_000;
const TARGET_CHANNELS: u16 = 2;

pub struct CaptureSession {
    pub receiver: mpsc::Receiver<Vec<u8>>,
    pub error_receiver: mpsc::Receiver<String>,
    pub stream: cpal::Stream,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
}

pub fn list_input_device_details() -> Result<Vec<crate::models::CaptureDeviceInfo>> {
    let host = select_host()?;
    let devices = host.input_devices().context("enumerate input devices")?;
    let mut results = Vec::new();
    for device in devices {
        let name = device
            .name()
            .unwrap_or_else(|_| "Unknown Device".to_string());
        let mut channels = 0u16;
        let mut rates = BTreeSet::new();
        if let Ok(configs) = device.supported_input_configs() {
            for config in configs {
                channels = channels.max(config.channels());
                let min = config.min_sample_rate().0;
                let max = config.max_sample_rate().0;
                rates.insert(min);
                rates.insert(max);
            }
        }
        results.push(crate::models::CaptureDeviceInfo {
            id: name.clone(),
            name,
            channels,
            sample_rates: rates.into_iter().collect(),
        });
    }
    Ok(results)
}

pub fn start_capture(device_name: &str) -> Result<CaptureSession> {
    let host = select_host()?;
    let device = host
        .input_devices()
        .context("enumerate input devices")?
        .find(|dev| dev.name().map(|name| name == device_name).unwrap_or(false))
        .context("capture device not found")?;

    let supported = device
        .default_input_config()
        .context("read default input config")?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let (tx, rx) = mpsc::channel::<Vec<u8>>(8);
    let (err_tx, err_rx) = mpsc::channel::<String>(4);
    let resampler = Arc::new(Mutex::new(Resampler::new(
        config.sample_rate.0,
        config.channels,
    )));

    let err_fn = move |err| {
        let message = format!("capture error: {}", err);
        warn!("{}", message);
        let _ = err_tx.try_send(message);
    };

    let tx_f32 = tx.clone();
    let tx_i16 = tx.clone();
    let tx_u16 = tx.clone();
    let resampler_f32 = Arc::clone(&resampler);
    let resampler_i16 = Arc::clone(&resampler);
    let resampler_u16 = Arc::clone(&resampler);
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            &config,
            move |data: &[f32], _| {
                handle_samples_f32(data, config.channels, &resampler_f32, tx_f32.clone());
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &config,
            move |data: &[i16], _| {
                let mut buffer = Vec::with_capacity(data.len());
                for sample in data {
                    buffer.push(*sample as f32 / i16::MAX as f32);
                }
                handle_samples_f32(&buffer, config.channels, &resampler_i16, tx_i16.clone());
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &config,
            move |data: &[u16], _| {
                let mut buffer = Vec::with_capacity(data.len());
                for sample in data {
                    let shifted = *sample as i32 - (i16::MAX as i32 + 1);
                    buffer.push(shifted as f32 / (i16::MAX as f32 + 1.0));
                }
                handle_samples_f32(&buffer, config.channels, &resampler_u16, tx_u16.clone());
            },
            err_fn,
            None,
        )?,
        _ => anyhow::bail!("unsupported sample format"),
    };

    stream.play().context("start capture stream")?;

    Ok(CaptureSession {
        receiver: rx,
        error_receiver: err_rx,
        stream,
        sample_rate: config.sample_rate.0,
        channels: config.channels,
        format: sample_format,
    })
}

fn select_host() -> Result<cpal::Host> {
    let hosts = cpal::available_hosts();
    if hosts.contains(&HostId::Alsa) {
        return cpal::host_from_id(HostId::Alsa).context("select ALSA host");
    }
    Ok(cpal::default_host())
}

fn handle_samples_f32(
    data: &[f32],
    channels: u16,
    resampler: &Arc<Mutex<Resampler>>,
    tx: mpsc::Sender<Vec<u8>>,
) {
    let output = {
        let mut resampler = match resampler.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if resampler.needs_resample(channels) {
            resampler.process(data, channels)
        } else {
            convert_direct_to_i16(data, channels)
        }
    };

    if output.is_empty() {
        return;
    }

    let mut bytes = Vec::with_capacity(output.len() * 2);
    for sample in output {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }

    let _ = tx.try_send(bytes);
}

fn convert_direct_to_i16(data: &[f32], channels: u16) -> Vec<i16> {
    if channels == TARGET_CHANNELS && data.len().is_multiple_of(2) {
        return data.iter().map(|s| f32_to_i16(*s)).collect();
    }

    let mut out = Vec::with_capacity(data.len());
    let mut idx = 0;
    while idx + channels as usize <= data.len() {
        let frame = &data[idx..idx + channels as usize];
        let (left, right) = map_channels(frame, channels);
        out.push(f32_to_i16(left));
        out.push(f32_to_i16(right));
        idx += channels as usize;
    }
    out
}

fn map_channels(frame: &[f32], channels: u16) -> (f32, f32) {
    match channels {
        0 => (0.0, 0.0),
        1 => (frame[0], frame[0]),
        _ => (frame[0], frame[1]),
    }
}

fn f32_to_i16(sample: f32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    (clamped * i16::MAX as f32) as i16
}

struct Resampler {
    in_rate: u32,
    pos: f64,
    buffer: Vec<f32>,
}

impl Resampler {
    fn new(in_rate: u32, in_channels: u16) -> Self {
        Self {
            in_rate,
            pos: 0.0,
            buffer: Vec::with_capacity(in_channels as usize * 2048),
        }
    }

    fn needs_resample(&self, channels: u16) -> bool {
        self.in_rate != TARGET_RATE || channels != TARGET_CHANNELS
    }

    fn process(&mut self, input: &[f32], in_channels: u16) -> Vec<i16> {
        if input.is_empty() || in_channels == 0 {
            return Vec::new();
        }

        self.buffer.extend_from_slice(input);
        let in_channels_usize = in_channels as usize;
        let step = self.in_rate as f64 / TARGET_RATE as f64;
        let max_samples = in_channels_usize * TARGET_RATE as usize;

        if self.buffer.len() > max_samples {
            let drop_samples = self.buffer.len() - max_samples;
            let drop_frames = drop_samples / in_channels_usize;
            self.buffer.drain(0..drop_samples);
            self.pos = (self.pos - drop_frames as f64).max(0.0);
        }

        let available_frames = self.buffer.len() / in_channels_usize;
        let mut out = Vec::new();

        while self.pos + 1.0 < available_frames as f64 {
            let idx = self.pos.floor() as usize;
            let frac = self.pos - idx as f64;
            let base = idx * in_channels_usize;
            let next = (idx + 1) * in_channels_usize;

            let frame_a = &self.buffer[base..base + in_channels_usize];
            let frame_b = &self.buffer[next..next + in_channels_usize];

            let (left_a, right_a) = map_channels(frame_a, in_channels);
            let (left_b, right_b) = map_channels(frame_b, in_channels);

            let left = left_a + ((left_b - left_a) * frac as f32);
            let right = right_a + ((right_b - right_a) * frac as f32);

            out.push(f32_to_i16(left));
            out.push(f32_to_i16(right));

            self.pos += step;
        }

        let drop_frames = self.pos.floor() as usize;
        if drop_frames > 0 {
            let drop_samples = drop_frames * in_channels_usize;
            self.buffer.drain(0..drop_samples);
            self.pos -= drop_frames as f64;
        }

        out
    }
}
