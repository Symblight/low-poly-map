//! geo interop (local frame: subtract an origin so i_overlay keeps full precision).

use geo::{Coord, LineString, MultiPolygon, Polygon};

use super::{MPoly, Poly, Pt, Ring};

pub fn to_geo(m: &MPoly, o: Pt, scale: f64) -> MultiPolygon<f64> {
    let ring = |r: &Ring| {
        let mut cs: Vec<Coord<f64>> = r.iter().map(|p| Coord { x: (p[0] - o[0]) * scale, y: (p[1] - o[1]) * scale }).collect();
        if let Some(f) = cs.first().copied() {
            cs.push(f);
        }
        LineString::new(cs)
    };
    MultiPolygon::new(
        m.iter()
            .filter(|p| !p.is_empty())
            .map(|p| Polygon::new(ring(&p[0]), p[1..].iter().map(ring).collect()))
            .collect(),
    )
}

pub fn from_geo(m: &MultiPolygon<f64>, o: Pt, scale: f64) -> MPoly {
    let ring = |l: &LineString<f64>| {
        let mut r: Ring = l.0.iter().map(|c| [c.x / scale + o[0], c.y / scale + o[1]]).collect();
        if r.len() > 1 && r.first() == r.last() {
            r.pop();
        }
        r
    };
    m.0.iter()
        .map(|p| {
            let mut q = vec![ring(p.exterior())];
            q.extend(p.interiors().iter().map(ring));
            q
        })
        .filter(|q: &Poly| q[0].len() >= 3)
        .collect()
}

/// Union of polygon sets (any orientation in; the output orientation is fixed by the encoder).
pub fn union(parts: &[MPoly]) -> MPoly {
    let Some(o) = parts.iter().flatten().flatten().flatten().next().copied() else { return Vec::new() };
    let gs: Vec<Polygon<f64>> = parts
        .iter()
        .flat_map(|m| to_geo(m, o, 1.0).0)
        .map(|p| {
            use geo::orient::{Direction, Orient};
            p.orient(Direction::Default)
        })
        .collect();
    from_geo(&geo::unary_union(&gs), o, 1.0)
}
