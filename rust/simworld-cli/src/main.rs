//! simworld-cli: headless runner for the SimWorld core (placeholder).
//!
//! Real commands (fly scripts, cache bake, tile dumps, benches) arrive in phase P6 of
//! docs/spec-library-packaging.md. For now this only proves the core links without Godot.

fn main() {
    println!(
        "simworld-cli {} (simworld-core {}): no commands yet (planned in P6: fly, bake, dump, bench)",
        env!("CARGO_PKG_VERSION"),
        simworld_core::VERSION
    );
}
