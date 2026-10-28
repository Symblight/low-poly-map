//! Street lamps (`street_lights` / `street_light_colors`, world profile `lights.street`, api 1.1).
//!
//! A lamp every `spacing_m x spacing_scale[band]` along the centerline of each lit road (tagged
//! `lit`, or of class `untagged_max_class` or more important), the first half a spacing from the
//! line start, `height_m` above the ground, in `color` or (per road, by OSM id) `alt_color`.
//! Only points inside the tile rectangle are kept, so a road crossing a seam is lit once.
//!
//! **Far bands [1.11, profile 7]** (perf review 3 item 3): an R3 tile carried up to 4200 lamps at
//! 2-3 px each. `spacing_scale` (default `[1, 1, 1, 2]`) spaces the lamps of a band `k` times
//! further apart, and each lamp then stands for `k` lamps: alpha = `intensity(k)` (the cluster
//! encoding, `sqrt(k) / 8`), so the host can restore the energy (`count = (8 a)^2`). Clusters
//! sum those counts ([`lamp_count`]).


use crate::mvt::{RoadLine, TileProjector};
use crate::profile::Profile;

/// Salt of the per-road lamp-type hash (keeps it independent of other uses of the OSM id).
const STREET_TYPE_SALT: u64 = 0x57EE_71A3_9C2D_4B61;

/// Lamps a light point stands for, from its intensity weight `a` (`intensity(count)`, exact for
/// counts up to 64, where the weight saturates).
pub fn lamp_count(a: f32) -> u32 {
    ((a as f64 * 8.0).powi(2)).round().max(1.0) as u32
}

