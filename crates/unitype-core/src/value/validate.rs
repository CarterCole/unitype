//! Checking that a value is what a type says it should be.
//!
//! This is where data from outside meets the type system: a JSON document
//! arrives, and validation decides whether it is a `demo::Circle`. It is also
//! what the runtime does at every dispatch boundary, in both directions — an
//! implementation that returns the wrong shape is caught rather than trusted.
//!
//! Errors carry a path in the familiar `$.orders[3].id` form, because being told
//! *that* a document is wrong is much less use than being told where.

use std::fmt;

use crate::check::CheckedWorkspace;
use crate::ir::{Field, LiteralValue, Type, TypeDef, fits_f64_exactly};
use crate::value::Value;

/// One step into a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueSeg {
    /// A named field.
    Field(String),
    /// A position in a list.
    Index(usize),
    /// A key in a map.
    Key(String),
    /// A position in a tuple.
    Item(usize),
    /// A variant's payload.
    Payload,
}

impl fmt::Display for ValueSeg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValueSeg::Field(n) => write!(f, ".{n}"),
            ValueSeg::Index(i) | ValueSeg::Item(i) => write!(f, "[{i}]"),
            ValueSeg::Key(k) => write!(f, "[{k:?}]"),
            ValueSeg::Payload => f.write_str(".value"),
        }
    }
}

/// Where in a value a problem is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValuePath(pub Vec<ValueSeg>);

impl ValuePath {
    /// The whole value.
    pub fn root() -> Self {
        Self::default()
    }

    /// This path extended by one step.
    pub fn then(&self, seg: ValueSeg) -> Self {
        let mut segs = self.0.clone();
        segs.push(seg);
        Self(segs)
    }
}

impl fmt::Display for ValuePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for seg in &self.0 {
            write!(f, "{seg}")?;
        }
        Ok(())
    }
}

/// Something in a value that the type does not allow.
#[derive(thiserror::Error, Debug, Clone, PartialEq)]
#[error("{path}: {detail}")]
pub struct ValueError {
    /// Where the problem is.
    pub path: ValuePath,
    /// What the type required there.
    pub expected: String,
    /// What was found instead.
    pub found: String,
    /// The problem, in words.
    pub detail: String,
}

impl ValueError {
    fn mismatch(path: &ValuePath, expected: &Type, found: &Value) -> Self {
        let expected = expected.to_string();
        let found_kind = found.kind_name();
        Self {
            path: path.clone(),
            detail: format!("expected {expected}, found {found_kind}"),
            expected,
            found: found_kind.to_string(),
        }
    }

    fn saying(path: &ValuePath, expected: &Type, found: &Value, detail: String) -> Self {
        Self {
            path: path.clone(),
            expected: expected.to_string(),
            found: found.kind_name().to_string(),
            detail,
        }
    }
}

