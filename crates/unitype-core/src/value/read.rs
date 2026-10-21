//! Reading text into the computable domain.
//!
//! A compiler, in the sense that matters here, is a function that takes a
//! literal and hands back something you can compute with. That is exactly what
//! this is: text goes in, a checked [`Value`] of a known type comes out.
//!
//! # Why knowing the type matters
//!
//! Read without a type, a JSON document can only become the obvious thing — an
//! object is a record, an array is a list, a string is a string. Read *against*
//! a type, the same document becomes what it was always meant to be. An object
//! becomes a map when the type says map. `"Red"` becomes a variant of an enum.
//! A base64 string becomes bytes. An array becomes a tuple. None of that is
//! guesswork; the type says which, and the text is read accordingly.
//!
//! So the type is not a check applied after the fact — it is what makes the
//! reading possible. Parsing and checking are one operation, and what comes out
//! the far end is known to be a value of the type asked for.
//!
//! # Text as a universe
//!
//! This gives one format, JSON, for free and for every type. A format that is
//! not JSON is expressed the same way any other crossing is: declare the text
//! form as a type, and link it to the domain type with a witness — a signature
//! someone implements. A parser is `fn(String) -> T`; a printer is
//! `fn(T) -> String`. Neither needs new machinery, because a compiler is a
//! proposition like any other, and an implementation of it is a proof.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use indexmap::IndexMap;
use serde_json::Value as Json;

use crate::check::CheckedWorkspace;
use crate::ir::{Type, TypeDef, fits_f64_exactly};
use crate::value::{Value, ValueDecodeError, ValueError};

