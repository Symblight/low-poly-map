//! A small JSON Schema (2020-12) checker for the seed files (`addons/simworld/seed/*.schema.json`).
//!
//! The schemas in the addon are the source of truth for the file shapes: the loader checks every
//! file against them before the typed parse, so an error names the file, the key path and the
//! value (`kaunas.json: districts[2].share: 1.5 is above the maximum 1`). Only the keywords the
//! seed schemas use are supported; [`check_schema_keywords`] fails on any other (pinned by a
//! test), so a schema can never silently say more than this checker enforces.
//!
//! Supported: `type` (string or array), `properties`, `required`, `additionalProperties` (bool or
//! schema), `items`, `prefixItems`, `minItems`, `maxItems`, `minimum`, `maximum`,
//! `exclusiveMinimum`, `exclusiveMaximum`, `enum`, `const`, `minLength`, `maxLength`, `pattern`
//! (anchored subset: literals, `.`, `[...]` classes with ranges, `\` escapes, groups are not
//! supported; quantifiers `+ * ? {n} {n,m}`), `anyOf`, `$ref` to `#/$defs/<name>` or
//! `<other file>#/$defs/<name>` (through a [`Registry`]); annotations
//! (`$schema`, `$id`, `title`, `description`, `default`, `examples`, `$comment`, `$defs`) are
//! ignored.

use serde_json::Value;

const ANNOTATIONS: &[&str] = &["$schema", "$id", "title", "description", "default", "examples", "$comment", "$defs"];
const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "prefixItems",
    "minItems",
    "maxItems",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "enum",
    "const",
    "minLength",
    "maxLength",
    "pattern",
    "anyOf",
    "$ref",
];

/// Schemas by file name, for `$ref`s across files (`airports.schema.json#/$defs/runway`).
pub type Registry<'a> = [(&'a str, &'a Value)];

/// Check `v` against `schema` (the root of its file, for local `$ref`s; `reg` resolves refs to
/// other files). Errors are appended as `<path>: <problem>`; at most `limit` are collected.
pub fn validate(reg: &Registry<'_>, schema: &Value, v: &Value, errs: &mut Vec<String>, limit: usize) {
    check(reg, schema, schema, v, "", errs, limit);
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 60 {
        format!("{}...", s.chars().take(57).collect::<String>())
    } else {
        s
    }
}

fn at(path: &str) -> &str {
    if path.is_empty() {
        "(top level)"
    } else {
        path
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

fn type_ok(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some() || v.as_f64().is_some_and(|f| f.fract() == 0.0 && f.is_finite()),
        _ => false,
    }
}

/// `#/$defs/<name>` in `root`, or `<file>#/$defs/<name>` in the registry: (new root, node).
fn resolve<'a>(reg: &Registry<'a>, root: &'a Value, r: &str) -> Option<(&'a Value, &'a Value)> {
    let (file, frag) = r.split_once('#')?;
    let root = if file.is_empty() { root } else { reg.iter().find(|(n, _)| *n == file)?.1 };
    let name = frag.strip_prefix("/$defs/")?;
    Some((root, root.get("$defs")?.get(name)?))
}

