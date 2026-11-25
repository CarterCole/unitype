//! Shorthand accepted when reading a workspace.
//!
//! The canonical object form is the normative contract — it is what hashes are
//! computed over and what the system emits. But writing `{"k":"optional","of":
//! {"k":"list","of":{"k":"i64"}}}` by hand is miserable, so on input a type may
//! also be written as a string:
//!
//! | Sugar        | Meaning                        |
//! |--------------|--------------------------------|
//! | `"i64"`      | the primitive of that name     |
//! | `"int"`      | `i64` (also `float`, `str`)    |
//! | `"User"`     | a reference in this universe   |
//! | `"geo::Point"` | a reference to another universe |
//! | `"i64?"`     | optional                       |
//! | `"[i64]"`    | a list                         |
//! | `"map<str, i64>"` | a map                     |
//! | `"future<T>"` | a future                      |
//! | `"i32[0..255]"` | an integer within bounds, both ends inclusive |
//! | `"[T; 4]"`, `"bytes[16]"` | a list or bytes of so many; `..8`, `1..` for one end |
//! | `"str[len 1..63, size ..253]"` | a string bounded in scalar values, in UTF-8 bytes, or both |
//! | `"str[match \"[a-z]+\"]"` | a string matching a regular expression in full |
//! | `"future<T>[wait 100..5000]"` | a future whose value arrives within the window, in milliseconds |
//!
//! Expansion happens as a pass over the parsed JSON before it is read into the
//! IR, directed by the schema so that a string is only ever expanded where a
//! type actually belongs. `{"k":"map","value":"i64"}` expands `value` because
//! the enclosing node is a map; `{"k":"lit","value":"hi"}` does not, because
//! there the payload is a value.

use serde_json::{Map, Value as Json};

/// Why shorthand could not be expanded.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum SugarError {
    /// A string in type position was not recognizable as a type.
    #[error(
        "`{0}` is not a type: expected a primitive, a name, `T?`, `[T]`, or bounds like `i32[0..255]`"
    )]
    NotAType(String),
    /// Brackets or a `?` were left dangling.
    #[error("`{0}` is malformed: {1}")]
    Malformed(String, &'static str),
}

/// Primitive spellings accepted on input, including the convenience aliases.
const PRIMITIVES: &[(&str, &str)] = &[
    ("never", "never"),
    ("any", "any"),
    ("unit", "unit"),
    ("null", "unit"),
    ("bool", "bool"),
    ("boolean", "bool"),
    ("i32", "i32"),
    ("i64", "i64"),
    ("int", "i64"),
    ("f64", "f64"),
    ("float", "f64"),
    ("string", "string"),
    ("str", "string"),
    ("bytes", "bytes"),
];

/// Expands shorthand throughout a workspace document, in place.
pub fn desugar_workspace(json: &mut Json) -> Result<(), SugarError> {
    let Some(obj) = json.as_object_mut() else {
        return Ok(());
    };
    if let Some(universes) = obj.get_mut("universes").and_then(Json::as_array_mut) {
        for u in universes {
            desugar_universe(u)?;
        }
    }
    Ok(())
}

/// Expands shorthand throughout one universe document, in place.
pub fn desugar_universe(json: &mut Json) -> Result<(), SugarError> {
    let Some(obj) = json.as_object_mut() else {
        return Ok(());
    };
    if let Some(types) = obj.get_mut("types").and_then(Json::as_array_mut) {
        for t in types {
            desugar_typedef(t)?;
        }
    }
    if let Some(functions) = obj.get_mut("functions").and_then(Json::as_array_mut) {
        for f in functions {
            desugar_function(f)?;
        }
    }
    Ok(())
}

fn desugar_typedef(json: &mut Json) -> Result<(), SugarError> {
    let Some(obj) = json.as_object_mut() else {
        return Ok(());
    };
    let kind = obj
        .get("k")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    desugar_param_decls(obj)?;
    match kind.as_str() {
        "struct" => {
            type_ref_slot(obj, "extends")?;
            type_ref_array(obj, "implements")?;
            desugar_fields(obj)?;
        }
        "interface" => {
            type_ref_array(obj, "extends")?;
            desugar_fields(obj)?;
        }
        "enum" => {
            if let Some(variants) = obj.get_mut("variants").and_then(Json::as_array_mut) {
                for v in variants {
                    if let Some(v) = v.as_object_mut() {
                        type_slot(v, "payload")?;
                    }
                }
            }
        }
        "alias" => type_slot(obj, "target")?,
        _ => {}
    }
    Ok(())
}

