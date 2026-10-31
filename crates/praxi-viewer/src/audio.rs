//! Audio output through cpal: mixes the player's audio tracks into the default device.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use praxi::AudioOutput;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

pub struct Device {
    device: cpal::Device,
}

impl Device {
    /// The default output device, if there is one.
    pub fn open_default() -> Option<Device> {
        cpal::default_host().default_output_device().map(|device| Device { device })
    }

    pub fn name(&self) -> String {
        self.device.description().map(|d| d.to_string()).unwrap_or_else(|_| "unknown device".into())
    }

    /// The channel count of the device's default configuration.
    pub fn channels(&self) -> Option<u16> {
        self.device.default_output_config().ok().map(|c| c.channels())
    }

    /// Starts a stream at `outputs[0]`'s rate and channel count that plays the sum of all
    /// `outputs`. The caller only passes outputs with that rate and channel count.
    pub fn start(&self, outputs: Vec<AudioOutput>, muted: Arc<AtomicBool>) -> Result<Stream, String> {
        let rate = outputs[0].sample_rate();
        let channels = u16::try_from(outputs[0].channels()).map_err(|_| "too many channels")?;
        let supported = self
            .device
            .supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|c| c.channels() == channels && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate))
            .min_by_key(|c| match c.sample_format() {
                cpal::SampleFormat::F32 => 0,
                cpal::SampleFormat::I16 => 1,
                cpal::SampleFormat::I32 => 2,
                _ => 3,
            })
            .ok_or_else(|| format!("the device does not play {channels} channels at {rate} Hz"))?;
        let config = cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let latency_us = Arc::new(AtomicU64::new(0));
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => self.build::<f32>(config, outputs, muted, latency_us.clone()),
            cpal::SampleFormat::I16 => self.build::<i16>(config, outputs, muted, latency_us.clone()),
            cpal::SampleFormat::I32 => self.build::<i32>(config, outputs, muted, latency_us.clone()),
            f => return Err(format!("unsupported device sample format {f}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Stream { _stream: stream, latency_us })
    }

    fn build<T>(
        &self,
        config: cpal::StreamConfig,
        outputs: Vec<AudioOutput>,
        muted: Arc<AtomicBool>,
        latency_us: Arc<AtomicU64>,
    ) -> Result<cpal::Stream, String>
    where
        T: cpal::SizedSample + cpal::FromSample<f32>,
    {
        let mut mix = Vec::new();
        let mut read = Vec::new();
        let frame_us = 1e6 / config.sample_rate as f64;
        let channels = config.channels as usize;
        self.device
            .build_output_stream(
                config,
                move |buf: &mut [T], info: &cpal::OutputCallbackInfo| {
                    // The buffer's last frame is heard this long after the read: until playback
                    // starts, then the buffer itself.
                    let t = info.timestamp();
                    let buffer_us = (buf.len() / channels) as f64 * frame_us;
                    let latency = t.playback.duration_since(t.callback).as_micros() as u64 + buffer_us as u64;
                    latency_us.store(latency, Ordering::Relaxed);
                    mix.clear();
                    mix.resize(buf.len(), 0.0f32);
                    read.resize(buf.len(), 0.0f32);
                    for out in &outputs {
                        // Muted tracks are still read so they stay in sync with the clock.
                        let n = out.read(&mut read);
                        for (m, s) in mix.iter_mut().zip(&read[..n]) {
                            *m += s;
                        }
                    }
                    let muted = muted.load(Ordering::Relaxed);
                    for (b, m) in buf.iter_mut().zip(&mix) {
                        *b = T::from_sample(if muted { 0.0 } else { m.clamp(-1.0, 1.0) });
                    }
                },
                |e| eprintln!("audio error: {e}"),
                None,
            )
            .map_err(|e| e.to_string())
    }
}

/// Plays until dropped.
pub struct Stream {
    _stream: cpal::Stream,
    latency_us: Arc<AtomicU64>,
}

impl Stream {
    /// Output latency of the last callback: from `AudioOutput::read` to the last frame read
    /// being heard.
    pub fn latency(&self) -> Duration {
        Duration::from_micros(self.latency_us.load(Ordering::Relaxed))
    }
}
