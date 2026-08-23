//! Reading a type off the data.
//!
//! Point this at a JSON document and it tells you what type that document is.
//! Point it at a hundred documents and it tells you what type they all are —
//! which is the useful case, because that is when optional fields, tag sets,
//! and mixed element types become visible.
//!
//! Inference is context-free and deterministic: the same samples always give
//! the same type, in whatever order they arrive. It is deliberately literal
//! about what it saw — it will not guess that a string field is a date, or that
//! an integer is an enum — because the point is to describe the data, not to be
//! clever about it.

use crate::Ident;
use crate::canon::normalize_type;
use crate::ir::{Field, Type};
use crate::value::Value;
use crate::value::unify::{unify, unify_all};

/// How inference should read the samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InferOptions {
    /// Read string fields as the set of values actually seen, rather than as
    /// `string`.
    ///
    /// Off by default, and deliberately so. With enough samples this finds the
    /// discriminator of a tagged union — `"circle" | "rect"` — which is exactly
    /// what you want. With three samples it decides your `name` field is
    /// `"Alice" | "Bob" | "Carol"`, which is exactly what you do not. It is a
    /// guess about how the data is used, not a fact about the data, so it has
    /// to be asked for. Sets that grow past sixteen distinct values collapse
    /// back to `string` on their own.
    pub detect_tags: bool,
}

impl InferOptions {
    /// Inference that reads string fields as sets of tags.
    pub fn with_tags() -> Self {
        Self { detect_tags: true }
    }
}

/// The type of one value.
pub fn infer(value: &Value) -> Type {
    infer_with(value, InferOptions::default())
}

/// The type of one value, read as instructed.
pub fn infer_with(value: &Value, options: InferOptions) -> Type {
    normalize_type(&infer_raw(value, options))
}

/// The type of a whole set of samples.
///
/// This is where a schema comes from: fields that only some samples carry
/// become optional, and elements of differing types become a union.
pub fn infer_many<'a>(values: impl IntoIterator<Item = &'a Value>) -> Type {
    infer_many_with(values, InferOptions::default())
}

/// The type of a whole set of samples, read as instructed.
pub fn infer_many_with<'a>(
    values: impl IntoIterator<Item = &'a Value>,
    options: InferOptions,
) -> Type {
    values
        .into_iter()
        .fold(Type::Never, |acc, v| unify(&acc, &infer_raw(v, options)))
}

