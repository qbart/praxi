//! The video's audio through Bevy's audio (`docs/features/bevy.md#audio`).

use crate::PraxiVideo;
use bevy::audio::{AddAudioSource, AudioPlayer, AudioSink, AudioSinkPlayback, ChannelCount, Decodable, PlaybackSettings, SampleRate, Source, Volume};
use bevy::prelude::*;
use praxi::{AudioOutput, Clock};
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Frames read from the output at a time.
const BLOCK_FRAMES: usize = 256;
/// Blocks read closer together than this belong to one device callback.
const BURST_GAP: Duration = Duration::from_millis(2);

/// Plays the audio of every [`PraxiVideo`] with a [`PraxiAudio`]; needs Bevy's `AudioPlugin`.
pub struct PraxiAudioPlugin;

impl Plugin for PraxiAudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<PraxiAudioSource>()
            .add_systems(Update, (attach, set_clock, set_volume).chain().before(crate::PraxiSystems::Advance))
            .add_observer(detach);
    }
}

/// Plays the audio tracks of the [`PraxiVideo`] on the same entity.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct PraxiAudio {
    /// Linear gain of every track.
    pub volume: f32,
    /// The video follows what is heard (`Clock::Audio` on track 0).
    pub clock: bool,
    /// How long after a sample is read it is heard; `None` estimates it.
    pub latency: Option<Duration>,
}

impl Default for PraxiAudio {
    fn default() -> Self {
        PraxiAudio { volume: 1.0, clock: true, latency: None }
    }
}

/// A child of a [`PraxiVideo`] entity playing one of its audio tracks.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct PraxiAudioTrack {
    /// Index into the player's `audio_tracks()`.
    pub track: usize,
}

/// The tracks playing for a video, and the player they were made for.
#[derive(Component)]
struct Playing {
    generation: u64,
    /// The estimated latency of track 0, in microseconds.
    latency_us: Arc<AtomicU64>,
}

/// One audio track of a player as a Bevy audio source.
#[derive(Asset, TypePath, Clone)]
pub struct PraxiAudioSource {
    output: AudioOutput,
    latency_us: Arc<AtomicU64>,
}

impl PraxiAudioSource {
    pub fn new(output: AudioOutput) -> PraxiAudioSource {
        PraxiAudioSource { output, latency_us: Arc::default() }
    }

    /// The latency estimated from how the device reads, once it has read.
    pub fn estimated_latency(&self) -> Option<Duration> {
        let us = self.latency_us.load(Ordering::Relaxed);
        (us > 0).then(|| Duration::from_micros(us))
    }
}

impl Decodable for PraxiAudioSource {
    type Decoder = PraxiAudioDecoder;

    fn decoder(&self) -> PraxiAudioDecoder {
        let channels = self.output.channels();
        PraxiAudioDecoder {
            output: self.output.clone(),
            channels: NonZero::new(channels as u16).expect("audio tracks have channels"),
            rate: NonZero::new(self.output.sample_rate()).expect("audio tracks have a sample rate"),
            block: vec![0.0; BLOCK_FRAMES * channels],
            at: 0,
            len: 0,
            latency_us: self.latency_us.clone(),
            last_read: None,
            burst_frames: 0,
        }
    }
}

/// Reads an [`AudioOutput`] in blocks, with silence when it has nothing; never ends.
pub struct PraxiAudioDecoder {
    output: AudioOutput,
    channels: ChannelCount,
    rate: SampleRate,
    block: Vec<f32>,
    at: usize,
    len: usize,
    latency_us: Arc<AtomicU64>,
    last_read: Option<Instant>,
    /// Frames read in the current burst of blocks.
    burst_frames: u64,
}

