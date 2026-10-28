//! Light points per building (R2/R3 sprites) and the 100 m light carpet.
//!
//! One point per *owned* building (none for the `dark` profile; a `landmark` one is in the
//! landmark color, its floodlight) at its footprint centroid and roof height, in Godot axes
//! (x = east, y = up, z = -north) relative to the tile origin (tile center, y = 0).
//! Color by resolved light profile (see design doc "Light profiles"; world profile
//! `lights.profiles.*.color`, cell size `lights.cluster_cell_m`).
//!
//! Contract: `light_colors` rgb = average color of the lights in the point, a = intensity
//! weight `clamp(sqrt(count) / 8, 0.125, 1)` (a single light has count 1). Above ~3000 m AGL
//! the points are clustered into 100 m cells; a cluster sits at the mean position of its
//! lights, at their mean roof height. Output order is deterministic (cells sorted by index).
//!
//! Street lamps (`street_lights` / `street_light_colors`, world profile `lights.street`; `lights/street.rs`, per-band spacing
//! since api 1.11): points every `spacing_m` along the
//! centerline of each lit road (tagged `lit`, or of class `untagged_max_class` or more important), `height_m` above the
//! ground, in `lights.street.color`, a = `intensity(1)`. Only points inside the tile rectangle are kept, so a road crossing a
//! seam is lit once. Clustered into the same cells as the building lights, as a separate set (they keep their color).
//!
//! Beacons (`beacon_lights` / `beacon_light_colors`, world profile `lights.beacons`): aircraft warning lights on every owned
//! building whose resolved roof height reaches `min_height_m`, from geometry alone (no tags), so synthetic cities get them by
//! the same rule. Roof corners (the outer ring's extreme vertices along the four diagonals, pulled 0.5 m inwards; one beacon in
//! the middle for footprints narrower than `corner_min_size_m`) 0.5 m above the roof, plus the same corners every
//! `level_spacing_m` up the facade. rgb = `color` or, on `alt_share` of the buildings (by OSM id), `alt_color`; a = blink phase
//! 0..1 of the building (all its beacons flash together). Never clustered.

use crate::mesher::{resolve, PROFILE_DARK};
use crate::mvt::{Building, RoadLine, TileProjector};
use crate::profile::Profile;

mod street;
pub use street::{lamp_count, street_points};

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
    pub street_positions: Vec<[f32; 3]>,
    pub street_colors: Vec<[f32; 4]>,
    pub clustered_street_positions: Vec<[f32; 3]>,
    pub clustered_street_colors: Vec<[f32; 4]>,
    pub beacon_positions: Vec<[f32; 3]>,
    pub beacon_colors: Vec<[f32; 4]>,
    /// Airport lights of a seed world (api 1.7): runway edge / threshold / end / approach and
    /// taxiway lights inside the tile rectangle, on the drawn pavement. Never clustered.
    pub airport_positions: Vec<[f32; 3]>,
    pub airport_colors: Vec<[f32; 4]>,
}

