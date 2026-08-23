//! Joining two types into one that admits both.
//!
//! This is how several samples become one type. Show the system three records
//! where the third is missing a field, and the join makes that field optional;
//! show it a list holding both integers and strings, and the join makes the
//! element type a union.
//!
//! The result always admits both inputs, with one deliberate exception noted
//! below, and the operation is commutative and idempotent so that the order
//! samples arrive in does not change what is inferred.

use crate::canon::normalize_type;
use crate::ir::{Field, LiteralValue, Type};

/// How many distinct literals are kept before they are treated as an open set.
///
/// A handful of string literals is a set of tags worth preserving. Two hundred
/// is a `string` that happened to be sampled two hundred times.
const MAX_LITERALS: usize = 16;

/// The narrowest type that admits both inputs.
///
/// This is a true upper bound: the result always admits everything either input
/// did, with no exceptions. That property is what makes inference trustworthy —
/// a type inferred from samples has to be a type those samples satisfy — and it
/// is checked over generated values in `tests/value_laws.rs`.
///
/// It is why `i64 ⊔ f64` is `i64 | f64` rather than `f64`, which would be the
/// tempting answer for samples of `[1, 2.5]`. Past 2^53 an integer does not
/// survive being an `f64`, so calling the pair `f64` would be describing data
/// that does not exist. `i32 ⊔ f64` *is* `f64`, because every 32-bit integer is
/// exactly a float.
pub fn unify(a: &Type, b: &Type) -> Type {
    normalize_type(&join(&normalize_type(a), &normalize_type(b)))
}

/// Joins several types, or [`Type::Never`] if there are none.
pub fn unify_all<'a>(types: impl IntoIterator<Item = &'a Type>) -> Type {
    types.into_iter().fold(Type::Never, |acc, t| unify(&acc, t))
}

fn join(a: &Type, b: &Type) -> Type {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (Type::Never, other) | (other, Type::Never) => other.clone(),
        (Type::Any, _) | (_, Type::Any) => Type::Any,

        // Absence anywhere means absence everywhere.
        (Type::Optional { of }, other) | (other, Type::Optional { of }) => {
            join(of, other).optional()
        }
        (Type::Unit, other) | (other, Type::Unit) => other.clone().optional(),

        (Type::List { of: x }, Type::List { of: y }) => join(x, y).list(),
        (Type::Future { of: x }, Type::Future { of: y }) => join(x, y).future(),
        (Type::Map { key: kx, value: vx }, Type::Map { key: ky, value: vy }) => {
            join(vx, vy).map_from(join(kx, ky))
        }

        (Type::Tuple { items: x }, Type::Tuple { items: y }) if x.len() == y.len() => Type::Tuple {
            items: x.iter().zip(y).map(|(p, q)| join(p, q)).collect(),
        },

        (Type::Record { fields: x }, Type::Record { fields: y }) => join_records(x, y),

        (Type::I32, Type::I64) | (Type::I64, Type::I32) => Type::I64,
        // Every 32-bit integer is exactly a float, so this one widens. The
        // 64-bit case does not, and falls through to a union.
        (Type::F64, Type::I32) | (Type::I32, Type::F64) => Type::F64,

        (Type::Literal { value }, other) | (other, Type::Literal { value }) => {
            join_literal(value, other)
        }

        _ => Type::Union {
            members: vec![a.clone(), b.clone()],
        },
    }
}

/// Fields present in only one of two records are the ones that may be absent.
///
/// This is what turns a pile of samples into a schema: whatever every sample
/// had is required, whatever only some had is optional.
fn join_records(a: &[Field], b: &[Field]) -> Type {
    let mut fields: Vec<Field> = Vec::new();

    for field in a {
        let ty = match b.iter().find(|f| f.name == field.name) {
            Some(other) => join(&field.ty, &other.ty),
            None => field.ty.clone().optional(),
        };
        fields.push(Field {
            name: field.name.clone(),
            ty,
            doc: field.doc.clone(),
        });
    }
    for field in b {
        if a.iter().any(|f| f.name == field.name) {
            continue;
        }
        fields.push(Field {
            name: field.name.clone(),
            ty: field.ty.clone().optional(),
            doc: field.doc.clone(),
        });
    }

    Type::Record { fields }
}