impl PraxiAudioDecoder {
    fn refill(&mut self) {
        let now = Instant::now();
        if self.last_read.is_some_and(|t| now - t > BURST_GAP) && self.burst_frames > 0 {
            // A burst is one device buffer; the one playing and the one just filled are queued.
            let us = 2 * self.burst_frames * 1_000_000 / self.rate.get() as u64;
            self.latency_us.store(us, Ordering::Relaxed);
            self.burst_frames = 0;
        }
        self.last_read = Some(now);
        let n = self.output.read(&mut self.block);
        self.block[n..].fill(0.0);
        // Silence is a whole block too, so the device keeps its pace and channels stay aligned.
        self.len = self.block.len();
        self.at = 0;
        self.burst_frames += BLOCK_FRAMES as u64;
    }
}

impl Iterator for PraxiAudioDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.at == self.len {
            self.refill();
        }
        self.at += 1;
        Some(self.block[self.at - 1])
    }
}

impl Source for PraxiAudioDecoder {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> ChannelCount {
        self.channels
    }

    fn sample_rate(&self) -> SampleRate {
        self.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// A video with [`PraxiAudio`], its tracks if they play, and its children.
type AudioVideo<'a> = (Entity, &'a mut PraxiVideo, &'a PraxiAudio, Option<&'a Playing>, Option<&'a Children>);

/// Spawns a child per audio track for videos with [`PraxiAudio`], again after `reopen`.
fn attach(
    mut commands: Commands,
    mut videos: Query<AudioVideo>,
    tracks: Query<(), With<PraxiAudioTrack>>,
    mut sources: ResMut<Assets<PraxiAudioSource>>,
) {
    for (entity, mut video, audio, playing, children) in &mut videos {
        let generation = video.generation;
        if playing.is_some_and(|p| p.generation == generation) {
            continue;
        }
        for child in children.into_iter().flatten().filter(|c| tracks.contains(**c)) {
            commands.entity(*child).despawn();
        }
        let player = video.player();
        let latency_us = Arc::new(AtomicU64::new(0));
        for track in 0..player.audio_tracks().len() {
            let Some(output) = player.audio_output(track) else { continue };
            let mut source = PraxiAudioSource::new(output);
            if track == 0 {
                source.latency_us = latency_us.clone();
            }
            let settings = PlaybackSettings { volume: Volume::Linear(audio.volume), ..PlaybackSettings::ONCE };
            commands.spawn((ChildOf(entity), PraxiAudioTrack { track }, AudioPlayer(sources.add(source)), settings));
        }
        commands.entity(entity).insert(Playing { generation, latency_us });
    }
}

/// Keeps each player's clock on its audio, or on Bevy's time.
fn set_clock(mut videos: Query<(&mut PraxiVideo, &PraxiAudio, &Playing)>) {
    for (mut video, audio, playing) in &mut videos {
        let player = video.player();
        let estimated = Duration::from_micros(playing.latency_us.load(Ordering::Relaxed));
        let clock = if audio.clock && !player.audio_tracks().is_empty() {
            Clock::Audio { track: 0, latency: audio.latency.unwrap_or(estimated) }
        } else {
            Clock::Game
        };
        if player.clock() != clock {
            player.set_clock(clock);
        }
    }
}

/// Applies a changed [`PraxiAudio::volume`] to the tracks' sinks.
fn set_volume(videos: Query<(&PraxiAudio, &Children), Changed<PraxiAudio>>, mut sinks: Query<&mut AudioSink, With<PraxiAudioTrack>>) {
    for (audio, children) in &videos {
        for child in children {
            if let Ok(mut sink) = sinks.get_mut(*child) {
                sink.set_volume(Volume::Linear(audio.volume));
            }
        }
    }
}

/// Stops the tracks and restores the game clock when [`PraxiAudio`] is removed.
fn detach(
    removed: On<Remove<PraxiAudio>>,
    mut commands: Commands,
    mut videos: Query<(&mut PraxiVideo, Option<&Children>)>,
    tracks: Query<(), With<PraxiAudioTrack>>,
) {
    let entity = removed.entity;
    let Ok((mut video, children)) = videos.get_mut(entity) else { return };
    video.player().set_clock(Clock::Game);
    // The video itself may be going away with its children.
    for child in children.into_iter().flatten().filter(|c| tracks.contains(**c)) {
        commands.entity(*child).try_despawn();
    }
    commands.entity(entity).try_remove::<Playing>();
}
