# praxi-bevy

[Bevy](https://bevyengine.org) 0.20 plugin for praxi video: the video plays into an
`Image` you can put on a sprite, in UI or on a material, with its audio through Bevy's
audio, and its events and cues as messages and observable entity events.

```rust
use bevy::prelude::*;
use bevy::render::renderer::RenderAdapter;
use praxi_bevy::{PraxiAudio, PraxiAudioPlugin, PraxiCue, PraxiCuePlugin, PraxiPlugin, PraxiVideo};

#[derive(serde::Deserialize, Clone)]
enum GameCue {
    #[serde(rename = "camera.shake")]
    Shake { strength: f32 },
    #[serde(rename = "subtitle")]
    Subtitle { text: String },
}

fn main() {
    App::new()
        .add_plugins((DefaultPlugins, PraxiPlugin, PraxiCuePlugin::<GameCue>::default(), PraxiAudioPlugin))
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands, adapter: Res<RenderAdapter>, mut images: ResMut<Assets<Image>>) {
    commands.spawn(Camera2d);
    let mut video = PraxiVideo::open("intro.praxi".as_ref(), &adapter, &mut images).unwrap();
    video.player().play().unwrap();
    commands
        .spawn((Sprite::from_image(video.image.clone()), video, PraxiAudio::default()))
        .observe(|cue: On<PraxiCue<GameCue>>| {
            // runs in the frame the cue is heard
            if let GameCue::Shake { strength } = cue.cue { info!("shake {strength}") }
        });
}
```

- `PraxiPlugin` advances every `PraxiVideo` by Bevy's time and draws it into its image.
- `PraxiCuePlugin::<T>` parses cues into your type and sends them as `PraxiCue<T>`.
- `PraxiAudioPlugin` plays the tracks of videos with `PraxiAudio` and lets the video
  follow what is heard. The `audio` feature (default) enables it.

Examples: `cue_game` (a cutscene driving subtitles, camera shakes and sounds from cues) and
`cue_editor` (a Bevy + egui cue editor).
