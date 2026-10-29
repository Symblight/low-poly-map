//! Tile bytes from a PMTiles v3 archive over HTTP(S) range requests, behind a disk LRU cache.
//!
//! * Network: the `pmtiles` crate (`AsyncPmTilesReader<HttpBackend, HashMapCache>`): header and
//!   root directory are read once when the reader opens, leaf directories are cached in memory,
//!   then each tile is one range request. reqwest uses rustls only; plain http:// also works.
//! * Disk: raw tile bytes exactly as stored in the archive (gzip MVT for planetiler) in one
//!   SQLite database per source, `{cache_dir}/tiles-{source-hash}.sqlite` (rows keyed by packed
//!   `z/x/y`; a zero-length blob records "no tile here"). Owned by one cache thread, writes
//!   batched, LRU by access time with a byte cap (world profile `cache.cap_mb`, default
//!   300 MiB), survives restarts (docs/spec-tile-cache.md). The cache is consulted before the
//!   archive is even opened, so a warm cache works fully offline.
//! * Staleness: the database's `meta` table holds the archive fingerprint (hash of the archive
//!   header) and the cache schema version. Once the archive has been opened, a different
//!   fingerprint wipes the cached tiles, so rebuilding the archive at the same URL never serves
//!   old tiles. While the archive is unreachable the cache is served as is (offline use) and
//!   validated as soon as it can be opened.
//!
//! * Tileset contract (audit A18, [`check_tileset`]): when the archive opens it must be either a
//!   two-level archive (metadata `simworld_levels` = `{"detail": 14, "overview": z}`, exactly
//!   those two zooms, layers `buildings` / `landuse` / `roads` + `blocks` / `lights`, optional
//!   `landmarks`; Tiles and LOD v2) or a compatibility archive (no `simworld_levels`, zooms
//!   covering 13..16, layers `buildings`, `landuse`, `roads`); otherwise every fetch fails with
//!   that error (reported once through [`Note::Error`]). The accepted levels
//!   ([`crate::data::DataLevels`]) are recorded in the disk cache's `meta` table, so a warm
//!   cache knows them offline ([`Fetcher::cached_levels`]).
//!
//! Everything here runs on tokio threads and the cache thread and never touches Godot.

use std::time::Duration;

use crate::data::DataLevels;
use crate::mvt::{BLOCKS_LAYER, BUILDINGS_LAYER, LANDMARKS_LAYER, LANDUSE_LAYER, LIGHTS_LAYER, ROADS_LAYER};
use crate::profile::Profile;
use crate::rings::{LEAF_ZOOM, ROOT_ZOOM};

mod cache_thread;
mod disk;
mod remote;
mod sqlite;
#[cfg(test)]
mod tests;

pub use disk::{cache_file_name, pack_key, remove_legacy_folders, source_hash, DiskCache, Notify, MSG_CACHE_RECREATED};
pub use remote::{Fetcher, Source};

/// Fetch / cache tuning (world profile `cache.*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FetchParams {
    /// Disk cache LRU cap (bytes).
    pub cache_cap_bytes: u64,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// Minimum wait before re-opening an archive that failed to open.
    pub open_retry: Duration,
    /// While the archive cannot be opened, try validating the cache this often.
    pub validate_retry: Duration,
}

impl FetchParams {
    pub fn from_profile(p: &Profile) -> Self {
        let c = &p.cache;
        Self {
            cache_cap_bytes: (c.cap_mb * 1024.0 * 1024.0).round() as u64,
            connect_timeout: Duration::from_secs_f64(c.connect_timeout_s),
            request_timeout: Duration::from_secs_f64(c.request_timeout_s),
            open_retry: Duration::from_secs_f64(c.open_retry_s),
            validate_retry: Duration::from_secs_f64(c.validate_retry_s),
        }
    }
}

impl Default for FetchParams {
    fn default() -> Self {
        Self::from_profile(&Profile::default())
    }
}

/// Messages from the fetcher to the main thread (logged there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Info(String),
    /// Something the host should notice but that does not stop streaming (e.g. the disk cache
    /// is held by another process; running without it).
    Warn(String),
    /// The disk cache was disabled for the session (repeated write failures): status degraded.
    CacheDisabled(String),
    /// Configuration problem that will not go away by retrying (e.g. a tileset mismatch).
    Error(String),
}

/// Why a fetch failed (for the host status, contract `status()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// The archive could not be opened (server unreachable, HTTP error on the header, ...).
    Unreachable,
    /// The archive opened but does not match the tileset contract (permanent).
    Contract,
    /// One tile request failed (or anything else).
    Tile,
    /// Aborted because the scheduler no longer wants the tile (`Worker::cancel`); not a failure.
    Cancelled,
}

/// A failed fetch: what kind of failure and the message that is logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
}

