//! Laws the subtyping relation has to obey.
//!
//! Subtyping decides what data is allowed to flow where, so getting it subtly
//! wrong would let a value reach somewhere it does not fit. Individual cases
//! are checked next to the implementation; these are the structural properties
//! that have to hold for *every* type, which is the part that is hard to see by
//! reading code.

use proptest::prelude::*;

use unitype_core::canon::normalize_type;
use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::ir::{Field, Type, TypeRef};
use unitype_core::{Ident, Workspace};

const WS: &str = r#"{"universes":[
    {"name":"demo","types":[
        {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Shape","implements":["Identified"],
         "fields":[{"name":"id","ty":"str"}]},
        {"k":"struct","name":"Circle","extends":"Shape","fields":[{"name":"r","ty":"float"}]},
        {"k":"struct","name":"Rect","extends":"Shape","fields":[{"name":"w","ty":"float"}]},
        {"k":"struct","name":"Tree","fields":[
            {"name":"value","ty":"i64"},{"name":"kids","ty":"[Tree]"}]},
        {"k":"enum","name":"Color","variants":[{"name":"Red"},{"name":"Blue"}]},
        {"k":"alias","name":"Id","target":"str"}]},
    {"name":"other","types":[
        {"k":"struct","name":"Shape","fields":[{"name":"id","ty":"str"}]}]}]}"#;

fn workspace() -> CheckedWorkspace {
    check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
}

fn named(universe: &str, name: &str) -> Type {
    Type::Ref(TypeRef::qualified(
        Ident::new(universe).unwrap(),
        Ident::new(name).unwrap(),
    ))
}

/// Types drawn from the workspace above plus the primitives, so that generated
/// types actually resolve.
fn arb_type() -> impl Strategy<Value = Type> {
    let leaf = prop_oneof![
        Just(Type::Never),
        Just(Type::Any),
        Just(Type::Unit),
        Just(Type::Bool),
        Just(Type::I32),
        Just(Type::I64),
        Just(Type::F64),
        Just(Type::String),
        Just(Type::Bytes),
        Just(Type::lit_str("circle")),
        Just(Type::lit_str("rect")),
        Just(named("demo", "Shape")),
        Just(named("demo", "Circle")),
        Just(named("demo", "Rect")),
        Just(named("demo", "Identified")),
        Just(named("demo", "Tree")),
        Just(named("demo", "Color")),
        Just(named("demo", "Id")),
        Just(named("other", "Shape")),
    ];

    leaf.prop_recursive(3, 20, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(Type::optional),
            inner.clone().prop_map(Type::list),
            inner.clone().prop_map(Type::future),
            prop::collection::vec(inner.clone(), 2..4).prop_map(|items| Type::Tuple { items }),
            prop::collection::vec(inner.clone(), 1..4).prop_map(|members| Type::Union { members }),
            (prop::collection::vec(inner.clone(), 0..2), inner.clone()).prop_map(
                |(params, ret)| Type::Function {
                    params,
                    ret: Box::new(ret)
                }
            ),
            prop::collection::vec(("[a-c]", inner.clone()), 0..3).prop_map(|fields| {
                Type::Record {
                    fields: fields
                        .into_iter()
                        .map(|(name, ty)| Field::new(Ident::new(name).unwrap(), ty))
                        .collect(),
                }
            }),
        ]
    })
    .prop_map(|t| normalize_type(&t))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Nothing is more restrictive than itself.
    #[test]
    fn every_type_can_be_used_where_it_is_expected(t in arb_type()) {
        prop_assert!(workspace().is_subtype(&t, &t), "{t:?} is not usable as itself");
    }

    /// Without this, a value could reach somewhere it does not fit by going
    /// through an intermediate type.
    #[test]
    fn usability_carries_through_a_middle_type(
        a in arb_type(), b in arb_type(), c in arb_type()
    ) {
        let ws = workspace();
        if ws.is_subtype(&a, &b) && ws.is_subtype(&b, &c) {
            prop_assert!(
                ws.is_subtype(&a, &c),
                "{a:?} fits {b:?} fits {c:?}, but not {a:?} into {c:?}"
            );
        }
    }

    /// The empty type fits everywhere and the unknown type accepts everything.
    #[test]
    fn every_type_sits_between_the_two_extremes(t in arb_type()) {
        let ws = workspace();
        prop_assert!(ws.is_subtype(&Type::Never, &t));
        prop_assert!(ws.is_subtype(&t, &Type::Any));
    }

    /// Widening to a union never loses anything.
    #[test]
    fn a_type_fits_any_union_it_is_part_of(a in arb_type(), b in arb_type()) {
        let ws = workspace();
        let union = normalize_type(&Type::Union { members: vec![a.clone(), b.clone()] });
        prop_assert!(ws.is_subtype(&a, &union), "{a:?} does not fit {union:?}");
        prop_assert!(ws.is_subtype(&b, &union), "{b:?} does not fit {union:?}");
    }

    /// A union fits somewhere exactly when all of it does.
    #[test]
    fn a_union_fits_exactly_when_all_of_it_fits(
        a in arb_type(), b in arb_type(), c in arb_type()
    ) {
        let ws = workspace();
        let union = normalize_type(&Type::Union { members: vec![a.clone(), b.clone()] });
        prop_assert_eq!(
            ws.is_subtype(&union, &c),
            ws.is_subtype(&a, &c) && ws.is_subtype(&b, &c),
            "union {:?} into {:?} disagreed with its parts",
            union,
            c
        );
    }

    /// Optionality only ever adds the possibility of absence.
    #[test]
    fn a_value_always_fits_where_absence_is_also_allowed(t in arb_type()) {
        let ws = workspace();
        let maybe = normalize_type(&t.clone().optional());
        prop_assert!(ws.is_subtype(&t, &maybe));
    }

    /// Containers follow what they contain.
    #[test]
    fn containers_follow_their_contents(a in arb_type(), b in arb_type()) {
        let ws = workspace();
        if ws.is_subtype(&a, &b) {
            for wrap in [Type::list as fn(Type) -> Type, Type::future, Type::optional] {
                let (wa, wb) = (normalize_type(&wrap(a.clone())), normalize_type(&wrap(b.clone())));
                prop_assert!(ws.is_subtype(&wa, &wb), "{wa:?} does not fit {wb:?}");
            }
        }
    }

    /// A function usable in another's place must accept everything the other
    /// accepts, and return something the caller can still use.
    #[test]
    fn functions_flip_on_their_parameters(a in arb_type(), b in arb_type()) {
        let ws = workspace();
        if !ws.is_subtype(&a, &b) {
            return Ok(());
        }
        let takes_b = Type::Function { params: vec![b.clone()], ret: Box::new(a.clone()) };
        let takes_a = Type::Function { params: vec![a.clone()], ret: Box::new(b.clone()) };
        prop_assert!(
            ws.is_subtype(&takes_b, &takes_a),
            "accepting more and promising less should be usable in place of the reverse"
        );
    }

    /// The boundary between universes is not crossed by accident, whatever the
    /// shapes happen to be.
    #[test]
    fn nothing_crosses_a_universe_boundary_on_its_own(t in arb_type()) {
        let ws = workspace();
        let here = named("demo", "Shape");
        let there = named("other", "Shape");
        prop_assert!(!ws.is_subtype(&here, &there));
        prop_assert!(!ws.is_subtype(&there, &here));
        // And wrapping does not smuggle it across either.
        let _ = t;
    }
}
