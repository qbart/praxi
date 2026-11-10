//! Audio output through cpal for the praxi viewer and editor: mixes the player's audio
//! tracks into the default device, converting rates and channels
//! (`docs/features/audio-output.md`).

mod convert;

pub use convert::{Resampler, map_channels};
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

    /// Starts a stream that plays the sum of `outputs`: at `outputs[0]`'s rate and channel
    /// count when the device supports them, otherwise at the device's default
    /// configuration, each output converted to it.
    pub fn start(&self, outputs: Vec<AudioOutput>, muted: Arc<AtomicBool>) -> Result<Stream, String> {
        let first = outputs.first().ok_or("no audio outputs")?;
        let (rate, channels) = (first.sample_rate(), u16::try_from(first.channels()).map_err(|_| "too many channels")?);
        let rank = |f: cpal::SampleFormat| match f {
            cpal::SampleFormat::F32 => 0,
            cpal::SampleFormat::I16 => 1,
            cpal::SampleFormat::I32 => 2,
            _ => 3,
        };
        let native = self
            .device
            .supported_output_configs()
            .map_err(|e| e.to_string())?
            .filter(|c| c.channels() == channels && (c.min_sample_rate()..=c.max_sample_rate()).contains(&rate))
            .min_by_key(|c| rank(c.sample_format()));
        let (config, format) = match native {
            Some(c) => (cpal::StreamConfig { channels, sample_rate: rate, buffer_size: cpal::BufferSize::Default }, c.sample_format()),
            None => {
                let c = self.device.default_output_config().map_err(|e| e.to_string())?;
                (c.config(), c.sample_format())
            }
        };
        let latency_us = Arc::new(AtomicU64::new(0));
        let stream = match format {
            cpal::SampleFormat::F32 => self.build::<f32>(config, outputs, muted, latency_us.clone()),
            cpal::SampleFormat::I16 => self.build::<i16>(config, outputs, muted, latency_us.clone()),
            cpal::SampleFormat::I32 => self.build::<i32>(config, outputs, muted, latency_us.clone()),
            f => return Err(format!("unsupported device sample format {f}")),
        }?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Stream { _stream: stream, latency_us, config: (config.sample_rate, config.channels) })
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
        let channels = config.channels as usize;
        let mut sources: Vec<Source> = outputs.into_iter().map(|o| Source::new(o, config.sample_rate)).collect();
        let mut mix = Vec::new();
        let frame_us = 1e6 / config.sample_rate as f64;
        self.device
            .build_output_stream(
                config,
                move |buf: &mut [T], info: &cpal::OutputCallbackInfo| {
                    // The buffer's last frame is heard this long after the read: until playback
                    // starts, then the buffer itself, then what the converter holds.
                    let t = info.timestamp();
                    let frames = buf.len() / channels;
                    let buffer_us = frames as f64 * frame_us;
                    mix.clear();
                    mix.resize(buf.len(), 0.0f32);
                    for source in &mut sources {
                        source.add_to(&mut mix, channels);
                    }
                    let held_us = sources.first().map_or(0.0, |s| s.held_us());
                    let latency = t.playback.duration_since(t.callback).as_micros() as u64 + (buffer_us + held_us) as u64;
                    latency_us.store(latency, Ordering::Relaxed);
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

/// One output and its conversion to the stream format.
struct Source {
    output: AudioOutput,
    /// `None` at the stream's rate.
    resampler: Option<Resampler>,
    channels: usize,
    read: Vec<f32>,
    converted: Vec<f32>,
}

impl Source {
    fn new(output: AudioOutput, rate: u32) -> Source {
        let channels = output.channels();
        let resampler = (output.sample_rate() != rate).then(|| Resampler::new(channels, output.sample_rate(), rate));
        Source { output, resampler, channels, read: Vec::new(), converted: Vec::new() }
    }

    /// Reads the frames of one callback and adds them to `mix`, which has `channels`.
    fn add_to(&mut self, mix: &mut [f32], channels: usize) {
        let frames = mix.len() / channels;
        self.read.resize(frames * self.channels, 0.0);
        match &mut self.resampler {
            Some(r) => r.process(&mut self.read, |dst| self.output.read(dst)),
            None => {
                let n = self.output.read(&mut self.read);
                self.read[n..].fill(0.0);
            }
        }
        let frames_in = if self.channels == channels {
            &self.read
        } else {
            self.converted.resize(mix.len(), 0.0);
            map_channels(&self.read, self.channels, &mut self.converted, channels);
            &self.converted
        };
        for (m, s) in mix.iter_mut().zip(frames_in) {
            *m += s;
        }
    }

    /// Microseconds of source audio read but not played yet.
    fn held_us(&self) -> f64 {
        self.resampler.as_ref().map_or(0.0, |r| r.pending_frames() * 1e6 / self.output.sample_rate() as f64)
    }
}

/// Plays until dropped.
pub struct Stream {
    _stream: cpal::Stream,
    latency_us: Arc<AtomicU64>,
    config: (u32, u16),
}

impl Stream {
    /// Output latency of the last callback: from `AudioOutput::read` of the first output to
    /// the last frame read being heard.
    pub fn latency(&self) -> Duration {
        Duration::from_micros(self.latency_us.load(Ordering::Relaxed))
    }

    /// The device's sample rate and channel count.
    pub fn config(&self) -> (u32, u16) {
        self.config
    }
}