impl LightSet {
    pub fn from_buildings(buildings: &[Building], profile: &Profile) -> Self {
        let (positions, colors) = light_points(buildings, profile);
        let (clustered_positions, clustered_colors) = cluster(&positions, &colors, profile.cluster_cell_m());
        Self { positions, colors, clustered_positions, clustered_colors, ..Self::default() }
    }
    /// [`LightSet::from_buildings`] for the tile or chunk of `proj`: ownership and centroid by
    /// [`TileProjector::place`] (identical to `from_buildings` for a whole tile).
    pub fn from_buildings_in<'a>(buildings: impl IntoIterator<Item = &'a Building>, proj: &TileProjector, profile: &Profile) -> Self {
        let (positions, colors) = light_points_in(buildings, proj, profile);
        let (clustered_positions, clustered_colors) = cluster(&positions, &colors, profile.cluster_cell_m());
        Self { positions, colors, clustered_positions, clustered_colors, ..Self::default() }
    }
    /// Adds the street lamps (and their clusters) of a tile's roads in band `ring`.
    pub fn set_streets(&mut self, roads: &[RoadLine], proj: &TileProjector, ring: usize, profile: &Profile) {
        let (p, c) = street_points(roads, proj, ring, profile);
        let (cp, cc) = cluster(&p, &c, profile.cluster_cell_m());
        (self.street_positions, self.street_colors, self.clustered_street_positions, self.clustered_street_colors) = (p, c, cp, cc);
    }
    /// Adds the beacons of the tile's tall buildings.
    pub fn set_beacons(&mut self, buildings: &[Building], proj: &TileProjector, profile: &Profile) {
        (self.beacon_positions, self.beacon_colors) = beacon_points(buildings, proj, profile);
    }
    /// [`LightSet::set_beacons`] over any sequence of buildings (a chunk's candidates).
    pub fn set_beacons_in<'a>(&mut self, buildings: impl IntoIterator<Item = &'a Building>, proj: &TileProjector, profile: &Profile) {
        (self.beacon_positions, self.beacon_colors) = beacon_points_in(buildings, proj, profile);
    }
    /// Adds a seed world's airport lights (tile pixels): y = terrain + the weighted drape margin
    /// (`Terrain::lift_weight` x the band's margin: 0 inside the fence, contract section 9
    /// "Airports") + the pavement's stacking level x step + `seed.airport.lights.height_m`, so a
    /// light sits on the runway as drawn in every band.
    pub fn set_airport(&mut self, lights: &[crate::seed::world::AirportLightPx], proj: &TileProjector, ring: usize, profile: &Profile) {
        let step = profile.ground_params(ring).map_or(0.0, |g| g.step_m);
        let lift = proj.terrain().drape_margin_m(ring) as f64;
        let h = profile.seed.airport.lights.height_m;
        self.airport_positions.clear();
        self.airport_colors.clear();
        for l in lights {
            let ([e, n], g) = proj.project_h(l.px);
            let (lat, lon) = proj.lat_lon(l.px);
            let y = g + lift * proj.terrain().lift_weight(lat, lon) + (l.level as f32 * step) as f64 + h;
            self.airport_positions.push([e as f32, y as f32, -n as f32]);
            self.airport_colors.push(l.color);
        }
    }
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty() && self.street_positions.is_empty() && self.beacon_positions.is_empty() && self.airport_positions.is_empty()
    }
    /// The version for the current mode.
    pub fn get(&self, clustered: bool) -> (&[[f32; 3]], &[[f32; 4]]) {
        if clustered {
            (&self.clustered_positions, &self.clustered_colors)
        } else {
            (&self.positions, &self.colors)
        }
    }
    /// The street lamp version for the current mode.
    pub fn get_street(&self, clustered: bool) -> (&[[f32; 3]], &[[f32; 4]]) {
        if clustered {
            (&self.clustered_street_positions, &self.clustered_street_colors)
        } else {
            (&self.street_positions, &self.street_colors)
        }
    }
    pub fn bytes(&self) -> usize {
        (self.positions.len()
            + self.clustered_positions.len()
            + self.street_positions.len()
            + self.clustered_street_positions.len()
            + self.beacon_positions.len()
            + self.airport_positions.len())
            * 28
    }
}

/// One light per owned building (count 1 each), except `dark` buildings, which have none.
pub fn light_points(buildings: &[Building], profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let placed = buildings.iter().filter(|b| b.owned).map(|b| (b, b.centroid));
    light_points_placed(placed, buildings.iter().filter(|b| b.owned).count(), profile)
}

/// [`light_points`] for the tile or chunk of `proj` ([`TileProjector::place`]: a chunk owns
/// the buildings whose centroid lies in its rectangle).
pub fn light_points_in<'a>(buildings: impl IntoIterator<Item = &'a Building>, proj: &TileProjector, profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    light_points_placed(buildings.into_iter().filter_map(|b| proj.place(b).map(|c| (b, c))), 0, profile)
}

fn light_points_placed<'a>(placed: impl Iterator<Item = (&'a Building, [f32; 2])>, n: usize, profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let mut pos = Vec::with_capacity(n);
    let mut col = Vec::with_capacity(n);
    for (b, centroid) in placed {
        let r = resolve(b, profile);
        if r.profile == PROFILE_DARK {
            continue; // dark buildings have no light point (a landmark's is its floodlight color)
        }
        pos.push([centroid[0], b.base_m + r.height, -centroid[1]]);
        let mut c = profile.profile_color(r.profile);
        c[3] = intensity(1);
        col.push(c);
    }
    (pos, col)
}

