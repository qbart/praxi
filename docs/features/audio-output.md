# Audio output

praxi hands audio to the engine's mixer and does not talk to audio devices
([audio.md](audio.md)). The viewer and the editor still need sound, so they share a small
output crate, `praxi-audio`. It is a tool for praxi's own programs, not part of the
runtime: games use their engine's audio.

## Crate

`crates/praxi-audio`, library, dependencies `praxi` and `cpal`. The viewer and the editor
depend on it through their `audio` feature (default on). Before this crate, the editor
included `crates/praxi-viewer/src/audio.rs` by path.

- `Device::open_default() -> Option<Device>`: the default output device.
- `Device::name()`, `Device::channels()`: its name and the channel count of its default
  configuration, which the programs pass to `Capabilities::max_audio_channels`.
- `Device::start(outputs: Vec<AudioOutput>, muted: Arc<AtomicBool>) -> Result<Stream>`:
  plays the sum of `outputs` until the `Stream` is dropped. Muted outputs are still read,
  so they stay in sync with the clock.
- `Stream::config() -> (u32, u16)`: the device's sample rate and channels.
- `Stream::latency() -> Duration`: from `AudioOutput::read` of `outputs[0]` to the last
  frame read being heard, for `Clock::Audio`.

## Stream format

`start` opens the device at the rate and channel count of `outputs[0]` when the device
supports them, so the common case converts nothing. Otherwise it opens the device's
default configuration. Sample formats are `f32`, `i16` or `i32`, preferred in that order.

Each output is then converted to the stream format on its own, so tracks with different
rates and channel counts all play (before, only tracks matching track 0 played, and
nothing played if the device refused track 0's format):

- **Rate**: cubic (Catmull-Rom) interpolation between source frames. It keeps the 3 most
  recent source frames between callbacks. The frames read but not yet played count toward
  `latency`. It is a preview, not a mastering resampler: downsampling aliases a little
  above the new Nyquist frequency.
- **Channels**: the same count is copied; mono goes to channels 0 and 1 (front left and
  right) of a wider stream; stereo to mono is the average; any other pair copies the
  channels both have and leaves the rest silent. The player has already picked the layout
  variant that fits `max_audio_channels` ([audio.md](audio.md)), so this is rare.

## Tests

The resampler and channel mapping are pure functions, tested without a device: a sine
resampled from 48 kHz to 44.1 kHz and back keeps its frequency and amplitude, equal rates
pass samples through unchanged, the output length follows the rate ratio over many
callbacks without drift, and every channel mapping above.