fn desugar_function(json: &mut Json) -> Result<(), SugarError> {
    let Some(obj) = json.as_object_mut() else {
        return Ok(());
    };
    if let Some(decls) = obj.get_mut("params_decl").and_then(Json::as_array_mut) {
        for d in decls {
            if let Some(d) = d.as_object_mut() {
                type_ref_array(d, "bounds")?;
            }
        }
    }
    if let Some(params) = obj.get_mut("params").and_then(Json::as_array_mut) {
        for p in params {
            if let Some(p) = p.as_object_mut() {
                type_slot(p, "ty")?;
            }
        }
    }
    type_slot(obj, "ret")?;
    if let Some(body) = obj.get_mut("body") {
        desugar_expr(body)?;
    }
    Ok(())
}

fn desugar_param_decls(obj: &mut Map<String, Json>) -> Result<(), SugarError> {
    if let Some(decls) = obj.get_mut("params").and_then(Json::as_array_mut) {
        for d in decls {
            if let Some(d) = d.as_object_mut() {
                // A parameter declaration has `bounds`; a function parameter has
                // `ty`. Only the former appears on a type declaration.
                if d.contains_key("bounds") {
                    type_ref_array(d, "bounds")?;
                }
            }
        }
    }
    Ok(())
}

fn desugar_fields(obj: &mut Map<String, Json>) -> Result<(), SugarError> {
    if let Some(fields) = obj.get_mut("fields").and_then(Json::as_array_mut) {
        for f in fields {
            if let Some(f) = f.as_object_mut() {
                type_slot(f, "ty")?;
            }
        }
    }
    Ok(())
}

/// Expands shorthand in the type-reference slots inside an expression body.
fn desugar_expr(json: &mut Json) -> Result<(), SugarError> {
    match json {
        Json::Object(obj) => {
            let kind = obj
                .get("k")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            match kind.as_str() {
                "make_record" | "make_variant" | "match" => type_ref_slot(obj, "ty")?,
                // The type a fold carries, when it is written out.
                "fold" => type_slot(obj, "ty")?,
                _ => {}
            }
            for (key, child) in obj.iter_mut() {
                // A literal's payload is a value, not an expression.
                if kind == "lit" && key == "value" {
                    continue;
                }
                desugar_expr(child)?;
            }
        }
        Json::Array(items) => items.iter_mut().try_for_each(desugar_expr)?,
        _ => {}
    }
    Ok(())
}

/// Expands the type held under `key`, if present.
fn type_slot(obj: &mut Map<String, Json>, key: &str) -> Result<(), SugarError> {
    if let Some(slot) = obj.get_mut(key) {
        desugar_type(slot)?;
    }
    Ok(())
}

/// Expands every type in the array held under `key`, if present.
fn type_array(obj: &mut Map<String, Json>, key: &str) -> Result<(), SugarError> {
    if let Some(items) = obj.get_mut(key).and_then(Json::as_array_mut) {
        for item in items {
            desugar_type(item)?;
        }
    }
    Ok(())
}

/// Expands shorthand for a type, in place, recursing into its children.
pub fn desugar_type(slot: &mut Json) -> Result<(), SugarError> {
    if let Some(s) = slot.as_str() {
        *slot = parse_type_sugar(s)?;
    }
    let Some(obj) = slot.as_object_mut() else {
        return Ok(());
    };
    let kind = obj
        .get("k")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    match kind.as_str() {
        "optional" | "list" | "future" | "bounded" => type_slot(obj, "of")?,
        "map" => {
            type_slot(obj, "key")?;
            type_slot(obj, "value")?;
        }
        "tuple" => type_array(obj, "items")?,
        "union" => type_array(obj, "members")?,
        "record" => desugar_fields(obj)?,
        "function" => {
            type_array(obj, "params")?;
            type_slot(obj, "ret")?;
        }
        "ref" | "apply" => type_array(obj, "args")?,
        _ => {}
    }
    Ok(())
}

