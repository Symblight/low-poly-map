//! Minimal MVT (protobuf) encoder for fixtures.

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
fn zz(v: i32) -> u64 {
    ((v << 1) ^ (v >> 31)) as u32 as u64
}

pub enum Val {
    S(&'static str),
    D(f64),
    B(bool),
}

pub struct Feat {
    pub id: u64,
    /// polygons -> rings (exterior clockwise in screen space, holes counter-clockwise).
    pub rings: Vec<Vec<(i32, i32)>>,
    pub props: Vec<(&'static str, Val)>,
}

/// Encode one polygon layer named `name` with `extent` 4096.
pub fn encode_layer(name: &str, feats: &[Feat]) -> Vec<u8> {
    encode_layer_typed(name, feats, false)
}

/// Encode one layer; `lines` = LINESTRING features (each "ring" is one open line).
pub fn encode_layer_typed(name: &str, feats: &[Feat], lines: bool) -> Vec<u8> {
    encode_layer_ext(name, feats, lines, 4096)
}

/// [`encode_layer_typed`] with the layer's `extent`.
pub fn encode_layer_ext(name: &str, feats: &[Feat], lines: bool, extent: u32) -> Vec<u8> {
    let mut keys: Vec<&str> = Vec::new();
    let mut vals: Vec<Vec<u8>> = Vec::new();
    let mut layer = Vec::new();
    tag(&mut layer, 15, 0);
    varint(&mut layer, 2);
    bytes_field(&mut layer, 1, name.as_bytes());
    for f in feats {
        let mut fb = Vec::new();
        tag(&mut fb, 1, 0);
        varint(&mut fb, f.id);
        let mut tags = Vec::new();
        for (k, v) in &f.props {
            let ki = keys.iter().position(|x| x == k).unwrap_or_else(|| {
                keys.push(k);
                keys.len() - 1
            });
            let mut vb = Vec::new();
            match v {
                Val::S(s) => bytes_field(&mut vb, 1, s.as_bytes()),
                Val::D(d) => {
                    tag(&mut vb, 3, 1);
                    vb.extend_from_slice(&d.to_le_bytes());
                }
                Val::B(b) => {
                    tag(&mut vb, 7, 0);
                    varint(&mut vb, *b as u64);
                }
            }
            vals.push(vb);
            varint(&mut tags, ki as u64);
            varint(&mut tags, (vals.len() - 1) as u64);
        }
        bytes_field(&mut fb, 2, &tags);
        tag(&mut fb, 3, 0);
        // One ring of one point = a POINT feature (the overview `lights` layer).
        let point = f.rings.len() == 1 && f.rings[0].len() == 1;
        varint(&mut fb, if point { 1 } else if lines { 2 } else { 3 }); // POINT / LINESTRING / POLYGON
        let mut geom = Vec::new();
        let (mut cx, mut cy) = (0, 0);
        if point {
            let q = f.rings[0][0];
            varint(&mut geom, (1 << 3) | 1);
            varint(&mut geom, zz(q.0));
            varint(&mut geom, zz(q.1));
        }
        for ring in f.rings.iter().filter(|_| !point) {
            varint(&mut geom, (1 << 3) | 1);
            varint(&mut geom, zz(ring[0].0 - cx));
            varint(&mut geom, zz(ring[0].1 - cy));
            (cx, cy) = ring[0];
            varint(&mut geom, (((ring.len() - 1) as u64) << 3) | 2);
            for p in &ring[1..] {
                varint(&mut geom, zz(p.0 - cx));
                varint(&mut geom, zz(p.1 - cy));
                (cx, cy) = *p;
            }
            if !lines {
                varint(&mut geom, (1 << 3) | 7);
            }
        }
        bytes_field(&mut fb, 4, &geom);
        bytes_field(&mut layer, 2, &fb);
    }
    for k in &keys {
        bytes_field(&mut layer, 3, k.as_bytes());
    }
    for v in &vals {
        bytes_field(&mut layer, 4, v);
    }
    tag(&mut layer, 5, 0);
    varint(&mut layer, extent as u64);
    let mut tile = Vec::new();
    bytes_field(&mut tile, 3, &layer);
    tile
}