/// Salt of the per-building beacon color / phase hashes.
const BEACON_SALT: u64 = 0xB3AC_0A11_6E77_D2F5;

/// Beacons of every owned building at least `min_height_m` tall (see module docs). Buildings come in decode order (sorted),
/// so the output is deterministic.
pub fn beacon_points(buildings: &[Building], proj: &TileProjector, profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    beacon_points_placed(buildings.iter().filter(|b| b.owned).map(|b| (b, b.centroid)), proj, profile)
}

/// [`beacon_points`] for the tile or chunk of `proj` (ownership by [`TileProjector::place`]).
pub fn beacon_points_in<'a>(buildings: impl IntoIterator<Item = &'a Building>, proj: &TileProjector, profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    beacon_points_placed(buildings.into_iter().filter_map(|b| proj.place(b).map(|c| (b, c))), proj, profile)
}

fn beacon_points_placed<'a>(placed: impl Iterator<Item = (&'a Building, [f32; 2])>, proj: &TileProjector, profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let s = &profile.lights.beacons;
    let mut pos = Vec::new();
    let mut col = Vec::new();
    let mut corners: Vec<[f32; 2]> = Vec::with_capacity(4);
    for (b, c) in placed {
        let h = resolve(b, profile).height;
        if (h as f64) < s.min_height_m {
            continue;
        }
        let Some(ring) = b.polygons.first().and_then(|p| p.first()) else { continue };
        // Outer ring in meters (east, north) relative to the centroid.
        let pts = ring.iter().map(|&p| {
            let [e, n] = proj.project64(p);
            [e as f32 - c[0], n as f32 - c[1]]
        });
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        let mut best = [([0.0f32; 2], f32::MIN); 4];
        for p in pts {
            for a in 0..2 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
            for (k, (sx, sy)) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)].into_iter().enumerate() {
                let v = sx * p[0] + sy * p[1];
                if v > best[k].1 {
                    best[k] = (p, v);
                }
            }
        }
        corners.clear();
        if ((hi[0] - lo[0]) as f64) < s.corner_min_size_m || ((hi[1] - lo[1]) as f64) < s.corner_min_size_m {
            corners.push([0.0, 0.0]);
        } else {
            for (p, _) in best {
                let len = (p[0] * p[0] + p[1] * p[1]).sqrt().max(1e-3);
                let q = [p[0] * (1.0 - 0.5 / len), p[1] * (1.0 - 0.5 / len)];
                if corners.iter().all(|o| crate::math::hypotf(o[0] - q[0], o[1] - q[1]) > 2.0) {
                    corners.push(q);
                }
            }
        }
        let seed = crate::mvt::splitmix64(b.id ^ (b.osm_type.tag() << 59) ^ BEACON_SALT);
        let u = (seed >> 11) as f64 / (1u64 << 53) as f64;
        let rgb = if u < s.alt_share { s.alt_color } else { s.color };
        let phase = (crate::mvt::splitmix64(seed) >> 40) as f32 / (1u64 << 24) as f32;
        let color = [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, phase];
        let mut levels = vec![h + 0.5];
        if s.level_spacing_m > 0.0 {
            let step = s.level_spacing_m as f32;
            let mut z = step;
            while z <= h - 0.5 * step {
                levels.push(z);
                z += step;
            }
        }
        for y in levels {
            for q in &corners {
                pos.push([c[0] + q[0], b.base_m + y, -(c[1] + q[1])]);
                col.push(color);
            }
        }
    }
    (pos, col)
}

