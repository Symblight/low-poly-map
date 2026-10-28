//! View points and tile boxes of the cover: the look-ahead view point per band, the terrain
//! box under a tile (memoized, pure per key) and the horizon cull.

use super::Scheduler;
use crate::geo::TileKey;
use crate::rings::{above_horizon, horizon_line_m, look_ahead, ViewPoint, MAX_RINGS};

impl Scheduler {
    /// Terrain [min, max] under a tile: the entry's, else the cache, else `Terrain::tile_range`
    /// (cached). `[0, 0]` with the terrain off.
    pub(super) fn terrain_box_of(&mut self, k: TileKey) -> [f64; 2] {
        if !self.terrain.is_active() {
            return [0.0, 0.0];
        }
        if let Some(e) = self.entries.get(&k) {
            return e.terrain_box;
        }
        if let Some(g) = self.terrain_cache.get(&k) {
            return g;
        }
        let g = match &mut self.proxy {
            // Terrain layer: one probe for the box and the error proxy.
            Some(p) => {
                let pr = p.probe(k);
                p.error_m_probed(k, &pr);
                self.terrain.range_from_probe(pr.lo, pr.hi, pr.step)
            }
            None => self.terrain.tile_range(k),
        };
        self.terrain_cache.insert(k, g);
        g
    }

    /// Look-ahead view point per band (entries past the layer's ring count repeat the last
    /// one). `cam` = (east, north, AGL), `vel` = (east, north, up).
    pub fn views(&self, cam: [f64; 3], vel: [f64; 3]) -> [ViewPoint; MAX_RINGS] {
        self.views_ground(cam, vel).0
    }

    /// [`Scheduler::views`] plus the terrain height under each view point and the camera
    /// itself as a view point (no look-ahead). Each distinct look-ahead point is projected and
    /// sampled once (bands whose clamp does not bind share one point).
    pub(super) fn views_ground(&self, cam: [f64; 3], vel: [f64; 3]) -> ([ViewPoint; MAX_RINGS], [f64; MAX_RINGS], ViewPoint) {
        let l = &self.layer;
        // The camera's MSL altitude (AGL + the terrain under it); the look-ahead moves it by the
        // velocity, so a hill under the camera does not become a hill under every view point.
        let (clat, clon) = self.frame.lat_lon(cam[0], cam[1]);
        let cam_ground = self.terrain.height(clat, clon);
        let cam_msl = [cam[0], cam[1], cam[2] + cam_ground];
        // Terrain scale of the tile boxes (terrain hook; 0 while flat). Never below ground.
        let view_at = |la: [f64; 3]| {
            let (lat, lon, g) = if la[0] == cam_msl[0] && la[1] == cam_msl[1] {
                (clat, clon, cam_ground)
            } else {
                let (lat, lon) = self.frame.lat_lon(la[0], la[1]);
                (lat, lon, self.terrain.height(lat, lon))
            };
            (ViewPoint::new(lat, lon, la[2].max(g)), g)
        };
        let cam_view = view_at(look_ahead(cam_msl, [0.0; 3], l.look_ahead_clamp_m(0), l.look_ahead_s)).0;
        let mut views = [cam_view; MAX_RINGS];
        let mut ground = [0.0; MAX_RINGS];
        let mut prev: Option<([f64; 3], ViewPoint, f64)> = None;
        for i in 0..MAX_RINGS {
            if i >= l.rings() {
                (views[i], ground[i]) = (views[i - 1], ground[i - 1]);
                continue;
            }
            let la = look_ahead(cam_msl, vel, l.look_ahead_clamp_m(i), l.look_ahead_s);
            let (v, g) = match prev {
                Some((p, v, g)) if p == la => (v, g),
                _ => view_at(la),
            };
            prev = Some((la, v, g));
            (views[i], ground[i]) = (v, g);
        }
        (views, ground, cam_view)
    }

    /// Horizon cull of the layer (none unless `horizon_margin_m` is set): whether tile `k` with
    /// terrain box `g` is above the horizon seen from `view` over ground `ground`, with
    /// hysteresis (`current` = it is in the cover now).
    pub(super) fn horizon_ok(&self, view: &ViewPoint, ground: f64, k: TileKey, g: [f64; 2], current: bool) -> bool {
        match self.layer.horizon_margin_m {
            None => true,
            Some(margin) => {
                let line = horizon_line_m(view.alt, ground, g[0], g[1] + self.layer.default_tile_height_m, margin);
                above_horizon(view.horizontal_m(k), line, self.layer.hysteresis, current)
            }
        }
    }
}
