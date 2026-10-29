//! CPU stages of a tile job: decode a payload, build a vector / seed / terrain tile, with stop
//! checks and panic capture.

use super::*;

/// CPU stage: payload -> buildings, ground areas (if `want_ground`), roads (if `want_roads`) and projector
/// (shared by real and synthetic tiles). Flat terrain; see [`decode_payload_on`].
pub fn decode_payload(
    key: TileKey,
    frame: Frame,
    payload: &Payload,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    decode_payload_on(key, frame, &Terrain::flat(), payload, want_ground, want_roads, profile)
}

/// [`decode_payload`] on `terrain` (the projector of the result carries it).
pub fn decode_payload_on(
    key: TileKey,
    frame: Frame,
    terrain: &Terrain,
    payload: &Payload,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    match payload {
        Payload::Synthetic => {
            let mut ground =
                if want_ground || want_roads { synthetic::generate_ground(key, frame, profile) } else { GroundFeatures::default() };
            if !want_ground {
                ground.areas.clear();
            }
            if !want_roads {
                ground.roads.clear();
            }
            Ok(DecodedTile {
                buildings: synthetic::generate_on(key, frame, terrain),
                ground,
                proj: TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone()),
            })
        }
        Payload::Bytes(b) => decode_tile_on(b, key, frame, terrain, want_ground, want_roads, profile),
    }
}

/// CPU stage: payload -> buildings + projector.
pub fn tile_buildings_proj(
    key: TileKey,
    frame: Frame,
    payload: &Payload,
    profile: &Profile,
) -> Result<(Vec<Building>, TileProjector), String> {
    decode_payload(key, frame, payload, false, false, profile).map(|t| (t.buildings, t.proj))
}

/// CPU stage: payload -> buildings.
pub fn tile_buildings(key: TileKey, frame: Frame, payload: &Payload, profile: &Profile) -> Result<Vec<Building>, String> {
    tile_buildings_proj(key, frame, payload, profile).map(|(b, _)| b)
}

/// CPU stage: payload -> flat per-tile output (building + ground meshes per ring, lights). The
/// decoded structs are dropped here, on the worker; only flat buffers cross to the main thread.
/// Flat terrain; see [`build_tile_on`].
pub fn build_tile(key: TileKey, ring: usize, frame: Frame, payload: &Payload, profile: &Profile) -> Result<TileData, String> {
    build_tile_on(key, ring, frame, &Terrain::flat(), payload, profile)
}

/// [`build_tile`] on `terrain`: building bases (+ foundations), draped ground and light heights
/// all sample it through the tile's projector. This is what the worker pool runs.
pub fn build_tile_on(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    payload: &Payload,
    profile: &Profile,
) -> Result<TileData, String> {
    build_tile_stoppable(key, ring, frame, terrain, payload, profile, &never).map(|d| d.expect("never stopped"))
}

fn never() -> bool {
    false
}

/// [`build_tile_on`] with a cancel probe checked between stages: `Ok(None)` = stopped. The
/// output of a build that is not stopped is identical.
pub(super) fn build_tile_stoppable(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    payload: &Payload,
    profile: &Profile,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    let t0 = std::time::Instant::now();
    let want_ground = profile.ground_params(ring).is_some();
    let want_streets = ring >= profile.lights.street.min_ring as usize;
    if stop() {
        return Ok(None);
    }
    let DecodedTile { buildings, ground: gf, proj } =
        decode_payload_on(key, frame, terrain, payload, want_ground, want_ground || want_streets, profile)?;
    Ok(finish_tile(buildings, gf, &proj, ring, profile, &[], t0, stop))
}

/// The seed generator's tile (api 1.7, `docs/spec-world-seed.md` section 7): generated in tile
/// pixels from `(key, seed)` alone, then projected, meshed and lit exactly like archive data,
/// plus the airport lights (`airport_lights`).
pub fn build_seed_tile_on(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    seed: &SeedWorld,
    profile: &Profile,
) -> Result<TileData, String> {
    build_seed_tile_stoppable(key, ring, frame, terrain, seed, profile, &never).map(|d| d.expect("never stopped"))
}