/// Cluster single lights into `cell` x `cell` m cells (tile-local grid on x/z). Each cluster:
/// mean position (x, z), mean roof height (y), average rgb, a = `intensity(count)` with `count` the lamps its points stand
/// for (`lamp_count` of their alpha: 1 for a single light, more for a far street lamp, api 1.11).
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
        let (mut n, mut lamps) = (0u32, 0u32);
        while i < idx.len() && idx[i].0 == cx && idx[i].1 == cz {
            let k = idx[i].2 as usize;
            for a in 0..3 {
                sum[a] += pos[k][a] as f64;
                sum[3 + a] += col[k][a] as f64;
            }
            n += 1;
            lamps += lamp_count(col[k][3]);
            i += 1;
        }
        let f = 1.0 / n as f64;
        out_p.push([(sum[0] * f) as f32, (sum[1] * f) as f32, (sum[2] * f) as f32]);
        out_c.push([(sum[3] * f) as f32, (sum[4] * f) as f32, (sum[5] * f) as f32, intensity(lamps)]);
    }
    (out_p, out_c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mvt::{Landuse, OsmType};


    /// Beacons from geometry: none below `min_height_m`, the four roof corners (+ intermediate rings every `level_spacing_m`)
    /// on a wide tall building, one centre beacon on a narrow one; one color and one blink phase per building.
    #[test]
    fn beacons_from_height_and_footprint_only() {
        let frame = crate::geo::Frame::new(54.6872, 25.2797);
        let key = crate::geo::tile_for(54.6872, 25.2797, 16);
        let proj = TileProjector::new(key, frame, 4096.0);
        let px = 1.0 / proj.m_per_px();
        let mk = |id: u64, h: f32, size_m: f64| {
            let (c, r) = (2048.0, 0.5 * size_m * px);
            let ring = vec![[c - r, c - r], [c + r, c - r], [c + r, c + r], [c - r, c + r], [c - r, c - r]];
            proj.finish(crate::mvt::RawBuilding {
                osm_type: OsmType::Way,
                id,
                kind: "yes".into(),
                height: Some(h),
                levels: None,
                min_height: 0.0,
                landuse: None,
                tags: Default::default(),
                polygons: vec![vec![ring]],
            })
            .expect("building")
        };
        let p = Profile::default();
        let s = &p.lights.beacons;
        // 59 m: below the 60 m default, nothing.
        assert!(beacon_points(&[mk(1, 59.0, 40.0)], &proj, &p).0.is_empty());
        // 100 m, 40 m wide: roof (100.5) + one intermediate level (45; 90 is less than half a spacing below the roof), 4 corners.
        let tall = mk(2, 100.0, 40.0);
        let (pos, col) = beacon_points(std::slice::from_ref(&tall), &proj, &p);
        assert_eq!(pos.len(), 8, "{pos:?}");
        let mut ys: Vec<f32> = pos.iter().map(|q| q[1] - tall.base_m).collect();
        ys.dedup();
        assert_eq!(ys, vec![100.5, 45.0]);
        for q in &pos {
            let (dx, dz) = (q[0] - tall.centroid[0], q[2] + tall.centroid[1]);
            assert!((dx.abs() - 19.65).abs() < 0.3 && (dz.abs() - 19.65).abs() < 0.3, "corner pulled 0.5 m in: {dx} {dz}");
        }
        assert!(col.iter().all(|c| *c == col[0]), "one color and phase per building");
        assert!(col[0][..3] == [1.0, 0.04, 0.02] || col[0][..3] == [0.1, 1.0, 0.3]);
        // Narrow (10 m < corner_min_size_m 15) and 80 m tall: one centre beacon on the roof, one at 45 m (57.5 m limit).
        let thin = mk(3, 80.0, 10.0);
        let (pos, _) = beacon_points(std::slice::from_ref(&thin), &proj, &p);
        assert_eq!(pos.len(), 2);
        assert!((pos[0][0] - thin.centroid[0]).abs() < 1e-3 && (pos[0][2] + thin.centroid[1]).abs() < 1e-3);
        // Roof only with level_spacing_m 0; about alt_share of the buildings are green.
        let mut w = crate::profile::WorldProfile::default();
        w.lights.beacons.level_spacing_m = 0.0;
        assert_eq!(beacon_points(std::slice::from_ref(&tall), &proj, &Profile::new(w).unwrap()).0.len(), 4);
        let green = (0..1000u64).filter(|&id| beacon_points(&[mk(id, 70.0, 10.0)], &proj, &p).1[0][1] == 1.0).count();
        assert!(((s.alt_share * 1000.0) as usize).abs_diff(green) < 50, "{green} of 1000 green");
    }

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
            tags: Default::default(),
            polygons: vec![],
            clipped: false,
            footprint_m2: 0.0,
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
        ], &Profile::default());
        assert_eq!(p.len(), 3);
        assert_eq!(c.len(), 3);
        // 20 m north -> z = -20.
        assert_eq!(p[0], [10.0, 30.0, -20.0]);
        assert_eq!(p[1], [10.0, 6.0, -20.0]);
        let w = |mut c: [f32; 4]| {
            c[3] = 0.125;
            c
        };
        // Cool 4500 K (office), warm 2700 K (residential), sodium (industrial).
        assert_eq!(c, vec![w([0.86, 0.9, 1.0, 1.0]), w([1.0, 0.655, 0.341, 1.0]), w([1.0, 0.55, 0.15, 1.0])]);
    }

    /// `dark` buildings have no light point, `landmark` ones the floodlight (landmark) color.
    #[test]
    fn dark_has_no_light_and_landmark_is_floodlight_colored() {
        let mk = |id, kind: &str, historic: Option<&str>| Building {
            osm_type: OsmType::Way,
            id,
            kind: kind.into(),
            height: Some(20.0),
            levels: None,
            min_height: 0.0,
            landuse: None,
            tags: crate::mvt::BuildingTags { historic: historic.map(Into::into), ..Default::default() },
            polygons: vec![],
            clipped: false,
            footprint_m2: 0.0,
            centroid_px: [0.0; 2],
            centroid: [id as f32, 0.0],
            base_m: 0.0,
            owned: true,
        };
        let p = Profile::default();
        let bs = [mk(1, "garage", None), mk(2, "castle", None), mk(3, "house", Some("ruins")), mk(4, "yes", Some("castle")), mk(5, "house", None)];
        let (pos, col) = light_points(&bs, &p);
        assert_eq!(pos.iter().map(|q| q[0]).collect::<Vec<_>>(), vec![2.0, 4.0, 5.0]);
        let flood = [1.0, 0.75, 0.45, intensity(1)];
        assert_eq!(col, vec![flood, flood, [1.0, 0.655, 0.341, intensity(1)]]);
        // Clusters count only the lights that exist.
        let set = LightSet::from_buildings(&bs, &p);
        assert_eq!(set.clustered_colors.len(), 1);
        assert_eq!(set.clustered_colors[0][3], intensity(3));
    }

    /// The synthetic castle, tower and ruin resolve to the same profile at z13 and z16, and
    /// their lights follow (floodlight color / none).
    #[test]
    fn synthetic_landmarks_resolve_the_same_at_z13_and_z16() {
        use crate::geo::{tile_for, Frame};
        use crate::mesher::{PROFILE_DARK, PROFILE_LANDMARK};
        let (lat, lon) = (54.6872, 25.2797);
        let f = Frame::new(lat, lon);
        let p = Profile::default();
        let tagged = |bs: &[Building]| -> Vec<(u64, u8)> {
            let mut v: Vec<(u64, u8)> = bs
                .iter()
                .filter(|b| b.tags.historic.is_some() || b.tags.man_made.is_some())
                .map(|b| (b.id, resolve(b, &p).profile))
                .collect();
            v.sort();
            v
        };
        let z16 = crate::synthetic::generate(tile_for(lat, lon, 16), f);
        let z13 = crate::synthetic::generate(tile_for(lat, lon, 13), f);
        let (a, b) = (tagged(&z16), tagged(&z13));
        assert_eq!(a.len(), 3, "castle, tower, ruin: {a:?}");
        assert_eq!(a, b, "same profiles at z13 and z16");
        let mut profiles: Vec<u8> = a.iter().map(|x| x.1).collect();
        profiles.sort();
        assert_eq!(profiles, vec![PROFILE_LANDMARK, PROFILE_LANDMARK, PROFILE_DARK]);
        // Lights at z13: the two landmarks in the floodlight color, the ruin has none.
        for bs in [&z13, &z16] {
            let lit: Vec<&Building> = bs.iter().filter(|b| b.owned && resolve(b, &p).profile != PROFILE_DARK).collect();
            let (pos, col) = light_points(bs, &p);
            assert_eq!(pos.len(), lit.len());
            for (b, c) in lit.iter().zip(&col) {
                if b.tags.historic.is_some() || b.tags.man_made.is_some() {
                    assert_eq!(*c, [1.0, 0.75, 0.45, intensity(1)]);
                }
            }
        }
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