impl FetchError {
    pub fn new(kind: FetchErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Plain messages are tile-level failures.
impl From<String> for FetchError {
    fn from(message: String) -> Self {
        Self::new(FetchErrorKind::Tile, message)
    }
}

impl From<&str> for FetchError {
    fn from(message: &str) -> Self {
        Self::new(FetchErrorKind::Tile, message)
    }
}

/// The detail zoom a two-level archive must have (R0 z16 / R1 z15 are cut from it, R2 z14 from
/// the overview level).
pub const DETAIL_ZOOM: u8 = 14;
/// Overview zooms a two-level archive may use (R2 z14 / R3 z13 are cut from it).
pub const OVERVIEW_ZOOMS: std::ops::RangeInclusive<u8> = 8..=13;

/// Check an opened archive against the tileset contract and return its data levels (`metadata`
/// = the archive's JSON metadata): a two-level archive (`simworld_levels`, exactly its two
/// zooms, the detail and overview layers; `landmarks` optional) or a compatibility archive
/// (zooms 13..16, layers `buildings`, `landuse`, `roads`).
pub fn check_tileset(min_zoom: u8, max_zoom: u8, metadata: &str) -> Result<DataLevels, String> {
    let meta: serde_json::Value = serde_json::from_str(metadata).map_err(|e| format!("tileset metadata is not JSON: {e}"))?;
    let Some(layers) = meta.get("vector_layers").and_then(|v| v.as_array()) else {
        return Err("tileset metadata has no `vector_layers`: cannot verify the required layers (buildings, landuse, roads)".into());
    };
    let ids: Vec<&str> = layers.iter().filter_map(|l| l.get("id").and_then(|i| i.as_str())).collect();
    let need = |want: &[&'static str]| -> Result<(), String> {
        let missing: Vec<&str> = want.iter().copied().filter(|l| !ids.contains(l)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "tileset lacks required layer(s) {} (has: {}); build it with the planetiler profile in data/ or `simworld-cli bake`",
            missing.join(", "),
            ids.join(", ")
        ))
    };
    if let Some(l) = meta.get("simworld_levels") {
        let zoom = |k: &str| l.get(k).and_then(|v| v.as_u64()).filter(|z| *z <= 30).map(|z| z as u8);
        let (Some(detail), Some(overview)) = (zoom("detail"), zoom("overview")) else {
            return Err(format!("tileset metadata simworld_levels must be {{\"detail\": <zoom>, \"overview\": <zoom>}}, got {l}"));
        };
        if detail != DETAIL_ZOOM || !OVERVIEW_ZOOMS.contains(&overview) {
            return Err(format!(
                "tileset simworld_levels detail z{detail} / overview z{overview} not supported (the library needs detail z{DETAIL_ZOOM} and overview z{}..z{})",
                OVERVIEW_ZOOMS.start(),
                OVERVIEW_ZOOMS.end()
            ));
        }
        if (min_zoom, max_zoom) != (overview, detail) {
            return Err(format!(
                "tileset zoom range z{min_zoom}-{max_zoom} does not match its simworld_levels (exactly z{overview} and z{detail}); rebuild it with `simworld-cli bake`"
            ));
        }
        need(&[BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER, BLOCKS_LAYER, LIGHTS_LAYER])?;
        return Ok(DataLevels::two_level(detail, overview, ids.contains(&LANDMARKS_LAYER)));
    }
    if min_zoom > ROOT_ZOOM || max_zoom < LEAF_ZOOM {
        return Err(format!(
            "tileset zoom range z{min_zoom}-{max_zoom} does not cover z{ROOT_ZOOM}-{LEAF_ZOOM} (the library's bands need every zoom from {ROOT_ZOOM} to {LEAF_ZOOM}, or a two-level archive with simworld_levels; rebuild the archive with data/build_tiles.sh or `simworld-cli bake`)"
        ));
    }
    need(&[BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER])?;
    Ok(DataLevels::COMPAT)
}

/// `DataLevels` as stored in the disk cache's `meta` table (`levels`).
fn levels_text(l: DataLevels) -> String {
    format!("{},{},{},{}", l.detail, l.overview, l.two_level as u8, l.landmarks as u8)
}

fn parse_levels(s: &str) -> Option<DataLevels> {
    let v: Vec<u8> = s.split(',').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    match v[..] {
        [detail, overview, two, lm] => Some(DataLevels { detail, overview, two_level: two == 1, landmarks: lm == 1 }),
        _ => None,
    }
}
/// Bump when the tile schema or the decoder's expectations change without a new archive URL.
/// v2 (M5 Ground): `landuse` gained leisure/natural/riverbank kinds and `roads` is decoded.
/// v3: one SQLite file per source (`tiles-{hash}.sqlite`, version in `meta.schema_version`);
/// the file-per-tile folders `pmtiles-v{N}-{hash}/` of the same source are deleted on open.
pub const CACHE_SCHEMA_VERSION: u32 = 3;
