//! Godot Variant <-> JSON (`serde_json::Value`) and the `configure_world` options Dictionary ->
//! `simworld_core::WorldOptions`. Type conversion only; all validation of profile values is in
//! the core.

use godot::classes::FileAccess;
use godot::prelude::*;
use serde_json::{Map, Number, Value};

use std::sync::Arc;

use simworld_core::{Diagnostics, SeedFileReader, WorldOptions};

/// Option keys `configure_world` understands (contract section 3, api 0.6).
const OPTION_KEYS: &[&str] = &[
    "origin_lat",
    "origin_lon",
    "tile_url",
    "cache_dir",
    "profile_path",
    "profile_overrides",
    "diagnostics",
    "layers",
    "terrain_center",
    "seed_world",
];

fn num(x: f64, path: &str) -> Result<Value, String> {
    Number::from_f64(x).map(Value::Number).ok_or_else(|| format!("{path}: non-finite number {x}"))
}

fn floats<T: Copy + Into<f64>>(s: &[T], path: &str) -> Result<Value, String> {
    s.iter().map(|&x| num(x.into(), path)).collect::<Result<Vec<_>, _>>().map(Value::Array)
}

/// Variant -> JSON. Dictionaries become objects (keys as strings), arrays and packed arrays
/// become arrays, `Color` becomes `[r, g, b]` (profile colors are rgb), vectors become arrays.
pub fn variant_to_json(v: &Variant, path: &str) -> Result<Value, String> {
    Ok(match v.get_type() {
        VariantType::NIL => Value::Null,
        VariantType::BOOL => Value::Bool(v.to::<bool>()),
        VariantType::INT => Value::from(v.to::<i64>()),
        VariantType::FLOAT => num(v.to::<f64>(), path)?,
        VariantType::STRING | VariantType::STRING_NAME | VariantType::NODE_PATH => Value::String(v.to_string()),
        VariantType::DICTIONARY => {
            let d = v.to::<VarDictionary>();
            let mut m = Map::new();
            for (k, x) in d.iter_shared() {
                let key = k.to_string();
                let p = if path.is_empty() { key.clone() } else { format!("{path}.{key}") };
                m.insert(key, variant_to_json(&x, &p)?);
            }
            Value::Object(m)
        }
        VariantType::ARRAY => {
            let a = v.to::<VarArray>();
            let p = format!("{path}[]");
            a.iter_shared().map(|x| variant_to_json(&x, &p)).collect::<Result<Vec<_>, _>>().map(Value::Array)?
        }
        VariantType::PACKED_STRING_ARRAY => {
            Value::Array(v.to::<PackedStringArray>().as_slice().iter().map(|s| Value::String(s.to_string())).collect())
        }
        VariantType::PACKED_FLOAT32_ARRAY => floats(v.to::<PackedFloat32Array>().as_slice(), path)?,
        VariantType::PACKED_FLOAT64_ARRAY => floats(v.to::<PackedFloat64Array>().as_slice(), path)?,
        VariantType::PACKED_INT32_ARRAY => Value::Array(v.to::<PackedInt32Array>().as_slice().iter().map(|&x| Value::from(x)).collect()),
        VariantType::PACKED_INT64_ARRAY => Value::Array(v.to::<PackedInt64Array>().as_slice().iter().map(|&x| Value::from(x)).collect()),
        VariantType::COLOR => {
            let c = v.to::<Color>();
            floats(&[c.r, c.g, c.b], path)?
        }
        VariantType::VECTOR2 => {
            let c = v.to::<Vector2>();
            floats(&[c.x, c.y], path)?
        }
        VariantType::VECTOR3 => {
            let c = v.to::<Vector3>();
            floats(&[c.x, c.y, c.z], path)?
        }
        t => return Err(format!("{path}: unsupported value type {t:?}")),
    })
}

