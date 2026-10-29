//! Loading: JSON text + overrides -> [`Profile`] (shape check, deep merge, version migration).

use serde_json::{Map, Value};

use crate::diag::Log;

use super::ranges::{check_ranges, fmt_num, shape_template, FREE_KEY_MAPS};
use super::*;

// ---------------------------------------------------------------------------------------------
// Loading: JSON text + overrides -> Profile

/// Kind of a JSON value, for type errors.
fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(n) if n.is_u64() || n.is_i64() => "an integer",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

pub(super) fn join(path: &str, k: &str) -> String {
    if path.is_empty() {
        k.to_string()
    } else {
        format!("{path}.{k}")
    }
}

/// Check `v` (from a file or overrides) against the defaults' shape `base`: unknown keys
/// (error, or warning + removal when `lenient`) and JSON types. Integral floats are turned into
/// integers where the default is an integer (Godot Dictionaries carry JSON numbers as floats).
fn check_shape(
    v: &mut Value,
    base: &Value,
    path: &str,
    lenient: bool,
    errs: &mut Vec<String>,
    warns: &mut Vec<String>,
) {
    match (v, base) {
        (Value::Object(m), Value::Object(b)) => {
            let free = FREE_KEY_MAPS.contains(&path);
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                let p = join(path, &k);
                let template = match b.get(&k) {
                    Some(t) => Some(t),
                    None if free => b.values().next(),
                    None => None,
                };
                match template {
                    Some(t) => {
                        if let Some(x) = m.get_mut(&k) {
                            check_shape(x, t, &p, lenient, errs, warns);
                        }
                    }
                    None if lenient => {
                        warns.push(format!("unknown key `{p}` ignored (written for a newer profile_version)"));
                        m.remove(&k);
                    }
                    None => {
                        let hint = if p.starts_with("lights.profiles.") && (p.ends_with(".windows") || p.ends_with(".floodlight")) {
                            " (`windows` exists on residential, office, retail and civic only, `floodlight` on landmark only)"
                        } else {
                            ""
                        };
                        errs.push(format!("unknown key `{p}`{hint}"))
                    }
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if let Some(t) = b.first() {
                for x in a.iter_mut() {
                    check_shape(x, t, &format!("{path}[]"), lenient, errs, warns);
                }
            }
        }
        (x @ Value::Number(_), Value::Number(bn)) => {
            let n = x.as_f64().unwrap_or(f64::NAN);
            if bn.is_u64() || bn.is_i64() {
                if !(n.fract() == 0.0 && n.is_finite()) {
                    errs.push(format!("{} = {} must be an integer", path.trim_end_matches("[]"), fmt_num(n)));
                } else if n < 0.0 {
                    // Range checks name the allowed range later; keep the value for the message.
                    *x = Value::from(n as i64);
                } else {
                    *x = Value::from(n as u64);
                }
            }
        }
        (x, b) => {
            if std::mem::discriminant(x) != std::mem::discriminant(b) {
                errs.push(format!("{} must be {}, got {}", path.trim_end_matches("[]"), kind_name(b), kind_name(x)));
            }
        }
    }
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(x) => merge(x, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// The fixed band edges removed in profile_version 7 (api 1.10, screen-space-error LOD).
const REMOVED_BAND_EDGES: [&str; 3] = ["r0_edge_m", "r1_edge_m", "r2_edge_m"];

/// profile_version 7 replaced `bands.r0..r2_edge_m` by the screen-space error (`lod`) and renamed
/// `bands.r3_edge_m` to `bands.max_distance_m`. A source declaring an older version migrates
/// (`r3_edge_m` -> `max_distance_m`, the other edges dropped with a warning); a current one (or
/// one without `profile_version`, e.g. overrides) that still names them is refused. A newer one
/// is left to the lenient shape check (unknown keys ignored with a warning).
fn migrate_band_edges(src: &mut Value, version: u64, errs: &mut Vec<String>, warns: &mut Vec<String>) {
    let Some(Value::Object(b)) = src.get_mut("bands") else { return };
    let old: Vec<String> =
        b.keys().filter(|k| REMOVED_BAND_EDGES.contains(&k.as_str()) || k.as_str() == "r3_edge_m").cloned().collect();
    if old.is_empty() || version > PROFILE_VERSION as u64 {
        return;
    }
    if version == PROFILE_VERSION as u64 {
        for k in old {
            let hint = if k == "r3_edge_m" {
                "renamed to `bands.max_distance_m`".to_string()
            } else {
                format!(
                    "removed in profile_version {PROFILE_VERSION}: the band edges come from the screen-space error, tune `lod.tau_px` / `lod.error_m` instead"
                )
            };
            errs.push(format!("`bands.{k}` is {hint}"));
        }
        return;
    }
    if let Some(v) = b.remove("r3_edge_m") {
        b.insert("max_distance_m".into(), v);
        warns.push(format!("`bands.r3_edge_m` migrated to `bands.max_distance_m` (profile_version {PROFILE_VERSION})"));
    }
    for k in REMOVED_BAND_EDGES {
        if b.remove(k).is_some() {
            warns.push(format!(
                "`bands.{k}` dropped: since profile_version {PROFILE_VERSION} the band edges come from the screen-space error (`lod.tau_px`, `lod.error_m`)"
            ));
        }
    }
}

/// Version policy + shape check of one source (file or overrides), merged onto `acc`.
fn apply_source(acc: &mut Value, defaults: &Value, mut src: Value, label: &str, log: &dyn Log) -> Result<(), String> {
    let Value::Object(m) = &src else {
        return Err(format!("{label}: the profile must be a JSON object, got {}", kind_name(&src)));
    };
    let version = match m.get("profile_version") {
        None => PROFILE_VERSION as u64,
        Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f.fract() == 0.0 && f >= 1.0) => n.as_f64().unwrap_or(1.0) as u64,
        Some(v) => {
            return Err(format!("{label}: profile_version must be a positive integer, got {v}"));
        }
    };
    let lenient = version > PROFILE_VERSION as u64;
    let mut errs = Vec::new();
    let mut warns = Vec::new();
    migrate_band_edges(&mut src, version, &mut errs, &mut warns);
    check_shape(&mut src, defaults, "", lenient, &mut errs, &mut warns);
    for w in warns {
        log.warn(&format!("WorldStreamer: {label} (profile_version {version}, library reads {PROFILE_VERSION}): {w}"));
    }
    if !errs.is_empty() {
        return Err(format!("{label}: {}", errs.join("; ")));
    }
    if let Value::Object(m) = &mut src {
        m.remove("profile_version");
    }
    merge(acc, src);
    Ok(())
}

/// Parse JSON text (with the position of a syntax error).
pub fn parse_json(text: &str, label: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{label}: invalid JSON: {e}"))
}

/// Build a profile: defaults, then the file (if any), then the overrides (if any). Both are
/// partial; objects merge deeply. `file` = (label for messages, JSON text). Warnings (unknown
/// keys of a newer profile version) go to `log`.
pub fn resolve(file: Option<(&str, &str)>, overrides: Option<&Value>, log: &dyn Log) -> Result<Profile, String> {
    let defaults = serde_json::to_value(WorldProfile::default()).map_err(|e| format!("profile: {e}"))?;
    let template = shape_template(&defaults);
    let mut acc = defaults;
    if let Some((label, text)) = file {
        let v = parse_json(text, label)?;
        apply_source(&mut acc, &template, v, label, log)?;
    }
    if let Some(o) = overrides {
        apply_source(&mut acc, &template, o.clone(), "profile_overrides", log)?;
    }
    if let Value::Object(m) = &mut acc {
        m.insert("profile_version".into(), Value::from(PROFILE_VERSION));
    }
    // Ranges first, on the JSON form: messages then name the key even for values the typed
    // fields could not hold (e.g. a negative count).
    let mut errs = Vec::new();
    check_ranges(&acc, "", &mut errs);
    if !errs.is_empty() {
        return Err(format!("profile: {}", errs.join("; ")));
    }
    let world: WorldProfile = serde_json::from_value(acc).map_err(|e| format!("profile: {e}"))?;
    let p = Profile::new(world).map_err(|e| format!("profile: {e}"))?;
    for w in p.warnings() {
        log.warn(&format!("WorldStreamer: profile: {w}"));
    }
    Ok(p)
}

/// [`resolve`] with the file read from disk (CLI, Rust hosts).
pub fn load_file(path: &str, overrides: Option<&Value>, log: &dyn Log) -> Result<Profile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    resolve(Some((path, &text)), overrides, log)
}

/// Keys of a JSON object tree as dotted paths (arrays are leaves). Used by the schema tests.
pub fn key_paths(v: &Value) -> Vec<String> {
    fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
        if let Value::Object(m) = v {
            for (k, x) in m {
                let p = join(path, k);
                out.push(p.clone());
                walk(x, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out.sort();
    out
}

/// Empty JSON object (no overrides).
pub fn empty_object() -> Value {
    Value::Object(Map::new())
}