/// Why text could not be read as a value of some type.
#[derive(thiserror::Error, Debug)]
pub enum ReadError {
    /// The text was not valid JSON.
    #[error("invalid JSON: {0}")]
    Syntax(#[from] serde_json::Error),
    /// The JSON was valid but could not be read as a value at all.
    #[error(transparent)]
    Malformed(#[from] crate::value::ValueDecodeError),
    /// The value was read, but it is not of the type asked for.
    #[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    Invalid(Vec<ValueError>),
}

impl ReadError {
    /// The type errors, if that is what went wrong.
    pub fn errors(&self) -> &[ValueError] {
        match self {
            ReadError::Invalid(errors) => errors,
            _ => &[],
        }
    }
}

impl CheckedWorkspace {
    /// Reads JSON text as a value of `ty`.
    ///
    /// The whole of it: parse, interpret against the type, and check. What
    /// comes back is known to satisfy `ty`.
    pub fn read_json(&self, text: &str, ty: &Type) -> Result<Value, ReadError> {
        let json: Json = serde_json::from_str(text)?;
        self.read(&json, ty)
    }

    /// Reads already-parsed JSON as a value of `ty`.
    pub fn read(&self, json: &Json, ty: &Type) -> Result<Value, ReadError> {
        let value = self.interpret(json, ty)?;
        match self.validate(&value, ty) {
            Ok(()) => Ok(value),
            Err(errors) => Err(ReadError::Invalid(errors)),
        }
    }

    /// Writes a value back out as JSON text, in the typed form of
    /// [`typed_json`](Self::typed_json).
    ///
    /// The value is checked against `ty` first, so nothing that does not belong
    /// leaves the system. Fields that are absent are left out rather than
    /// written as `null`: absence has one representation here, and this is it.
    pub fn write_json(&self, value: &Value, ty: &Type) -> Result<String, ReadError> {
        self.validate(value, ty).map_err(ReadError::Invalid)?;
        Ok(self.typed_json(value, ty)?.to_string())
    }

    /// A value as JSON written against its type: the **typed form**.
    ///
    /// The untyped encoding has to say what a tuple, a map and a byte string
    /// are, because nothing else will. Written against a type, the type already
    /// says, so each is written the way every language's own JSON writes it — a
    /// tuple as an array, a map as an object whose property names are its keys
    /// as text, bytes as base64 text — which is what generated code reads and writes, and what
    /// [`read_json`](Self::read_json) reads back. Where the type does not say —
    /// under `any`, or in a union where the plain shape would read back as one
    /// of the other alternatives — the escape is written, as it would be with no
    /// type at all.
    ///
    /// Absent fields are left out, as by [`canonical`](Self::canonical). The
    /// value is not checked against `ty`; [`write_json`](Self::write_json) is
    /// what checks first.
    pub fn typed_json(&self, value: &Value, ty: &Type) -> Result<Json, ValueDecodeError> {
        self.typed(&self.canonical(value, ty), ty)
    }

    fn typed(&self, value: &Value, ty: &Type) -> Result<Json, ValueDecodeError> {
        use crate::value::json::{KEY_MAP, KEY_TAG, KEY_VALUE};
        let ty = self.expand_alias(ty);

        Ok(match (&ty, value) {
            (Type::Optional { of }, present) if !matches!(present, Value::Null) => {
                self.typed(present, of)?
            }
            (Type::List { of }, Value::List(items)) => Json::Array(
                items
                    .iter()
                    .map(|i| self.typed(i, of))
                    .collect::<Result<_, _>>()?,
            ),
            (Type::Tuple { items: types }, Value::Tuple(items)) if types.len() == items.len() => {
                Json::Array(
                    types
                        .iter()
                        .zip(items)
                        .map(|(t, i)| self.typed(i, t))
                        .collect::<Result<_, _>>()?,
                )
            }
            // An object has text for keys, which holds a string as it is and an
            // integer as its digits. Keys that would not read back as
            // themselves keep the escape, which can hold any key at all.
            (Type::Map { key, value: of }, Value::Map(entries)) => {
                match self.property_names(entries, key) {
                    Some(names) => {
                        let mut obj = serde_json::Map::with_capacity(entries.len());
                        for (name, (_, v)) in names.into_iter().zip(entries) {
                            obj.insert(name, self.typed(v, of)?);
                        }
                        Json::Object(obj)
                    }
                    None => {
                        let pairs = entries
                            .iter()
                            .map(|(k, v)| {
                                Ok(Json::Array(vec![self.typed(k, key)?, self.typed(v, of)?]))
                            })
                            .collect::<Result<Vec<_>, ValueDecodeError>>()?;
                        Json::Object(serde_json::Map::from_iter([(
                            KEY_MAP.to_string(),
                            Json::Array(pairs),
                        )]))
                    }
                }
            }
            (Type::Bytes, Value::Bytes(bytes)) => Json::String(BASE64.encode(bytes)),
            (Type::Record { fields }, Value::Record(entries)) => {
                let mut obj = serde_json::Map::with_capacity(entries.len());
                for (name, entry) in entries {
                    let written = match fields.iter().find(|f| f.name.as_str() == name) {
                        Some(field) => self.typed(entry, &field.ty)?,
                        // A field this type does not know about has no type to
                        // be written against.
                        None => entry.to_json()?,
                    };
                    obj.insert(name.clone(), written);
                }
                Json::Object(obj)
            }
            (Type::Ref(r), Value::Record(_)) => match self.fields_of(r) {
                Some(fields) => self.typed(value, &Type::Record { fields })?,
                None => value.to_json()?,
            },
            (
                Type::Ref(r),
                Value::Variant {
                    tag,
                    value: payload,
                },
            ) => {
                let declared = self
                    .variants_of(r)
                    .and_then(|vs| vs.into_iter().find(|v| v.name.as_str() == tag));
                let Some(variant) = declared else {
                    return value.to_json();
                };
                let mut obj = serde_json::Map::new();
                obj.insert(KEY_TAG.to_string(), Json::String(tag.clone()));
                if !matches!(**payload, Value::Null) {
                    obj.insert(
                        KEY_VALUE.to_string(),
                        self.typed(payload, &variant.payload)?,
                    );
                }
                Json::Object(obj)
            }
            // The plain shape can mean more than one thing in a union — `[1, 2]`
            // is a pair as well as a list — so it is written only where reading
            // it back against the whole union gives this value again.
            (Type::Union { members }, _) => {
                let Some(member) = members.iter().find(|m| self.validate(value, m).is_ok()) else {
                    return value.to_json();
                };
                let plain = self.typed(value, member)?;
                if self.interpret(&plain, &ty).is_ok_and(|back| &back == value) {
                    plain
                } else {
                    value.to_json()?
                }
            }
            _ => value.to_json()?,
        })
    }

    /// A value with nothing absent spelled out.
    ///
    /// A field that is not there and a field explicitly set to null are the
    /// same state, so a canonical value carries only one of them — the absence.
    /// Without this, unitype would emit `"note": null` where every generated
    /// language omits the field, and two systems holding the same value would
    /// disagree about its bytes.
    pub fn canonical(&self, value: &Value, ty: &Type) -> Value {
        let ty = self.expand_alias(ty);

        match (&ty, value) {
            (Type::Optional { of }, present) if !matches!(present, Value::Null) => {
                self.canonical(present, of)
            }
            (Type::List { of }, Value::List(items)) => {
                Value::List(items.iter().map(|i| self.canonical(i, of)).collect())
            }
            (Type::Map { value: v, .. }, Value::Map(entries)) => Value::Map(
                entries
                    .iter()
                    .map(|(k, e)| (k.clone(), self.canonical(e, v)))
                    .collect(),
            ),
            (Type::Tuple { items: types }, Value::Tuple(items)) if types.len() == items.len() => {
                Value::Tuple(
                    types
                        .iter()
                        .zip(items)
                        .map(|(t, i)| self.canonical(i, t))
                        .collect(),
                )
            }
            (Type::Record { fields }, Value::Record(entries)) => {
                let mut out = IndexMap::with_capacity(entries.len());
                for (name, entry) in entries {
                    let declared = fields.iter().find(|f| f.name.as_str() == name);
                    match declared {
                        // Absent, and allowed to be: leave it out.
                        Some(field)
                            if matches!(entry, Value::Null) && self.admits_absence(&field.ty) =>
                        {
                            continue;
                        }
                        Some(field) => {
                            out.insert(name.clone(), self.canonical(entry, &field.ty));
                        }
                        // A field this type does not declare is kept as it is,
                        // so that it survives a round trip.
                        None => {
                            out.insert(name.clone(), entry.clone());
                        }
                    }
                }
                Value::Record(out)
            }
            (Type::Ref(r), Value::Record(_)) => match self.fields_of(r) {
                Some(fields) => self.canonical(value, &Type::Record { fields }),
                None => value.clone(),
            },
            (
                Type::Ref(r),
                Value::Variant {
                    tag,
                    value: payload,
                },
            ) => {
                let Some(variants) = self.variants_of(r) else {
                    return value.clone();
                };
                match variants.iter().find(|v| v.name.as_str() == tag) {
                    Some(variant) => Value::variant(
                        tag.clone(),
                        self.canonical(payload, &variant.payload.clone()),
                    ),
                    None => value.clone(),
                }
            }
            (Type::Union { members }, _) => {
                // Whichever alternative this value actually is decides how it
                // gets tidied.
                for member in members {
                    if self.validate(value, member).is_ok() {
                        return self.canonical(value, member);
                    }
                }
                value.clone()
            }
            _ => value.clone(),
        }
    }

    /// The key an object's property name stands for, read against the key type.
    ///
    /// A property name is always text, so a map keyed by integers writes `1`
    /// as `"1"`. Whichever of the text itself and the number it spells is a
    /// key of this type is what the name means, text first; and where neither
    /// is, the text is kept so that the check that follows can say what was
    /// wrong with it.
    fn map_key(&self, name: &str, key: &Type) -> Value {
        let text = Value::Str(name.to_string());
        let number = name
            .parse::<i64>()
            .ok()
            .filter(|n| n.to_string() == name)
            .map(Value::Int);
        std::iter::once(text.clone())
            .chain(number)
            .find(|candidate| self.validate(candidate, key).is_ok())
            .unwrap_or(text)
    }

    /// The property names a map's keys are written as, if an object can hold
    /// it: every key a string or an integer, no two with the same text, and
    /// each reading back as the key it came from. Mixed keys can fail that
    /// last test — in `str | i64`, `"1"` and `1` are both written `1`.
    fn property_names(&self, entries: &[(Value, Value)], key: &Type) -> Option<Vec<String>> {
        let mut seen = std::collections::HashSet::new();
        entries
            .iter()
            .map(|(k, _)| {
                let name = match k {
                    Value::Str(s) => s.clone(),
                    Value::Int(i) => i.to_string(),
                    _ => return None,
                };
                (self.map_key(&name, key) == *k && seen.insert(name.clone())).then_some(name)
            })
            .collect()
    }

    /// Whether a field of this type may simply not be there.
    fn admits_absence(&self, ty: &Type) -> bool {
        matches!(
            self.expand_alias(ty),
            Type::Optional { .. } | Type::Unit | Type::Any
        )
    }

    /// Interprets JSON as the type says to, without checking the result yet.
    fn interpret(&self, json: &Json, ty: &Type) -> Result<Value, crate::value::ValueDecodeError> {
        let ty = self.expand_alias(ty);

        match (&ty, json) {
            // Nothing to interpret against.
            (Type::Any, _) => Value::from_json(json),

            (Type::Optional { .. }, Json::Null) => Ok(Value::Null),
            (Type::Optional { of }, present) => self.interpret(present, of),

            // A whole number where a float belongs really is that float.
            (Type::F64, Json::Number(n)) => match n.as_i64() {
                Some(i) if fits_f64_exactly(i) => Ok(Value::Float(i as f64)),
                _ => Value::from_json(json),
            },

            // Bytes travel as base64. The escape form still works; this just
            // means a plain string is understood too.
            (Type::Bytes, Json::String(s)) => {
                Ok(Value::Bytes(BASE64.decode(s).map_err(|e| {
                    crate::value::ValueDecodeError::BadBase64(e.to_string())
                })?))
            }

            (Type::List { of }, Json::Array(items)) => Ok(Value::List(
                items
                    .iter()
                    .map(|i| self.interpret(i, of))
                    .collect::<Result<_, _>>()?,
            )),

            // An array of the right length is a tuple when the type says so.
            (Type::Tuple { items: types }, Json::Array(items)) if types.len() == items.len() => {
                Ok(Value::Tuple(
                    types
                        .iter()
                        .zip(items)
                        .map(|(t, i)| self.interpret(i, t))
                        .collect::<Result<_, _>>()?,
                ))
            }

            // A tuple written in its explicit form still reads against the
            // type, so the elements are interpreted rather than guessed at.
            (Type::Tuple { items: types }, Json::Object(obj))
                if obj.contains_key(crate::value::json::KEY_TUPLE) =>
            {
                match obj.get(crate::value::json::KEY_TUPLE) {
                    Some(Json::Array(items)) if items.len() == types.len() => Ok(Value::Tuple(
                        types
                            .iter()
                            .zip(items)
                            .map(|(t, i)| self.interpret(i, t))
                            .collect::<Result<_, _>>()?,
                    )),
                    _ => Value::from_json(json),
                }
            }

            // An object is a map when the type says map, and a record when it
            // says record. Untyped reading could never tell these apart.
            //
            // Both spellings are accepted: the explicit escape, which is what
            // gets written out, and a plain object, which is what a person
            // writes when the keys are strings.
            (Type::Map { key, value }, Json::Object(entries)) => {
                if let Some(escaped) = entries.get(crate::value::json::KEY_MAP) {
                    let Some(pairs) = escaped.as_array() else {
                        return Value::from_json(json);
                    };
                    let mut out = Vec::with_capacity(pairs.len());
                    for pair in pairs {
                        let Some([k, v]) = pair.as_array().map(Vec::as_slice) else {
                            return Value::from_json(json);
                        };
                        out.push((self.interpret(k, key)?, self.interpret(v, value)?));
                    }
                    return Ok(Value::Map(out));
                }
                Ok(Value::Map(
                    entries
                        .iter()
                        .map(|(k, v)| Ok((self.map_key(k, key), self.interpret(v, value)?)))
                        .collect::<Result<Vec<_>, crate::value::ValueDecodeError>>()?,
                ))
            }

            (Type::Record { fields }, Json::Object(entries)) => {
                let mut out = IndexMap::with_capacity(entries.len());
                for (name, raw) in entries {
                    let value = match fields.iter().find(|f| f.name.as_str() == name) {
                        Some(field) => self.interpret(raw, &field.ty)?,
                        // A field this type does not know about is kept as it
                        // reads, so that it survives a round trip.
                        None => Value::from_json(raw)?,
                    };
                    out.insert(name.clone(), value);
                }
                Ok(Value::Record(out))
            }

            // Try each alternative and take the first that actually fits. The
            // discriminating literal in a tagged union is what makes this
            // decisive rather than a guess.
            (Type::Union { members }, _) => {
                for member in members {
                    if let Ok(value) = self.interpret(json, member) {
                        if self.validate(&value, member).is_ok() {
                            return Ok(value);
                        }
                    }
                }
                Value::from_json(json)
            }

            (Type::Ref(r), _) => {
                let Some(def) = self.ref_target(r).and_then(|q| self.def(&q).cloned()) else {
                    return Value::from_json(json);
                };
                match def {
                    TypeDef::Struct(_) | TypeDef::Interface(_) => {
                        let fields = self.fields_of(r).unwrap_or_default();
                        self.interpret(json, &Type::Record { fields })
                    }
                    TypeDef::Enum(_) => match json {
                        // A bare string names a variant that carries nothing,
                        // which is how enums are usually written by hand.
                        Json::String(tag)
                            if self
                                .variants_of(r)
                                .unwrap_or_default()
                                .iter()
                                .any(|v| v.name.as_str() == tag && v.payload == Type::Unit) =>
                        {
                            Ok(Value::unit_variant(tag.clone()))
                        }
                        Json::Object(obj) => {
                            let Some(Json::String(tag)) = obj.get(crate::value::json::KEY_TAG)
                            else {
                                return Value::from_json(json);
                            };
                            let variants = self.variants_of(r).unwrap_or_default();
                            let Some(variant) = variants.iter().find(|v| v.name.as_str() == tag)
                            else {
                                return Value::from_json(json);
                            };
                            let payload = match obj.get(crate::value::json::KEY_VALUE) {
                                Some(raw) => self.interpret(raw, &variant.payload)?,
                                None => Value::Null,
                            };
                            Ok(Value::variant(tag.clone(), payload))
                        }
                        _ => Value::from_json(json),
                    },
                    // Expanded before the match.
                    TypeDef::Alias(_) => Value::from_json(json),
                }
            }

            _ => Value::from_json(json),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Workspace;
    use crate::check::check_workspace;
    use crate::ir::TypeRef;
    use crate::{Ident, QualifiedName};

    const WS: &str = r#"{"universes":[{"name":"demo","types":[
        {"k":"struct","name":"Shape","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Circle","extends":"Shape","fields":[
            {"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}},
            {"name":"radius","ty":"float"},
            {"name":"note","ty":"str?"}]},
        {"k":"struct","name":"Rect","extends":"Shape","fields":[
            {"name":"kind","ty":{"k":"literal","value":{"str":"rect"}}},
            {"name":"w","ty":"float"}]},
        {"k":"alias","name":"AnyShape","target":{"k":"union","members":["Circle","Rect"]}},
        {"k":"enum","name":"Colour","variants":[
            {"name":"Red"},{"name":"Custom","payload":"str"}]},
        {"k":"struct","name":"Doc","fields":[
            {"name":"tags","ty":{"k":"map","key":"str","value":"i64"}},
            {"name":"blob","ty":"bytes"},
            {"name":"span","ty":{"k":"tuple","items":["i64","i64"]}},
            {"name":"colour","ty":"Colour"}]}]}]}"#;

    fn ws() -> CheckedWorkspace {
        check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
    }

    fn named(name: &str) -> Type {
        Type::Ref(TypeRef::qualified(
            Ident::new("demo").unwrap(),
            Ident::new(name).unwrap(),
        ))
    }

    #[test]
    fn text_becomes_a_value_of_the_type_it_was_read_as() {
        let ws = ws();
        let circle = ws
            .read_json(
                r#"{"id":"a","kind":"circle","radius":2.5}"#,
                &named("Circle"),
            )
            .expect("reads");
        assert_eq!(circle.get("radius"), Some(&Value::Float(2.5)), "{circle:?}");
    }

    #[test]
    fn text_that_is_not_of_the_type_is_refused_with_a_path() {
        let ws = ws();
        let err = ws
            .read_json(
                r#"{"id":"a","kind":"circle","radius":"wide"}"#,
                &named("Circle"),
            )
            .unwrap_err();
        assert!(
            err.errors()
                .iter()
                .any(|e| e.path.to_string() == "$.radius"),
            "{err}"
        );
    }

    #[test]
    fn malformed_text_is_refused_before_anything_else_happens() {
        let ws = ws();
        assert!(matches!(
            ws.read_json("{not json", &named("Circle")),
            Err(ReadError::Syntax(_))
        ));
    }

    #[test]
    fn an_object_becomes_a_map_when_the_type_says_map() {
        let ws = ws();
        let doc = ws
            .read_json(
                r#"{"tags":{"a":1,"b":2},"blob":"AQI=","span":[1,2],"colour":"Red"}"#,
                &named("Doc"),
            )
            .expect("reads");

        assert!(
            matches!(doc.get("tags"), Some(Value::Map(_))),
            "untyped reading could only ever have made this a record: {doc:?}"
        );
    }

    #[test]
    fn the_type_decides_what_each_piece_of_the_document_becomes() {
        let ws = ws();
        let doc = ws
            .read_json(
                r#"{"tags":{},"blob":"AQI=","span":[1,2],"colour":"Red"}"#,
                &named("Doc"),
            )
            .expect("reads");

        assert_eq!(doc.get("blob"), Some(&Value::Bytes(vec![1, 2])));
        assert!(matches!(doc.get("span"), Some(Value::Tuple(_))));
        assert_eq!(doc.get("colour"), Some(&Value::unit_variant("Red")));
    }

    #[test]
    fn a_variant_that_carries_something_is_read_with_its_payload() {
        let ws = ws();
        let colour = ws
            .read_json(r#"{"$tag":"Custom","value":"teal"}"#, &named("Colour"))
            .expect("reads");
        assert_eq!(colour, Value::variant("Custom", Value::Str("teal".into())));
    }

    #[test]
    fn a_tagged_union_picks_its_alternative_by_the_discriminator() {
        let ws = ws();
        let rect = ws
            .read_json(r#"{"id":"a","kind":"rect","w":3.0}"#, &named("AnyShape"))
            .expect("reads");
        assert_eq!(rect.get("w"), Some(&Value::Float(3.0)));

        assert!(
            ws.read_json(r#"{"id":"a","kind":"hexagon"}"#, &named("AnyShape"))
                .is_err()
        );
    }

    #[test]
    fn a_whole_number_where_a_float_belongs_is_read_as_that_float() {
        let ws = ws();
        let circle = ws
            .read_json(r#"{"id":"a","kind":"circle","radius":2}"#, &named("Circle"))
            .expect("reads");
        assert_eq!(
            circle.get("radius"),
            Some(&Value::Float(2.0)),
            "the type says this is a float, so it is one"
        );
    }

    #[test]
    fn a_field_this_type_does_not_know_about_survives_the_trip() {
        let ws = ws();
        let circle = ws
            .read_json(
                r#"{"id":"a","kind":"circle","radius":1.0,"opacity":0.5}"#,
                &named("Circle"),
            )
            .expect("reads");
        assert_eq!(circle.get("opacity"), Some(&Value::Float(0.5)));
    }

    #[test]
    fn what_was_read_writes_back_out_and_reads_the_same_again() {
        let ws = ws();
        let ty = named("Doc");
        let source = r#"{"tags":{"a":1},"blob":"AQI=","span":[1,2],"colour":"Red"}"#;

        let once = ws.read_json(source, &ty).expect("reads");
        let text = ws.write_json(&once, &ty).expect("writes");
        let twice = ws.read_json(&text, &ty).expect("re-reads");

        assert_eq!(once, twice, "a round trip through text changed the value");
    }

    #[test]
    fn a_map_written_in_its_explicit_form_reads_back_as_a_map() {
        let ws = ws();
        let counts = Type::I64.map_from(Type::String);
        let plain = ws.read_json(r#"{"a":1}"#, &counts).expect("reads");
        let escaped = ws
            .read_json(r#"{"$map":[["a",1]]}"#, &counts)
            .expect("reads the escape too");
        assert_eq!(plain, escaped, "both spellings have to mean the same thing");
    }

    #[test]
    fn written_against_its_type_a_value_takes_the_shape_every_language_gives_it() {
        let ws = ws();
        let doc = ws
            .read_json(
                r#"{"tags":{"$map":[["a",1]]},"blob":{"$bytes":"AQI="},
                    "span":{"$tuple":[1,2]},"colour":"Red"}"#,
                &named("Doc"),
            )
            .expect("reads the escapes");
        let written: serde_json::Value =
            serde_json::from_str(&ws.write_json(&doc, &named("Doc")).expect("writes")).unwrap();
        assert_eq!(
            written,
            serde_json::json!({"tags":{"a":1},"blob":"AQI=","span":[1,2],"colour":{"$tag":"Red"}}),
            "a map with string keys is an object, bytes are base64 text, and a tuple is an array"
        );
    }

    #[test]
    fn a_map_keyed_by_integers_is_an_object_whose_names_are_their_digits() {
        let ws = ws();
        let by_number = Type::String.map_from(Type::I64);
        // In the order an object hands its properties back, which is by name:
        // a map's entries are a list, and `==` on one is order-sensitive.
        let value = ws
            .read_json(r#"{"$map":[[-20,"minus"],[1,"one"]]}"#, &by_number)
            .expect("reads the escape");
        let text = ws.write_json(&value, &by_number).expect("writes");
        assert_eq!(text, r#"{"-20":"minus","1":"one"}"#);
        assert_eq!(
            ws.read_json(&text, &by_number).expect("re-reads"),
            value,
            "the digits are read back as the integers they spell"
        );
    }

    #[test]
    fn an_object_with_a_name_that_is_not_an_integer_is_not_a_map_of_integers() {
        let ws = ws();
        let by_number = Type::String.map_from(Type::I64);
        for name in ["one", "01", "1.0", "+1", " 1"] {
            let err = ws
                .read_json(&format!(r#"{{"{name}":"x"}}"#), &by_number)
                .expect_err(name);
            assert!(
                err.to_string().contains("expected i64"),
                "`{name}` is not how an integer is written: {err}"
            );
        }
    }

    #[test]
    fn keys_that_would_not_read_back_as_themselves_keep_the_escape() {
        let ws = ws();
        // `"1"` and `1` are both written `1`, so an object could hold only one
        // of them and could not say which it was.
        let either = Type::String.map_from(Type::Union {
            members: vec![Type::String, Type::I64],
        });
        let both = Value::Map(vec![
            (Value::Str("1".into()), Value::Str("text".into())),
            (Value::Int(1), Value::Str("number".into())),
        ]);
        let text = ws.write_json(&both, &either).expect("writes");
        assert_eq!(text, r#"{"$map":[["1","text"],[1,"number"]]}"#);
        assert_eq!(ws.read_json(&text, &either).expect("re-reads"), both);
    }

    #[test]
    fn without_a_type_to_say_so_a_tuple_keeps_the_escape() {
        let ws = ws();
        let pair = Value::Tuple(vec![Value::Int(1), Value::Str("a".into())]);
        assert_eq!(
            ws.write_json(&pair, &Type::Any).expect("writes"),
            r#"{"$tuple":[1,"a"]}"#
        );
    }

    #[test]
    fn in_a_union_the_plain_shape_is_written_only_where_it_reads_back_as_itself() {
        let ws = ws();
        let pair = Type::Tuple {
            items: vec![Type::I64, Type::I64],
        };
        // A list comes first, so `[1, 2]` would read back as a list.
        let either = Type::Union {
            members: vec![Type::I64.list(), pair.clone()],
        };
        let tuple = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
        let text = ws.write_json(&tuple, &either).expect("writes");
        assert_eq!(text, r#"{"$tuple":[1,2]}"#);
        assert_eq!(ws.read_json(&text, &either).expect("re-reads"), tuple);

        // Where nothing else could claim it, the array is enough.
        let only_pairs = Type::Union {
            members: vec![Type::String, pair],
        };
        assert_eq!(ws.write_json(&tuple, &only_pairs).expect("writes"), "[1,2]");
    }

    #[test]
    fn a_field_that_is_absent_is_left_out_rather_than_written_as_null() {
        let ws = ws();
        let circle = ws
            .read_json(
                r#"{"id":"a","kind":"circle","radius":1.0,"note":null}"#,
                &named("Circle"),
            )
            .expect("reads");
        let text = ws.write_json(&circle, &named("Circle")).expect("writes");
        assert!(
            !text.contains("note"),
            "every generated language omits it, and so must this: {text}"
        );
        assert!(
            ws.read_json(&text, &named("Circle")).is_ok(),
            "and what is written still reads back: {text}"
        );
    }

    #[test]
    fn tidying_reaches_inside_lists_and_records() {
        let ws = ws();
        let ty = named("Circle").list();
        let circles = ws
            .read_json(
                r#"[{"id":"a","kind":"circle","radius":1.0,"note":null},
                    {"id":"b","kind":"circle","radius":2.0}]"#,
                &ty,
            )
            .expect("reads");
        let text = ws.write_json(&circles, &ty).expect("writes");
        assert!(!text.contains("note"), "{text}");
    }

    #[test]
    fn a_field_the_type_does_not_declare_survives_being_written_out() {
        let ws = ws();
        // Tidying only removes absence the type knows about. A field this
        // reader has never heard of is passed along untouched, so a producer
        // running ahead of it does not lose data on the way through.
        let doc = ws
            .read_json(
                r#"{"tags":{},"blob":"AQI=","span":[1,2],"colour":"Red","extra":7}"#,
                &named("Doc"),
            )
            .expect("reads");
        let text = ws.write_json(&doc, &named("Doc")).expect("writes");
        assert!(text.contains("extra"), "{text}");
    }

    #[test]
    fn a_value_that_does_not_belong_is_not_written_out() {
        let ws = ws();
        assert!(
            ws.write_json(&Value::Int(1), &named("Circle")).is_err(),
            "nothing leaves the system without being what it claims to be"
        );
    }

    #[test]
    fn a_parser_for_another_format_is_an_ordinary_witnessed_link() {
        // JSON comes for free, for every type. Anything else is stated the same
        // way any crossing is: text is a type, and a parser is the function
        // that witnesses the link into the domain.
        let ws = check_workspace(
            Workspace::from_json(
                r#"{"universes":[
                    {"name":"text","types":[{"k":"alias","name":"Csv","target":"str"}]},
                    {"name":"demo","types":[{"k":"struct","name":"Row","fields":[
                        {"name":"a","ty":"str"}]}],
                     "functions":[{"name":"parse_row",
                        "params":[{"name":"src","ty":"text::Csv"}],"ret":"Row"}]}],
                  "links":[{"name":"csv_to_row","from":"text::Csv","to":"demo::Row",
                            "kind":{"k":"witness","sig":"demo::parse_row"}}]}"#,
            )
            .expect("reads"),
        )
        .expect("checks");

        let path = ws
            .bridge_path(
                &"text::Csv".parse::<QualifiedName>().unwrap(),
                &"demo::Row".parse().unwrap(),
            )
            .expect("a parser is a way across");
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].name.as_str(), "csv_to_row");
    }
}
