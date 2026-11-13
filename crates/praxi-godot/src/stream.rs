//! `VideoStreamPraxi` and the `.praxi` resource loader (`docs/features/godot.md#classes`).

use crate::playback::VideoStreamPlaybackPraxi;
use godot::classes::{IResourceFormatLoader, IVideoStream, ResourceFormatLoader, VideoStream, VideoStreamPlayback};
use godot::prelude::*;

/// A `.praxi` file for `VideoStreamPlayer`.
#[derive(GodotClass)]
#[class(base = VideoStream, init, tool)]
pub struct VideoStreamPraxi {
    base: Base<VideoStream>,
    /// Seamless looping in the player.
    #[export]
    #[var(set = set_looping)]
    looping: bool,
    /// Accepted texture formats by name; empty: what the rendering device supports.
    #[var]
    #[export]
    formats: PackedStringArray,
    /// The playback made last, which signals and cue queries refer to.
    playback: Option<InstanceId>,
}

#[godot_api]
impl IVideoStream for VideoStreamPraxi {
    fn instantiate_playback(&mut self) -> Option<Gd<VideoStreamPlayback>> {
        let path = self.base().get_file().to_string();
        let mut playback = VideoStreamPlaybackPraxi::new_gd();
        playback.bind_mut().open(&path, &self.formats, self.looping, self.to_gd());
        self.playback = Some(playback.instance_id());
        Some(playback.upcast())
    }
}

#[godot_api]
impl VideoStreamPraxi {
    /// A cue started; `position` is a `Vector2` in `0..1` of the video, or null.
    #[signal]
    pub(crate) fn cue(id: i64, cue_type: GString, fields: VarDictionary, time: f64, duration: f64, position: Variant);

    /// A cue with a duration ended.
    #[signal]
    pub(crate) fn cue_ended(id: i64);

    /// Playback continued from the start (with `looping`).
    #[signal]
    pub(crate) fn looped();

    #[func]
    fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
        if let Some(mut p) = self.current() {
            p.bind_mut().set_looping(looping);
        }
    }

    /// Cues active at the clock of the current playback.
    #[func]
    fn active_cues(&self) -> Array<VarDictionary> {
        self.current().map_or_else(Array::new, |p| p.bind().active_cues())
    }

    /// Cues starting within `within` seconds, each with `until`.
    #[func]
    fn upcoming_cues(&self, within: f64) -> Array<VarDictionary> {
        self.current().map_or_else(Array::new, |p| p.bind().upcoming_cues(within))
    }
}

impl VideoStreamPraxi {
    fn current(&self) -> Option<Gd<VideoStreamPlaybackPraxi>> {
        self.playback.and_then(|id| Gd::try_from_instance_id(id).ok())
    }
}

/// Loads `.praxi` files as [`VideoStreamPraxi`].
#[derive(GodotClass)]
#[class(base = ResourceFormatLoader, init, tool)]
pub struct ResourceFormatLoaderPraxi {
    base: Base<ResourceFormatLoader>,
}

const EXTENSION: &str = "praxi";
const TYPE: &str = "VideoStreamPraxi";

#[godot_api]
impl IResourceFormatLoader for ResourceFormatLoaderPraxi {
    fn get_recognized_extensions(&self) -> PackedStringArray {
        PackedStringArray::from(&[GString::from(EXTENSION)])
    }

    fn handles_type(&self, type_: StringName) -> bool {
        type_ == StringName::from(TYPE) || type_ == StringName::from("VideoStream")
    }

    fn get_resource_type(&self, path: GString) -> GString {
        if path.to_string().to_lowercase().ends_with(".praxi") { GString::from(TYPE) } else { GString::new() }
    }

    fn load(&self, path: GString, _original_path: GString, _use_sub_threads: bool, _cache_mode: i32) -> Variant {
        let mut stream = VideoStreamPraxi::new_gd();
        stream.upcast_mut::<VideoStream>().set_file(&path);
        stream.to_variant()
    }
}
