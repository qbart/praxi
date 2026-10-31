//! Audio outputs (`docs/features/audio.md`).

use praxi_format::AudioParams;
use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// Queue room beyond preload and sync tolerance, in microseconds.
const QUEUE_SLACK_US: u64 = 100_000;

/// Converts microseconds to sample frames, rounding down.
pub(crate) fn us_to_frames(us: u64, sample_rate: u32) -> u64 {
    (us as u128 * sample_rate as u128 / 1_000_000) as u64
}

/// A handle to the decoded samples of one audio track. Cheap to clone; usable from any
/// thread, typically the engine's audio callback.
#[derive(Clone)]
pub struct AudioOutput {
    shared: Arc<Shared>,
}

struct Shared {
    sample_rate: u32,
    channels: usize,
    channel_mask: u32,
    queue: Mutex<Queue>,
}

/// Samples of one track on the output timeline. Output frames count up from 0 across
/// loops, so a looping track plays back seamlessly.
pub(crate) struct Queue {
    samples: VecDeque<f32>,
    channels: usize,
    sample_rate: u32,
    preload_us: u64,
    /// Output frame of the first queued sample.
    start: u64,
    /// Output frame after the last one returned by `read` (or skipped by it).
    read_pos: u64,
    /// Whether reads return samples: the player is playing and the track is enabled.
    pub(crate) open: bool,
    /// Player clock in output frames.
    pub(crate) clock: u64,
    /// Reads behind `clock` by more than this many frames skip forward first.
    tolerance: u64,
    /// Frames kept besides an incoming chunk; older ones are dropped.
    keep: u64,
}

impl AudioOutput {
    pub(crate) fn new(params: &AudioParams, tolerance_us: u64) -> AudioOutput {
        let channels = params.channels as usize;
        let mut queue = Queue {
            samples: VecDeque::new(),
            channels,
            sample_rate: params.sample_rate,
            preload_us: params.preload_us as u64,
            start: 0,
            read_pos: 0,
            open: false,
            clock: 0,
            tolerance: 0,
            keep: 0,
        };
        queue.set_tolerance(tolerance_us);
        // Room for a typical fill (`keep`) plus a large chunk, so steady playback never grows it.
        let reserve = queue.keep + params.sample_rate as u64 / 2;
        queue.samples.reserve(reserve as usize * channels);
        AudioOutput {
            shared: Arc::new(Shared {
                sample_rate: params.sample_rate,
                channels,
                channel_mask: params.channel_mask,
                queue: Mutex::new(queue),
            }),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Queue> {
        self.shared.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn sample_rate(&self) -> u32 {
        self.shared.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.shared.channels
    }

    /// WAVE speaker bits of the channels, `0` for unspecified.
    pub fn channel_mask(&self) -> u32 {
        self.shared.channel_mask
    }

    /// Fills `out` with interleaved samples in `[-1, 1]` and returns how many it wrote, a
    /// multiple of `channels()`. Writes fewer (possibly none) when no more audio is decoded
    /// yet, and none while the player is not playing or the track is disabled.
    pub fn read(&self, out: &mut [f32]) -> usize {
        let mut q = self.lock();
        if !q.open {
            return 0;
        }
        let lag = q.clock.saturating_sub(q.start);
        if lag > q.tolerance {
            q.skip(lag);
        }
        let ch = q.channels;
        let n = (out.len() / ch).min(q.samples.len() / ch) * ch;
        for (o, s) in out[..n].iter_mut().zip(q.samples.drain(..n)) {
            *o = s;
        }
        q.start += (n / ch) as u64;
        q.read_pos = q.start;
        n
    }
}

impl fmt::Debug for AudioOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AudioOutput")
            .field("sample_rate", &self.shared.sample_rate)
            .field("channels", &self.shared.channels)
            .field("channel_mask", &self.shared.channel_mask)
            .finish()
    }
}

impl Queue {
    pub(crate) fn set_tolerance(&mut self, tolerance_us: u64) {
        self.tolerance = us_to_frames(tolerance_us, self.sample_rate);
        self.keep = us_to_frames(self.preload_us + tolerance_us + QUEUE_SLACK_US, self.sample_rate);
    }

    /// Output frame after the last frame read (the master position of `Clock::Audio`).
    pub(crate) fn read_position(&self) -> u64 {
        self.read_pos
    }

    /// Output frame after the last queued sample.
    fn end(&self) -> u64 {
        self.start + (self.samples.len() / self.channels) as u64
    }

    /// Empties the queue; the next sample to queue or read is output frame `at`.
    pub(crate) fn reset(&mut self, at: u64) {
        self.samples.clear();
        self.start = at;
        self.read_pos = at;
    }

    /// Drops `frames` frames from the front, moving the read position even past the end.
    fn skip(&mut self, frames: u64) {
        let n = (frames.min(usize::MAX as u64 / self.channels as u64) as usize * self.channels).min(self.samples.len());
        self.samples.drain(..n);
        self.start += frames;
    }

    /// Queues interleaved samples starting at output frame `at`. Frames already queued are
    /// skipped, gaps are filled with silence, and the oldest frames beyond `keep` dropped.
    pub(crate) fn push(&mut self, at: u64, samples: &[f32]) {
        let ch = self.channels;
        let frames = (samples.len() / ch) as u64;
        let end = self.end();
        let mut skip = 0;
        if at < end {
            skip = end - at;
            if skip >= frames {
                return;
            }
        } else if at > end {
            let gap = at - end;
            if gap > self.keep {
                self.reset(at);
            } else {
                self.samples.extend(std::iter::repeat_n(0.0, gap as usize * ch));
            }
        }
        self.samples.extend(&samples[skip as usize * ch..]);
        let limit = (self.keep + frames) as usize * ch;
        if self.samples.len() > limit {
            let excess = self.samples.len() - limit;
            self.samples.drain(..excess);
            self.start += (excess / ch) as u64;
        }
    }
}
