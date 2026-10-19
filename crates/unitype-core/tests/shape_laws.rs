//! Laws structural addressing has to obey.
//!
//! A shape address is what two systems compare to find out that they hold the
//! same type, without having agreed on anything first — not what to call
//! things, and not what order to write them in. The cases next to the
//! implementation show particular groups coming out right; this says it for
//! every workspace, which is the only way to be sure no name has crept back
//! into an address somewhere.

use proptest::prelude::*;
use serde_json::{Value as Json, json};

use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::{TypeId, Workspace};

/// A type over the declarations of a generated workspace, before anything in
/// it has been named.
#[derive(Clone, Debug)]
enum Ty {
    Prim(&'static str),
    /// A declaration, by its position in the generated list.
    Ref(usize),
    List(Box<Ty>),
    Optional(Box<Ty>),
    MapFromStr(Box<Ty>),
    Tuple(Vec<Ty>),
    Union(Vec<Ty>),
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Struct,
    Interface,
    Enum,
    Alias,
}

/// One declaration: the universe it lives in, what kind of thing it is, and
/// its parts — fields, variants, or for an alias the one type it names.
#[derive(Clone, Debug)]
struct Decl {
    universe: usize,
    kind: Kind,
    parts: Vec<Ty>,
}

/// What every universe and declaration is called, in one telling.
struct Naming {
    universes: Vec<String>,
    decls: Vec<String>,
}

const UNIVERSES: usize = 3;

const UNIVERSE_NAMES: [&str; UNIVERSES] = ["north", "east", "south"];

const DECL_NAMES: [&str; 6] = ["Zebra", "Apple", "Mango", "Kiwi", "Fig", "Quince"];

fn arb_ty(decls: usize) -> impl Strategy<Value = Ty> {
    // References are weighted up so that most workspaces have recursion in
    // them, which is the case worth generating.
    let leaf = prop_oneof![
        1 => prop_oneof![Just("i64"), Just("str"), Just("bool")].prop_map(Ty::Prim),
        3 => (0..decls).prop_map(Ty::Ref),
    ];
    leaf.prop_recursive(2, 8, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| Ty::List(Box::new(t))),
            inner.clone().prop_map(|t| Ty::Optional(Box::new(t))),
            inner.clone().prop_map(|t| Ty::MapFromStr(Box::new(t))),
            prop::collection::vec(inner.clone(), 2..4).prop_map(Ty::Tuple),
            prop::collection::vec(inner, 2..4).prop_map(Ty::Union),
        ]
    })
}

fn arb_kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        3 => Just(Kind::Struct),
        1 => Just(Kind::Interface),
        2 => Just(Kind::Enum),
        1 => Just(Kind::Alias),
    ]
}

/// A few declarations that mention one another freely, with a renaming for
/// every universe and declaration and an order to write them down in.
fn arb_case() -> impl Strategy<Value = (Vec<Decl>, Vec<&'static str>, Vec<&'static str>, Vec<usize>)>
{
    (1..=DECL_NAMES.len())
        .prop_flat_map(|n| {
            prop::collection::vec(
                (
                    0..UNIVERSES,
                    arb_kind(),
                    prop::collection::vec(arb_ty(n), 1..=3),
                )
                    .prop_map(|(universe, kind, parts)| Decl {
                        universe,
                        kind,
                        parts,
                    }),
                n,
            )
        })
        .prop_flat_map(|decls| {
            let order: Vec<usize> = (0..decls.len()).collect();
            (
                Just(decls),
                Just(UNIVERSE_NAMES.to_vec()).prop_shuffle(),
                Just(DECL_NAMES.to_vec()).prop_shuffle(),
                Just(order).prop_shuffle(),
            )
        })
}

