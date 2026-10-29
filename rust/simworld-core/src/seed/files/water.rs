//! `water.json`: sea, lake and river polygons.

use serde::{Deserialize, Serialize};

use super::q7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaterKind {
    Sea,
    Lake,
    River,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outside {
    Sea,
    Land,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterPolygon {
    pub kind: WaterKind,
    /// Outer ring first, then holes; [lon, lat]; open (the loader drops a closing duplicate).
    pub rings: Vec<Vec<[f64; 2]>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterFile {
    pub format: u32,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_date: Option<String>,
    /// [west, south, east, north].
    pub bbox: [f64; 4],
    pub outside: Outside,
    pub tolerance_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_area_m2: Option<f64>,
    pub polygons: Vec<WaterPolygon>,
}

/// Quantize, drop consecutive duplicates and the closing duplicate. `None` when fewer than 3
/// distinct points remain.
pub fn clean_ring(ring: &[[f64; 2]]) -> Option<Vec<[f64; 2]>> {
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(ring.len());
    for p in ring {
        let q = [q7(p[0]), q7(p[1])];
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    while out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    (out.len() >= 3).then_some(out)
}