impl CheckedWorkspace {
    /// Checks a value against a type, reporting everything that is wrong with
    /// it rather than only the first thing.
    pub fn validate(&self, value: &Value, ty: &Type) -> Result<(), Vec<ValueError>> {
        let mut errors = Vec::new();
        self.check_value(value, ty, &ValuePath::root(), &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn check_value(
        &self,
        value: &Value,
        ty: &Type,
        path: &ValuePath,
        errors: &mut Vec<ValueError>,
    ) {
        let ty = &self.expand_alias(ty);
        let mismatch = |errors: &mut Vec<ValueError>| {
            errors.push(ValueError::mismatch(path, ty, value));
        };

        match (ty, value) {
            (Type::Any, _) => {}
            (Type::Unit, Value::Null) => {}
            (Type::Bool, Value::Bool(_)) => {}
            (Type::I64, Value::Int(_)) => {}
            (Type::String, Value::Str(_)) => {}
            (Type::Bytes, Value::Bytes(_)) => {}

            (Type::I32, Value::Int(i)) => {
                if i32::try_from(*i).is_err() {
                    errors.push(ValueError::saying(
                        path,
                        ty,
                        value,
                        format!("{i} does not fit in a 32-bit integer"),
                    ));
                }
            }

            // A whole number is a perfectly good float — but only while it is
            // one. Past 2^53 an integer no longer survives the trip, and
            // accepting it would be quietly losing data.
            (Type::F64, Value::Float(_)) => {}
            (Type::F64, Value::Int(i)) => {
                if !fits_f64_exactly(*i) {
                    errors.push(ValueError::saying(
                        path,
                        ty,
                        value,
                        format!("{i} cannot be represented exactly as a float"),
                    ));
                }
            }

            (Type::Literal { value: want }, found) => {
                if !literal_matches(want, found) {
                    errors.push(ValueError::saying(
                        path,
                        ty,
                        value,
                        format!("expected exactly {}", describe_literal(want)),
                    ));
                }
            }

            (Type::Optional { of }, Value::Null) => {
                let _ = of;
            }
            (Type::Optional { of }, present) => self.check_value(present, of, path, errors),

            (Type::List { of }, Value::List(items)) => {
                for (i, item) in items.iter().enumerate() {
                    self.check_value(item, of, &path.then(ValueSeg::Index(i)), errors);
                }
            }

            (Type::Tuple { items: types }, Value::Tuple(items)) => {
                if types.len() != items.len() {
                    errors.push(ValueError::saying(
                        path,
                        ty,
                        value,
                        format!("expected {} element(s), found {}", types.len(), items.len()),
                    ));
                    return;
                }
                for (i, (want, item)) in types.iter().zip(items).enumerate() {
                    self.check_value(item, want, &path.then(ValueSeg::Item(i)), errors);
                }
            }

            (Type::Map { key, value: v }, Value::Map(entries)) => {
                for (k, entry) in entries {
                    let at = path.then(ValueSeg::Key(describe_key(k)));
                    self.check_value(k, key, &at, errors);
                    self.check_value(entry, v, &at, errors);
                }
            }
            // A JSON object is the natural form for a map with string keys.
            (Type::Map { key, value: v }, Value::Record(entries)) => {
                for (k, entry) in entries {
                    let at = path.then(ValueSeg::Key(k.clone()));
                    self.check_value(&Value::Str(k.clone()), key, &at, errors);
                    self.check_value(entry, v, &at, errors);
                }
            }

            (Type::Record { fields }, Value::Record(_)) => {
                self.check_fields(value, fields, path, errors)
            }

            (Type::Union { members }, _) => self.check_union(value, members, ty, path, errors),

            (Type::Ref(r), _) => {
                let Some(ix) = self.ref_target(r).and_then(|q| self.resolve(&q)) else {
                    errors.push(ValueError::saying(
                        path,
                        ty,
                        value,
                        "this type could not be resolved".into(),
                    ));
                    return;
                };
                match self.def_at(ix) {
                    TypeDef::Struct(_) | TypeDef::Interface(_) => {
                        let fields = self.fields_of(r).unwrap_or_default();
                        match value {
                            Value::Record(_) => self.check_fields(value, &fields, path, errors),
                            _ => mismatch(errors),
                        }
                    }
                    TypeDef::Enum(_) => match value {
                        Value::Variant {
                            tag,
                            value: payload,
                        } => match self
                            .variants_of(r)
                            .unwrap_or_default()
                            .iter()
                            .find(|v| v.name.as_str() == tag)
                        {
                            Some(variant) => self.check_value(
                                payload,
                                &variant.payload,
                                &path.then(ValueSeg::Payload),
                                errors,
                            ),
                            None => {
                                let variants = self.variants_of(r).unwrap_or_default();
                                let known: Vec<&str> =
                                    variants.iter().map(|v| v.name.as_str()).collect();
                                errors.push(ValueError::saying(
                                    path,
                                    ty,
                                    value,
                                    format!(
                                        "`{tag}` is not one of its variants ({})",
                                        known.join(", ")
                                    ),
                                ));
                            }
                        },
                        _ => mismatch(errors),
                    },
                    // Aliases were expanded before the match.
                    TypeDef::Alias(_) => mismatch(errors),
                }
            }

            (Type::Never, _) => errors.push(ValueError::saying(
                path,
                ty,
                value,
                "no value can go here".into(),
            )),

            (Type::Function { .. } | Type::Future { .. }, _) => errors.push(ValueError::saying(
                path,
                ty,
                value,
                "this is a runtime handle, not something data can hold".into(),
            )),

            (Type::Param { name }, _) => errors.push(ValueError::saying(
                path,
                ty,
                value,
                format!("`{name}` is still generic here; supply its type argument first"),
            )),

            _ => mismatch(errors),
        }
    }

    /// Checks a record's fields, allowing absent optional ones and ignoring
    /// extra ones.
    ///
    /// Extra fields are allowed on purpose: a producer that has added a field
    /// this reader does not know about is still sending something this reader
    /// can use, and refusing it would make every schema change a break.
    fn check_fields(
        &self,
        value: &Value,
        fields: &[Field],
        path: &ValuePath,
        errors: &mut Vec<ValueError>,
    ) {
        let Value::Record(present) = value else {
            return;
        };
        for field in fields {
            let at = path.then(ValueSeg::Field(field.name.to_string()));
            match present.get(field.name.as_str()) {
                Some(found) => self.check_value(found, &field.ty, &at, errors),
                // Missing and null are one state, so an optional field may
                // simply not be there.
                None => self.check_value(&Value::Null, &field.ty, &at, errors),
            }
        }
    }

    /// Tries each alternative, and on total failure reports the one that came
    /// closest rather than every failure at once.
    fn check_union(
        &self,
        value: &Value,
        members: &[Type],
        ty: &Type,
        path: &ValuePath,
        errors: &mut Vec<ValueError>,
    ) {
        let mut best: Option<Vec<ValueError>> = None;
        for member in members {
            let mut attempt = Vec::new();
            self.check_value(value, member, path, &mut attempt);
            if attempt.is_empty() {
                return;
            }
            if best.as_ref().is_none_or(|b| attempt.len() < b.len()) {
                best = Some(attempt);
            }
        }

        errors.push(ValueError::saying(
            path,
            ty,
            value,
            format!(
                "does not match any alternative of {}",
                describe_union(members)
            ),
        ));
        if let Some(closest) = best {
            errors.extend(closest);
        }
    }
}

fn literal_matches(want: &LiteralValue, found: &Value) -> bool {
    match (want, found) {
        (LiteralValue::Bool(a), Value::Bool(b)) => a == b,
        (LiteralValue::Int(a), Value::Int(b)) => a == b,
        (LiteralValue::Str(a), Value::Str(b)) => a == b,
        _ => false,
    }
}

fn describe_literal(v: &LiteralValue) -> String {
    match v {
        LiteralValue::Bool(b) => b.to_string(),
        LiteralValue::Int(i) => i.to_string(),
        LiteralValue::Str(s) => format!("{s:?}"),
    }
}

fn describe_union(members: &[Type]) -> String {
    let names: Vec<String> = members
        .iter()
        .map(|m| match m {
            Type::Ref(r) => r.name.to_string(),
            Type::Literal { value } => describe_literal(value),
            other => other.kind_name().to_string(),
        })
        .collect();
    names.join(" | ")
}

fn describe_key(k: &Value) -> String {
    match k {
        Value::Str(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        other => other.kind_name().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_workspace;
    use crate::ir::TypeRef;
    use crate::{Ident, Workspace};

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
        {"k":"enum","name":"Color","variants":[{"name":"Red"},{"name":"Custom","payload":"str"}]},
        {"k":"enum","name":"Maybe","params":[{"name":"T"}],"variants":[
            {"name":"nothing"},{"name":"just","payload":{"k":"param","name":"T"}}]},
        {"k":"struct","name":"Order","fields":[
            {"name":"items","ty":"[Circle]"}]}]}]}"#;

