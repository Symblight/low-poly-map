//! Minimal Mapbox Vector Tile 2.1 encoder (protobuf by hand): layers with their own extent,
//! deduplicated keys / values, point / line / polygon geometry in integer tile coordinates.
//! Polygons are written with the spec's orientation (exterior positive area by the surveyor's
//! formula in y-down tile space, holes negative), which is what `mvt-reader` (the runtime's
//! decoder) uses to split rings into polygons.

use std::collections::HashMap;

use super::geom::{ring_area, MPoly, Pt, Ring};

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Str(String),
    F64(f64),
    I64(i64),
    Bool(bool),
}

impl Val {
    pub fn from_reader(v: &mvt_reader::feature::Value) -> Option<Val> {
        use mvt_reader::feature::Value as V;
        Some(match v {
            V::String(s) => Val::Str(s.clone()),
            V::Float(f) => Val::F64(*f as f64),
            V::Double(d) => Val::F64(*d),
            V::Int(i) | V::SInt(i) => Val::I64(*i),
            V::UInt(u) => Val::I64(*u as i64),
            V::Bool(b) => Val::Bool(*b),
            V::Null => return None,
        })
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::F64(v) => Some(*v),
            Val::I64(v) => Some(*v as f64),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum ValKey {
    S(String),
    F(u64),
    I(i64),
    B(bool),
}

fn key_of(v: &Val) -> ValKey {
    match v {
        Val::Str(s) => ValKey::S(s.clone()),
        Val::F64(f) => ValKey::F(f.to_bits()),
        Val::I64(i) => ValKey::I(*i),
        Val::Bool(b) => ValKey::B(*b),
    }
}

/// Feature geometry in integer tile coordinates.
pub enum Geom {
    Points(Vec<[i32; 2]>),
    Lines(Vec<Vec<[i32; 2]>>),
    /// Polygons -> rings (first exterior); orientation is fixed on encode.
    Polys(Vec<Vec<Vec<[i32; 2]>>>),
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}
fn tag(out: &mut Vec<u8>, field: u32, wire: u32) {
    varint(out, ((field << 3) | wire) as u64);
}
fn bytes_field(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    tag(out, field, 2);
    varint(out, b.len() as u64);
    out.extend_from_slice(b);
}
fn zz(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

pub struct Layer {
    name: String,
    pub extent: u32,
    keys: Vec<String>,
    kidx: HashMap<String, u32>,
    vals: Vec<Val>,
    vidx: HashMap<ValKey, u32>,
    feats: Vec<u8>,
    pub count: usize,
}

impl Layer {
    pub fn new(name: &str, extent: u32) -> Self {
        Self { name: name.into(), extent, keys: Vec::new(), kidx: HashMap::new(), vals: Vec::new(), vidx: HashMap::new(), feats: Vec::new(), count: 0 }
    }

    /// Adds a feature; returns false (and adds nothing) when the geometry is empty after
    /// dropping degenerate parts.
    pub fn add(&mut self, id: Option<u64>, props: &[(&str, Val)], geom: &Geom) -> bool {
        let (gtype, cmds) = match encode_geom(geom) {
            Some(g) => g,
            None => return false,
        };
        let mut tags = Vec::with_capacity(props.len() * 2);
        for (k, v) in props {
            let ki = match self.kidx.get(*k) {
                Some(i) => *i,
                None => {
                    let i = self.keys.len() as u32;
                    self.keys.push(k.to_string());
                    self.kidx.insert(k.to_string(), i);
                    i
                }
            };
            let vk = key_of(v);
            let vi = match self.vidx.get(&vk) {
                Some(i) => *i,
                None => {
                    let i = self.vals.len() as u32;
                    self.vals.push(v.clone());
                    self.vidx.insert(vk, i);
                    i
                }
            };
            tags.push(ki);
            tags.push(vi);
        }
        let mut f = Vec::new();
        if let Some(id) = id {
            tag(&mut f, 1, 0);
            varint(&mut f, id);
        }
        if !tags.is_empty() {
            let mut packed = Vec::new();
            for t in tags {
                varint(&mut packed, t as u64);
            }
            bytes_field(&mut f, 2, &packed);
        }
        tag(&mut f, 3, 0);
        varint(&mut f, gtype);
        let mut packed = Vec::new();
        for c in cmds {
            varint(&mut packed, c as u64);
        }
        bytes_field(&mut f, 4, &packed);
        bytes_field(&mut self.feats, 2, &f);
        self.count += 1;
        true
    }

    /// The `Layer` message (empty when no feature was added).
    pub fn encode(&self) -> Vec<u8> {
        if self.count == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        tag(&mut out, 15, 0);
        varint(&mut out, 2);
        bytes_field(&mut out, 1, self.name.as_bytes());
        out.extend_from_slice(&self.feats);
        for k in &self.keys {
            bytes_field(&mut out, 3, k.as_bytes());
        }
        for v in &self.vals {
            let mut m = Vec::new();
            match v {
                Val::Str(s) => bytes_field(&mut m, 1, s.as_bytes()),
                Val::F64(d) => {
                    tag(&mut m, 3, 1);
                    m.extend_from_slice(&d.to_le_bytes());
                }
                Val::I64(i) => {
                    if *i >= 0 {
                        tag(&mut m, 5, 0);
                        varint(&mut m, *i as u64);
                    } else {
                        tag(&mut m, 6, 0);
                        varint(&mut m, ((*i << 1) ^ (*i >> 63)) as u64);
                    }
                }
                Val::Bool(b) => {
                    tag(&mut m, 7, 0);
                    varint(&mut m, *b as u64);
                }
            }
            bytes_field(&mut out, 4, &m);
        }
        tag(&mut out, 5, 0);
        varint(&mut out, self.extent as u64);
        out
    }
}

/// Wraps encoded layers into a `Tile` message (empty layers are skipped).
pub fn tile(layers: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in layers.iter().filter(|l| !l.is_empty()) {
        bytes_field(&mut out, 3, l);
    }
    out
}

fn dedup_ring(r: &[[i32; 2]], closed: bool) -> Vec<[i32; 2]> {
    let mut out: Vec<[i32; 2]> = Vec::with_capacity(r.len());
    for p in r {
        if out.last() != Some(p) {
            out.push(*p);
        }
    }
    if closed {
        while out.len() > 1 && out.first() == out.last() {
            out.pop();
        }
    }
    out
}

fn area_i(r: &[[i32; 2]]) -> f64 {
    let pts: Vec<Pt> = r.iter().map(|p| [p[0] as f64, p[1] as f64]).collect();
    ring_area(&pts)
}

fn encode_geom(g: &Geom) -> Option<(u64, Vec<u32>)> {
    let mut cmds = Vec::new();
    let mut cur = [0i32; 2];
    let push_pts = |cmds: &mut Vec<u32>, pts: &[[i32; 2]], cur: &mut [i32; 2]| {
        for p in pts {
            cmds.push(zz(p[0] - cur[0]));
            cmds.push(zz(p[1] - cur[1]));
            *cur = *p;
        }
    };
    match g {
        Geom::Points(ps) => {
            if ps.is_empty() {
                return None;
            }
            cmds.push(1 | ((ps.len() as u32) << 3));
            push_pts(&mut cmds, ps, &mut cur);
            Some((1, cmds))
        }
        Geom::Lines(ls) => {
            for l in ls {
                let l = dedup_ring(l, false);
                if l.len() < 2 {
                    continue;
                }
                cmds.push(1 | (1 << 3));
                push_pts(&mut cmds, &l[..1], &mut cur);
                cmds.push(2 | (((l.len() - 1) as u32) << 3));
                push_pts(&mut cmds, &l[1..], &mut cur);
            }
            (!cmds.is_empty()).then_some((2, cmds))
        }
        Geom::Polys(ps) => {
            for p in ps {
                for (k, r) in p.iter().enumerate() {
                    let mut r = dedup_ring(r, true);
                    if r.len() < 3 {
                        if k == 0 {
                            break; // exterior collapsed: drop the polygon with its holes
                        }
                        continue;
                    }
                    let a = area_i(&r);
                    if a == 0.0 {
                        if k == 0 {
                            break;
                        }
                        continue;
                    }
                    if (k == 0) != (a > 0.0) {
                        r.reverse();
                    }
                    cmds.push(1 | (1 << 3));
                    push_pts(&mut cmds, &r[..1], &mut cur);
                    cmds.push(2 | (((r.len() - 1) as u32) << 3));
                    push_pts(&mut cmds, &r[1..], &mut cur);
                    cmds.push(7 | (1 << 3));
                }
            }
            (!cmds.is_empty()).then_some((3, cmds))
        }
    }
}

/// Global-unit geometry -> tile integers: `(g - origin) / g_per_unit`, rounded.
pub struct Quant {
    pub ox: f64,
    pub oy: f64,
    pub g_per_unit: f64,
}

impl Quant {
    pub fn pt(&self, p: Pt) -> [i32; 2] {
        [((p[0] - self.ox) / self.g_per_unit).round() as i32, ((p[1] - self.oy) / self.g_per_unit).round() as i32]
    }
    pub fn ring(&self, r: &[Pt]) -> Vec<[i32; 2]> {
        r.iter().map(|p| self.pt(*p)).collect()
    }
    pub fn mpoly(&self, m: &MPoly) -> Vec<Vec<Vec<[i32; 2]>>> {
        m.iter().map(|p| p.iter().map(|r| self.ring(r)).collect()).collect()
    }
    pub fn lines(&self, l: &[Ring]) -> Vec<Vec<[i32; 2]>> {
        l.iter().map(|r| self.ring(r)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mvt_reader::feature::Value;

    #[test]
    fn round_trips_through_mvt_reader_with_per_layer_extents() {
        let mut a = Layer::new("buildings", 8192);
        // Exterior given counter-clockwise on screen (negative area): the encoder flips it.
        let ext = vec![[0, 0], [0, 100], [100, 100], [100, 0]];
        let hole = vec![[10, 10], [20, 10], [20, 20], [10, 20]];
        assert!(a.add(Some(7), &[("id", Val::F64(123.0)), ("kind", Val::Str("yes".into())), ("ruins", Val::Bool(true))], &Geom::Polys(vec![vec![ext, hole], vec![vec![[200, 200], [300, 200], [300, 300], [200, 300]]]])));
        assert!(!a.add(None, &[], &Geom::Polys(vec![vec![vec![[0, 0], [0, 0], [1, 1]]]])));
        let mut b = Layer::new("lights", 16384);
        b.add(None, &[("height", Val::I64(-5))], &Geom::Points(vec![[5, 6]]));
        let mut c = Layer::new("roads", 4096);
        c.add(None, &[("kind", Val::Str("primary".into()))], &Geom::Lines(vec![vec![[0, 0], [10, 0]], vec![[20, 20], [20, 30], [20, 30]]]));
        let bytes = tile(&[a.encode(), b.encode(), Layer::new("empty", 4096).encode(), c.encode()]);
        let r = mvt_reader::Reader::new(bytes).unwrap();
        let layers = r.get_layer_metadata().unwrap();
        assert_eq!(layers.iter().map(|l| (l.name.as_str(), l.extent)).collect::<Vec<_>>(), vec![("buildings", 8192), ("lights", 16384), ("roads", 4096)]);
        let f = r.get_features(0).unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].id, Some(7));
        let p = f[0].properties.as_ref().unwrap();
        assert_eq!(p.get("id"), Some(&Value::Double(123.0)));
        assert_eq!(p.get("ruins"), Some(&Value::Bool(true)));
        match &f[0].geometry {
            geo::Geometry::MultiPolygon(m) => {
                assert_eq!(m.0.len(), 2);
                assert_eq!(m.0[0].interiors().len(), 1);
            }
            g => panic!("{g:?}"),
        }
        let l = r.get_features(1).unwrap();
        assert_eq!(l[0].properties.as_ref().unwrap().get("height"), Some(&Value::SInt(-5)));
        match &r.get_features(2).unwrap()[0].geometry {
            geo::Geometry::MultiLineString(m) => assert_eq!(m.0.len(), 2),
            g => panic!("{g:?}"),
        }
    }
}