/// JSON -> Variant: objects -> Dictionary, arrays -> Array, integers -> int, other numbers ->
/// float, strings -> String.
pub fn json_to_variant(v: &Value) -> Variant {
    match v {
        Value::Null => Variant::nil(),
        Value::Bool(b) => b.to_variant(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_variant(),
            None => n.as_f64().unwrap_or(f64::NAN).to_variant(),
        },
        Value::String(s) => GString::from(s.as_str()).to_variant(),
        Value::Array(a) => {
            let mut out = VarArray::new();
            for x in a {
                out.push(&json_to_variant(x));
            }
            out.to_variant()
        }
        Value::Object(m) => {
            let mut d = VarDictionary::new();
            for (k, x) in m {
                d.set(&GString::from(k.as_str()), &json_to_variant(x));
            }
            d.to_variant()
        }
    }
}

fn get_f64(d: &VarDictionary, key: &str) -> Result<Option<f64>, String> {
    match d.get(key) {
        None => Ok(None),
        Some(v) => match v.get_type() {
            VariantType::FLOAT => Ok(Some(v.to::<f64>())),
            VariantType::INT => Ok(Some(v.to::<i64>() as f64)),
            t => Err(format!("{key} must be a number, got {t:?}")),
        },
    }
}

fn get_string(d: &VarDictionary, key: &str) -> Result<Option<String>, String> {
    match d.get(key) {
        None => Ok(None),
        Some(v) => match v.get_type() {
            VariantType::NIL => Ok(None),
            VariantType::STRING | VariantType::STRING_NAME => Ok(Some(v.to_string())),
            t => Err(format!("{key} must be a String, got {t:?}")),
        },
    }
}

/// Read a profile file through Godot's file system (`res://`, `user://` or an OS path).
fn read_profile(path: &str) -> Result<String, String> {
    if !FileAccess::file_exists(path) {
        return Err(format!("profile_path {path}: file not found"));
    }
    let text = FileAccess::get_file_as_string(path).to_string();
    if text.is_empty() {
        let err = FileAccess::get_open_error();
        if err != godot::global::Error::OK {
            return Err(format!("profile_path {path}: cannot read ({err:?})"));
        }
    }
    Ok(text)
}

/// Seed world files (api 1.7) through Godot's file system (`res://`, `user://`, OS paths); the
/// core resolves relative paths against the manifest. Called by the core only inside
/// `configure_world`, i.e. on the main thread.
fn read_seed_file(path: &str) -> Result<String, String> {
    if !FileAccess::file_exists(path) {
        return Err("file not found".to_string());
    }
    let text = FileAccess::get_file_as_string(path).to_string();
    if text.is_empty() {
        let err = FileAccess::get_open_error();
        if err != godot::global::Error::OK {
            return Err(format!("cannot read ({err:?})"));
        }
    }
    Ok(text)
}

