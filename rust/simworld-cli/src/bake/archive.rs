//! PMTiles v3 on local files: a small synchronous reader (header, root + leaf directories, gzip)
//! and the `pmtiles` crate's writer. The runtime reads archives over HTTP with the same crate.

use std::collections::BTreeMap;
use std::io::Read;

use pmtiles::{Compression, PmTilesWriter, TileCoord, TileId, TileType};

pub type Key = (u8, u32, u32);

pub struct Archive {
    bytes: Vec<u8>,
    data_offset: u64,
    tile_gzip: bool,
    /// (z, x, y) -> (offset in the data section, length).
    tiles: BTreeMap<Key, (u64, u32)>,
    pub metadata: serde_json::Value,
    pub bounds: [f64; 4],
    pub min_zoom: u8,
    pub max_zoom: u8,
}

fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn i32_at(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub fn gunzip(b: &[u8]) -> Result<Vec<u8>, String> {
    if b.len() >= 2 && b[0] == 0x1f && b[1] == 0x8b {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(b).read_to_end(&mut out).map_err(|e| format!("gzip: {e}"))?;
        Ok(out)
    } else {
        Ok(b.to_vec())
    }
}

pub fn gzip(b: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(b).expect("in-memory gzip");
    e.finish().expect("in-memory gzip")
}

fn read_varint(b: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let byte = *b.get(*pos).ok_or("directory: truncated varint")?;
        *pos += 1;
        v |= ((byte & 0x7f) as u64) << shift;
        if byte < 0x80 {
            return Ok(v);
        }
        shift += 7;
        if shift > 63 {
            return Err("directory: bad varint".into());
        }
    }
}

/// (tile_id, offset, length, run_length); offsets resolved.
fn parse_dir(b: &[u8]) -> Result<Vec<(u64, u64, u32, u32)>, String> {
    let mut pos = 0;
    let n = read_varint(b, &mut pos)? as usize;
    let mut e = vec![(0u64, 0u64, 0u32, 0u32); n];
    let mut id = 0;
    for x in e.iter_mut() {
        id += read_varint(b, &mut pos)?;
        x.0 = id;
    }
    for x in e.iter_mut() {
        x.3 = read_varint(b, &mut pos)? as u32;
    }
    for x in e.iter_mut() {
        x.2 = read_varint(b, &mut pos)? as u32;
    }
    for i in 0..n {
        let o = read_varint(b, &mut pos)?;
        e[i].1 = if o == 0 {
            if i == 0 {
                return Err("directory: first offset is 0".into());
            }
            e[i - 1].1 + e[i - 1].2 as u64
        } else {
            o - 1
        };
    }
    Ok(e)
}

impl Archive {
    pub fn open(path: &str) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
        Self::from_bytes(bytes).map_err(|e| format!("{path}: {e}"))
    }

    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        if bytes.len() < 127 || &bytes[0..7] != b"PMTiles" || bytes[7] != 3 {
            return Err("not a PMTiles v3 archive".into());
        }
        let b = &bytes;
        let (root_off, root_len) = (u64_at(b, 8), u64_at(b, 16));
        let (meta_off, meta_len) = (u64_at(b, 24), u64_at(b, 32));
        let leaf_off = u64_at(b, 40);
        let data_offset = u64_at(b, 56);
        let internal_gzip = b[97] == 2;
        if !(b[97] == 1 || internal_gzip) {
            return Err(format!("internal compression {} not supported", b[97]));
        }
        let tile_gzip = b[98] == 2;
        let section = |o: u64, l: u64| -> Result<Vec<u8>, String> {
            let s = b.get(o as usize..(o + l) as usize).ok_or("section out of range")?;
            if internal_gzip {
                gunzip(s)
            } else {
                Ok(s.to_vec())
            }
        };
        let metadata = if meta_len == 0 {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&section(meta_off, meta_len)?).map_err(|e| format!("metadata: {e}"))?
        };
        let mut tiles = BTreeMap::new();
        let mut stack = vec![(root_off, root_len)];
        while let Some((o, l)) = stack.pop() {
            for (id, off, len, run) in parse_dir(&section(o, l)?)? {
                if run == 0 {
                    stack.push((leaf_off + off, len as u64));
                    continue;
                }
                for k in 0..run as u64 {
                    let c: TileCoord = TileId::new(id + k).map_err(|e| format!("tile id: {e}"))?.into();
                    tiles.insert((c.z(), c.x(), c.y()), (off, len));
                }
            }
        }
        let e7 = |o| i32_at(b, o) as f64 / 1e7;
        Ok(Self {
            data_offset,
            tile_gzip,
            tiles,
            metadata,
            bounds: [e7(102), e7(106), e7(110), e7(114)],
            min_zoom: b[100],
            max_zoom: b[101],
            bytes,
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = Key> + '_ {
        self.tiles.keys().copied()
    }
    pub fn keys_at(&self, z: u8) -> Vec<Key> {
        self.tiles.keys().filter(|k| k.0 == z).copied().collect()
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.tiles.len()
    }
    pub fn file_len(&self) -> usize {
        self.bytes.len()
    }
    /// Stored (compressed) bytes of a tile.
    pub fn raw(&self, k: Key) -> Option<&[u8]> {
        let (o, l) = *self.tiles.get(&k)?;
        let s = (self.data_offset + o) as usize;
        self.bytes.get(s..s + l as usize)
    }
    /// Decompressed MVT bytes of a tile.
    pub fn tile(&self, k: Key) -> Result<Option<Vec<u8>>, String> {
        match self.raw(k) {
            None => Ok(None),
            Some(r) if self.tile_gzip => gunzip(r).map(Some),
            Some(r) => Ok(Some(r.to_vec())),
        }
    }
}

