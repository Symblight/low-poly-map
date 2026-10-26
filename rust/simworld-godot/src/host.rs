//! Godot host mapping for the core's diagnostics: `Log` onto `godot_print!` / `godot_warn!` /
//! `godot_error!`, and the `SIMPLANE_PROFILE=1` environment switch onto `Diagnostics`.

use godot::prelude::*;
use simworld_core::{Diagnostics, Log};

pub struct GodotLog;

impl Log for GodotLog {
    fn info(&self, msg: &str) {
        godot_print!("{msg}");
    }
    fn warn(&self, msg: &str) {
        godot_warn!("{msg}");
    }
    fn error(&self, msg: &str) {
        godot_error!("{msg}");
    }
}

/// `SIMPLANE_PROFILE=1`: log a phase breakdown for main-thread calls slower than 1 ms.
pub fn diagnostics_from_env() -> Diagnostics {
    Diagnostics {
        profile: std::env::var("SIMPLANE_PROFILE").is_ok_and(|v| v == "1"),
        ..Diagnostics::default()
    }
}