fn ty_json(t: &Ty, decls: &[Decl], naming: &Naming) -> Json {
    let inner = |t: &Ty| ty_json(t, decls, naming);
    match t {
        Ty::Prim(p) => json!(p),
        Ty::Ref(i) => json!({
            "k": "ref",
            "universe": naming.universes[decls[*i].universe],
            "name": naming.decls[*i],
        }),
        Ty::List(of) => json!({"k": "list", "of": inner(of)}),
        Ty::Optional(of) => json!({"k": "optional", "of": inner(of)}),
        Ty::MapFromStr(value) => json!({"k": "map", "key": "str", "value": inner(value)}),
        Ty::Tuple(items) => {
            json!({"k": "tuple", "items": items.iter().map(inner).collect::<Vec<_>>()})
        }
        Ty::Union(members) => {
            json!({"k": "union", "members": members.iter().map(inner).collect::<Vec<_>>()})
        }
    }
}

/// The workspace as it would be written under `naming`, declarations in
/// `order` and universes in the order their first declaration comes.
fn written(decls: &[Decl], naming: &Naming, order: &[usize]) -> String {
    let mut universes: Vec<(usize, Vec<Json>)> = Vec::new();
    for &i in order {
        let decl = &decls[i];
        let name = &naming.decls[i];
        let part = |p: usize, t: &Ty| match decl.kind {
            Kind::Enum => json!({"name": format!("V{p}"), "payload": ty_json(t, decls, naming)}),
            _ => json!({"name": format!("f{p}"), "ty": ty_json(t, decls, naming)}),
        };
        let parts: Vec<Json> = decl
            .parts
            .iter()
            .enumerate()
            .map(|(p, t)| part(p, t))
            .collect();
        let written = match decl.kind {
            Kind::Struct => json!({"k": "struct", "name": name, "fields": parts}),
            Kind::Interface => json!({"k": "interface", "name": name, "fields": parts}),
            Kind::Enum => json!({"k": "enum", "name": name, "variants": parts}),
            Kind::Alias => json!({
                "k": "alias",
                "name": name,
                "target": ty_json(&decl.parts[0], decls, naming),
            }),
        };
        match universes.iter_mut().find(|(u, _)| *u == decl.universe) {
            Some((_, types)) => types.push(written),
            None => universes.push((decl.universe, vec![written])),
        }
    }
    let universes: Vec<Json> = universes
        .into_iter()
        .map(|(u, types)| json!({"name": naming.universes[u], "types": types}))
        .collect();
    json!({ "universes": universes }).to_string()
}

fn checked(text: &str) -> Option<CheckedWorkspace> {
    check_workspace(Workspace::from_json(text).expect("generated workspaces read")).ok()
}

fn shape(ws: &CheckedWorkspace, decls: &[Decl], naming: &Naming, i: usize) -> TypeId {
    let name = format!(
        "{}::{}",
        naming.universes[decls[i].universe], naming.decls[i]
    );
    ws.shape_hash(&name.parse().unwrap())
        .unwrap_or_else(|| panic!("{name} has no shape"))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    /// Two systems that hold the same declarations under different names,
    /// written down in a different order, agree on every address.
    #[test]
    fn renaming_everything_and_reordering_it_leaves_every_shape_alone(
        (decls, universe_names, decl_names, order) in arb_case()
    ) {
        let n = decls.len();
        let before = Naming {
            universes: (0..UNIVERSES).map(|u| format!("u{u}")).collect(),
            decls: (0..n).map(|i| format!("T{i}")).collect(),
        };
        let after = Naming {
            universes: universe_names.iter().map(ToString::to_string).collect(),
            decls: decl_names[..n].iter().map(ToString::to_string).collect(),
        };

        // A generated alias can expand to itself, which no naming rescues.
        let original = checked(&written(&decls, &before, &(0..n).collect::<Vec<_>>()));
        prop_assume!(original.is_some());
        let original = original.expect("just checked");
        let renamed = checked(&written(&decls, &after, &order));
        prop_assert!(renamed.is_some(), "renaming should not change whether a workspace checks");
        let renamed = renamed.expect("just checked");

        for i in 0..n {
            prop_assert_eq!(
                shape(&original, &decls, &before, i),
                shape(&renamed, &decls, &after, i),
                "{} became {} and its shape moved",
                before.decls[i],
                after.decls[i]
            );
        }
    }
}