fn infer_raw(value: &Value, options: InferOptions) -> Type {
    let infer_raw = |v: &Value| infer_raw(v, options);
    match value {
        // The one value `unit` holds. Joined with anything else it becomes
        // optionality, which is how a null in one sample makes a field
        // optional across the set.
        Value::Null => Type::Unit,
        Value::Bool(_) => Type::Bool,
        // Always the wider integer. Narrowing to i32 on the strength of the
        // samples happening to be small would be a guess, and a fragile one.
        Value::Int(_) => Type::I64,
        Value::Float(_) => Type::F64,
        Value::Str(s) if options.detect_tags => Type::lit_str(s),
        Value::Str(_) => Type::String,
        Value::Bytes(_) => Type::Bytes,

        Value::List(items) => {
            let element: Vec<Type> = items.iter().map(&infer_raw).collect();
            unify_all(element.iter()).list()
        }
        Value::Tuple(items) => Type::Tuple {
            items: items.iter().map(&infer_raw).collect(),
        },
        Value::Record(fields) => Type::Record {
            fields: fields
                .iter()
                .filter_map(|(name, v)| {
                    Some(Field::new(Ident::new(name.clone()).ok()?, infer_raw(v)))
                })
                .collect(),
        },
        Value::Map(entries) => {
            let keys: Vec<Type> = entries.iter().map(|(k, _)| infer_raw(k)).collect();
            let values: Vec<Type> = entries.iter().map(|(_, v)| infer_raw(v)).collect();
            unify_all(values.iter()).map_from(unify_all(keys.iter()))
        }

        // A tagged value could belong to any number of enums, and inventing one
        // from a single sample would be a guess. Enums come from declarations.
        Value::Variant { .. } => Type::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(json: &str) -> Value {
        Value::parse_json(json).expect("reads")
    }

    fn field_named<'a>(t: &'a Type, name: &str) -> &'a Type {
        let Type::Record { fields } = t else {
            panic!("expected a record, got {t:?}");
        };
        &fields
            .iter()
            .find(|f| f.name.as_str() == name)
            .unwrap_or_else(|| panic!("no field `{name}` in {t:?}"))
            .ty
    }

    #[test]
    fn primitives_are_read_as_written() {
        assert_eq!(infer(&v("true")), Type::Bool);
        assert_eq!(infer(&v("1")), Type::I64);
        assert_eq!(infer(&v("1.5")), Type::F64);
        assert_eq!(infer(&v(r#""hi""#)), Type::String);
        assert_eq!(infer(&v("null")), Type::Unit);
    }

    #[test]
    fn an_integer_is_never_narrowed_on_the_strength_of_a_sample() {
        assert_eq!(
            infer(&v("3")),
            Type::I64,
            "small numbers in the sample say nothing about the range"
        );
    }

    #[test]
    fn an_object_becomes_a_record_of_its_fields() {
        let t = infer(&v(r#"{"id":"a","n":2}"#));
        assert_eq!(field_named(&t, "id"), &Type::String);
        assert_eq!(field_named(&t, "n"), &Type::I64);
    }

    #[test]
    fn a_list_takes_the_type_that_admits_all_its_elements() {
        assert_eq!(infer(&v("[1,2,3]")), Type::I64.list());
        let numbers = infer(&v("[1,2.5]"));
        assert!(
            matches!(&numbers, Type::List { of } if matches!(**of, Type::Union { .. })),
            "whole numbers and fractions together are both, not one: {numbers:?}"
        );
        let mixed = infer(&v(r#"[1,"a"]"#));
        assert!(
            matches!(&mixed, Type::List { of } if matches!(**of, Type::Union { .. })),
            "{mixed:?}"
        );
    }

    #[test]
    fn an_empty_list_says_nothing_about_its_elements() {
        assert_eq!(infer(&v("[]")), Type::Never.list());
    }

    #[test]
    fn a_field_missing_from_some_samples_becomes_optional() {
        let samples = [
            v(r#"{"id":"a","note":"hello"}"#),
            v(r#"{"id":"b"}"#),
            v(r#"{"id":"c","note":"again"}"#),
        ];
        let t = infer_many(&samples);
        assert_eq!(field_named(&t, "id"), &Type::String);
        assert_eq!(field_named(&t, "note"), &Type::String.optional());
    }

    #[test]
    fn a_field_that_is_null_in_some_samples_becomes_optional() {
        let samples = [v(r#"{"note":"hi"}"#), v(r#"{"note":null}"#)];
        assert_eq!(
            field_named(&infer_many(&samples), "note"),
            &Type::String.optional()
        );
    }

    #[test]
    fn a_string_field_is_a_string_unless_tags_were_asked_for() {
        let samples = [
            v(r#"{"kind":"circle"}"#),
            v(r#"{"kind":"rect"}"#),
            v(r#"{"kind":"circle"}"#),
        ];
        let plain = infer_many(&samples);
        assert_eq!(
            field_named(&plain, "kind"),
            &Type::String,
            "deciding these are tags is a guess, so it is not made by default"
        );
    }

    #[test]
    fn asking_for_tags_finds_the_discriminator() {
        let samples = [
            v(r#"{"kind":"circle","r":1.0}"#),
            v(r#"{"kind":"rect","r":2.0}"#),
            v(r#"{"kind":"circle","r":3.0}"#),
        ];
        let tagged = infer_many_with(&samples, InferOptions::with_tags());
        let kind = field_named(&tagged, "kind");
        let Type::Union { members } = kind else {
            panic!("expected a set of tags, got {kind:?}");
        };
        assert_eq!(members.len(), 2, "circle and rect, seen three times");
    }

    #[test]
    fn a_field_with_too_many_distinct_values_is_not_a_set_of_tags() {
        let samples: Vec<Value> = (0..25)
            .map(|i| v(&format!(r#"{{"name":"person{i}"}}"#)))
            .collect();
        let tagged = infer_many_with(&samples, InferOptions::with_tags());
        assert_eq!(
            field_named(&tagged, "name"),
            &Type::String,
            "past a point, a set of values is just a string"
        );
    }

    #[test]
    fn the_order_samples_arrive_in_does_not_change_the_answer() {
        let a = v(r#"{"id":"a","note":"x"}"#);
        let b = v(r#"{"id":"b"}"#);
        assert_eq!(infer_many([&a, &b]), infer_many([&b, &a]));
    }

    #[test]
    fn inference_is_stable_under_repetition() {
        let sample = v(r#"{"id":"a","tags":["x"]}"#);
        assert_eq!(infer(&sample), infer_many([&sample, &sample, &sample]));
    }

    #[test]
    fn what_is_inferred_actually_admits_what_it_was_inferred_from() {
        let ws = crate::check::check_workspace(crate::Workspace::default()).expect("checks");
        let samples = [
            v(r#"{"id":"a","note":"hello","tags":["x","y"]}"#),
            v(r#"{"id":"b","tags":[]}"#),
            v(r#"{"id":"c","note":null,"tags":["z"]}"#),
        ];
        let inferred = infer_many(&samples);
        for sample in &samples {
            assert!(
                ws.validate(sample, &inferred).is_ok(),
                "{sample:?} does not fit {inferred:?}"
            );
        }
    }

    #[test]
    fn extended_values_keep_their_shapes() {
        assert_eq!(infer(&Value::Bytes(vec![1, 2])), Type::Bytes);
        assert_eq!(
            infer(&Value::Tuple(vec![Value::Int(1), Value::Bool(true)])),
            Type::Tuple {
                items: vec![Type::I64, Type::Bool]
            }
        );
    }

    #[test]
    fn a_tagged_value_is_not_guessed_into_an_enum() {
        assert_eq!(
            infer(&Value::unit_variant("Red")),
            Type::Any,
            "one sample cannot say which enum this belongs to"
        );
    }
}
