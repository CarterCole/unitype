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
use crate::value::{Value, ValueError};

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

    /// Writes a value back out as JSON text.
    ///
    /// The value is checked against `ty` first, so nothing that does not belong
    /// leaves the system. Fields that are absent are left out rather than
    /// written as `null`: absence has one representation here, and this is it.
    pub fn write_json(&self, value: &Value, ty: &Type) -> Result<String, ReadError> {
        self.validate(value, ty).map_err(ReadError::Invalid)?;
        Ok(self.canonical(value, ty).to_json_string()?)
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
                let Some(TypeDef::Enum(d)) = self.ref_target(r).and_then(|q| self.def(&q)) else {
                    return value.clone();
                };
                match d.variants.iter().find(|v| v.name.as_str() == tag) {
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
                        .map(|(k, v)| {
                            Ok((
                                self.interpret(&Json::String(k.clone()), key)?,
                                self.interpret(v, value)?,
                            ))
                        })
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
                    TypeDef::Enum(d) => match json {
                        // A bare string names a variant that carries nothing,
                        // which is how enums are usually written by hand.
                        Json::String(tag)
                            if d.variants
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
                            let Some(variant) = d.variants.iter().find(|v| v.name.as_str() == tag)
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
        let text = ws.write_json(&plain, &counts).expect("writes");
        assert!(
            text.contains("$map"),
            "the escape is what gets written: {text}"
        );
        assert_eq!(
            ws.read_json(&text, &counts).expect("re-reads"),
            plain,
            "both spellings have to mean the same thing"
        );
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
