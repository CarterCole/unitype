//! The system, described in its own terms.
//!
//! A universe called `unitype` declares the representation this crate is built
//! out of: what a type is, what a declaration is, what a universe is. Load it
//! alongside your own universes and every one of those becomes something you
//! can compute with — validate, address, name in three words, move between
//! universes, generate code for, hand to a proposition.
//!
//! That is not a trick, and it needed no new machinery. Types, declarations,
//! and universes already serialize to plain JSON, so each of them was always a
//! value. This just says which value.
//!
//! # Kept honest
//!
//! A description of a representation is worth nothing if it drifts from the
//! representation. So the real declarations are serialized and checked against
//! these ones, in a test that fails the moment they stop agreeing. Adding a
//! variant to the IR without adding it here breaks the build, which is the only
//! arrangement under which a claim like this stays true.

use crate::ir::{LoadError, Universe};

/// The name of the universe that describes the representation.
pub const UNIVERSE: &str = "unitype";

/// The universe describing this system's own representation.
///
/// Read afresh each time rather than cached: it is a document, and a caller may
/// want to add to it, strip it down, or link it to something of their own.
pub fn universe() -> Result<Universe, LoadError> {
    Universe::from_json(include_str!("unitype.json"))
}

/// The name of the declaration describing a type.
pub const TYPE: &str = "unitype::Type";

/// The name of the declaration describing a type declaration.
pub const TYPE_DEF: &str = "unitype::TypeDef";

/// The name of the declaration describing a universe.
pub const UNIVERSE_DEF: &str = "unitype::Universe";

