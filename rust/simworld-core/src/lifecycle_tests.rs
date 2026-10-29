//! Host API and lifecycle (api 0.7): geodetic input and conversions, origin shift reporting,
//! status, shutdown, the cache folder lock, several worlds side by side and teleport.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::geo::TileKey;
use crate::rings::LayerKey;
use crate::status::{StatusState, MSG_NOT_CONFIGURED, MSG_OFFLINE, MSG_SHUT_DOWN};
use crate::world::{World, WorldOptions};

mod cache_profile;
mod host_api;
mod jobs_memory;
mod probes;
mod terrain_layer;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);
/// Nothing listens on the discard port: connections are refused at once (no network needed).
const DEAD_URL: &str = "http://127.0.0.1:9/none.pmtiles";

fn world() -> World {
    World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test")
}

fn synthetic(lat: f64, lon: f64) -> WorldOptions {
    WorldOptions { origin_lat: lat, origin_lon: lon, ..WorldOptions::default() }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("simworld-lifecycle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn with_cache(url: &str, dir: &std::path::Path) -> WorldOptions {
    WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        tile_url: url.into(),
        cache_dir: dir.to_string_lossy().into_owned(),
        ..WorldOptions::default()
    }
}

/// Fly a static camera at 300 m AGL until `want` tiles were delivered (or 20 s). Returns the
/// keys held by the host (delivered minus unloaded) and every (key, origin) delivered.
fn run_until(w: &mut World, want: usize) -> (BTreeSet<LayerKey>, Vec<(LayerKey, [f64; 3])>) {
    let mut held = BTreeSet::new();
    let mut delivered = Vec::new();
    let t0 = Instant::now();
    while delivered.len() < want && t0.elapsed() < Duration::from_secs(20) {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(64, |t| {
            held.insert(t.layer_key());
            delivered.push((t.layer_key(), t.origin));
        });
        let _ = w.drain_fades();
        for k in w.drain_unload() {
            held.remove(k);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    (held, delivered)
}