fn check<'a>(reg: &Registry<'a>, root: &'a Value, s: &'a Value, v: &Value, path: &str, errs: &mut Vec<String>, limit: usize) {
    if errs.len() >= limit {
        return;
    }
    let Some(s) = s.as_object() else {
        return; // `true` schema
    };
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        match resolve(reg, root, r) {
            Some((rt, t)) => check(reg, rt, t, v, path, errs, limit),
            None => errs.push(format!("{}: schema error: unresolved $ref {r}", at(path))),
        }
    }
    if let Some(any) = s.get("anyOf").and_then(Value::as_array) {
        let mut best: Option<Vec<String>> = None;
        for alt in any {
            let mut e = Vec::new();
            check(reg, root, alt, v, path, &mut e, limit);
            if e.is_empty() {
                best = None;
                break;
            }
            if best.as_ref().is_none_or(|b| e.len() < b.len()) {
                best = Some(e);
            }
        }
        if let Some(e) = best {
            if any.iter().all(|a| a.get("type").is_some()) {
                // Report a type mismatch once, naming the alternatives.
                let types: Vec<String> = any.iter().filter_map(|a| a.get("type")).map(|t| t.to_string().replace('"', "")).collect();
                if !any.iter().any(|a| a.get("type").and_then(Value::as_str).is_some_and(|t| type_ok(t, v))) {
                    errs.push(format!("{}: {} is not one of the types {}", at(path), short(v), types.join(" | ")));
                    return;
                }
            }
            errs.extend(e);
            return;
        }
    }
    if let Some(t) = s.get("type") {
        let ok = match t {
            Value::String(t) => type_ok(t, v),
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).any(|t| type_ok(t, v)),
            _ => true,
        };
        if !ok {
            errs.push(format!("{}: {} is not of type {}", at(path), short(v), t.to_string().replace('"', "")));
            return;
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            let list: Vec<String> = e.iter().map(|x| x.to_string()).collect();
            errs.push(format!("{}: {} is not one of {}", at(path), short(v), list.join(", ")));
        }
    }
    if let Some(c) = s.get("const") {
        if c != v {
            errs.push(format!("{}: {} must be {}", at(path), short(v), c));
        }
    }
    if let Some(x) = v.as_f64() {
        let bound = |k: &str| s.get(k).and_then(Value::as_f64);
        if let Some(m) = bound("minimum").filter(|m| x < *m) {
            errs.push(format!("{}: {} is below the minimum {m}", at(path), short(v)));
        }
        if let Some(m) = bound("maximum").filter(|m| x > *m) {
            errs.push(format!("{}: {} is above the maximum {m}", at(path), short(v)));
        }
        if let Some(m) = bound("exclusiveMinimum").filter(|m| x <= *m) {
            errs.push(format!("{}: {} must be above {m}", at(path), short(v)));
        }
        if let Some(m) = bound("exclusiveMaximum").filter(|m| x >= *m) {
            errs.push(format!("{}: {} must be below {m}", at(path), short(v)));
        }
    }
    if let Some(st) = v.as_str() {
        let n = st.chars().count() as u64;
        if let Some(m) = s.get("minLength").and_then(Value::as_u64).filter(|m| n < *m) {
            errs.push(format!("{}: {} is shorter than {m} characters", at(path), short(v)));
        }
        if let Some(m) = s.get("maxLength").and_then(Value::as_u64).filter(|m| n > *m) {
            errs.push(format!("{}: {} is longer than {m} characters", at(path), short(v)));
        }
        if let Some(p) = s.get("pattern").and_then(Value::as_str) {
            match pattern_matches(p, st) {
                Ok(true) => {}
                Ok(false) => errs.push(format!("{}: {} does not match {p}", at(path), short(v))),
                Err(e) => errs.push(format!("{}: schema error: {e}", at(path))),
            }
        }
    }
    if let Some(a) = v.as_array() {
        let n = a.len() as u64;
        if let Some(m) = s.get("minItems").and_then(Value::as_u64).filter(|m| n < *m) {
            errs.push(format!("{}: {} items, at least {m} required", at(path), n));
        }
        if let Some(m) = s.get("maxItems").and_then(Value::as_u64).filter(|m| n > *m) {
            errs.push(format!("{}: {} items, at most {m} allowed", at(path), n));
        }
        let prefix = s.get("prefixItems").and_then(Value::as_array);
        let np = prefix.map_or(0, Vec::len);
        for (i, x) in a.iter().enumerate() {
            let p = format!("{path}[{i}]");
            if let Some(ps) = prefix.and_then(|ps| ps.get(i)) {
                check(reg, root, ps, x, &p, errs, limit);
            } else if i >= np {
                if let Some(it) = s.get("items") {
                    check(reg, root, it, x, &p, errs, limit);
                }
            }
            if errs.len() >= limit {
                return;
            }
        }
    }
    if let Some(o) = v.as_object() {
        if let Some(req) = s.get("required").and_then(Value::as_array) {
            for k in req.iter().filter_map(Value::as_str) {
                if !o.contains_key(k) {
                    errs.push(format!("{}: missing required key `{k}`", at(path)));
                }
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        for (k, x) in o {
            let p = join(path, k);
            if let Some(ps) = props.and_then(|ps| ps.get(k)) {
                check(reg, root, ps, x, &p, errs, limit);
            } else {
                match s.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        let known: Vec<&str> = props.map(|ps| ps.keys().map(String::as_str).collect()).unwrap_or_default();
                        errs.push(format!("{}: unknown key `{k}` (known: {})", at(path), known.join(", ")));
                    }
                    Some(ap @ Value::Object(_)) => check(reg, root, ap, x, &p, errs, limit),
                    _ => {}
                }
            }
            if errs.len() >= limit {
                return;
            }
        }
    }
}

