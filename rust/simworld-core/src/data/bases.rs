//! Building bases resolved on first use (perf review item 4).
//!
//! A synthetic overview tile (z11) carries one block per street block (thousands), and each
//! block's base samples the terrain about a hundred times ([`TileProjector::base_height`]):
//! about 100 ms per tile with the terrain on, most of a cold start's overview cost. Only R2
//! chunks mesh blocks (R3 draws light points), i.e. the blocks within the R2 reach, so the tile
//! leaves the bases open and the chunk jobs resolve the ones they mesh, each once
//! (`OnceLock`), in parallel. Same function, same outline, same projector: the same base bits
//! as an eager decode.

use std::borrow::Cow;
use std::sync::OnceLock;

use crate::mesher::ClipRect;
use crate::mvt::{Building, TileProjector};

/// Per-building bases still to compute (empty = every `base_m` is final).
#[derive(Debug, Default)]
pub struct LazyBases(Vec<OnceLock<f32>>);

impl LazyBases {
    /// `n` open bases (one per building of the tile).
    pub fn open(n: usize) -> Self {
        Self((0..n).map(|_| OnceLock::new()).collect())
    }

    /// Buildings `idx` of `buildings` with their final base: borrowed when the bases are final,
    /// else copies whose base is computed by `proj` (the tile's projector) on first use.
    pub fn based<'a>(&self, buildings: &'a [Building], idx: &[usize], proj: &TileProjector) -> Vec<Cow<'a, Building>> {
        idx.iter()
            .map(|&i| {
                let b = &buildings[i];
                match self.0.get(i) {
                    None => Cow::Borrowed(b),
                    Some(cell) => {
                        let base = *cell.get_or_init(|| proj.base_height(&b.polygons, b.centroid_px) as f32);
                        Cow::Owned(Building { base_m: base, ..b.clone() })
                    }
                }
            })
            .collect()
    }
}

/// Indices of the buildings whose bbox meets `r`.
pub(crate) fn select_idx(bboxes: &[[f64; 4]], r: ClipRect) -> Vec<usize> {
    bboxes.iter().enumerate().filter(|(_, bb)| !(bb[2] <= r.x0 || bb[0] >= r.x1 || bb[3] <= r.y0 || bb[1] >= r.y1)).map(|(i, _)| i).collect()
}
