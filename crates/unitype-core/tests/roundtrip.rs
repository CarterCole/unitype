//! Properties the canonical form has to satisfy for content addressing to mean
//! anything.
//!
//! If normalization were not idempotent, the "canonical" form would depend on
//! how many times it had been applied. If encoding did not round trip, a type
//! could not survive being written down. If normalizing changed a type's
//! address, the address would be naming the spelling rather than the type.

use proptest::prelude::*;

use unitype_core::canon::{canonical_string, local_hash, normalize_type, normalize_workspace};
use unitype_core::ir::{Field, LiteralValue, Type};
use unitype_core::{Ident, Workspace};

/// Types drawn from across the IR, shallow enough to stay fast.
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
        any::<bool>().prop_map(|b| Type::Literal {
            value: LiteralValue::Bool(b)
        }),
        any::<i64>().prop_map(|i| Type::Literal {
            value: LiteralValue::Int(i)
        }),
        "[a-z]{1,4}".prop_map(|s| Type::lit_str(&s)),
        "[A-Z][a-z]{0,3}".prop_map(|s| Type::local_ref(&s)),
    ];

    leaf.prop_recursive(4, 32, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(Type::optional),
            inner.clone().prop_map(Type::list),
            inner.clone().prop_map(Type::future),
            inner.clone().prop_map(|t| t.map_from(Type::String)),
            prop::collection::vec(inner.clone(), 0..4).prop_map(|items| Type::Tuple { items }),
            prop::collection::vec(inner.clone(), 0..4).prop_map(|members| Type::Union { members }),
            (prop::collection::vec(inner.clone(), 0..3), inner.clone()).prop_map(
                |(params, ret)| Type::Function {
                    params,
                    ret: Box::new(ret)
                }
            ),
            prop::collection::vec(("[a-z]{1,3}", inner.clone()), 0..4).prop_map(|fields| {
                Type::Record {
                    fields: fields
                        .into_iter()
                        .map(|(name, ty)| Field::new(Ident::new(name).unwrap(), ty))
                        .collect(),
                }
            }),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Normalizing something already normalized must change nothing, or the
    /// canonical form would depend on how many times it had been computed.
    #[test]
    fn normalizing_is_idempotent(t in arb_type()) {
        let once = normalize_type(&t);
        let twice = normalize_type(&once);
        prop_assert_eq!(&once, &twice);
    }

    /// A type written out and read back is the same type.
    #[test]
    fn encoding_round_trips(t in arb_type()) {
        let canonical = normalize_type(&t);
        let text = canonical_string(&canonical);
        let parsed: Type = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(&canonical, &parsed);
        prop_assert_eq!(text, canonical_string(&parsed));
    }

    /// An address names the type, not the spelling, so normalizing cannot move
    /// it.
    #[test]
    fn normalizing_does_not_change_the_address(t in arb_type()) {
        prop_assert_eq!(local_hash(&t), local_hash(&normalize_type(&t)));
    }

    /// Order within a union is not information.
    #[test]
    fn union_member_order_is_not_information(members in prop::collection::vec(arb_type(), 1..5)) {
        let forward = normalize_type(&Type::Union { members: members.clone() });
        let backward = normalize_type(&Type::Union {
            members: members.into_iter().rev().collect(),
        });
        prop_assert_eq!(forward, backward);
    }
}

