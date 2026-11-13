//! Godot 4 extension playing praxi files in `VideoStreamPlayer` (`docs/features/godot.md`).

// `#[derive(GodotClass)]` expands `base: base` in its generated `init`.
#![allow(clippy::redundant_field_names)]

mod file;
mod playback;
mod stream;

use godot::classes::{ResourceFormatLoader, ResourceLoader};
use godot::prelude::*;
use std::cell::RefCell;

pub use playback::VideoStreamPlaybackPraxi;
pub use stream::{ResourceFormatLoaderPraxi, VideoStreamPraxi};

struct PraxiExtension;

thread_local! {
    /// The `.praxi` loader, registered while the extension is loaded.
    static LOADER: RefCell<Option<Gd<ResourceFormatLoader>>> = const { RefCell::new(None) };
}

#[gdextension]
unsafe impl ExtensionLibrary for PraxiExtension {
    fn on_stage_init(stage: InitStage) {
        if stage == InitStage::Scene {
            let loader = ResourceFormatLoaderPraxi::new_gd().upcast::<ResourceFormatLoader>();
            ResourceLoader::singleton().add_resource_format_loader(&loader);
            LOADER.with(|l| *l.borrow_mut() = Some(loader));
        }
    }

    fn on_stage_deinit(stage: InitStage) {
        if stage == InitStage::Scene
            && let Some(loader) = LOADER.with(|l| l.borrow_mut().take())
        {
            ResourceLoader::singleton().remove_resource_format_loader(&loader);
        }
    }
}
