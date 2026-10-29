//! `airports.json`: airports and runways (OurAirports fields), quantized, with overrides.

use serde::{Deserialize, Serialize};

use super::{q7, RunwayOverride};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportFile {
    pub format: u32,
    pub source: String,
    pub source_date: String,
    pub airports: Vec<Airport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Airport {
    pub ident: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub elevation_ft: Option<f64>,
    #[serde(default)]
    pub country: String,
    pub runways: Vec<Runway>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runway {
    pub le: String,
    pub he: String,
    #[serde(default)]
    pub length_ft: Option<f64>,
    #[serde(default)]
    pub width_ft: Option<f64>,
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub lighted: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub le_lat: Option<f64>,
    #[serde(default)]
    pub le_lon: Option<f64>,
    #[serde(default)]
    pub he_lat: Option<f64>,
    #[serde(default)]
    pub he_lon: Option<f64>,
    #[serde(default, rename = "le_heading_degT")]
    pub le_heading_deg_t: Option<f64>,
    #[serde(default)]
    pub le_displaced_threshold_ft: Option<f64>,
    #[serde(default)]
    pub he_displaced_threshold_ft: Option<f64>,
}

impl Runway {
    /// Key in `airports.overrides.<ident>.runways`: `"<le>/<he>"`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.le, self.he)
    }

    /// Both ends known (else the generator places it approximately, section 5.1 step 1).
    pub fn has_both_ends(&self) -> bool {
        self.le_lat.is_some() && self.le_lon.is_some() && self.he_lat.is_some() && self.he_lon.is_some()
    }

    pub(super) fn quantize(&mut self) {
        for v in [&mut self.le_lat, &mut self.le_lon, &mut self.he_lat, &mut self.he_lon].into_iter().flatten() {
            *v = q7(*v);
        }
    }

    pub(super) fn apply(&mut self, o: &RunwayOverride) {
        macro_rules! set {
            ($($f:ident),*) => {$( if o.$f.is_some() { self.$f = o.$f.clone(); } )*};
        }
        set!(le_lat, le_lon, he_lat, he_lon, width_ft, length_ft, le_heading_deg_t, le_displaced_threshold_ft, he_displaced_threshold_ft);
        if let Some(s) = &o.surface {
            self.surface = s.clone();
        }
        if let Some(b) = o.lighted {
            self.lighted = b;
        }
        if let Some(b) = o.closed {
            self.closed = b;
        }
    }
}