/// Expands a type-reference slot, where only a name is meaningful.
fn type_ref_slot(obj: &mut Map<String, Json>, key: &str) -> Result<(), SugarError> {
    if let Some(slot) = obj.get_mut(key) {
        desugar_type_ref(slot)?;
    }
    Ok(())
}

/// Expands every type reference in the array held under `key`.
fn type_ref_array(obj: &mut Map<String, Json>, key: &str) -> Result<(), SugarError> {
    if let Some(items) = obj.get_mut(key).and_then(Json::as_array_mut) {
        for item in items {
            desugar_type_ref(item)?;
        }
    }
    Ok(())
}

/// Expands one type reference: the name itself may be shorthand, and so may the
/// generic arguments applied to it.
fn desugar_type_ref(slot: &mut Json) -> Result<(), SugarError> {
    if let Some(s) = slot.as_str() {
        *slot = name_to_ref_fields(s);
    }
    if let Some(obj) = slot.as_object_mut() {
        type_array(obj, "args")?;
    }
    Ok(())
}

/// Splits `Universe::Name` or `Name` into the fields of a type reference.
fn name_to_ref_fields(s: &str) -> Json {
    let mut obj = Map::new();
    match s.split_once("::") {
        Some((u, n)) => {
            obj.insert("universe".into(), Json::String(u.to_string()));
            obj.insert("name".into(), Json::String(n.to_string()));
        }
        None => {
            obj.insert("name".into(), Json::String(s.to_string()));
        }
    }
    Json::Object(obj)
}

/// The inside of `name<...>`, if that is what this is.
fn angled<'a>(t: &'a str, name: &str) -> Option<&'a str> {
    t.strip_prefix(name)?.strip_prefix('<')?.strip_suffix('>')
}

/// Splits at the first comma that is not inside brackets of some kind.
///
/// `map<str, map<str, i64>>` has two commas and only the first one separates
/// this map's key from its value.
fn split_once_at_depth_zero(t: &str) -> Option<(&str, &str)> {
    split_once_at_depth_zero_on(t, ',')
}

/// Splits at the first `sep` that is not inside brackets of some kind.
fn split_once_at_depth_zero_on(t: &str, sep: char) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (at, c) in t.char_indices() {
        match c {
            '<' | '[' | '(' | '{' => depth += 1,
            '>' | ']' | ')' | '}' => depth = depth.saturating_sub(1),
            c if c == sep && depth == 0 => return Some((&t[..at], &t[at + c.len_utf8()..])),
            _ => {}
        }
    }
    None
}

