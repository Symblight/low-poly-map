//! SimWorld Godot binding: the `WorldStreamer` GDExtension node.
//!
//! Rust owns the world (`simworld-core`); Godot only displays what `WorldStreamer` hands it.
//! This crate converts types and maps host switches (logging, `SIMPLANE_PROFILE`) onto the core;
//! it holds no world logic.

use godot::prelude::*;

mod host;
mod streamer;

pub use streamer::WorldStreamer;

/// Crate version plus a build marker, exposed via `debug_stats()["version"]` so the
/// editor can show that a hot reload picked up a fresh build.
pub fn version_string() -> String {
    format!(
        "{} {} ({})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        env!("SIMPLANE_BUILD_MARKER")
    )
}

struct SimplaneWorld;

#[gdextension]
unsafe impl ExtensionLibrary for SimplaneWorld {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_contains_crate_version_and_marker() {
        let v = version_string();
        assert!(v.starts_with("simworld-godot "));
        assert!(v.contains(env!("CARGO_PKG_VERSION")));
        assert!(v.contains("build "));
    }
}