/// [`build_seed_tile_on`] with a cancel probe (see [`build_tile_stoppable`]).
pub(super) fn build_seed_tile_stoppable(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    seed: &SeedWorld,
    profile: &Profile,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    if stop() {
        return Ok(None);
    }
    let t0 = std::time::Instant::now();
    let want_ground = profile.ground_params(ring).is_some();
    let want_streets = ring >= profile.lights.street.min_ring as usize;
    let st = match crate::seed::world::route(key, seed) {
        crate::seed::world::TileSource::Seed => crate::seed::world::generate(key, seed),
    };
    let proj = TileProjector::new(key, frame, crate::seed::world::EXTENT).with_terrain(terrain.clone());
    let buildings: Vec<Building> = st.buildings.into_iter().filter_map(|b| proj.finish(b)).collect();
    let mut gf = st.ground;
    if !want_ground {
        gf.areas.clear();
    }
    if !(want_ground || want_streets) {
        gf.roads.clear();
    }
    Ok(finish_tile(buildings, gf, &proj, ring, profile, &st.airport_lights, t0, stop))
}

/// Mesh, ground and lights of a decoded or generated tile (shared by every source). `stop` is
/// checked between the stages (safe points); `None` = stopped.
#[allow(clippy::too_many_arguments)]
fn finish_tile(
    buildings: Vec<Building>,
    gf: GroundFeatures,
    proj: &TileProjector,
    ring: usize,
    profile: &Profile,
    airport_lights: &[crate::seed::world::AirportLightPx],
    t0: std::time::Instant,
    stop: &dyn Fn() -> bool,
) -> Option<TileData> {
    let want_streets = ring >= profile.lights.street.min_ring as usize;
    if stop() {
        return None;
    }
    let mesh = mesh_tile(&buildings, proj, ring, profile);
    if stop() {
        return None;
    }
    let ground = mesh_ground(&gf, proj, ring, profile);
    if stop() {
        return None;
    }
    let mut lights = if ring >= profile.lights.min_ring as usize { LightSet::from_buildings(&buildings, profile) } else { LightSet::default() };
    if want_streets {
        lights.set_streets(&gf.roads, proj, ring, profile);
    }
    if ring >= profile.lights.beacons.min_ring as usize {
        lights.set_beacons(&buildings, proj, profile);
    }
    if !airport_lights.is_empty() {
        lights.set_airport(airport_lights, proj, ring, profile);
    }
    drop(gf);
    let building_count = buildings.len();
    let max_height = buildings.iter().map(|b| crate::mesher::resolve(b, profile).height).fold(0.0f32, f32::max);
    drop(buildings);
    Some(TileData {
        building_count,
        lights: Arc::new(lights),
        mesh: Arc::new(mesh),
        ground: Arc::new(ground),
        max_height,
        cpu_us: t0.elapsed().as_micros() as u64,
        terrain: None,
    })
}

/// Terrain layer: the TIN of terrain tile `key` (ring = index in `terrain.bands`) as a
/// [`TileData`] with only `terrain` set (box height 0: the box is the terrain range).
pub fn build_terrain_data(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
    seed: Option<&SeedWorld>,
) -> Result<TileData, String> {
    build_terrain_data_until(key, ring, frame, terrain, profile, detail, seed, &|| false).map(|d| d.expect("never stopped"))
}

/// [`build_terrain_data`] that stops at the TIN build's safe points when `stop()` (`Ok(None)`).
#[allow(clippy::too_many_arguments)]
pub fn build_terrain_data_until(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
    seed: Option<&SeedWorld>,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    let t0 = std::time::Instant::now();
    // A seed world's terrain carries COLOR: r = ground class / 8, g = tint / 8 (api 1.7).
    let color = seed.map(|s| {
        move |lat: f64, lon: f64| {
            let (class, tint) = s.class_at(lat, lon);
            [class as f32 / 8.0, tint as f32 / 8.0, 0.0, 1.0]
        }
    });
    let color = color.as_ref().map(|c| c as &dyn Fn(f64, f64) -> [f32; 4]);
    let Some(tile) = build_terrain_tile_until(key, ring, frame, terrain, profile, detail, color, stop)? else { return Ok(None) };
    Ok(Some(TileData { terrain: Some(Arc::new(tile)), cpu_us: t0.elapsed().as_micros() as u64, ..TileData::default() }))
}

pub(super) fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}
