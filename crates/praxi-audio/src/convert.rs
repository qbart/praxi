//! Rate and channel conversion of one output (`docs/features/audio-output.md#stream-format`).

/// Converts interleaved frames from one rate to another by cubic (Catmull-Rom)
/// interpolation, pulling source frames as needed.
pub struct Resampler {
    channels: usize,
    /// Source frames per output frame.
    step: f64,
    /// Source frames, interleaved; frame 0 is the one before `pos`.
    buf: Vec<f32>,
    /// Position of the next output frame in `buf`, in frames (at least 1).
    pos: f64,
}

impl Resampler {
    pub fn new(channels: usize, from_rate: u32, to_rate: u32) -> Resampler {
        Resampler {
            channels,
            step: from_rate as f64 / to_rate as f64,
            buf: vec![0.0; channels],
            pos: 1.0,
        }
    }

    /// Fills `out` (interleaved, whole frames) with converted frames, reading source frames
    /// with `read`, which fills a buffer and returns how many samples it wrote; the rest is
    /// silence.
    pub fn process(&mut self, out: &mut [f32], mut read: impl FnMut(&mut [f32]) -> usize) {
        let c = self.channels;
        let frames = out.len() / c;
        // Frames up to the last output's position plus the two after it.
        let needed = (self.pos + (frames.max(1) - 1) as f64 * self.step).floor() as usize + 3;
        let have = self.buf.len() / c;
        if needed > have {
            let start = self.buf.len();
            self.buf.resize(needed * c, 0.0);
            let n = read(&mut self.buf[start..]);
            self.buf[start + n..].fill(0.0);
        }
        for f in 0..frames {
            let at = self.pos + f as f64 * self.step;
            let i = at.floor() as usize;
            let t = (at - i as f64) as f32;
            for ch in 0..c {
                let p = |k: usize| self.buf[k * c + ch];
                let (p0, p1, p2, p3) = (p(i - 1), p(i), p(i + 1), p(i + 2));
                out[f * c + ch] = p1
                    + 0.5
                        * t
                        * (p2 - p0
                            + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3
                                + t * (3.0 * (p1 - p2) + p3 - p0)));
            }
        }
        // Keep the frame before the next position and everything after it.
        let next = self.pos + frames as f64 * self.step;
        let drop = (next.floor() as usize - 1).min(self.buf.len() / c);
        self.buf.drain(..drop * c);
        self.pos = next - drop as f64;
    }

    /// Source frames read but not played yet, after the next output position.
    pub fn pending_frames(&self) -> f64 {
        (self.buf.len() / self.channels) as f64 - self.pos
    }
}

/// Copies interleaved frames of `from` channels into `out` with `to` channels: the same
/// count is copied, mono goes to channels 0 and 1, stereo to mono is the average, other
/// pairs copy the channels both have and leave the rest silent.
pub fn map_channels(input: &[f32], from: usize, out: &mut [f32], to: usize) {
    for (src, dst) in input.chunks_exact(from).zip(out.chunks_exact_mut(to)) {
        match (from, to) {
            (a, b) if a == b => dst.copy_from_slice(src),
            (1, _) => {
                dst.fill(0.0);
                dst[0] = src[0];
                dst[1] = src[0];
            }
            (2, 1) => dst[0] = 0.5 * (src[0] + src[1]),
            _ => {
                let n = from.min(to);
                dst[..n].copy_from_slice(&src[..n]);
                dst[n..].fill(0.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f64, frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|i| (std::f64::consts::TAU * hz * i as f64 / rate as f64).sin() as f32 * 0.5)
            .collect()
    }

    /// Resamples `input` (mono) in callbacks of varying size.
    fn run(input: &[f32], from: u32, to: u32, frames: usize) -> Vec<f32> {
        let mut r = Resampler::new(1, from, to);
        let mut at = 0;
        let mut out = Vec::new();
        for k in 0.. {
            let n = [256, 441, 1, 1024][k % 4].min(frames - out.len());
            if n == 0 {
                break;
            }
            let mut buf = vec![0.0; n];
            r.process(&mut buf, |dst| {
                let n = dst.len().min(input.len() - at);
                dst[..n].copy_from_slice(&input[at..at + n]);
                at += n;
                n
            });
            out.extend(buf);
        }
        out
    }

    #[test]
    fn equal_rates_pass_samples_through() {
        let input = sine(48_000, 440.0, 5000);
        let out = run(&input, 48_000, 48_000, 4000);
        assert_eq!(out, input[..4000]);
    }

    #[test]
    fn resampling_keeps_frequency_and_amplitude() {
        let input = sine(48_000, 1000.0, 48_000);
        let down = run(&input, 48_000, 44_100, 44_000);
        let expected = sine(44_100, 1000.0, 44_000);
        let err = down
            .iter()
            .zip(&expected)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.002, "48 kHz to 44.1 kHz: max error {err}");
        let up = run(&down, 44_100, 48_000, 47_000);
        let err = up
            .iter()
            .zip(&input)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.004, "and back: max error {err}");
    }

    #[test]
    fn consumes_source_at_the_rate_ratio() {
        let mut r = Resampler::new(2, 44_100, 48_000);
        let mut read = 0;
        let mut out = vec![0.0; 2 * 480];
        for _ in 0..1000 {
            r.process(&mut out, |dst| {
                read += dst.len() / 2;
                dst.len()
            });
        }
        // 480 000 output frames at 48 kHz are 441 000 source frames, plus the lookahead.
        let played = read as f64 - r.pending_frames();
        assert!(
            (played - 441_000.0).abs() < 1e-6,
            "played {played} source frames"
        );
        assert!(r.pending_frames() < 3.0);
    }

    #[test]
    fn maps_channels() {
        let mut out = vec![9.0; 6];
        map_channels(&[0.5, -0.5], 1, &mut out, 3);
        assert_eq!(out, [0.5, 0.5, 0.0, -0.5, -0.5, 0.0]);
        let mut out = vec![9.0; 2];
        map_channels(&[0.2, 0.4, 1.0, 0.0], 2, &mut out, 1);
        assert_eq!(out, [0.3, 0.5]);
        let mut out = vec![9.0; 2];
        map_channels(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, &mut out, 2);
        assert_eq!(out, [1.0, 2.0]);
        let mut out = vec![9.0; 6];
        map_channels(&[1.0, 2.0], 2, &mut out, 6);
        assert_eq!(out, [1.0, 2.0, 0.0, 0.0, 0.0, 0.0]);
        let mut out = vec![9.0; 2];
        map_channels(&[1.0, 2.0], 2, &mut out, 2);
        assert_eq!(out, [1.0, 2.0]);
    }
}