/// A workspace exercising inheritance, generics, unions, links, and a body.
const DEMO: &str = r#"{
  "universes": [
    {
      "name": "demo",
      "doc": "shapes and the things done to them",
      "types": [
        {"k": "interface", "name": "Identified",
         "fields": [{"name": "id", "ty": "str"}]},
        {"k": "struct", "name": "Shape", "implements": ["Identified"],
         "fields": [{"name": "id", "ty": "str"}]},
        {"k": "struct", "name": "Circle", "extends": "Shape",
         "fields": [{"name": "kind", "ty": {"k": "literal", "value": {"str": "circle"}}},
                    {"name": "radius", "ty": "float"}]},
        {"k": "struct", "name": "Rect", "extends": "Shape",
         "fields": [{"name": "kind", "ty": {"k": "literal", "value": {"str": "rect"}}},
                    {"name": "w", "ty": "float"},
                    {"name": "h", "ty": "float"}]},
        {"k": "alias", "name": "AnyShape",
         "target": {"k": "union", "members": ["Circle", "Rect"]}},
        {"k": "struct", "name": "Box", "params": [{"name": "T"}],
         "fields": [{"name": "item", "ty": {"k": "param", "name": "T"}}]},
        {"k": "enum", "name": "Color",
         "variants": [{"name": "Red"}, {"name": "Custom", "payload": "str"}]}
      ],
      "functions": [
        {"name": "sort", "params": [{"name": "xs", "ty": "[int]"}], "ret": "[int]"},
        {"name": "area", "params": [{"name": "s", "ty": "AnyShape"}], "ret": "float"},
        {"name": "load", "params": [{"name": "id", "ty": "str"}],
         "ret": {"k": "future", "of": "Shape?"}}
      ]
    },
    {
      "name": "geo",
      "types": [
        {"k": "struct", "name": "Point",
         "fields": [{"name": "x", "ty": "float"}, {"name": "y", "ty": "float"}]}
      ]
    }
  ],
  "links": [
    {"name": "point_is_a_vec2", "from": "geo::Point", "to": "gfx::Vec2",
     "kind": {"k": "identical"}}
  ]
}"#;

#[test]
fn a_workspace_survives_being_written_out_and_read_back() {
    let loaded = normalize_workspace(Workspace::from_json(DEMO).expect("reads"));
    let text = canonical_string(&loaded);

    let reloaded = normalize_workspace(Workspace::from_json(&text).expect("re-reads"));
    assert_eq!(loaded, reloaded, "a canonical workspace is not stable");
    assert_eq!(
        text,
        canonical_string(&reloaded),
        "re-emitting canonical form produced different bytes"
    );
}

#[test]
fn how_a_universe_is_written_does_not_change_what_it_means() {
    // The same declarations in a different order, using the object form where
    // the first spelling used shorthand.
    let shuffled = r#"{
      "universes": [
        {"name": "geo", "types": [
          {"k": "struct", "name": "Point", "fields": [
            {"name": "x", "ty": {"k": "f64"}},
            {"name": "y", "ty": {"k": "f64"}}]}]},
        {"name": "demo", "doc": "shapes and the things done to them",
         "functions": [
           {"name": "area", "params": [{"name": "s", "ty": "demo::AnyShape"}], "ret": "f64"},
           {"name": "load", "params": [{"name": "id", "ty": "string"}],
            "ret": {"k": "future", "of": {"k": "optional", "of": "demo::Shape"}}},
           {"name": "sort", "params": [{"name": "xs", "ty": {"k": "list", "of": "i64"}}],
            "ret": {"k": "list", "of": "i64"}}],
         "types": [
           {"k": "struct", "name": "Rect", "extends": "demo::Shape",
            "fields": [{"name": "kind", "ty": {"k": "literal", "value": {"str": "rect"}}},
                       {"name": "w", "ty": "f64"}, {"name": "h", "ty": "f64"}]},
           {"k": "enum", "name": "Color",
            "variants": [{"name": "Red", "payload": {"k": "unit"}},
                         {"name": "Custom", "payload": "string"}]},
           {"k": "struct", "name": "Box", "params": [{"name": "T"}],
            "fields": [{"name": "item", "ty": {"k": "param", "name": "T"}}]},
           {"k": "alias", "name": "AnyShape",
            "target": {"k": "union", "members": ["demo::Rect", "demo::Circle"]}},
           {"k": "struct", "name": "Circle", "extends": "demo::Shape",
            "fields": [{"name": "kind", "ty": {"k": "literal", "value": {"str": "circle"}}},
                       {"name": "radius", "ty": "f64"}]},
           {"k": "interface", "name": "Identified", "fields": [{"name": "id", "ty": "string"}]},
           {"k": "struct", "name": "Shape", "implements": ["demo::Identified"],
            "fields": [{"name": "id", "ty": "string"}]}]}
      ],
      "links": [
        {"name": "point_is_a_vec2", "from": "geo::Point", "to": "gfx::Vec2",
         "kind": {"k": "identical"}}
      ]
    }"#;

    let a = normalize_workspace(Workspace::from_json(DEMO).expect("reads"));
    let b = normalize_workspace(Workspace::from_json(shuffled).expect("reads"));
    assert_eq!(
        canonical_string(&a),
        canonical_string(&b),
        "two spellings of one workspace produced different canonical forms"
    );
}