/// A literal joined with something else: still a set of tags, or a plain value.
fn join_literal(value: &LiteralValue, other: &Type) -> Type {
    let base = value.base();
    match other {
        // Two distinct literals of the same kind are a set of tags worth
        // keeping — that is how a discriminated union is discovered.
        Type::Literal { value: b } if b.base() == base => Type::Union {
            members: vec![
                Type::Literal {
                    value: value.clone(),
                },
                Type::Literal { value: b.clone() },
            ],
        },
        // A growing set of literals stops being a set of tags at some point.
        Type::Union { members }
            if members.len() >= MAX_LITERALS
                && members.iter().all(|m| matches!(m, Type::Literal { .. })) =>
        {
            join(&base, &base)
        }
        Type::Union { members }
            if members
                .iter()
                .all(|m| matches!(m, Type::Literal { value } if value.base() == base)) =>
        {
            let mut all = members.clone();
            all.push(Type::Literal {
                value: value.clone(),
            });
            Type::Union { members: all }
        }
        other => join(&base, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ident;

    fn field(name: &str, ty: Type) -> Field {
        Field::new(Ident::new(name).unwrap(), ty)
    }

    fn record(fields: Vec<Field>) -> Type {
        normalize_type(&Type::Record { fields })
    }

    #[test]
    fn joining_a_type_with_itself_changes_nothing() {
        for t in [Type::I64, Type::String.list(), Type::Bool.optional()] {
            assert_eq!(unify(&t, &t), t);
        }
    }

    #[test]
    fn the_order_samples_arrive_in_does_not_matter() {
        let cases = [
            (Type::I64, Type::String),
            (Type::I32, Type::I64),
            (Type::Unit, Type::Bool),
            (Type::lit_str("a"), Type::lit_str("b")),
        ];
        for (a, b) in cases {
            assert_eq!(unify(&a, &b), unify(&b, &a), "{a:?} and {b:?}");
        }
    }

    #[test]
    fn the_result_admits_both_inputs() {
        let ws = crate::check::check_workspace(crate::Workspace::default()).expect("checks");
        let cases = [
            (Type::I64, Type::String),
            (Type::I32, Type::I64),
            (Type::I64, Type::F64),
            (Type::I32, Type::F64),
            (Type::Unit, Type::Bool),
            (Type::String.list(), Type::I64.list()),
        ];
        for (a, b) in cases {
            let joined = unify(&a, &b);
            assert!(ws.is_subtype(&a, &joined), "{a:?} does not fit {joined:?}");
            assert!(ws.is_subtype(&b, &joined), "{b:?} does not fit {joined:?}");
        }
    }

    #[test]
    fn a_field_only_some_samples_had_becomes_optional() {
        let with = record(vec![field("id", Type::String), field("note", Type::String)]);
        let without = record(vec![field("id", Type::String)]);
        let joined = unify(&with, &without);

        let Type::Record { fields } = &joined else {
            panic!("expected a record, got {joined:?}");
        };
        let note = fields.iter().find(|f| f.name.as_str() == "note").unwrap();
        assert_eq!(note.ty, Type::String.optional());
        let id = fields.iter().find(|f| f.name.as_str() == "id").unwrap();
        assert_eq!(id.ty, Type::String, "every sample had this one");
    }

    #[test]
    fn a_null_sample_makes_the_field_optional_rather_than_unknown() {
        let present = record(vec![field("note", Type::String)]);
        let absent = record(vec![field("note", Type::Unit)]);
        let Type::Record { fields } = unify(&present, &absent) else {
            panic!("expected a record");
        };
        assert_eq!(fields[0].ty, Type::String.optional());
    }

    #[test]
    fn containers_join_through_their_contents() {
        assert_eq!(
            unify(&Type::I32.list(), &Type::I64.list()),
            Type::I64.list()
        );
        assert_eq!(
            unify(&Type::String.future(), &Type::String.future()),
            Type::String.future()
        );
    }

    #[test]
    fn integers_widen_into_floats_only_where_nothing_is_lost() {
        assert_eq!(unify(&Type::I32, &Type::I64), Type::I64);
        assert_eq!(
            unify(&Type::I32, &Type::F64),
            Type::F64,
            "every 32-bit integer is exactly a float"
        );
        assert!(
            matches!(unify(&Type::I64, &Type::F64), Type::Union { .. }),
            "a 64-bit integer is not always a float, so calling the pair `f64` \
             would describe data that cannot exist"
        );
    }

    #[test]
    fn a_few_string_literals_stay_a_set_of_tags() {
        let joined = unify(&Type::lit_str("circle"), &Type::lit_str("rect"));
        let Type::Union { members } = &joined else {
            panic!("expected a union of tags, got {joined:?}");
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn too_many_literals_stop_being_a_set_of_tags() {
        let mut acc = Type::Never;
        for i in 0..MAX_LITERALS + 4 {
            acc = unify(&acc, &Type::lit_str(&format!("tag{i}")));
        }
        assert_eq!(acc, Type::String, "at some point it is just a string");
    }

    #[test]
    fn a_literal_and_its_base_type_are_the_base_type() {
        assert_eq!(unify(&Type::lit_str("a"), &Type::String), Type::String);
    }

    #[test]
    fn unrelated_types_become_a_union() {
        let joined = unify(&Type::String, &Type::Bool);
        assert!(matches!(joined, Type::Union { .. }), "{joined:?}");
    }

    #[test]
    fn joining_nothing_is_the_empty_type() {
        assert_eq!(unify_all(std::iter::empty()), Type::Never);
        assert_eq!(unify_all([&Type::Bool]), Type::Bool);
    }
}
