//! Turning an inferred shape into declarations you can name.
//!
//! Inference produces anonymous records: useful, but not something you can
//! refer to, generate a class for, or link to another universe. Lifting walks
//! the inferred type, gives every record a name, and adds it to a universe, so
//! that a pile of sample JSON becomes an actual schema in one step.
//!
//! Names are derived from where a record sits — `order.items[]` becomes
//! `OrderItem` — and identical shapes are given one declaration rather than
//! several, so a record that appears in three places is named once.

use crate::canon::hash::TypeId;
use crate::canon::local_hash;
use crate::ident::Ident;
use crate::ir::{Field, StructDef, Type, TypeDef, TypeRef, Universe};
use crate::meta::Metadata;
use std::collections::HashMap;

/// What lifting produced.
#[derive(Clone, Debug)]
pub struct Lifted {
    /// The type with its anonymous records replaced by references.
    pub ty: Type,
    /// The declarations that were added, in the order they were created.
    pub created: Vec<Ident>,
}

/// Hoists every anonymous record in `ty` into a named struct in `universe`.
///
/// `hint` names the outermost record; everything nested is named after the path
/// that reaches it.
pub fn lift(universe: &mut Universe, hint: &str, ty: &Type) -> Lifted {
    let mut lifter = Lifter {
        universe,
        created: Vec::new(),
        by_shape: HashMap::new(),
    };
    let ty = lifter.walk(ty, hint);
    Lifted {
        ty,
        created: lifter.created,
    }
}

struct Lifter<'a> {
    universe: &'a mut Universe,
    created: Vec<Ident>,
    /// Shapes already given a name, so that the same record appearing twice
    /// becomes one declaration rather than two.
    by_shape: HashMap<TypeId, Ident>,
}

impl Lifter<'_> {
    fn walk(&mut self, ty: &Type, hint: &str) -> Type {
        match ty {
            Type::Record { fields } => {
                let fields: Vec<Field> = fields
                    .iter()
                    .map(|f| Field {
                        name: f.name.clone(),
                        ty: self.walk(&f.ty, &nested_hint(hint, f.name.as_str())),
                        doc: f.doc.clone(),
                    })
                    .collect();
                Type::Ref(TypeRef::local(self.declare(fields, hint)))
            }
            // A list's elements share the list's name, made singular: the
            // elements of `items` are each an `Item`.
            Type::List { of } => self.walk(of, &singular(hint)).list(),
            Type::Optional { of } => self.walk(of, hint).optional(),
            Type::Future { of } => self.walk(of, hint).future(),
            Type::Map { key, value } => self
                .walk(value, &format!("{hint}Value"))
                .map_from((**key).clone()),
            Type::Tuple { items } => Type::Tuple {
                items: items
                    .iter()
                    .enumerate()
                    .map(|(i, t)| self.walk(t, &format!("{hint}{}", i + 1)))
                    .collect(),
            },
            Type::Union { members } => Type::Union {
                members: members.iter().map(|m| self.walk(m, hint)).collect(),
            },
            other => other.clone(),
        }
    }

    /// Adds a struct for these fields, or reuses one that already describes
    /// them.
    fn declare(&mut self, fields: Vec<Field>, hint: &str) -> Ident {
        let shape = local_hash(&Type::Record {
            fields: fields.clone(),
        });
        if let Some(existing) = self.by_shape.get(&shape) {
            return existing.clone();
        }

        let name = self.available_name(hint);
        self.universe.types.push(TypeDef::Struct(StructDef {
            name: name.clone(),
            doc: None,
            params: Vec::new(),
            extends: None,
            implements: Vec::new(),
            fields,
            meta: Metadata::default(),
        }));
        self.by_shape.insert(shape, name.clone());
        self.created.push(name.clone());
        name
    }

    /// A name nothing in the universe is using yet.
    fn available_name(&self, hint: &str) -> Ident {
        let base = pascal_case(hint);
        let base = if base.is_empty() {
            "Record".to_string()
        } else {
            base
        };
        let taken = |name: &str| {
            self.universe
                .types
                .iter()
                .any(|d| d.name().as_str() == name)
        };

        if !taken(&base) {
            if let Ok(name) = Ident::new(base.clone()) {
                return name;
            }
        }
        for n in 2.. {
            let candidate = format!("{base}{n}");
            if !taken(&candidate) {
                if let Ok(name) = Ident::new(candidate) {
                    return name;
                }
            }
        }
        unreachable!("a free name exists somewhere in an unbounded sequence")
    }
}

/// The name for a record found at `field` inside something called `parent`.
fn nested_hint(parent: &str, field: &str) -> String {
    format!("{}{}", pascal_case(parent), pascal_case(field))
}

/// Trims an English plural, well enough for naming purposes.
///
/// A heuristic, and known to be one: `data` stays `data` and `bus` becomes
/// `bu`. Names it gets wrong are still unique and still legible, and a person
/// renaming a generated declaration is expected.
fn singular(word: &str) -> String {
    if let Some(stem) = word.strip_suffix("ies") {
        if !stem.is_empty() {
            return format!("{stem}y");
        }
    }
    if word.ends_with("ss") || word.ends_with("us") || word.ends_with("is") {
        return word.to_string();
    }
    match word.strip_suffix('s') {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => word.to_string(),
    }
}