/// The name of the declaration describing a whole workspace.
pub const WORKSPACE: &str = "unitype::Workspace";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::canonical_string;
    use crate::check::{CheckedWorkspace, check_workspace};
    use crate::ir::{Type, TypeRef};
    use crate::value::Value;
    use crate::{Ident, Workspace};

    fn ws() -> CheckedWorkspace {
        check_workspace(Workspace::of(
            [universe().expect("the meta-universe reads")],
        ))
        .expect("and it checks")
    }

    fn named(name: &str) -> Type {
        Type::Ref(TypeRef::qualified(
            Ident::new("unitype").unwrap(),
            Ident::new(name).unwrap(),
        ))
    }

    /// A real thing from the IR, as the value it serializes to.
    fn as_value<T: serde::Serialize>(thing: &T) -> Value {
        Value::parse_json(&canonical_string(thing)).expect("the IR serializes to values")
    }

    fn check(ws: &CheckedWorkspace, value: &Value, ty: &Type) {
        if let Err(problems) = ws.validate(value, ty) {
            panic!(
                "the description has drifted from what it describes.\n\
                 Checking {value:?}\n\
                 against {ty}:\n{}",
                problems
                    .iter()
                    .map(|p| format!("  {p}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
    }

    #[test]
    fn the_meta_universe_checks_like_any_other() {
        let ws = ws();
        assert!(ws.def(&TYPE.parse().unwrap()).is_some());
        assert!(ws.def(&WORKSPACE.parse().unwrap()).is_some());
    }

    #[test]
    fn every_kind_of_type_is_described_by_the_description() {
        let ws = ws();
        let ty = named("Type");

        // One of each shape a type can take, checked against what this claims a
        // type is. A variant added to the IR and not added here fails right
        // here, which is the point.
        let every_shape = vec![
            Type::Never,
            Type::Any,
            Type::Unit,
            Type::Bool,
            Type::I32,
            Type::I64,
            Type::F64,
            Type::String,
            Type::Bytes,
            Type::lit_str("circle"),
            Type::Literal {
                value: crate::ir::LiteralValue::Int(3),
            },
            Type::Literal {
                value: crate::ir::LiteralValue::Bool(true),
            },
            Type::String.optional(),
            Type::I64.list(),
            Type::Bool.future(),
            Type::I64.map_from(Type::String),
            Type::Tuple {
                items: vec![Type::I32, Type::String],
            },
            Type::Record {
                fields: vec![crate::ir::Field::new(Ident::new("x").unwrap(), Type::F64)],
            },
            Type::Union {
                members: vec![Type::I64, Type::String],
            },
            Type::Function {
                params: vec![Type::I64],
                ret: Box::new(Type::Bool),
            },
            named("Type"),
            Type::Param {
                name: Ident::new("T").unwrap(),
            },
        ];

        for shape in &every_shape {
            check(&ws, &as_value(shape), &ty);
        }
        assert_eq!(
            every_shape.len(),
            22,
            "one of every shape, plus the three kinds of literal"
        );
    }

    #[test]
    fn every_kind_of_expression_is_described_by_the_description() {
        use crate::ir::{Expr, FieldInit, Functor, MatchArm, PrimOp};

        let ws = ws();
        let expr = named("Expr");
        let id = |s: &str| Ident::new(s).unwrap();
        let x = || Box::new(Expr::var("x"));

        // One of each form a body can take. A form added to the IR and not
        // added to the description fails here, as a type does above.
        let every_form = vec![
            Expr::lit(Value::Int(1)),
            Expr::var("x"),
            Expr::var("x").field("y"),
            Expr::MakeRecord {
                ty: TypeRef::local(id("Label")),
                fields: vec![FieldInit::new(id("text"), Expr::var("x"))],
            },
            Expr::MakeList {
                items: vec![Expr::var("x")],
            },
            Expr::MakeTuple {
                items: vec![Expr::var("x"), Expr::var("y")],
            },
            Expr::MakeVariant {
                ty: TypeRef::local(id("Size")),
                tag: id("small"),
                value: None,
            },
            Expr::Call {
                sig: "demo::f".parse().unwrap(),
                args: vec![Expr::var("x")],
            },
            Expr::Let {
                name: id("y"),
                value: x(),
                body: x(),
            },
            Expr::If {
                cond: x(),
                then: x(),
                otherwise: x(),
            },
            Expr::MapThrough {
                functor: Functor::Optional,
                value: x(),
                var: id("y"),
                body: x(),
            },
            Expr::Prim {
                op: PrimOp::Le,
                args: vec![Expr::var("x"), Expr::var("y")],
            },
            Expr::Fold {
                list: x(),
                init: x(),
                ty: Some(Type::I64.optional()),
                acc: id("a"),
                item: id("i"),
                body: x(),
            },
            Expr::Case {
                value: x(),
                var: id("y"),
                some: x(),
                none: x(),
            },
            Expr::Match {
                ty: TypeRef::local(id("Size")),
                value: x(),
                arms: vec![MatchArm {
                    tag: id("large"),
                    var: Some(id("n")),
                    body: Expr::var("n"),
                }],
            },
        ];

        for form in &every_form {
            check(&ws, &as_value(form), &expr);
        }
        assert_eq!(every_form.len(), 15, "one of every form");
    }

    #[test]
    fn a_nested_type_is_described_all_the_way_down() {
        let ws = ws();
        let deep = Type::Union {
            members: vec![
                Type::String.optional().list(),
                Type::I64.map_from(Type::String).future(),
            ],
        };
        check(&ws, &as_value(&deep), &named("Type"));
    }

    #[test]
    fn every_kind_of_declaration_is_described() {
        let ws = ws();
        let source = r#"{"name":"demo","types":[
            {"k":"interface","name":"Identified","fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Shape","implements":["Identified"],
             "fields":[{"name":"id","ty":"str"}]},
            {"k":"struct","name":"Circle","extends":"Shape","doc":"A round one.",
             "fields":[{"name":"kind","ty":{"k":"literal","value":{"str":"circle"}}},
                       {"name":"radius","ty":"float"},{"name":"note","ty":"str?"}]},
            {"k":"struct","name":"Box","params":[{"name":"T"}],
             "fields":[{"name":"item","ty":{"k":"param","name":"T"}}]},
            {"k":"enum","name":"Colour","variants":[
                {"name":"Red"},{"name":"Custom","payload":"str"}]},
            {"k":"alias","name":"AnyShape",
             "target":{"k":"union","members":["Circle","Shape"]}}]}"#;

        let demo = crate::Universe::from_json(source).expect("reads");
        for def in &demo.types {
            check(&ws, &as_value(def), &named("TypeDef"));
        }
    }

    #[test]
    fn a_proposition_is_described_including_the_ways_it_can_be_written() {
        let ws = ws();
        let demo = crate::Universe::from_json(
            r#"{"name":"demo","functions":[
                {"name":"sort","params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"},
                {"name":"mergesort","refines":"demo::sort",
                 "params":[{"name":"xs","ty":"[int]"}],"ret":"[int]"},
                {"name":"load","doc":"Fetches one.",
                 "params":[{"name":"id","ty":"str"}],
                 "ret":{"k":"future","of":"str?"}},
                {"name":"identity","params_decl":[{"name":"T"}],
                 "params":[{"name":"v","ty":{"k":"param","name":"T"}}],
                 "ret":{"k":"param","name":"T"}}]}"#,
        )
        .expect("reads");

        for sig in &demo.functions {
            check(&ws, &as_value(sig), &named("FunctionSig"));
        }
    }

    #[test]
    fn a_whole_workspace_is_a_value_of_a_type_it_declares() {
        // The strongest form of the claim: a real workspace, with universes,
        // inheritance, generics, links, and a body, checked against what this
        // says a workspace is.
        let ws = ws();
        let real = Workspace::from_json(
            r#"{"universes":[
                {"name":"geo","doc":"Where things are.",
                 "types":[{"k":"struct","name":"Point",
                    "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}]},
                {"name":"gfx",
                 "types":[{"k":"struct","name":"Vec2",
                    "fields":[{"name":"x","ty":"float"},{"name":"y","ty":"float"}]}],
                 "functions":[{"name":"origin","params":[],"ret":"Vec2",
                    "body":{"k":"make_record","ty":"Vec2","fields":[
                        {"name":"x","value":{"k":"lit","value":0.0}},
                        {"name":"y","value":{"k":"lit","value":0.0}}]}}]}],
              "links":[{"name":"point_is_a_vec2","from":"geo::Point","to":"gfx::Vec2",
                        "kind":{"k":"identical"}}]}"#,
        )
        .expect("reads");
        let normalized = crate::canon::normalize_workspace(real);

        check(&ws, &as_value(&normalized), &named("Workspace"));
    }

    #[test]
    fn a_link_is_described_however_it_is_justified() {
        let ws = ws();
        let workspace = Workspace::from_json(
            r#"{"universes":[
                {"name":"a","types":[{"k":"struct","name":"X",
                    "fields":[{"name":"id","ty":"str"},{"name":"n","ty":"i32"}]}]},
                {"name":"b","types":[{"k":"struct","name":"Y",
                    "fields":[{"name":"ident","ty":"str"},{"name":"n","ty":"i64"}]}]}],
              "links":[
                {"name":"plain","from":"a::X","to":"b::Y","kind":{"k":"identical"}},
                {"name":"mapped","from":"a::X","to":"b::Y","kind":{"k":"field_map",
                  "dropped":["extra"],
                  "pairs":[
                    {"from":"id","to":"ident","coercion":{"k":"identity"}},
                    {"from":"n","to":"n","coercion":{"k":"widen_int"}},
                    {"from":"n","to":"n","coercion":{"k":"through","functor":"list",
                        "of":{"k":"via_link","link":"plain"}}}]}},
                {"name":"witnessed","from":"a::X","to":"b::Y",
                 "kind":{"k":"witness","sig":"a::convert"}},
                {"name":"renamed","from":"a::X","to":"b::Y","kind":{"k":"equivalent",
                  "pairs":[
                    {"from":"id","to":"ident","coercion":{"k":"identity"}},
                    {"from":"n","to":"n","coercion":{"k":"via_link","link":"back",
                        "reversed":true}}]}},
                {"name":"left_open","from":"a::X","to":"b::Y",
                 "kind":{"k":"equivalent"}},
                {"name":"first_is_head","from":"a::first","to":"b::head","kind":{"k":"equivalent",
                  "pairs":[{"from":"xs","to":"items"}],
                  "generics":[{"from":"T","to":"E"}]}}]}"#,
        )
        .expect("reads");

        for link in &workspace.links {
            check(&ws, &as_value(link), &named("Link"));
        }
    }

    #[test]
    fn the_description_describes_itself() {
        // If it can describe a workspace, it can describe the one it is in.
        let ws = ws();
        let itself = crate::canon::normalize_workspace(Workspace::of([universe().expect("reads")]));
        check(&ws, &as_value(&itself), &named("Workspace"));
    }

    #[test]
    fn a_type_gets_an_address_and_three_words_like_anything_else() {
        let ws = ws();
        let name = TYPE.parse().unwrap();
        let shape = ws.shape_hash(&name).expect("addressed");
        let words = ws.alias_index().alias_for(&shape).expect("named");

        assert_eq!(
            words.split('-').count(),
            3,
            "the description of a type is a type, and is named like one: {words}"
        );
    }
}