/// `configure_world` options Dictionary -> core options. Unknown option keys are returned as
/// warnings (a host written for a newer contract may pass keys this library does not know).
pub fn options_from_dict(d: &VarDictionary, current: &Diagnostics) -> Result<(WorldOptions, Vec<String>), String> {
    let mut warns = Vec::new();
    for (k, _) in d.iter_shared() {
        let k = k.to_string();
        if !OPTION_KEYS.contains(&k.as_str()) {
            warns.push(format!("configure_world: unknown option `{k}` ignored"));
        }
    }
    let origin_lat = get_f64(d, "origin_lat")?.ok_or("origin_lat is required")?;
    let origin_lon = get_f64(d, "origin_lon")?.ok_or("origin_lon is required")?;
    let tile_url = get_string(d, "tile_url")?.unwrap_or_default();
    let cache_dir = get_string(d, "cache_dir")?.unwrap_or_default();
    let profile_json = match get_string(d, "profile_path")?.filter(|p| !p.is_empty()) {
        Some(p) => Some((p.clone(), read_profile(&p)?)),
        None => None,
    };
    let profile_overrides = match d.get("profile_overrides") {
        None => None,
        Some(v) if v.is_nil() => None,
        Some(v) if v.get_type() == VariantType::DICTIONARY => Some(variant_to_json(&v, "profile_overrides")?),
        Some(v) => return Err(format!("profile_overrides must be a Dictionary, got {:?}", v.get_type())),
    };
    // api 1.4: Array (or PackedStringArray) of layer names; validated by the core.
    let layers = match d.get("layers") {
        None => None,
        Some(v) if v.is_nil() => None,
        Some(v) => match v.get_type() {
            VariantType::PACKED_STRING_ARRAY => Some(v.to::<PackedStringArray>().as_slice().iter().map(|s| s.to_string()).collect()),
            VariantType::ARRAY => {
                let mut out = Vec::new();
                for x in v.to::<VarArray>().iter_shared() {
                    match x.get_type() {
                        VariantType::STRING | VariantType::STRING_NAME => out.push(x.to_string()),
                        t => return Err(format!("layers must hold Strings, got {t:?}")),
                    }
                }
                Some(out)
            }
            t => return Err(format!("layers must be an Array of String, got {t:?}")),
        },
    };
    // api 1.6: [lat, lon] (Array of 2 numbers, PackedFloat64Array or Vector2 = (lat, lon)); range-checked by the core.
    let terrain_center = match d.get("terrain_center") {
        None => None,
        Some(v) if v.is_nil() => None,
        Some(v) => {
            let pair = match v.get_type() {
                VariantType::VECTOR2 => {
                    let c = v.to::<Vector2>();
                    vec![c.x as f64, c.y as f64]
                }
                VariantType::PACKED_FLOAT64_ARRAY => v.to::<PackedFloat64Array>().as_slice().to_vec(),
                VariantType::PACKED_FLOAT32_ARRAY => v.to::<PackedFloat32Array>().as_slice().iter().map(|&x| x as f64).collect(),
                VariantType::ARRAY => {
                    let mut out = Vec::new();
                    for x in v.to::<VarArray>().iter_shared() {
                        match x.get_type() {
                            VariantType::FLOAT => out.push(x.to::<f64>()),
                            VariantType::INT => out.push(x.to::<i64>() as f64),
                            t => return Err(format!("terrain_center must hold numbers, got {t:?}")),
                        }
                    }
                    out
                }
                t => return Err(format!("terrain_center must be an Array of 2 numbers [lat, lon], got {t:?}")),
            };
            match pair[..] {
                [lat, lon] => Some([lat, lon]),
                _ => return Err(format!("terrain_center must hold exactly 2 numbers [lat, lon], got {}", pair.len())),
            }
        }
    };
    // api 1.7: path of a seed world manifest (validated and loaded by the core).
    let seed_world = get_string(d, "seed_world")?.filter(|p| !p.trim().is_empty());
    let diagnostics = match d.get("diagnostics") {
        None => None,
        Some(v) if v.is_nil() => None,
        Some(v) if v.get_type() == VariantType::DICTIONARY => {
            let dd = v.to::<VarDictionary>();
            let mut out = current.clone();
            for (k, x) in dd.iter_shared() {
                match k.to_string().as_str() {
                    "profile" => {
                        out.profile = x.try_to::<bool>().map_err(|_| "diagnostics.profile must be a bool".to_string())?
                    }
                    "warnings_per_second" => {
                        let n = match x.get_type() {
                            VariantType::INT => x.to::<i64>() as f64,
                            VariantType::FLOAT => x.to::<f64>(),
                            t => return Err(format!("diagnostics.warnings_per_second must be a number, got {t:?}")),
                        };
                        if !(n.fract() == 0.0 && (1.0..=1000.0).contains(&n)) {
                            return Err(format!("diagnostics.warnings_per_second = {n} is out of range [1, 1000] (integer)"));
                        }
                        out.warnings_per_second = n as u32;
                    }
                    other => warns.push(format!("configure_world: unknown diagnostics option `{other}` ignored")),
                }
            }
            Some(out)
        }
        Some(v) => return Err(format!("diagnostics must be a Dictionary, got {:?}", v.get_type())),
    };
    Ok((
        WorldOptions {
            origin_lat,
            origin_lon,
            tile_url,
            cache_dir,
            profile_path: None,
            profile_json,
            profile_overrides,
            diagnostics,
            layers,
            terrain_center,
            seed_world,
            seed_reader: Some(SeedFileReader(Arc::new(read_seed_file))),
        },
        warns,
    ))
}
