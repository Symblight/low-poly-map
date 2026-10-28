//! Godot's side of the contract, simulated from keys: which tiles are uploaded, which are
//! present ("uploaded or all 4 children present"), which are visible ("uploaded and (z13 or
//! all 4 siblings present) and not all 4 children present", or uploaded with a fade entry),
//! and whether the visible tiles form an exact cover (overlaps only between the two sides of
//! a fade, with complementary values). One `GodotSim` per layer: the host evaluates the rule
//! per layer with the layer's root zoom in place of z13 (api 1.5).

use ahash::{AHashMap, AHashSet};

use crate::geo::TileKey;
use crate::rings::{children, LEAF_ZOOM, ROOT_ZOOM};

#[derive(Debug, Clone)]
pub struct GodotSim {
    pub uploaded: AHashSet<TileKey>,
    /// The last `poll_fades` result (replaced as a whole each frame).
    pub fades: AHashMap<TileKey, f32>,
    /// Zoom range of the layer (vector world: 13..16).
    pub root_zoom: u8,
    pub leaf_zoom: u8,
}

impl Default for GodotSim {
    fn default() -> Self {
        Self::for_zooms(ROOT_ZOOM, LEAF_ZOOM)
    }
}

impl GodotSim {
    /// The host side of a layer with roots at `root_zoom` and leaves at `leaf_zoom`.
    pub fn for_zooms(root_zoom: u8, leaf_zoom: u8) -> Self {
        Self { uploaded: AHashSet::new(), fades: AHashMap::new(), root_zoom, leaf_zoom }
    }
    fn parent(&self, k: TileKey) -> Option<TileKey> {
        (k.z > self.root_zoom).then(|| TileKey::new(k.z - 1, k.x / 2, k.y / 2))
    }
    fn siblings(&self, k: TileKey) -> [TileKey; 4] {
        match self.parent(k) {
            Some(p) => children(p),
            None => [k; 4],
        }
    }
    pub fn upload(&mut self, k: TileKey) {
        self.uploaded.insert(k);
    }
    pub fn unload(&mut self, k: TileKey) {
        self.uploaded.remove(&k);
        self.fades.remove(&k);
    }
    pub fn set_fades(&mut self, f: &[(TileKey, f32)]) {
        self.fades.clear();
        self.fades.extend(f.iter().copied());
    }
    pub fn present(&self, k: TileKey) -> bool {
        self.uploaded.contains(&k) || (k.z < self.leaf_zoom && children(k).iter().all(|&c| self.present(c)))
    }
    fn all_present(&self, ks: [TileKey; 4]) -> bool {
        ks.iter().all(|&k| self.present(k))
    }
    pub fn visible(&self, k: TileKey) -> bool {
        self.uploaded.contains(&k)
            && (self.fades.contains_key(&k)
                || ((k.z == self.root_zoom || self.all_present(self.siblings(k)))
                    && !(k.z < self.leaf_zoom && self.all_present(children(k)))))
    }
    /// Both sides of one fade: `p` is `k`'s parent and the two values sum to 1.
    fn complementary(&self, p: TileKey, k: TileKey) -> bool {
        self.parent(k) == Some(p)
            && match (self.fades.get(&p), self.fades.get(&k)) {
                (Some(a), Some(b)) => (a + b - 1.0).abs() < 1e-6,
                _ => false,
            }
    }
    pub fn visible_tiles(&self) -> Vec<TileKey> {
        let mut v: Vec<TileKey> = self.uploaded.iter().copied().filter(|&k| self.visible(k)).collect();
        v.sort();
        v
    }
    fn covered(&self, k: TileKey) -> bool {
        self.visible(k)
            || (k.z < self.leaf_zoom && self.all_present(children(k)) && children(k).iter().all(|&c| self.covered(c)))
    }
    fn root_of(&self, mut k: TileKey) -> TileKey {
        while let Some(p) = self.parent(k) {
            k = p;
        }
        k
    }
    /// Overlaps (a visible tile with a visible ancestor), holes (a root holding uploaded
    /// tiles whose area is not completely covered) and orphans (uploaded tiles under a
    /// root that is not present). Empty = exact cover of the present roots.
    pub fn check(&self) -> Vec<String> {
        let mut errs = Vec::new();
        let vis: AHashSet<TileKey> = self.uploaded.iter().copied().filter(|&k| self.visible(k)).collect();
        for &k in &vis {
            let mut a = self.parent(k);
            while let Some(p) = a {
                if vis.contains(&p) && !self.complementary(p, k) {
                    errs.push(format!("overlap: {k} and ancestor {p} both visible"));
                }
                a = self.parent(p);
            }
        }
        // Fade entries: only for uploaded tiles; a partial value needs its other side.
        for (&k, &f) in &self.fades {
            if !self.uploaded.contains(&k) {
                errs.push(format!("fade entry for {k} which is not uploaded"));
            }
            if !(0.0..=1.0).contains(&f) {
                errs.push(format!("fade {k} = {f} out of range"));
            }
            if f < 1.0 {
                let up = self.parent(k).is_some_and(|p| self.complementary(p, k));
                let down = k.z < self.leaf_zoom && children(k).iter().all(|&c| self.complementary(k, c));
                if !up && !down {
                    errs.push(format!("fade {k} = {f} without a complementary other side"));
                }
            }
        }
        let mut roots: AHashMap<TileKey, TileKey> = AHashMap::new();
        for &k in &self.uploaded {
            roots.entry(self.root_of(k)).or_insert(k);
        }
        for (r, example) in roots {
            if !self.present(r) {
                errs.push(format!("orphan: {example} uploaded under non-present root {r}"));
            } else if !self.covered(r) {
                errs.push(format!("hole under root {r}"));
            }
        }
        errs
    }
    /// The visible tiles containing a point (Mercator world units).
    pub fn tile_at(&self, wx: f64, wy: f64) -> Vec<TileKey> {
        let mut out = Vec::new();
        for z in self.root_zoom..=self.leaf_zoom {
            let n = (1u64 << z) as f64;
            let k = TileKey::new(z, (wx * n).floor() as u32, (wy * n).floor() as u32);
            if self.visible(k) {
                out.push(k);
            }
        }
        out
    }
}
