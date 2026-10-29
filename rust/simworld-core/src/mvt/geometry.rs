//! Ring geometry in tile pixels: areas, centroids, MVT geometry -> pixel rings.

use super::PxPolygons;

/// Signed shoelace area (pixel units; positive = counter-clockwise in a y-up system).
pub fn ring_area2(ring: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let p = ring[i];
        let q = ring[(i + 1) % ring.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a
}

/// Area-weighted centroid of all rings (signed, so holes subtract). Falls back to the vertex
/// mean for degenerate geometry.
pub fn centroid_px(polys: &[Vec<Vec<[f64; 2]>>]) -> Option<[f64; 2]> {
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    let (mut n, mut sx, mut sy) = (0usize, 0.0, 0.0);
    for poly in polys {
        for ring in poly {
            for i in 0..ring.len() {
                let p = ring[i];
                let q = ring[(i + 1) % ring.len()];
                let cross = p[0] * q[1] - q[0] * p[1];
                a += cross;
                cx += (p[0] + q[0]) * cross;
                cy += (p[1] + q[1]) * cross;
                n += 1;
                sx += p[0];
                sy += p[1];
            }
        }
    }
    if n == 0 {
        return None;
    }
    if a.abs() > 1e-9 {
        Some([cx / (3.0 * a), cy / (3.0 * a)])
    } else {
        Some([sx / n as f64, sy / n as f64])
    }
}

fn ring_px(ls: &geo_types::LineString<f32>) -> Vec<[f64; 2]> {
    let mut pts: Vec<[f64; 2]> = ls.0.iter().map(|c| [c.x as f64, c.y as f64]).collect();
    // Drop the closing duplicate.
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

fn poly_px(p: &geo_types::Polygon<f32>) -> Vec<Vec<[f64; 2]>> {
    let mut rings = vec![ring_px(p.exterior())];
    rings.extend(p.interiors().iter().map(ring_px));
    rings
}

pub(super) fn feature_polys(g: &geo_types::Geometry<f32>) -> Option<PxPolygons> {
    match g {
        geo_types::Geometry::Polygon(p) => Some(vec![poly_px(p)]),
        geo_types::Geometry::MultiPolygon(mp) => Some(mp.0.iter().map(poly_px).collect()),
        _ => None,
    }
}

pub(super) fn feature_lines(g: &geo_types::Geometry<f32>) -> Option<Vec<Vec<[f64; 2]>>> {
    let pts = |ls: &geo_types::LineString<f32>| ls.0.iter().map(|c| [c.x as f64, c.y as f64]).collect::<Vec<_>>();
    match g {
        geo_types::Geometry::LineString(l) => Some(vec![pts(l)]),
        geo_types::Geometry::MultiLineString(ml) => Some(ml.0.iter().map(pts).collect()),
        _ => None,
    }
}