/// The street lamps of band `ring` (see the module docs). Roads come sorted
/// (`GroundFeatures::sort`), so the order is deterministic.
pub fn street_points(roads: &[RoadLine], proj: &TileProjector, ring: usize, profile: &Profile) -> (Vec<[f32; 3]>, Vec<[f32; 4]>) {
    let s = &profile.lights.street;
    let scale = s.spacing_scale.get(ring).copied().unwrap_or(1.0).max(1.0);
    let spacing = (s.spacing_m * scale).max(1.0);
    // `intensity(k)` for a fractional k too (the same f32 formula).
    let weight = ((scale as f32).sqrt() / 8.0).clamp(0.125, 1.0);
    // The tile rectangle, or the chunk's (a cut projector): each lamp lands in exactly one.
    let rect = proj.rect();
    let main = [s.color[0] as f32, s.color[1] as f32, s.color[2] as f32, weight];
    let alt = [s.alt_color[0] as f32, s.alt_color[1] as f32, s.alt_color[2] as f32, weight];
    let mut pos = Vec::new();
    let mut col = Vec::new();
    let mut proj_pts: Vec<([f64; 2], f64)> = Vec::new();
    for r in roads.iter().filter(|r| r.lit || (r.class as u32) <= s.untagged_max_class) {
        // Lamp type per road (OSM id): the same in every tile and band the road crosses.
        let u = (crate::mvt::splitmix64(r.id ^ STREET_TYPE_SALT) >> 11) as f64 / (1u64 << 53) as f64;
        let color = if u < s.alt_share { alt } else { main };
        for line in &r.lines {
            proj_pts.clear();
            proj_pts.extend(line.iter().map(|&px| proj.project_h(px)));
            let mut next = 0.5 * spacing; // arc length of the next lamp
            let mut walked = 0.0;
            for i in 1..line.len() {
                let ((a, _), (b, _)) = (proj_pts[i - 1], proj_pts[i]);
                let seg = crate::math::hypot(b[0] - a[0], b[1] - a[1]);
                while next <= walked + seg {
                    let t = if seg > 0.0 { (next - walked) / seg } else { 0.0 };
                    let px = [line[i - 1][0] + (line[i][0] - line[i - 1][0]) * t, line[i - 1][1] + (line[i][1] - line[i - 1][1]) * t];
                    if (rect.x0..rect.x1).contains(&px[0]) && (rect.y0..rect.y1).contains(&px[1]) {
                        let e = a[0] + (b[0] - a[0]) * t;
                        let n = a[1] + (b[1] - a[1]) * t;
                        // The terrain under the lamp, not the chord between the polyline ends.
                        let h = proj.ground_at(px) + s.height_m;
                        pos.push([e as f32, h as f32, -n as f32]);
                        col.push(color);
                    }
                    next += spacing;
                }
                walked += seg;
            }
        }
    }
    (pos, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lights::intensity;

    fn road(class: u8, lit: bool, line: Vec<[f64; 2]>) -> RoadLine {
        RoadLine { class, link: false, minor: false, lit, id: 1, lines: vec![line] }
    }

    fn proj() -> TileProjector {
        let frame = crate::geo::Frame::new(54.6872, 25.2797);
        TileProjector::new(crate::geo::tile_for(54.6872, 25.2797, 16), frame, 4096.0)
    }

    /// Lamps every `spacing_m` from half a spacing in, `height_m` up, in the street color; only inside the tile; lit or
    /// important-enough roads only.
    #[test]
    fn street_lamps_spacing_tile_clip_and_lit_rule() {
        let proj = proj();
        let p = Profile::default();
        let s = &p.lights.street;
        // A straight east-west road across the tile and 200 px beyond both edges.
        let y = 2048.0;
        let line = vec![[-200.0, y], [4296.0, y]];
        let (pos, col) = street_points(&[road(crate::mvt::ROAD_RESIDENTIAL, false, line.clone())], &proj, 0, &p);
        let tile_w = proj.project64([4096.0, y])[0] - proj.project64([0.0, y])[0];
        let expect = (tile_w / s.spacing_m).round() as usize;
        assert!((pos.len() as i64 - expect as i64).abs() <= 1, "{} lamps, expected ~{expect}", pos.len());
        for w in pos.windows(2) {
            assert!(((w[1][0] - w[0][0]) as f64 - s.spacing_m).abs() < 0.5, "spacing {:?}", w);
        }
        let half = 0.5 * tile_w as f32 + 1.0;
        assert!(pos.iter().all(|q| q[0].abs() <= half && (q[1] as f64 - s.height_m).abs() < 1e-3));
        // One lamp type per road: every lamp of this road in the main or the alt color, never mixed.
        assert!(col.iter().all(|c| *c == col[0]));
        assert!(col[0] == [1.0, 0.42, 0.1, intensity(1)] || col[0] == [0.78, 0.86, 1.0, intensity(1)]);
        // Over many roads about alt_share of them get the alt color.
        let alt_roads = (0..1000u64)
            .filter(|&id| {
                let mut r = road(crate::mvt::ROAD_RESIDENTIAL, false, vec![[100.0, y], [400.0, y]]);
                r.id = id;
                street_points(&[r], &proj, 0, &p).1[0][2] == 1.0
            })
            .count();
        assert!((250..350).contains(&alt_roads), "{alt_roads} of 1000 roads alt-colored");
        // Untagged "other" roads are dark, tagged ones lit; untagged_max_class 0 = tagged only.
        assert!(street_points(&[road(crate::mvt::ROAD_OTHER, false, line.clone())], &proj, 0, &p).0.is_empty());
        assert_eq!(street_points(&[road(crate::mvt::ROAD_OTHER, true, line.clone())], &proj, 0, &p).0.len(), pos.len());
        let mut w = crate::profile::WorldProfile::default();
        w.lights.street.untagged_max_class = 0;
        let tagged_only = Profile::new(w).unwrap();
        assert!(street_points(&[road(crate::mvt::ROAD_PRIMARY, false, line)], &proj, 0, &tagged_only).0.is_empty());
    }

    /// `spacing_scale`: R3 lamps (default scale 2) are twice as far apart, half as many, each standing for 2 lamps
    /// (alpha `intensity(2)`, `lamp_count` 2); the lamp count summed over the band is about the R0 one.
    #[test]
    fn far_band_lamps_are_sparser_and_carry_their_count() {
        let proj = proj();
        let p = Profile::default();
        assert_eq!(p.lights.street.spacing_scale, [1.0, 1.0, 1.0, 2.0]);
        let line = vec![[-200.0, 2048.0], [4296.0, 2048.0]];
        let r = [road(crate::mvt::ROAD_PRIMARY, false, line)];
        let (near, near_c) = street_points(&r, &proj, 0, &p);
        let (far, far_c) = street_points(&r, &proj, 3, &p);
        assert_eq!(street_points(&r, &proj, 2, &p), (near.clone(), near_c.clone()), "R2 keeps the base spacing");
        for w in far.windows(2) {
            assert!(((w[1][0] - w[0][0]) as f64 - 2.0 * p.lights.street.spacing_m).abs() < 0.5);
        }
        assert!(far_c.iter().all(|c| c[3] == intensity(2) && lamp_count(c[3]) == 2));
        let lamps: u32 = far_c.iter().map(|c| lamp_count(c[3])).sum();
        assert!((lamps as i64 - near.len() as i64).abs() <= 2, "{lamps} lamps at R3 vs {} at R0", near.len());
        assert!(near_c.iter().all(|c| lamp_count(c[3]) == 1));
        for (a, n) in [(0.125f32, 1u32), (intensity(2), 2), (intensity(9), 9), (intensity(40), 40), (1.0, 64)] {
            assert_eq!(lamp_count(a), n);
        }
    }
}