/// Writes gzip-compressed tiles (already compressed by the caller) in tile-id order.
pub fn write(
    path: &str,
    tiles: &BTreeMap<Key, Vec<u8>>,
    metadata: &serde_json::Value,
    bounds: [f64; 4],
    center_zoom: u8,
) -> Result<(), String> {
    let mut ordered: Vec<(u64, &Key, &Vec<u8>)> = tiles
        .iter()
        .map(|(k, v)| Ok((TileId::from(TileCoord::new(k.0, k.1, k.2).map_err(|e| format!("{k:?}: {e}"))?).value(), k, v)))
        .collect::<Result<_, String>>()?;
    ordered.sort_by_key(|t| t.0);
    let (zmin, zmax) = tiles.keys().fold((u8::MAX, 0u8), |(a, b), k| (a.min(k.0), b.max(k.0)));
    let tmp = format!("{path}.tmp");
    let file = std::fs::File::create(&tmp).map_err(|e| format!("{tmp}: {e}"))?;
    let mut w = PmTilesWriter::new(TileType::Mvt)
        .precompressed(Compression::Gzip)
        .internal_compression(Compression::Gzip)
        .min_zoom(zmin.min(zmax))
        .max_zoom(zmax)
        .bounds(bounds[0], bounds[1], bounds[2], bounds[3])
        .center((bounds[0] + bounds[2]) / 2.0, (bounds[1] + bounds[3]) / 2.0)
        .center_zoom(center_zoom)
        .metadata(metadata.to_string())
        .create(std::io::BufWriter::new(file))
        .map_err(|e| format!("pmtiles writer: {e}"))?;
    for (_, k, v) in ordered {
        w.add_tile(TileCoord::new(k.0, k.1, k.2).map_err(|e| e.to_string())?, v).map_err(|e| format!("pmtiles add {k:?}: {e}"))?;
    }
    w.finalize().map_err(|e| format!("pmtiles finalize: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{path}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_back_many_tiles_with_metadata() {
        let dir = std::env::temp_dir().join(format!("simworld-bake-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.pmtiles");
        let mut tiles = BTreeMap::new();
        // Enough entries to need leaf directories in some writers; identical tiles too.
        for x in 0..64u32 {
            for y in 0..64u32 {
                tiles.insert((14u8, 9000 + x, 5000 + y), gzip(format!("tile {}", (x * y) % 7).as_bytes()));
            }
        }
        tiles.insert((11, 1170, 650), gzip(b"overview"));
        let meta = serde_json::json!({"simworld_levels": {"detail": 14, "overview": 11}});
        write(path.to_str().unwrap(), &tiles, &meta, [24.0, 54.0, 26.0, 55.0], 12).unwrap();
        let a = Archive::open(path.to_str().unwrap()).unwrap();
        assert_eq!(a.len(), tiles.len());
        assert_eq!(a.metadata, meta);
        assert_eq!((a.min_zoom, a.max_zoom), (11, 14));
        assert!((a.bounds[0] - 24.0).abs() < 1e-6 && (a.bounds[3] - 55.0).abs() < 1e-6);
        for (k, v) in &tiles {
            assert_eq!(a.raw(*k).unwrap(), v.as_slice(), "{k:?}");
        }
        assert_eq!(a.tile((11, 1170, 650)).unwrap().unwrap(), b"overview");
        std::fs::remove_dir_all(&dir).ok();
    }
}
