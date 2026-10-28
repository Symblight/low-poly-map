//! The synthetic city as a two-level data source (step T2c): its data tiles are generated, not
//! decoded, and cut by the same chunk path as an archive's.

use std::time::Instant;

use super::{build_chunk_with, DataLevel, DataLevels, DataTile};
use crate::geo::{Frame, TileKey};
use crate::profile::Profile;
use crate::terrain::Terrain;
use crate::worker::TileData;

/// The synthetic city's data tile `key` of `level` (step T2c): generated, not decoded.
pub fn synthetic_data_tile(key: TileKey, level: DataLevel, frame: Frame, terrain: &Terrain, profile: &Profile) -> DataTile {
    let t0 = Instant::now();
    match level {
        DataLevel::Detail => {
            let (buildings, ground, proj) = crate::synthetic::detail_level(key, frame, terrain, profile);
            DataTile::detail(key, buildings, ground, proj, t0)
        }
        DataLevel::Overview => {
            let o = crate::synthetic::overview_level(key, frame, terrain, profile);
            DataTile::overview(key, o.blocks, o.landmarks, o.lights, o.light_landmark, o.ground, o.proj, t0).with_open_bases()
        }
    }
}

/// A synthetic city chunk exactly as the streaming path builds it (T2c): generated data tile(s)
/// of [`DataLevels::SYNTHETIC`], cut by [`build_chunk_with`]; landmarks folded in unless
/// `landmark_layer` (the host draws the landmark layer). Golden tests and reference builds.
pub fn synthetic_chunk(chunk: TileKey, ring: usize, frame: Frame, terrain: &Terrain, profile: &Profile, landmark_layer: bool) -> Result<TileData, String> {
    let l = DataLevels::SYNTHETIC;
    let dk = l.data_of(chunk).ok_or_else(|| format!("{chunk}: not a synthetic chunk"))?;
    let dt = synthetic_data_tile(dk, l.level_of(dk), frame, terrain, profile);
    let lk = if landmark_layer { None } else { l.landmarks_of(chunk) };
    let other = lk.filter(|k| *k != dk).map(|k| synthetic_data_tile(k, l.level_of(k), frame, terrain, profile));
    let lm = match (lk, &other) {
        (None, _) => None,
        (Some(_), Some(o)) => Some(o),
        (Some(_), None) => Some(&dt),
    };
    build_chunk_with(&dt, lm, chunk, ring, profile, &|| false).map(|d| d.expect("never stopped"))
}