/// Every keyword of `schema` (recursively) is supported or an annotation.
pub fn check_schema_keywords(schema: &Value) -> Result<(), String> {
    fn walk(v: &Value, path: &str) -> Result<(), String> {
        let Some(o) = v.as_object() else {
            return Ok(());
        };
        for (k, x) in o {
            let p = join(path, k);
            match k.as_str() {
                "properties" | "$defs" => {
                    for (n, sub) in x.as_object().into_iter().flatten() {
                        walk(sub, &join(&p, n))?;
                    }
                }
                "items" | "additionalProperties" => walk(x, &p)?,
                "prefixItems" | "anyOf" => {
                    for (i, sub) in x.as_array().into_iter().flatten().enumerate() {
                        walk(sub, &format!("{p}[{i}]"))?;
                    }
                }
                "pattern" => {
                    let pat = x.as_str().ok_or(format!("{p}: not a string"))?;
                    pattern_matches(pat, "").map_err(|e| format!("{p}: {e}"))?;
                }
                k if KEYWORDS.contains(&k) || ANNOTATIONS.contains(&k) => {}
                k => return Err(format!("{p}: unsupported schema keyword `{k}`")),
            }
        }
        Ok(())
    }
    walk(schema, "")
}

// ---------------------------------------------------------------------------------------------
// Pattern subset

#[derive(Debug)]
enum Atom {
    Any,
    Lit(char),
    Class(Vec<(char, char)>, bool),
}

impl Atom {
    fn matches(&self, c: char) -> bool {
        match self {
            Atom::Any => true,
            Atom::Lit(l) => *l == c,
            Atom::Class(r, neg) => r.iter().any(|(a, b)| (*a..=*b).contains(&c)) != *neg,
        }
    }
}

/// (anchored at start, atoms with their repeat range, anchored at end).
type Pattern = (bool, Vec<(Atom, usize, usize)>, bool);

fn parse_pattern(p: &str) -> Result<Pattern, String> {
    let mut cs: Vec<char> = p.chars().collect();
    let start = cs.first() == Some(&'^');
    if start {
        cs.remove(0);
    }
    let end = cs.last() == Some(&'$') && !(cs.len() >= 2 && cs[cs.len() - 2] == '\\');
    if end {
        cs.pop();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        let atom = match cs[i] {
            '.' => {
                i += 1;
                Atom::Any
            }
            '\\' => {
                let c = *cs.get(i + 1).ok_or("pattern ends in \\")?;
                i += 2;
                Atom::Lit(c)
            }
            '[' => {
                let mut j = i + 1;
                let neg = cs.get(j) == Some(&'^');
                if neg {
                    j += 1;
                }
                let mut ranges = Vec::new();
                while j < cs.len() && cs[j] != ']' {
                    let a = if cs[j] == '\\' {
                        j += 1;
                        *cs.get(j).ok_or("bad class")?
                    } else {
                        cs[j]
                    };
                    if cs.get(j + 1) == Some(&'-') && cs.get(j + 2).is_some_and(|c| *c != ']') {
                        ranges.push((a, cs[j + 2]));
                        j += 3;
                    } else {
                        ranges.push((a, a));
                        j += 1;
                    }
                }
                if j >= cs.len() {
                    return Err(format!("unterminated class in pattern {p}"));
                }
                i = j + 1;
                Atom::Class(ranges, neg)
            }
            c @ ('(' | ')' | '|' | '*' | '+' | '?' | '{' | '}') => {
                return Err(format!("unsupported pattern syntax `{c}` in {p}"));
            }
            c => {
                i += 1;
                Atom::Lit(c)
            }
        };
        let (lo, hi) = match cs.get(i) {
            Some('+') => {
                i += 1;
                (1, usize::MAX)
            }
            Some('*') => {
                i += 1;
                (0, usize::MAX)
            }
            Some('?') => {
                i += 1;
                (0, 1)
            }
            Some('{') => {
                let close = cs[i..].iter().position(|c| *c == '}').ok_or("unterminated {")? + i;
                let body: String = cs[i + 1..close].iter().collect();
                i = close + 1;
                let (a, b) = match body.split_once(',') {
                    Some((a, "")) => (a.parse().map_err(|_| "bad {")?, usize::MAX),
                    Some((a, b)) => (a.parse().map_err(|_| "bad {")?, b.parse().map_err(|_| "bad {")?),
                    None => {
                        let n = body.parse().map_err(|_| "bad {")?;
                        (n, n)
                    }
                };
                (a, b)
            }
            _ => (1, 1),
        };
        out.push((atom, lo, hi));
    }
    Ok((start, out, end))
}