/// `order_items` and `order-items` both become `OrderItems`.
fn pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut capitalize = true;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if capitalize {
                out.extend(c.to_uppercase());
                capitalize = false;
            } else {
                out.push(c);
            }
        } else {
            capitalize = true;
        }
    }
    // An identifier cannot start with a digit.
    if out.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_workspace;
    use crate::value::{Value, infer_many};
    use crate::{Ident, Workspace};

    fn universe() -> Universe {
        Universe::new(Ident::new("app").unwrap())
    }

    fn v(json: &str) -> Value {
        Value::parse_json(json).expect("reads")
    }

    fn fields_of<'a>(u: &'a Universe, name: &str) -> &'a [Field] {
        &u.type_def(&Ident::new(name).unwrap())
            .unwrap_or_else(|| panic!("no `{name}` in {:?}", names(u)))
            .as_struct()
            .expect("a struct")
            .fields
    }

    fn names(u: &Universe) -> Vec<String> {
        u.types.iter().map(|d| d.name().to_string()).collect()
    }

    #[test]
    fn a_record_becomes_a_named_struct() {
        let mut u = universe();
        let inferred = infer_many([&v(r#"{"id":"a","n":1}"#)]);
        let lifted = lift(&mut u, "user", &inferred);

        assert_eq!(names(&u), ["User"]);
        assert_eq!(lifted.created.len(), 1);
        assert_eq!(lifted.ty, Type::local_ref("User"));
        assert_eq!(fields_of(&u, "User").len(), 2);
    }

    #[test]
    fn a_nested_record_is_named_after_the_path_that_reaches_it() {
        let mut u = universe();
        let inferred = infer_many([&v(r#"{"id":"a","items":[{"sku":"x","qty":2}]}"#)]);
        lift(&mut u, "order", &inferred);

        assert!(names(&u).contains(&"Order".to_string()), "{:?}", names(&u));
        assert!(
            names(&u).contains(&"OrderItem".to_string()),
            "the elements of `items` should each be an OrderItem: {:?}",
            names(&u)
        );
    }

    #[test]
    fn the_same_shape_in_two_places_is_declared_once() {
        let mut u = universe();
        let inferred = infer_many([&v(r#"{"from":{"x":1.0,"y":2.0},"to":{"x":3.0,"y":4.0}}"#)]);
        let lifted = lift(&mut u, "line", &inferred);

        assert_eq!(
            lifted.created.len(),
            2,
            "one for the line, one shared by both points: {:?}",
            names(&u)
        );
        let line = fields_of(&u, "Line");
        assert_eq!(
            line[0].ty, line[1].ty,
            "both ends should point at the same declaration"
        );
    }

    #[test]
    fn a_name_already_in_use_is_not_taken_over() {
        let mut u = universe();
        u.types.push(TypeDef::Struct(StructDef {
            name: Ident::new("User").unwrap(),
            doc: None,
            params: vec![],
            extends: None,
            implements: vec![],
            fields: vec![],
            meta: Metadata::default(),
        }));

        let inferred = infer_many([&v(r#"{"id":"a"}"#)]);
        lift(&mut u, "user", &inferred);
        assert_eq!(names(&u), ["User", "User2"]);
    }

    #[test]
    fn an_optional_record_is_lifted_through_its_optionality() {
        let mut u = universe();
        let inferred = infer_many([&v(r#"{"profile":{"bio":"hi"}}"#), &v(r#"{"profile":null}"#)]);
        let lifted = lift(&mut u, "user", &inferred);

        assert!(
            names(&u).contains(&"UserProfile".to_string()),
            "{:?}",
            names(&u)
        );
        let user = fields_of(&u, "User");
        assert_eq!(
            user[0].ty,
            Type::local_ref("UserProfile").optional(),
            "a profile that is sometimes absent is still a UserProfile"
        );
        let _ = lifted;
    }

    #[test]
    fn what_was_lifted_still_admits_the_samples_it_came_from() {
        let samples = [
            v(r#"{"id":"a","items":[{"sku":"x","qty":1}],"note":"first"}"#),
            v(r#"{"id":"b","items":[]}"#),
        ];
        let mut u = universe();
        let lifted = lift(&mut u, "order", &infer_many(&samples));

        let ws = check_workspace(Workspace::of([u])).expect("the lifted universe checks");
        for sample in &samples {
            assert!(
                ws.validate(sample, &qualify(&lifted.ty)).is_ok(),
                "{sample:?} no longer fits what was inferred from it"
            );
        }
    }

    /// Lifting produces local references; checking qualifies them.
    fn qualify(ty: &Type) -> Type {
        match ty {
            Type::Ref(r) => Type::Ref(TypeRef::qualified(
                Ident::new("app").unwrap(),
                r.name.clone(),
            )),
            other => other.clone(),
        }
    }

    #[test]
    fn plurals_are_trimmed_where_trimming_helps() {
        assert_eq!(singular("items"), "item");
        assert_eq!(singular("entries"), "entry");
        assert_eq!(singular("address"), "address");
        assert_eq!(singular("status"), "status");
        assert_eq!(singular("item"), "item");
    }

    #[test]
    fn names_are_built_from_whatever_the_field_was_called() {
        assert_eq!(pascal_case("order_items"), "OrderItems");
        assert_eq!(pascal_case("order-items"), "OrderItems");
        assert_eq!(pascal_case("orderItems"), "OrderItems");
        assert_eq!(pascal_case("2fa"), "_2fa");
    }
}
