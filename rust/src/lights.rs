//! Light points per building (R2/R3 sprites) and the 100 m light carpet.
//!
//! One point per *owned* building at its footprint centroid and roof height, in Godot axes
//! (x = east, y = up, z = -north) relative to the tile origin (tile center, y = 0).
//! Color by resolved light profile (see design doc "Light profiles").
//!
//! Contract: `light_colors` rgb = average color of the lights in the point, a = intensity
//! weight `clamp(sqrt(count) / 8, 0.125, 1)` (a single light has count 1). Above ~3000 m AGL
//! the points are clustered into 100 m cells; a cluster sits at the mean position of its
//! lights, at their mean roof height. Output order is deterministic (cells sorted by index).

use crate::mesher::{resolve, PROFILE_CIVIC, PROFILE_INDUSTRIAL, PROFILE_OFFICE, PROFILE_RETAIL};
use crate::mvt::Building;

/// Warm 2700 K (residential).
pub const WARM_2700K: [f32; 4] = [1.0, 0.655, 0.341, 1.0];
/// Cool 4500 K (office).
pub const COOL_4500K: [f32; 4] = [0.86, 0.9, 1.0, 1.0];
/// Bright white (retail).
pub const BRIGHT_WHITE: [f32; 4] = [1.0, 0.98, 0.95, 1.0];
/// Sodium orange (industrial).
pub const SODIUM: [f32; 4] = [1.0, 0.55, 0.15, 1.0];
/// Warm floodlight ~3000 K (civic).
pub const FLOOD_3000K: [f32; 4] = [1.0, 0.706, 0.42, 1.0];

pub fn profile_color(profile: u8) -> [f32; 4] {
    match profile {
        PROFILE_OFFICE => COOL_4500K,
        PROFILE_RETAIL => BRIGHT_WHITE,
        PROFILE_INDUSTRIAL => SODIUM,
        PROFILE_CIVIC => FLOOD_3000K,
        _ => WARM_2700K,
    }
}

/// Cell size (m) of the clustered light carpet.
pub const CLUSTER_CELL_M: f32 = 100.0;

/// Intensity weight of a point that stands for `count` lights.
pub fn intensity(count: u32) -> f32 {
    ((count as f32).sqrt() / 8.0).clamp(0.125, 1.0)
}

/// Both light versions of one tile: one point per building, and the 100 m clusters.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LightSet {
    pub positions: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub clustered_positions: Vec<[f32; 3]>,
    pub clustered_colors: Vec<[f32; 4]>,
}

impl LightSet {
    pub fn from_buildings(buildings: &[Building]) -> Self {
        let (positions, colors) = light_points(buildings);
        let (clustered_positions, clustered_colors) = cluster(&positions, &colors, CLUSTER_CELL_M);
        Self { positions, colors, clustered_positions, clustered_colors }
    }
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }
    /// The version for the current mode.
    pub fn get(&self, clustered: bool) -> (&[[f32; 3]], &[[f32; 4]]) {
        if clustered {
            (&self.clustered_positions, &self.clustered_colors)
        } else {
            (&self.positions, &self.colors)
        }
    }
    pub fn bytes(&self) -> usize {
        (self.positions.len() + self.clustered_positions.len()) * 28
    }
}

/// One light per owned building (count 1 each).
pub fn light_points(buildings: &[Building]) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let n = buildings.iter().filter(|b| b.owned).count();
    let mut pos = Vec::with_capacity(n);
    let mut col = Vec::with_capacity(n);
    for b in buildings.iter().filter(|b| b.owned) {
        let r = resolve(b);
        pos.push([b.centroid[0], b.base_m + r.height, -b.centroid[1]]);
        let mut c = profile_color(r.profile);
        c[3] = intensity(1);
        col.push(c);
    }
    (pos, col)
}