    fn ws() -> CheckedWorkspace {
        check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
    }

    fn named(name: &str) -> Type {
        Type::Ref(TypeRef::qualified(
            Ident::new("demo").unwrap(),
            Ident::new(name).unwrap(),
        ))
    }

    fn value(json: &str) -> Value {
        Value::parse_json(json).expect("reads")
    }

    fn errors(ws: &CheckedWorkspace, json: &str, ty: &Type) -> Vec<String> {
        ws.validate(&value(json), ty)
            .err()
            .map(|e| e.iter().map(ToString::to_string).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_generic_enum_checks_its_payload_at_the_argument_not_at_the_parameter() {
        let ws = ws();
        let maybe_int = Type::Ref(TypeRef {
            universe: Some(Ident::new("demo").unwrap()),
            name: Ident::new("Maybe").unwrap(),
            args: vec![Type::I64],
        });
        assert!(
            ws.validate(&Value::variant("just", Value::Int(1)), &maybe_int)
                .is_ok(),
            "a `Maybe<i64>` holding an integer is exactly what it says"
        );
        let wrong = ws
            .validate(
                &Value::variant("just", Value::Str("one".into())),
                &maybe_int,
            )
            .unwrap_err();
        assert!(
            wrong.iter().any(|e| e.to_string().contains("i64")),
            "and holding a string is refused by name: {wrong:?}"
        );
    }

    #[test]
    fn a_value_that_fits_its_type_validates() {
        let ws = ws();
        assert!(
            ws.validate(
                &value(r#"{"id":"a","kind":"circle","radius":2.0}"#),
                &named("Circle")
            )
            .is_ok()
        );
    }

    #[test]
    fn an_inherited_field_is_required_too() {
        let ws = ws();
        let msgs = errors(&ws, r#"{"kind":"circle","radius":2.0}"#, &named("Circle"));
        assert!(msgs.iter().any(|m| m.starts_with("$.id:")), "{msgs:?}");
    }

    #[test]
    fn a_problem_deep_in_a_document_says_exactly_where() {
        let ws = ws();
        let msgs = errors(
            &ws,
            r#"{"items":[{"id":"a","kind":"circle","radius":1.0},
                        {"id":"b","kind":"circle","radius":"wide"}]}"#,
            &named("Order"),
        );
        assert!(
            msgs.iter().any(|m| m.starts_with("$.items[1].radius:")),
            "{msgs:?}"
        );
    }

    #[test]
    fn everything_wrong_is_reported_not_just_the_first_thing() {
        let ws = ws();
        let msgs = errors(&ws, r#"{"kind":"square"}"#, &named("Circle"));
        assert!(msgs.len() >= 3, "expected id, kind, and radius: {msgs:?}");
    }

    #[test]
    fn an_absent_optional_field_is_fine_but_a_required_one_is_not() {
        let ws = ws();
        assert!(
            errors(
                &ws,
                r#"{"id":"a","kind":"circle","radius":1.0}"#,
                &named("Circle")
            )
            .is_empty()
        );
        assert!(
            !errors(&ws, r#"{"id":"a","kind":"circle"}"#, &named("Circle")).is_empty(),
            "radius is not optional"
        );
    }

    #[test]
    fn an_explicit_null_and_a_missing_field_are_the_same_state() {
        let ws = ws();
        let with_null = r#"{"id":"a","kind":"circle","radius":1.0,"note":null}"#;
        let without = r#"{"id":"a","kind":"circle","radius":1.0}"#;
        assert_eq!(
            errors(&ws, with_null, &named("Circle")),
            errors(&ws, without, &named("Circle"))
        );
    }

    #[test]
    fn a_field_this_reader_does_not_know_about_is_not_an_error() {
        let ws = ws();
        assert!(
            errors(
                &ws,
                r#"{"id":"a","kind":"circle","radius":1.0,"opacity":0.5}"#,
                &named("Circle")
            )
            .is_empty(),
            "a producer running ahead of this reader should still be readable"
        );
    }

    #[test]
    fn a_discriminated_union_picks_the_right_alternative() {
        let ws = ws();
        assert!(
            errors(
                &ws,
                r#"{"id":"a","kind":"rect","w":3.0}"#,
                &named("AnyShape")
            )
            .is_empty()
        );
        let msgs = errors(&ws, r#"{"id":"a","kind":"hexagon"}"#, &named("AnyShape"));
        assert!(
            msgs[0].contains("does not match any alternative"),
            "{msgs:?}"
        );
    }

    #[test]
    fn a_whole_number_is_accepted_as_a_float_while_it_still_is_one() {
        let ws = ws();
        assert!(ws.validate(&Value::Int(3), &Type::F64).is_ok());
        let too_big = Value::Int(9_007_199_254_740_993);
        let err = ws.validate(&too_big, &Type::F64).unwrap_err();
        assert!(
            err[0].detail.contains("cannot be represented exactly"),
            "{err:?}"
        );
    }

    #[test]
    fn an_integer_is_checked_against_the_width_it_was_given() {
        let ws = ws();
        assert!(ws.validate(&Value::Int(3), &Type::I32).is_ok());
        assert!(ws.validate(&Value::Int(i64::MAX), &Type::I32).is_err());
        assert!(ws.validate(&Value::Int(i64::MAX), &Type::I64).is_ok());
    }

    #[test]
    fn an_enum_value_must_name_a_variant_it_has() {
        let ws = ws();
        assert!(
            ws.validate(&Value::unit_variant("Red"), &named("Color"))
                .is_ok()
        );
        assert!(
            ws.validate(
                &Value::variant("Custom", Value::Str("teal".into())),
                &named("Color")
            )
            .is_ok()
        );
        let err = ws
            .validate(&Value::unit_variant("Mauve"), &named("Color"))
            .unwrap_err();
        assert!(err[0].detail.contains("not one of its variants"), "{err:?}");
    }

    #[test]
    fn an_object_may_stand_in_for_a_map_with_string_keys() {
        let ws = ws();
        let counts = Type::I64.map_from(Type::String);
        assert!(errors(&ws, r#"{"a":1,"b":2}"#, &counts).is_empty());
        assert!(!errors(&ws, r#"{"a":"one"}"#, &counts).is_empty());
    }

    #[test]
    fn nothing_at_all_fits_the_empty_type() {
        let ws = ws();
        for v in [Value::Null, Value::Int(1), Value::Str("x".into())] {
            assert!(ws.validate(&v, &Type::Never).is_err());
        }
    }

    #[test]
    fn anything_at_all_fits_the_unknown_type() {
        let ws = ws();
        for v in [Value::Null, Value::Int(1), Value::List(vec![])] {
            assert!(ws.validate(&v, &Type::Any).is_ok());
        }
    }
}