/// Splits `base[spec]` into the type and the bounds written after it, when
/// that is what this is. A list, which begins with `[`, is not. Brackets inside
/// a quoted pattern do not count, so `str[match "[a-z]+"]` splits where it
/// should.
fn split_bounds_suffix(t: &str) -> Option<(&str, &str)> {
    if !t.ends_with(']') || t.starts_with('[') {
        return None;
    }
    let mut open = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for (at, c) in t.char_indices() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '[' => open.push(at),
            ']' => {
                let start = open.pop()?;
                if at + 1 == t.len() {
                    return (start > 0).then(|| (&t[..start], &t[start + 1..at]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Splits at every comma that is not inside a quoted string, so a pattern may
/// contain one.
fn split_outside_quotes(t: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (at, c) in t.char_indices() {
        if quoted {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => {
                parts.push(&t[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&t[start..]);
    parts
}

/// `n`, `a..b`, `a..`, or `..b`, both ends inclusive, as a bounds object.
fn parse_bounds(s: &str, spec: &str) -> Result<Json, SugarError> {
    let spec = spec.trim();
    let end = |text: &str, which: &'static str| -> Result<Option<i64>, SugarError> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<i64>()
            .map(Some)
            .map_err(|_| SugarError::Malformed(s.to_string(), which))
    };
    let (min, max) = match spec.split_once("..") {
        Some((lo, hi)) => (
            end(lo, "a bound must be an integer")?,
            end(hi, "a bound must be an integer")?,
        ),
        None => {
            let n = end(spec, "bounds are `n`, `a..b`, `a..`, or `..b`")?;
            (n, n)
        }
    };
    if min.is_none() && max.is_none() {
        return Err(SugarError::Malformed(s.to_string(), "`..` bounds nothing"));
    }
    let mut obj = Map::new();
    if let Some(min) = min {
        obj.insert("min".into(), Json::from(min));
    }
    if let Some(max) = max {
        obj.insert("max".into(), Json::from(max));
    }
    Ok(Json::Object(obj))
}

/// A future's window: a range written as bounds are, `100..5000`, or limits in
/// words. `not before 100` is no sooner than 100 milliseconds. `up until 5000`
/// is no later than 5000. `not by 5000` is before 5000, which in whole
/// milliseconds is no later than 4999. Two limits join with `and`, one of each
/// kind, in either order.
fn parse_window(s: &str, spec: &str) -> Result<Json, SugarError> {
    let spec = spec.trim();
    // A range has no spaces in it, and the words always do.
    if !spec.contains(char::is_whitespace) {
        return parse_bounds(s, spec);
    }
    let limit = |text: &str| -> Result<i64, SugarError> {
        text.trim().parse::<i64>().map_err(|_| {
            SugarError::Malformed(
                s.to_string(),
                "a window's limit is a whole number of milliseconds",
            )
        })
    };
    let (mut min, mut max): (Option<i64>, Option<i64>) = (None, None);
    for clause in spec.split(" and ") {
        let clause = clause.trim();
        if let Some(text) = clause.strip_prefix("not before ") {
            if min.replace(limit(text)?).is_some() {
                return Err(SugarError::Malformed(
                    s.to_string(),
                    "a window has one `not before`",
                ));
            }
        } else if let Some(text) = clause.strip_prefix("not by ") {
            if max.replace(limit(text)?.saturating_sub(1)).is_some() {
                return Err(SugarError::Malformed(
                    s.to_string(),
                    "a window has one upper limit",
                ));
            }
        } else if let Some(text) = clause.strip_prefix("up until ") {
            if max.replace(limit(text)?).is_some() {
                return Err(SugarError::Malformed(
                    s.to_string(),
                    "a window has one upper limit",
                ));
            }
        } else {
            return Err(SugarError::Malformed(
                s.to_string(),
                "a window is `not before N`, `not by N`, or `up until N`, joined by `and`",
            ));
        }
    }
    let mut obj = Map::new();
    if let Some(min) = min {
        obj.insert("min".into(), Json::from(min));
    }
    if let Some(max) = max {
        obj.insert("max".into(), Json::from(max));
    }
    Ok(Json::Object(obj))
}

/// Puts bounds on an already-expanded type, under the slot the type takes them
/// in: an integer's `value`, a container's `len`, a string's `len` or `size`.
fn bounds_on(s: &str, base: Json, spec: &str) -> Result<Json, SugarError> {
    let kind = base.get("k").and_then(Json::as_str).unwrap_or("");
    match kind {
        "i32" | "i64" => Ok(bounded(base, "value", parse_bounds(s, spec)?)),
        "bytes" | "list" | "map" => Ok(bounded(base, "len", parse_bounds(s, spec)?)),
        "string" => {
            let mut out = base;
            for part in split_outside_quotes(spec) {
                let Some((word, rest)) = part.trim().split_once(char::is_whitespace) else {
                    return Err(SugarError::Malformed(
                        s.to_string(),
                        "a string has two lengths: say `len` for scalar values or `size` for bytes",
                    ));
                };
                match word {
                    "len" | "size" => out = bounded(out, word, parse_bounds(s, rest)?),
                    "match" => {
                        let pattern: String = serde_json::from_str(rest.trim()).map_err(|_| {
                            SugarError::Malformed(
                                s.to_string(),
                                "a pattern is a regular expression in double quotes",
                            )
                        })?;
                        out = bounded(out, "match", Json::String(pattern));
                    }
                    _ => {
                        return Err(SugarError::Malformed(
                            s.to_string(),
                            "a string's bounds are `len`, `size`, or `match`",
                        ));
                    }
                }
            }
            Ok(out)
        }
        "future" => {
            let Some(window) = spec.trim().strip_prefix("wait ") else {
                return Err(SugarError::Malformed(
                    s.to_string(),
                    "a future's bound is its window: `future<T>[wait 100..5000]`",
                ));
            };
            Ok(bounded(base, "wait", parse_window(s, window)?))
        }
        "bounded" => Err(SugarError::Malformed(
            s.to_string(),
            "bounds are given once",
        )),
        _ => Err(SugarError::Malformed(
            s.to_string(),
            "only an integer, a string, bytes, a list, or a map takes bounds",
        )),
    }
}

/// `base` with `bounds` under `slot`, joining a `bounded` that is already there.
fn bounded(base: Json, slot: &str, bounds: Json) -> Json {
    let mut obj = match base {
        Json::Object(obj) if obj.get("k").and_then(Json::as_str) == Some("bounded") => obj,
        other => {
            let mut obj = Map::new();
            obj.insert("k".into(), Json::String("bounded".into()));
            obj.insert("of".into(), other);
            obj
        }
    };
    obj.insert(slot.to_string(), bounds);
    Json::Object(obj)
}

/// Expands one shorthand string into the canonical object form.
pub fn parse_type_sugar(s: &str) -> Result<Json, SugarError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(SugarError::NotAType(s.to_string()));
    }

    if let Some(inner) = t.strip_suffix('?') {
        if inner.is_empty() {
            return Err(SugarError::Malformed(s.to_string(), "`?` needs a type"));
        }
        let mut obj = Map::new();
        obj.insert("k".into(), Json::String("optional".into()));
        obj.insert("of".into(), parse_type_sugar(inner)?);
        return Ok(Json::Object(obj));
    }

    // `[T]`, and `[T; 4]` for a list of so many: exactly, `..8` at most, `1..`
    // at least, `1..8` between.
    if let Some(inner) = t.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or(SugarError::Malformed(s.to_string(), "unclosed `[`"))?;
        let (item, len) = match split_once_at_depth_zero_on(inner, ';') {
            Some((item, len)) => (item, Some(len)),
            None => (inner, None),
        };
        let mut obj = Map::new();
        obj.insert("k".into(), Json::String("list".into()));
        obj.insert("of".into(), parse_type_sugar(item)?);
        let list = Json::Object(obj);
        return match len {
            Some(len) => Ok(bounded(list, "len", parse_bounds(s, len)?)),
            None => Ok(list),
        };
    }

    // `i32[0..255]`, `bytes[16]`, `map<str, i64>[..10]`, and a string's two
    // lengths, each under its own word: `str[len 1..63, size ..253]`.
    if let Some((base, spec)) = split_bounds_suffix(t) {
        return bounds_on(s, parse_type_sugar(base)?, spec);
    }

    // `map<K, V>` and `future<T>`, which are what a type prints as. Reading
    // back what the system writes is worth having: a person who copies a type
    // out of `unitype ls` and pastes it into a universe should not be told it
    // is not a type.
    if let Some(inner) = angled(t, "map") {
        let (key, value) = split_once_at_depth_zero(inner).ok_or(SugarError::Malformed(
            s.to_string(),
            "`map` needs a key and a value",
        ))?;
        let mut obj = Map::new();
        obj.insert("k".into(), Json::String("map".into()));
        obj.insert("key".into(), parse_type_sugar(key)?);
        obj.insert("value".into(), parse_type_sugar(value)?);
        return Ok(Json::Object(obj));
    }
    if let Some(inner) = angled(t, "future") {
        if split_once_at_depth_zero(inner).is_some() {
            return Err(SugarError::Malformed(
                s.to_string(),
                "`future` takes one type",
            ));
        }
        let mut obj = Map::new();
        obj.insert("k".into(), Json::String("future".into()));
        obj.insert("of".into(), parse_type_sugar(inner)?);
        return Ok(Json::Object(obj));
    }

    if let Some((_, canonical)) = PRIMITIVES.iter().find(|(spelling, _)| *spelling == t) {
        let mut obj = Map::new();
        obj.insert("k".into(), Json::String((*canonical).into()));
        return Ok(Json::Object(obj));
    }

    let looks_like_a_name = t.split("::").all(|part| {
        !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    });
    if !looks_like_a_name {
        return Err(SugarError::NotAType(s.to_string()));
    }

    let mut obj = match name_to_ref_fields(t) {
        Json::Object(o) => o,
        _ => unreachable!("name_to_ref_fields always builds an object"),
    };
    obj.insert("k".into(), Json::String("ref".into()));
    Ok(Json::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Bounds, Type};

    fn sugar(s: &str) -> Type {
        let json = parse_type_sugar(s).expect("expands");
        serde_json::from_value(json).expect("reads as a type")
    }

    #[test]
    fn bounds_expand_and_read_back_the_way_they_print() {
        let octet = Type::I32.valued(Bounds::between(0, 255));
        assert_eq!(sugar("i32[0..255]"), octet);
        assert_eq!(
            sugar("[i32[0..255]; 4]"),
            octet.list().counted(Bounds::exactly(4))
        );
        assert_eq!(
            sugar("str[len 1..63, size ..253]"),
            Type::String
                .counted(Bounds::between(1, 63))
                .sized(Bounds::at_most(253))
        );
        for spelled in [
            "i32[0..255]",
            "i64[..100]",
            "i32[5..]",
            "i32[5]",
            "[i64; 4]",
            "[str; 1..8]",
            "[[i32; 4]; 2]",
            "bytes[16]",
            "map<str, i64>[..10]",
            "str[len 1..63]",
            "str[size ..253]",
            "str[len 1..63, size ..253]",
            "i32[0..255]?",
            "[i32[0..255]; 4]?",
        ] {
            assert_eq!(sugar(spelled).to_string(), spelled);
        }
    }

    #[test]
    fn bounds_that_make_no_sense_are_refused_with_a_reason() {
        for (spelled, why) in [
            ("str[1..3]", "two lengths"),
            ("Point[4]", "only an integer"),
            ("i32[..]", "bounds nothing"),
            ("i32[a..b]", "must be an integer"),
            ("i32[0..5][1]", "given once"),
            ("str[width 3]", "`len`, `size`, or `match`"),
        ] {
            let err = parse_type_sugar(spelled).expect_err(spelled).to_string();
            assert!(err.contains(why), "{spelled}: {err}");
        }
    }

    #[test]
    fn a_pattern_and_a_window_expand_and_read_back_the_way_they_print() {
        assert_eq!(
            sugar(r#"str[match "[a-z]+"]"#),
            Type::String.matching("[a-z]+")
        );
        assert_eq!(
            sugar(r#"str[len 1..63, match "a,b]c"]"#),
            Type::String
                .counted(Bounds::between(1, 63))
                .matching("a,b]c"),
            "a comma and a bracket inside the quotes do not end the pattern"
        );
        assert_eq!(
            sugar("future<str>[wait 100..5000]"),
            Type::String.future().waiting(Bounds::between(100, 5000))
        );
        for spelled in [
            r#"str[match "[a-z]+"]"#,
            r#"str[len 1..63, match "\\d{3}"]"#,
            "future<str>[wait 100..5000]",
            "future<str>[wait 100..]",
            "future<i64>[wait ..30000]",
        ] {
            assert_eq!(sugar(spelled).to_string(), spelled);
        }
    }

    #[test]
    fn a_pattern_or_a_window_on_the_wrong_kind_is_refused_as_malformed() {
        for (spelled, why) in [
            ("str[match [a-z]]", "double quotes"),
            ("future<str>[100..5000]", "window"),
            ("i32[wait 1..5]", "must be an integer"),
        ] {
            let err = parse_type_sugar(spelled).expect_err(spelled).to_string();
            assert!(err.contains(why), "{spelled}: {err}");
        }
    }

    #[test]
    fn a_window_can_be_said_in_words() {
        let soon = |w: Bounds| Type::String.future().waiting(w);
        assert_eq!(
            sugar("future<str>[wait not before 100]"),
            soon(Bounds::at_least(100))
        );
        assert_eq!(
            sugar("future<str>[wait up until 5000]"),
            soon(Bounds::at_most(5000))
        );
        assert_eq!(
            sugar("future<str>[wait not by 5000]"),
            soon(Bounds::at_most(4999)),
            "before 5000 ms is no later than 4999 whole milliseconds"
        );
        assert_eq!(
            sugar("future<str>[wait not before 100 and not by 5000]"),
            soon(Bounds::between(100, 4999))
        );
        assert_eq!(
            sugar("future<str>[wait up until 5000 and not before 100]"),
            sugar("future<str>[wait 100..5000]"),
            "the words and the range are the same window"
        );
    }

    #[test]
    fn a_window_in_words_refuses_repeats_and_nonsense() {
        for (spelled, why) in [
            (
                "future<str>[wait not before 1 and not before 2]",
                "one `not before`",
            ),
            (
                "future<str>[wait not by 5 and up until 6]",
                "one upper limit",
            ),
            ("future<str>[wait after 5]", "`not before N`"),
            ("future<str>[wait not before soon]", "whole number"),
        ] {
            let err = parse_type_sugar(spelled).expect_err(spelled).to_string();
            assert!(err.contains(why), "{spelled}: {err}");
        }
    }

    #[test]
    fn primitives_and_their_aliases_expand() {
        assert_eq!(sugar("i64"), Type::I64);
        assert_eq!(sugar("int"), Type::I64);
        assert_eq!(sugar("str"), Type::String);
        assert_eq!(sugar("float"), Type::F64);
        assert_eq!(sugar("bool"), Type::Bool);
    }

    #[test]
    fn a_type_reads_back_the_way_it_prints() {
        // The property worth having: whatever `Display` writes, this reads.
        for spelled in [
            "map<str, i64>",
            "future<bool>",
            "map<str, map<str, i64>>",
            "future<[str]>",
            "map<str, [i64]>?",
            "[map<str, str>]",
        ] {
            assert_eq!(sugar(spelled).to_string(), spelled, "{spelled}");
        }
    }

    #[test]
    fn a_map_needs_both_halves_and_a_future_needs_one() {
        assert!(parse_type_sugar("map<str>").is_err());
        assert!(parse_type_sugar("future<a, b>").is_err());
        // The comma that separates belongs to this map, not to the inner one.
        assert_eq!(
            sugar("map<str, map<i64, bool>>").to_string(),
            "map<str, map<i64, bool>>"
        );
    }

    #[test]
    fn names_become_references() {
        assert_eq!(sugar("User"), Type::local_ref("User"));
        let Type::Ref(r) = sugar("geo::Point") else {
            panic!("expected a reference");
        };
        assert_eq!(r.universe.unwrap().as_str(), "geo");
        assert_eq!(r.name.as_str(), "Point");
    }

    #[test]
    fn suffix_and_bracket_forms_nest() {
        assert_eq!(sugar("i64?"), Type::I64.optional());
        assert_eq!(sugar("[str]"), Type::String.list());
        assert_eq!(sugar("[User]?"), Type::local_ref("User").list().optional());
        assert_eq!(sugar("[i64?]"), Type::I64.optional().list());
    }

    #[test]
    fn malformed_shorthand_is_reported_not_guessed() {
        assert!(parse_type_sugar("[i64").is_err());
        assert!(parse_type_sugar("?").is_err());
        assert!(parse_type_sugar("").is_err());
        assert!(parse_type_sugar("a b").is_err());
    }

    #[test]
    fn expansion_reaches_nested_type_positions() {
        let mut json: Json =
            serde_json::from_str(r#"{"k":"map","key":"str","value":{"k":"list","of":"User"}}"#)
                .unwrap();
        desugar_type(&mut json).unwrap();
        let t: Type = serde_json::from_value(json).unwrap();
        assert_eq!(t, Type::local_ref("User").list().map_from(Type::String));
    }

    #[test]
    fn an_applied_parameter_reaches_the_shorthand_for_its_argument() {
        let mut json: Json =
            serde_json::from_str(r#"{"k":"apply","name":"M","args":["i64"]}"#).unwrap();
        desugar_type(&mut json).unwrap();
        let t: Type = serde_json::from_value(json).unwrap();
        assert_eq!(
            t,
            Type::Apply {
                name: crate::ident::Ident::new("M").unwrap(),
                args: vec![Type::I64],
            }
        );
    }

    #[test]
    fn a_literal_payload_is_left_alone_because_it_is_a_value() {
        // `value` means a type under `map` but a value under `lit`. Only the
        // former may be expanded, or `"i64"` the string would become a type.
        let mut json: Json = serde_json::from_str(r#"{"k":"lit","value":"i64"}"#).unwrap();
        desugar_expr(&mut json).expect("expands");
        assert_eq!(json["value"], Json::String("i64".into()));
    }
}
