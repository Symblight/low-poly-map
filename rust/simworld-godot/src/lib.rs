//! SimWorld Godot binding: the `WorldStreamer` GDExtension node.
//!
//! Rust owns the world (`simworld-core`); Godot only displays what `WorldStreamer` hands it.
//! This crate converts types and maps host switches (logging, `SIMPLANE_PROFILE`) onto the core;
//! it holds no world logic.

use godot::prelude::*;

mod convert;
mod host;
mod ready;
mod seed;
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

    /// Every `#[func]` of `WorldStreamer` is documented in `docs/contract.md` (as `` `name(``).
    #[test]
    fn every_func_is_in_contract_doc() {
        let src = include_str!("streamer.rs");
        let doc = include_str!("../../../docs/contract.md");
        let mut lines = src.lines();
        let mut n = 0;
        while let Some(l) = lines.next() {
            if l.trim() != "#[func]" {
                continue;
            }
            let sig = lines.next().expect("fn after #[func]");
            let name = sig.trim().strip_prefix("fn ").and_then(|r| r.split('(').next()).expect("fn name");
            assert!(doc.contains(&format!("`{name}(")), "#[func] {name} is missing from docs/contract.md");
            n += 1;
        }
        assert_eq!(n, 24, "WorldStreamer #[func] count changed: update docs/contract.md and this test");
    }

    /// Every `#[signal]` of `WorldStreamer` is documented in `docs/contract.md` (as
    /// `` signal `name( ``).
    #[test]
    fn every_signal_is_in_contract_doc() {
        let src = include_str!("streamer.rs");
        let doc = include_str!("../../../docs/contract.md");
        let mut lines = src.lines();
        let mut names = Vec::new();
        while let Some(l) = lines.next() {
            if l.trim() != "#[signal]" {
                continue;
            }
            let sig = lines.next().expect("fn after #[signal]");
            let name = sig.trim().strip_prefix("fn ").and_then(|r| r.split('(').next()).expect("signal name");
            assert!(doc.contains(&format!("signal `{name}(")), "#[signal] {name} is missing from docs/contract.md");
            names.push(name);
        }
        assert_eq!(names, ["origin_shifted", "status_changed"], "WorldStreamer signals changed: update docs/contract.md and this test");
    }
}
