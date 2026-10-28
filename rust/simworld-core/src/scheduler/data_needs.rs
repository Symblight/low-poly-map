//! The data needs pass of the vector layer (Tiles v2): which data tiles the tiles of the cover
//! wait for, use, or merely map to (kept resident, review M3), handed to the
//! [`super::DataStore`].

use std::time::Instant;

use super::landmarks::LmState;
use super::{Scheduler, TileState};

/// Why a tile of the cover needs a data tile this `update` (`update_data_needs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Need {
    /// Waiting for it (fetch / decode it).
    Want,
    /// A job runs on it.
    InUse,
    /// Delivered, split or frozen: keep it resident, fetch nothing.
    Cover,
}

impl Scheduler {
    /// Data needs of this frame: which data tiles the chunks of the cover wait for or use;
    /// cancels the data work nobody needs (see [`super::DataStore::end_needs`]).
    pub(super) fn update_data_needs(&mut self, now: Instant) {
        if self.data.is_none() || self.data_pending() {
            return;
        }
        let mut needs = std::mem::take(&mut self.data_needs);
        needs.clear();
        for e in self.entries.values() {
            for dk in self.data_keys(e.key).into_iter().flatten() {
                match e.state {
                    TileState::Queued | TileState::WaitData if !e.frozen => needs.push((dk, self.order_key(e).0, Need::Want)),
                    TileState::Decoding => needs.push((dk, f64::INFINITY, Need::InUse)),
                    _ => needs.push((dk, f64::INFINITY, Need::Cover)),
                }
            }
        }
        // Landmark tiles: their key is their overview data tile.
        for le in self.landmarks.entries.values() {
            match le.state {
                LmState::Wait => needs.push((le.key, le.cam_d, Need::Want)),
                LmState::Building => needs.push((le.key, f64::INFINITY, Need::InUse)),
                _ => needs.push((le.key, f64::INFINITY, Need::Cover)),
            }
        }
        let cam = self.cam_view;
        let ds = self.data.as_mut().expect("checked");
        ds.begin_needs(now);
        for &(dk, order, need) in &needs {
            match need {
                Need::InUse => ds.use_tile(dk),
                Need::Want => ds.want(dk, order),
                Need::Cover => ds.cover(dk),
            }
        }
        ds.end_needs(now, &mut self.cancelled, |k| cam.map_or(f64::INFINITY, |v| v.slant_box_m(k, 0.0, 0.0)));
        self.data_needs = needs;
    }

}