/// Cluster single lights into `cell` x `cell` m cells (tile-local grid on x/z). Each cluster:
/// mean position (x, z), mean roof height (y), average rgb, a = `intensity(count)`.
pub fn cluster(pos: &[[f32; 3]], col: &[[f32; 4]], cell: f32) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    // (cell x, cell z, index): sort for a deterministic order independent of input order.
    let mut idx: Vec<(i32, i32, u32)> = pos
        .iter()
        .enumerate()
        .map(|(i, p)| ((p[0] / cell).floor() as i32, (p[2] / cell).floor() as i32, i as u32))
        .collect();
    idx.sort_unstable();
    let mut out_p = Vec::new();
    let mut out_c = Vec::new();
    let mut i = 0;
    while i < idx.len() {
        let (cx, cz, _) = idx[i];
        let mut sum = [0.0f64; 6];
        let mut n = 0u32;
        while i < idx.len() && idx[i].0 == cx && idx[i].1 == cz {
            let k = idx[i].2 as usize;
            for a in 0..3 {
                sum[a] += pos[k][a] as f64;
                sum[3 + a] += col[k][a] as f64;
            }
            n += 1;
            i += 1;
        }
        let f = 1.0 / n as f64;
        out_p.push([(sum[0] * f) as f32, (sum[1] * f) as f32, (sum[2] * f) as f32]);
        out_c.push([(sum[3] * f) as f32, (sum[4] * f) as f32, (sum[5] * f) as f32, intensity(n)]);
    }
    (out_p, out_c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{Landuse, OsmType};

    #[test]
    fn one_light_per_owned_building_godot_axes_and_profile_colors() {
        let mk = |owned, h, kind: &str, lu| Building {
            osm_type: OsmType::Way,
            id: 1,
            kind: kind.into(),
            height: h,
            levels: None,
            min_height: 0.0,
            landuse: lu,
            polygons: vec![],
            centroid_px: [0.0; 2],
            centroid: [10.0, 20.0],
            base_m: 0.0,
            owned,
        };
        let (p, c) = light_points(&[
            mk(true, Some(30.0), "office", None),
            mk(false, None, "yes", None),
            mk(true, None, "yes", None),
            mk(true, None, "yes", Some(Landuse::Industrial)),
        ]);
        assert_eq!(p.len(), 3);
        assert_eq!(c.len(), 3);
        // 20 m north -> z = -20.
        assert_eq!(p[0], [10.0, 30.0, -20.0]);
        assert_eq!(p[1], [10.0, 6.0, -20.0]);
        let w = |mut c: [f32; 4]| {
            c[3] = 0.125;
            c
        };
        assert_eq!(c, vec![w(COOL_4500K), w(WARM_2700K), w(SODIUM)]);
    }

    #[test]
    fn intensity_weight_formula() {
        assert_eq!(intensity(1), 0.125);
        assert_eq!(intensity(4), 0.25);
        assert_eq!(intensity(16), 0.5);
        assert_eq!(intensity(64), 1.0);
        assert_eq!(intensity(1000), 1.0);
    }

    #[test]
    fn clustering_means_average_color_and_is_deterministic() {
        let pos = vec![[10.0, 20.0, -10.0], [90.0, 40.0, -90.0], [150.0, 9.0, 5.0], [-5.0, 3.0, 1.0]];
        let col = vec![[1.0, 0.0, 0.0, 0.125], [0.0, 1.0, 0.0, 0.125], [0.0, 0.0, 1.0, 0.125], [1.0, 1.0, 1.0, 0.125]];
        let (p, c) = cluster(&pos, &col, 100.0);
        // Cells: (0,-1) has the first two, (1,0) the third, (-1,0) the fourth; sorted by cell.
        assert_eq!(p.len(), 3);
        assert_eq!(p[0], [-5.0, 3.0, 1.0]);
        assert_eq!(p[1], [50.0, 30.0, -50.0]);
        assert_eq!(c[1], [0.5, 0.5, 0.0, intensity(2)]);
        assert_eq!(p[2], [150.0, 9.0, 5.0]);
        // Input order does not matter.
        let mut pr: Vec<_> = pos.iter().copied().zip(col.iter().copied()).collect();
        pr.reverse();
        let (p2, c2) = cluster(&pr.iter().map(|x| x.0).collect::<Vec<_>>(), &pr.iter().map(|x| x.1).collect::<Vec<_>>(), 100.0);
        assert_eq!((p, c), (p2, c2));
    }
}
