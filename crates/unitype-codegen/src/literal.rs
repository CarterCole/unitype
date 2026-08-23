//! Writing a value out as a literal, in a language, against its type.
//!
//! Without a type, a record is only a bag of names and there is nothing better
//! to write than a map. With one, it is a `Circle`, and what should come out is
//! whatever that language says for constructing one.
//!
//! This is the half of "move a value between languages" that the reading side
//! already had: text goes in and becomes a value of a known type, and here that
//! value goes back out as something you can paste into a source file.

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{Type, TypeDef};
use unitype_core::lang::LanguageProfile;
use unitype_core::value::Value;

/// How one language writes the parts a value is made of.
///
/// Only the pieces that differ. What a float or a string looks like is already
/// each backend's untyped literal writer; what differs once a type is known is
/// how a struct is built and how a variant is named.
pub struct Constructors {
    /// Builds a named struct from `name` and its `field = value` parts.
    pub record: fn(&str, &[(String, String)]) -> String,
    /// Builds one alternative of a named enum.
    pub variant: fn(&str, &str, Option<&str>) -> String,
    /// Writes a value with no type to guide it.
    pub plain: fn(&Value) -> String,
}

/// Writes a value as a literal of `ty`.
pub fn typed(
    ws: &CheckedWorkspace,
    profile: &LanguageProfile,
    how: &Constructors,
    value: &Value,
    ty: &Type,
) -> String {
    let ty = ws.expand_alias(ty);

    match (&ty, value) {
        // Absence is whatever this language writes for nothing, which the
        // untyped writer already knows.
        (_, Value::Null) => (how.plain)(value),

        (Type::Optional { of }, present) => typed(ws, profile, how, present, of),

        (Type::List { of }, Value::List(items)) => {
            let parts: Vec<String> = items
                .iter()
                .map(|i| typed(ws, profile, how, i, of))
                .collect();
            wrap_list(how, &parts)
        }

        (Type::Tuple { items: types }, Value::Tuple(items)) if types.len() == items.len() => {
            let parts: Vec<String> = types
                .iter()
                .zip(items)
                .map(|(t, i)| typed(ws, profile, how, i, t))
                .collect();
            (how.plain)(&Value::Tuple(
                // Rebuilt so the untyped writer supplies this language's
                // bracketing, with the parts already written.
                parts.iter().map(|p| Value::Str(p.clone())).collect(),
            ))
            .replace('"', "")
        }

        // Whichever alternative it actually is decides how it is written, which
        // is what makes a discriminated union come out as the right thing.
        (Type::Union { members }, _) => {
            for member in members {
                if ws.validate(value, member).is_ok() {
                    return typed(ws, profile, how, value, member);
                }
            }
            (how.plain)(value)
        }

        (Type::Record { fields }, Value::Record(entries)) => {
            // An anonymous record has no name to construct, so it stays a map.
            let parts: Vec<String> = entries
                .iter()
                .map(|(name, entry)| {
                    let declared = fields.iter().find(|f| f.name.as_str() == name);
                    let written = match declared {
                        Some(field) => typed(ws, profile, how, entry, &field.ty),
                        None => (how.plain)(entry),
                    };
                    format!("{}: {written}", (how.plain)(&Value::Str(name.clone())))
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }

        (Type::Ref(r), _) => {
            let Some(def) = ws.ref_target_of(r).and_then(|q| ws.def(&q).cloned()) else {
                return (how.plain)(value);
            };
            match (&def, value) {
                (TypeDef::Struct(_) | TypeDef::Interface(_), Value::Record(entries)) => {
                    let declared = ws.fields_of(r).unwrap_or_default();
                    // Declaration order, not whatever order the value arrived
                    // in: generated code should look the way the declaration
                    // does.
                    let parts: Vec<(String, String)> = declared
                        .iter()
                        .filter_map(|field| {
                            let entry = entries.get(field.name.as_str())?;
                            // A field that is absent is left out, the same way
                            // it is left out of the wire form.
                            if matches!(entry, Value::Null) {
                                return None;
                            }
                            Some((
                                profile.name(&field.name),
                                typed(ws, profile, how, entry, &field.ty),
                            ))
                        })
                        .collect();
                    (how.record)(&profile.name(&r.name), &parts)
                }
                (
                    TypeDef::Enum(d),
                    Value::Variant {
                        tag,
                        value: payload,
                    },
                ) => {
                    let carried = d
                        .variants
                        .iter()
                        .find(|v| v.name.as_str() == tag)
                        .filter(|v| !matches!(v.payload, Type::Unit))
                        .map(|v| typed(ws, profile, how, payload, &v.payload));
                    (how.variant)(&profile.name(&r.name), tag, carried.as_deref())
                }
                _ => (how.plain)(value),
            }
        }

        _ => (how.plain)(value),
    }
}

/// This language's brackets for a list, borrowed from its untyped writer.
fn wrap_list(how: &Constructors, parts: &[String]) -> String {
    // Written as a list of placeholders and then filled in, so the brackets
    // come from the same place they would for any other list.
    let shape = (how.plain)(&Value::List(parts.iter().map(|_| Value::Null).collect()));
    let mut out = String::with_capacity(shape.len() + parts.iter().map(String::len).sum::<usize>());
    let mut parts = parts.iter();
    let mut rest = shape.as_str();
    // The placeholder is whatever this language writes for nothing.
    let nothing = (how.plain)(&Value::Null);
    while let Some(at) = rest.find(&nothing) {
        out.push_str(&rest[..at]);
        match parts.next() {
            Some(part) => out.push_str(part),
            None => out.push_str(&nothing),
        }
        rest = &rest[at + nothing.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::Workspace;
    use unitype_core::check::check_workspace;
    use unitype_core::ir::TypeRef;
    use unitype_core::lang::profile;
    use unitype_core::{Ident, QualifiedName};

    const WS: &str = r#"{"universes":[{"name":"demo","types":[
        {"k":"struct","name":"Shape","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Circle","extends":"Shape","fields":[
            {"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}},
            {"name":"radius","ty":"float"},
            {"name":"note","ty":"str?"}]},
        {"k":"enum","name":"Colour","variants":[
            {"name":"Red"},{"name":"Custom","payload":"str"}]},
        {"k":"struct","name":"Bag","fields":[{"name":"items","ty":"[Circle]"}]}]}]}"#;

    fn ws() -> CheckedWorkspace {
        check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
    }

    fn named(name: &str) -> Type {
        Type::Ref(TypeRef::qualified(
            Ident::new("demo").unwrap(),
            Ident::new(name).unwrap(),
        ))
    }

    fn write(lang: &str, json: &str, ty: &Type) -> String {
        let ws = ws();
        let value = ws.read_json(json, ty).expect("a real value");
        let profile = profile(lang).expect("ships");
        let how = crate::constructors(lang).expect("has constructors");
        typed(&ws, &profile, &how, &value, ty)
    }

    #[test]
    fn a_struct_becomes_the_way_that_language_builds_one() {
        let json = r#"{"id":"c-1","kind":"circle","radius":2.5}"#;
        assert_eq!(
            write("python", json, &named("Circle")),
            r#"Circle(id="c-1", kind="circle", radius=2.5)"#
        );
        assert_eq!(
            write("rust", json, &named("Circle")),
            r#"Circle { id: "c-1".to_string(), kind: "circle".to_string(), radius: 2.5 }"#
        );
        assert_eq!(
            write("typescript", json, &named("Circle")),
            r#"{ id: "c-1", kind: "circle", radius: 2.5 }"#
        );
    }

    #[test]
    fn fields_come_out_in_the_order_they_were_declared() {
        // The value arrived with its fields in a different order; generated
        // code should still look the way the declaration does.
        let scrambled = r#"{"radius":2.5,"kind":"circle","id":"c-1"}"#;
        assert_eq!(
            write("python", scrambled, &named("Circle")),
            r#"Circle(id="c-1", kind="circle", radius=2.5)"#
        );
    }

    #[test]
    fn a_field_that_is_absent_is_left_out_of_the_literal_too() {
        let with_null = r#"{"id":"c-1","kind":"circle","radius":2.5,"note":null}"#;
        let written = write("python", with_null, &named("Circle"));
        assert!(!written.contains("note"), "{written}");
    }

    #[test]
    fn a_nested_struct_is_built_rather_than_flattened_into_a_map() {
        let json = r#"{"items":[{"id":"c-1","kind":"circle","radius":1.0}]}"#;
        let written = write("rust", json, &named("Bag"));
        assert!(
            written.contains("Circle {"),
            "the element is a Circle, so it should be built as one: {written}"
        );
        assert!(written.starts_with("Bag {"), "{written}");
    }

    #[test]
    fn a_variant_is_named_the_way_that_language_names_one() {
        assert_eq!(write("rust", r#""Red""#, &named("Colour")), "Colour::Red");
        assert_eq!(
            write(
                "rust",
                r#"{"$tag":"Custom","value":"teal"}"#,
                &named("Colour")
            ),
            r#"Colour::Custom("teal".to_string())"#
        );
        assert_eq!(
            write("python", r#""Red""#, &named("Colour")),
            r#"{"$tag": "Red"}"#
        );
    }

    #[test]
    fn a_list_gets_that_language_brackets_with_the_parts_filled_in() {
        let json = r#"{"items":[{"id":"a","kind":"circle","radius":1.0},
                                {"id":"b","kind":"circle","radius":2.0}]}"#;
        let written = write("python", json, &named("Bag"));
        assert!(written.contains("items=[Circle("), "{written}");
        assert_eq!(written.matches("Circle(").count(), 2, "{written}");
    }

    #[test]
    fn without_a_type_to_go_on_a_record_stays_a_map() {
        let ws = ws();
        let value = ws
            .read_json(r#"{"whatever":1}"#, &Type::Any)
            .expect("reads");
        let profile = profile("python").expect("ships");
        let how = crate::constructors("python").expect("has constructors");
        assert_eq!(
            typed(&ws, &profile, &how, &value, &Type::Any),
            r#"{"whatever": 1}"#
        );
    }

    #[test]
    fn every_shipped_language_has_constructors() {
        for lang in unitype_core::LANGUAGES {
            assert!(crate::constructors(lang).is_some(), "{lang}");
        }
        let _ = QualifiedName::new(Ident::new("a").unwrap(), Ident::new("b").unwrap());
    }
}
