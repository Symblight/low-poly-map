// Emits SIMPLANE_BUILD_MARKER (profile + unix build time) so each `cargo build`
// produces a visibly different version string after a hot reload.
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=SIMPLANE_BUILD_MARKER=build {profile} @{secs}");
    // Re-run whenever any source changes, so the marker updates with every real rebuild.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
}
