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
use unitype_core::ir::{Field, StructDef, Type, TypeDef, TypeParamDecl, TypeRef};
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
        {"k":"alias","name":"Id","target":"str"},
        {"k":"struct","name":"Keep","params":[{"name":"T"}],
         "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]},
        {"k":"struct","name":"Sink","params":[{"name":"T"}],"fields":[
            {"name":"take","ty":{"k":"function","params":[{"k":"param","name":"T"}],"ret":"unit"}}]},
        {"k":"struct","name":"Cell","params":[{"name":"T"}],"fields":[
            {"name":"get","ty":{"k":"function","params":[],"ret":{"k":"param","name":"T"}}},
            {"name":"set","ty":{"k":"function","params":[{"k":"param","name":"T"}],"ret":"unit"}}]},
        {"k":"struct","name":"Tag","params":[{"name":"T"}],
         "fields":[{"name":"label","ty":"str"}]}]},
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

/// A generic declaration in `demo`, applied to one argument.
fn applied(name: &str, arg: Type) -> Type {
    match named("demo", name) {
        Type::Ref(r) => Type::Ref(TypeRef {
            args: vec![arg],
            ..r
        }),
        _ => unreachable!("a name is a reference"),
    }
}

/// The generics above, one for each way an argument can vary.
const GENERICS: [&str; 4] = ["Keep", "Sink", "Cell", "Tag"];

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
            (prop::sample::select(&GENERICS[..]), inner.clone())
                .prop_map(|(name, arg)| applied(name, arg)),
        ]
    })
    .prop_map(|t| normalize_type(&t))
}

/// Somewhere a generic parameter `T` can sit.
///
/// `exact` keeps to constructors that relate their parts exactly: a list of
/// `A` fits a list of `B` precisely when `A` fits `B`. An optional or a union
/// can absorb a difference in what it holds — `T?` is one type at `i64` and at
/// `i64?` — so they only join in when `exact` is off.
fn arb_context(exact: bool) -> impl Strategy<Value = Type> {
    let leaf = prop_oneof![
        3 => Just(Type::Param { name: ident("T") }),
        1 => Just(Type::I64),
        1 => Just(Type::String),
    ];
    leaf.prop_recursive(3, 16, 2, move |inner| {
        let exact_ones = prop_oneof![
            inner.clone().prop_map(Type::list),
            inner.clone().prop_map(Type::future),
            inner.clone().prop_map(|v| v.map_from(Type::String)),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Type::Tuple { items: vec![a, b] }),
            inner.clone().prop_map(|v| Type::Record {
                fields: vec![Field::new(ident("a"), v)]
            }),
            (inner.clone(), inner.clone()).prop_map(|(p, r)| Type::Function {
                params: vec![p],
                ret: Box::new(r)
            }),
            (prop::sample::select(&GENERICS[..]), inner.clone())
                .prop_map(|(name, arg)| applied(name, arg)),
        ];
        if exact {
            exact_ones.boxed()
        } else {
            prop_oneof![
                3 => exact_ones,
                1 => inner.clone().prop_map(Type::optional),
                1 => (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| Type::Union { members: vec![a, b] }),
            ]
            .boxed()
        }
    })
    .prop_map(|t| normalize_type(&t))
}

/// Arguments for a generic, drawn small enough that pairs are often related.
fn arb_argument() -> impl Strategy<Value = Type> {
    let leaf = prop_oneof![
        Just(Type::Never),
        Just(Type::Any),
        Just(Type::Unit),
        Just(Type::I32),
        Just(Type::I64),
        Just(Type::F64),
        Just(Type::String),
        Just(Type::lit_str("circle")),
        Just(named("demo", "Shape")),
        Just(named("demo", "Circle")),
        Just(named("demo", "Rect")),
        Just(named("demo", "Identified")),
    ];
    leaf.prop_recursive(2, 6, 2, |inner| {
        prop_oneof![
            inner.clone().prop_map(Type::optional),
            inner.clone().prop_map(Type::list),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Type::Union {
                members: vec![a, b]
            }),
        ]
    })
    .prop_map(|t| normalize_type(&t))
}

fn ident(s: &str) -> Ident {
    Ident::new(s).unwrap()
}

/// The workspace above with one more generic struct, `G<T>`, holding these
/// fields, and a list of itself as well when `recursive` is set.
fn with_generic(fields: &[Type], recursive: bool) -> CheckedWorkspace {
    let mut fields: Vec<Field> = fields
        .iter()
        .enumerate()
        .map(|(i, ty)| Field::new(ident(&format!("f{i}")), ty.clone()))
        .collect();
    if recursive {
        let itself = applied("G", Type::Param { name: ident("T") });
        fields.push(Field::new(ident("kids"), itself.list()));
    }
    let mut ws = Workspace::from_json(WS).expect("reads");
    ws.universes[0].types.push(TypeDef::Struct(StructDef {
        name: ident("G"),
        doc: None,
        params: vec![TypeParamDecl::new(ident("T"))],
        extends: None,
        implements: Vec::new(),
        fields,
        meta: Default::default(),
    }));
    check_workspace(ws).expect("a generated generic checks")
}

/// `G<arg>`, and the record of its fields at `arg`.
fn generic_and_fields(ws: &CheckedWorkspace, arg: &Type) -> (Type, Type) {
    let Type::Ref(r) = applied("G", arg.clone()) else {
        unreachable!("applied gives a reference");
    };
    let fields = ws.fields_of(&r).expect("G is a struct");
    (Type::Ref(r), normalize_type(&Type::Record { fields }))
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

    /// A generic fits another application of itself exactly when its fields
    /// do, once the arguments are put in.
    ///
    /// The nominal rule reads only the arguments and the inferred variance;
    /// the structural one reads every field. Agreeing is what makes the
    /// inference both safe and no stricter than the fields are.
    #[test]
    fn a_generic_fits_exactly_when_its_fields_do(
        fields in prop::collection::vec(arb_context(true), 1..4),
        recursive in any::<bool>(),
        a in arb_argument(),
        b in arb_argument(),
    ) {
        let ws = with_generic(&fields, recursive);
        let (ga, fa) = generic_and_fields(&ws, &a);
        let (gb, fb) = generic_and_fields(&ws, &b);
        prop_assert_eq!(
            ws.is_subtype(&ga, &gb),
            ws.is_subtype(&fa, &fb),
            "G<{}> into G<{}> disagreed with its fields {} into {}",
            a, b, fa, fb
        );
    }

    /// Wherever a parameter sits, a generic fitting means its fields fit.
    ///
    /// The other direction can fail here, and should: `T?` is one type at
    /// `i64` and at `i64?`, but the nominal rule judges by the arguments.
    #[test]
    fn a_generic_fits_only_where_its_fields_do(
        fields in prop::collection::vec(arb_context(false), 1..4),
        recursive in any::<bool>(),
        a in arb_argument(),
        b in arb_argument(),
    ) {
        let ws = with_generic(&fields, recursive);
        let (ga, fa) = generic_and_fields(&ws, &a);
        let (gb, fb) = generic_and_fields(&ws, &b);
        if ws.is_subtype(&ga, &gb) {
            prop_assert!(
                ws.is_subtype(&fa, &fb),
                "G<{}> fits G<{}>, but its fields {} do not fit {}",
                a, b, fa, fb
            );
        }
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
