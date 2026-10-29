//! `cities/<id>.json`: centre, hex layout, districts, fills, real sources, landmarks.

use serde::{Deserialize, Serialize};

use super::q7;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutType {
    OldCenterHex,
    Grid,
    Radial,
    Linear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(rename = "type")]
    pub kind: LayoutType,
    pub cell_size_m: f64,
    #[serde(default)]
    pub rotation_deg: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistrictType {
    OldTown,
    Business,
    Residential,
    Industrial,
    Port,
    Park,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Organic,
    Grid,
    Microdistrict,
    Houses,
    Quay,
    Paths,
}

impl DistrictType {
    pub fn name(self) -> &'static str {
        match self {
            DistrictType::OldTown => "old_town",
            DistrictType::Business => "business",
            DistrictType::Residential => "residential",
            DistrictType::Industrial => "industrial",
            DistrictType::Port => "port",
            DistrictType::Park => "park",
        }
    }

    /// Default style of the type (section 6.2).
    pub fn default_style(self) -> Style {
        match self {
            DistrictType::OldTown => Style::Organic,
            DistrictType::Business | DistrictType::Industrial => Style::Grid,
            DistrictType::Residential => Style::Microdistrict,
            DistrictType::Port => Style::Quay,
            DistrictType::Park => Style::Paths,
        }
    }

    /// Styles the type has (section 6.2).
    pub fn styles(self) -> &'static [Style] {
        match self {
            DistrictType::OldTown => &[Style::Organic],
            DistrictType::Business | DistrictType::Industrial => &[Style::Grid],
            DistrictType::Residential => &[Style::Microdistrict, Style::Houses],
            DistrictType::Port => &[Style::Quay],
            DistrictType::Park => &[Style::Paths],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NearFeature {
    Water,
    Coast,
    River,
    Airport,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Near {
    One(NearFeature),
    Any(Vec<NearFeature>),
}

impl Near {
    pub fn features(&self) -> Vec<NearFeature> {
        match self {
            Near::One(f) => vec![*f],
            Near::Any(v) => v.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistrictRequest {
    #[serde(rename = "type")]
    pub kind: DistrictType,
    #[serde(default)]
    pub style: Option<Style>,
    #[serde(default)]
    pub cells: Option<u32>,
    #[serde(default)]
    pub share: Option<f64>,
    #[serde(default)]
    pub rings: Option<[u32; 2]>,
    #[serde(default)]
    pub near: Option<Near>,
    #[serde(default)]
    pub max_height_m: Option<f64>,
    #[serde(default)]
    pub density: Option<f64>,
    #[serde(default)]
    pub landmarks: Option<u32>,
    #[serde(default)]
    pub lit: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fill {
    #[serde(rename = "type")]
    pub kind: DistrictType,
    #[serde(default)]
    pub style: Option<Style>,
    #[serde(default)]
    pub max_height_m: Option<f64>,
    #[serde(default)]
    pub density: Option<f64>,
    #[serde(default)]
    pub lit: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealSource {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandmarkGround {
    Park,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InReal {
    Skip,
    Replace,
}

fn default_landmark_kind() -> String {
    "tower".into()
}
fn default_ground() -> LandmarkGround {
    LandmarkGround::Park
}
fn default_in_real() -> InReal {
    InReal::Skip
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Landmark {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub model: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub heading_deg: f64,
    #[serde(default)]
    pub footprint_m: Option<[f64; 2]>,
    #[serde(default)]
    pub footprint: Option<Vec<[f64; 2]>>,
    pub height_m: f64,
    #[serde(default)]
    pub clear_m: f64,
    #[serde(default = "default_ground")]
    pub ground: LandmarkGround,
    #[serde(default = "default_landmark_kind")]
    pub kind: String,
    #[serde(default = "default_in_real")]
    pub in_real: InReal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct City {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub variant: i64,
    pub center: LatLon,
    pub radius_km: f64,
    pub layout: Layout,
    #[serde(default)]
    pub max_height_m: Option<f64>,
    #[serde(default)]
    pub density: Option<f64>,
    #[serde(default)]
    pub airports: Vec<String>,
    pub districts: Vec<DistrictRequest>,
    pub fill: Fill,
    #[serde(default)]
    pub real: Option<RealSource>,
    #[serde(default)]
    pub edits: Option<String>,
    #[serde(default)]
    pub landmarks: Vec<Landmark>,
}

/// Upper bound of the footprint radius: the outer-ring edge noise keeps a cell while
/// `distance < radius * (1 + 0.15 * noise)` (section 5.3 step 1), noise in [-1, 1].
pub const FOOTPRINT_NOISE: f64 = 0.15;

/// Pointy-top axial hex (q, r) -> centre offset (east, north) in meters for a centre-to-centre
/// distance `cell_m`, unrotated.
pub fn hex_center(q: i32, r: i32, cell_m: f64) -> [f64; 2] {
    let s3h = 0.866_025_403_784_438_6; // sqrt(3) / 2
    [cell_m * (q as f64 + r as f64 * 0.5), cell_m * s3h * r as f64]
}

impl City {
    /// Rings a cell of this city can be in: 0..ring_count. The innermost centre of ring `r` lies
    /// `r * cell * sqrt(3)/2` from the centre, and a cell is kept up to `radius * (1 + 0.15)`.
    pub fn ring_count(&self) -> u32 {
        let reach = self.radius_km * 1000.0 * (1.0 + FOOTPRINT_NOISE);
        (reach / (self.layout.cell_size_m * 0.866_025_403_784_438_6)).floor() as u32 + 1
    }

    /// Cells whose centre is within `radius * factor` (no edge noise): `factor` 1 = the nominal
    /// footprint, `1 + FOOTPRINT_NOISE` = the most cells the footprint can ever have.
    pub fn cell_count(&self, factor: f64) -> u32 {
        let rad = self.radius_km * 1000.0 * factor;
        let n = self.ring_count() as i32;
        let mut count = 0;
        for q in -n..=n {
            for r in -n..=n {
                let [x, y] = hex_center(q, r, self.layout.cell_size_m);
                if (x * x + y * y).sqrt() < rad {
                    count += 1;
                }
            }
        }
        count
    }

    pub(super) fn quantize(&mut self) {
        self.center.lat = q7(self.center.lat);
        self.center.lon = q7(self.center.lon);
        for l in &mut self.landmarks {
            l.lat = q7(l.lat);
            l.lon = q7(l.lon);
            for p in l.footprint.iter_mut().flatten() {
                *p = [q7(p[0]), q7(p[1])];
            }
        }
    }
}