fn match_here(atoms: &[(Atom, usize, usize)], s: &[char], end: bool) -> bool {
    let Some(((a, lo, hi), rest)) = atoms.split_first() else {
        return !end || s.is_empty();
    };
    let mut n = 0;
    while n < s.len() && n < *hi && a.matches(s[n]) {
        n += 1;
    }
    loop {
        if n >= *lo && match_here(rest, &s[n..], end) {
            return true;
        }
        if n == 0 || n <= *lo {
            return false;
        }
        n -= 1;
    }
}

/// Does `s` match the (subset) pattern `p`? Unanchored patterns search like JSON Schema.
pub fn pattern_matches(p: &str, s: &str) -> Result<bool, String> {
    let (start, atoms, end) = parse_pattern(p)?;
    let cs: Vec<char> = s.chars().collect();
    if start {
        return Ok(match_here(&atoms, &cs, end));
    }
    Ok((0..=cs.len()).any(|i| match_here(&atoms, &cs[i..], end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patterns() {
        let id = "^[a-z0-9_-]+$";
        assert!(pattern_matches(id, "kaunas_2").unwrap());
        assert!(!pattern_matches(id, "Kaunas").unwrap());
        assert!(!pattern_matches(id, "").unwrap());
        let date = "^[0-9]{4}-[0-9]{2}-[0-9]{2}$";
        assert!(pattern_matches(date, "2026-10-27").unwrap());
        assert!(!pattern_matches(date, "2026-1-27").unwrap());
        assert!(pattern_matches("^res://.+\\.(glb)$", "x").is_err());
        assert!(pattern_matches("a.c", "xxabcxx").unwrap());
        assert!(pattern_matches("^ab?c*$", "acc").unwrap());
        assert!(pattern_matches("^[^/]+$", "08/26").is_ok_and(|m| !m));
    }

    #[test]
    fn errors_name_path_and_value() {
        let schema = json!({
            "type": "object", "required": ["a"], "additionalProperties": false,
            "$defs": { "pos": { "type": "number", "minimum": 0 } },
            "properties": {
                "a": { "type": "array", "items": { "$ref": "#/$defs/pos" }, "minItems": 1 },
                "b": { "anyOf": [ { "type": "string", "enum": ["x"] }, { "type": "array" } ] },
                "c": { "type": ["integer", "null"] }
            }
        });
        check_schema_keywords(&schema).unwrap();
        let mut e = Vec::new();
        validate(&[], &schema, &json!({ "a": [1, -2], "b": "y", "c": 1.5, "d": 0 }), &mut e, 50);
        let all = e.join("\n");
        assert!(all.contains("a[1]: -2 is below the minimum 0"), "{all}");
        assert!(all.contains("b: \"y\" is not one of \"x\""), "{all}");
        assert!(all.contains("c: 1.5 is not of type [integer,null]"), "{all}");
        assert!(all.contains("unknown key `d`"), "{all}");
        let mut e = Vec::new();
        validate(&[], &schema, &json!({ "b": 3 }), &mut e, 50);
        let all = e.join("\n");
        assert!(all.contains("missing required key `a`"), "{all}");
        assert!(all.contains("b: 3 is not one of the types string | array"), "{all}");
        assert!(check_schema_keywords(&json!({ "oneOf": [] })).is_err());
    }
}
