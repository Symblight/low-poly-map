//! Feature attributes: MVT property values -> numbers, text, flags, building tags.

use std::collections::HashMap;

use mvt_reader::feature::Value;

use super::BuildingTags;

pub(super) fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f as f64),
        Value::Double(d) => Some(*d),
        Value::Int(i) | Value::SInt(i) => Some(*i as f64),
        Value::UInt(u) => Some(*u as f64),
        Value::String(s) => {
            // Lenient: "12", "12.5 m", "12,5".
            let t: String = s
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '-')
                .map(|c| if c == ',' { '.' } else { c })
                .collect();
            t.parse().ok()
        }
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

pub(super) fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => num(other).map(|n| n.to_string()),
    }
}

/// A boolean tile attribute: MVT bool, or the strings `yes` / `true` / `1` (and numbers != 0).
fn flag(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        },
        Value::Null => None,
        other => num(other).map(|n| n != 0.0),
    }
}

/// The optional building tags of a feature (absent attributes = absent tags).
pub(super) fn building_tags(props: &HashMap<String, Value>) -> BuildingTags {
    let s = |k: &str| props.get(k).and_then(text).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let b = |k: &str| props.get(k).and_then(flag).unwrap_or(false);
    BuildingTags {
        historic: s("historic"),
        man_made: s("man_made"),
        windows: props.get("windows").and_then(flag),
        ruins: b("ruins"),
        disused: b("disused"),
        abandoned: b("abandoned"),
    }
}
