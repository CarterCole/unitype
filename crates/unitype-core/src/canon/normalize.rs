//! Normalization: reducing every type to the one form that stands for it.
//!
//! Two people describing the same thing will write it differently — `A | B` and
//! `B | A`, `T??` and `T?`, a union with a redundant member. Normalization
//! rewrites all of those into a single representative, which is what makes
//! content addressing possible: if the canonical forms differ, the types really
//! do differ.
//!
//! The rules are purely syntactic. Nothing here consults a workspace or decides
//! a subtyping question, so a type can be normalized in isolation, and the
//! result never depends on what else happens to be loaded. Semantic redundancy
//! — `i32 | i64` collapsing, say — is left to the checker, precisely so that
//! canonical form stays context-free.
//!
//! # The rules
//!
//! - Nested unions flatten; `never` members drop; a member of `any` swallows
//!   the union; an empty union is `never` and a single-member union is that
//!   member.
//! - A union containing `unit` or an optional member becomes an optional union:
//!   after normalization, no union directly contains either.
//! - `true | false` becomes `bool`, and a literal is dropped when the union
//!   already admits its base type.
//! - Members are deduplicated and ordered by their own canonical bytes, which
//!   is a total order that needs no separate ranking table.
//! - `T??` collapses to `T?`; `never?` and `unit?` become `unit`; `any?` is
//!   `any`.
//! - A tuple of one is its element and a tuple of none is `unit`.
//! - Anonymous record fields sort by name. Struct fields do **not**: their
//!   order is declared and drives generated code.
//! - `Future<Future<T>>` is left alone. It is a distinct type in Rust, and
//!   flattening belongs to the backends whose languages flatten it.

use std::collections::VecDeque;

use crate::canon::encode::canonical_bytes;
use crate::ident::Ident;
use crate::ir::expr::{Expr, FieldInit};
use crate::ir::{
    AliasDef, EnumDef, Field, FunctionSig, InterfaceDef, LiteralValue, StructDef, Type, TypeDef,
    TypeParamDecl, TypeRef, Universe, Workspace,
};

/// Reduces a type to its canonical form.
pub fn normalize_type(t: &Type) -> Type {
    let kids: Vec<Type> = t.children().into_iter().map(normalize_type).collect();
    let node = t.with_children(kids);
    match node {
        Type::Union { members } => normalize_union(members),
        Type::Optional { of } => normalize_optional(*of),
        Type::Tuple { items } => normalize_tuple(items),
        Type::Record { fields } => normalize_record(fields),
        other => other,
    }
}

/// Wraps an already-normalized type in `Optional`, applying the collapses.
fn normalize_optional(inner: Type) -> Type {
    match inner {
        // Every one of these already admits absence, or has nothing to admit.
        Type::Any => Type::Any,
        Type::Never | Type::Unit => Type::Unit,
        already @ Type::Optional { .. } => already,
        other => Type::Optional {
            of: Box::new(other),
        },
    }
}

fn normalize_tuple(items: Vec<Type>) -> Type {
    match items.len() {
        0 => Type::Unit,
        1 => items.into_iter().next().expect("length checked"),
        _ => Type::Tuple { items },
    }
}

fn normalize_record(mut fields: Vec<Field>) -> Type {
    // Stable, so duplicate names end up adjacent for the checker to find rather
    // than being silently merged here.
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    Type::Record { fields }
}

/// Builds an already-normalized union from members that are themselves already
/// normalized.
fn normalize_union(members: Vec<Type>) -> Type {
    let mut optional = false;
    let mut flat: Vec<Type> = Vec::with_capacity(members.len());

    let mut queue: VecDeque<Type> = members.into();
    while let Some(member) = queue.pop_front() {
        match member {
            Type::Union { members } => queue.extend(members),
            // Hoisting optionality out to the union keeps `T? | U` and `T | U?`
            // from being two different spellings of one type.
            Type::Optional { of } => {
                optional = true;
                queue.push_back(*of);
            }
            Type::Unit => optional = true,
            Type::Never => {}
            other => flat.push(other),
        }
    }

    if flat.iter().any(|t| matches!(t, Type::Any)) {
        return Type::Any;
    }

    complete_booleans(&mut flat);
    absorb_literals(&mut flat);

    flat.sort_by_key(canonical_bytes);
    flat.dedup_by(|a, b| a == b);

    let joined = match flat.len() {
        0 => Type::Never,
        1 => flat.into_iter().next().expect("length checked"),
        _ => Type::Union { members: flat },
    };

    if optional {
        normalize_optional(joined)
    } else {
        joined
    }
}

/// `true | false` is every boolean, so it is `bool`.
fn complete_booleans(members: &mut Vec<Type>) {
    let has = |b: bool, members: &[Type]| {
        members
            .iter()
            .any(|t| matches!(t, Type::Literal { value: LiteralValue::Bool(v) } if *v == b))
    };
    if has(true, members) && has(false, members) {
        members.retain(|t| {
            !matches!(
                t,
                Type::Literal {
                    value: LiteralValue::Bool(_)
                }
            )
        });
        members.push(Type::Bool);
    }
}

/// A literal adds nothing to a union that already admits its base type.
fn absorb_literals(members: &mut Vec<Type>) {
    let bases: Vec<Type> = members
        .iter()
        .filter(|t| !matches!(t, Type::Literal { .. }))
        .cloned()
        .collect();

    members.retain(|member| {
        let Type::Literal { value } = member else {
            return true;
        };
        let subsumed = bases.contains(&value.base())
            || matches!(
                (value, bases.contains(&Type::I32)),
                (LiteralValue::Int(i), true) if i32::try_from(*i).is_ok()
            );
        !subsumed
    });
}

/// Reduces a workspace to canonical form: every reference is qualified, every
/// type is normalized, and declarations are ordered by name.
///
/// Qualifying references is what makes a canonical type portable. An
/// unqualified reference means "the universe this was written in", which is not
/// information the type itself carries; once qualified, the type means the same
/// thing wherever it is read.
pub fn normalize_workspace(mut ws: Workspace) -> Workspace {
    for universe in &mut ws.universes {
        normalize_universe(universe);
    }
    ws.universes.sort_by(|a, b| a.name.cmp(&b.name));
    ws.links.sort_by(|a, b| a.name.cmp(&b.name));
    ws
}

/// Reduces one universe to canonical form, resolving its unqualified references
/// against its own name.
pub fn normalize_universe(universe: &mut Universe) {
    let home = universe.name.clone();
    for def in &mut universe.types {
        normalize_def(def, &home);
    }
    for sig in &mut universe.functions {
        normalize_sig(sig, &home);
    }
    universe.types.sort_by(|a, b| a.name().cmp(b.name()));
    universe.functions.sort_by(|a, b| a.name.cmp(&b.name));
}

fn prepare(t: &Type, home: &Ident) -> Type {
    normalize_type(&qualify_type(t, home))
}

fn normalize_def(def: &mut TypeDef, home: &Ident) {
    match def {
        TypeDef::Struct(StructDef {
            params,
            extends,
            implements,
            fields,
            ..
        }) => {
            normalize_param_decls(params, home);
            if let Some(parent) = extends {
                qualify_ref_in_place(parent, home);
            }
            implements
                .iter_mut()
                .for_each(|r| qualify_ref_in_place(r, home));
            // Struct field order is declared, so only the types are rewritten.
            for field in fields {
                field.ty = prepare(&field.ty, home);
            }
        }
        TypeDef::Interface(InterfaceDef {
            params,
            extends,
            fields,
            ..
        }) => {
            normalize_param_decls(params, home);
            extends
                .iter_mut()
                .for_each(|r| qualify_ref_in_place(r, home));
            for field in fields {
                field.ty = prepare(&field.ty, home);
            }
        }
        TypeDef::Enum(EnumDef {
            params, variants, ..
        }) => {
            normalize_param_decls(params, home);
            for variant in variants {
                variant.payload = prepare(&variant.payload, home);
            }
        }
        TypeDef::Alias(AliasDef { params, target, .. }) => {
            normalize_param_decls(params, home);
            *target = prepare(target, home);
        }
    }
}

fn normalize_sig(sig: &mut FunctionSig, home: &Ident) {
    normalize_param_decls(&mut sig.params_decl, home);
    for param in &mut sig.params {
        param.ty = prepare(&param.ty, home);
    }
    sig.ret = prepare(&sig.ret, home);
    if let Some(body) = &mut sig.body {
        qualify_expr_in_place(body, home);
    }
}

fn normalize_param_decls(params: &mut [TypeParamDecl], home: &Ident) {
    for param in params {
        param
            .bounds
            .iter_mut()
            .for_each(|r| qualify_ref_in_place(r, home));
    }
}

/// Names the universe of every reference that did not name one.
fn qualify_type(t: &Type, home: &Ident) -> Type {
    let kids: Vec<Type> = t.children().iter().map(|c| qualify_type(c, home)).collect();
    let mut node = t.with_children(kids);
    if let Type::Ref(r) = &mut node {
        qualify_ref_in_place(r, home);
    }
    node
}

fn qualify_ref_in_place(r: &mut TypeRef, home: &Ident) {
    if r.universe.is_none() {
        r.universe = Some(home.clone());
    }
    for arg in &mut r.args {
        *arg = qualify_type(arg, home);
    }
}

fn qualify_expr_in_place(e: &mut Expr, home: &Ident) {
    match e {
        Expr::Lit { .. } | Expr::Var { .. } => {}
        Expr::Field { of, .. } => qualify_expr_in_place(of, home),
        Expr::MakeRecord { ty, fields } => {
            qualify_ref_in_place(ty, home);
            for FieldInit { value, .. } in fields {
                qualify_expr_in_place(value, home);
            }
        }
        Expr::MakeList { items } | Expr::MakeTuple { items } => {
            items
                .iter_mut()
                .for_each(|i| qualify_expr_in_place(i, home));
        }
        Expr::MakeVariant { ty, value, .. } => {
            qualify_ref_in_place(ty, home);
            if let Some(v) = value {
                qualify_expr_in_place(v, home);
            }
        }
        Expr::Call { args, .. } => {
            args.iter_mut().for_each(|a| qualify_expr_in_place(a, home));
        }
        Expr::Let { value, body, .. } => {
            qualify_expr_in_place(value, home);
            qualify_expr_in_place(body, home);
        }
        Expr::If {
            cond,
            then,
            otherwise,
        } => {
            qualify_expr_in_place(cond, home);
            qualify_expr_in_place(then, home);
            qualify_expr_in_place(otherwise, home);
        }
        Expr::MapList { list, body, .. } => {
            qualify_expr_in_place(list, home);
            qualify_expr_in_place(body, home);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Ident {
        Ident::new(s).unwrap()
    }

    fn union(members: Vec<Type>) -> Type {
        normalize_type(&Type::Union { members })
    }

    fn lit_int(i: i64) -> Type {
        Type::Literal {
            value: LiteralValue::Int(i),
        }
    }

    fn lit_bool(b: bool) -> Type {
        Type::Literal {
            value: LiteralValue::Bool(b),
        }
    }

    #[test]
    fn unions_flatten_and_lose_their_written_order() {
        let a = union(vec![Type::I64, Type::String]);
        let b = union(vec![Type::String, Type::I64]);
        let nested = union(vec![
            Type::Union {
                members: vec![Type::String],
            },
            Type::I64,
        ]);
        assert_eq!(a, b);
        assert_eq!(a, nested);
    }

    #[test]
    fn the_lattice_extremes_absorb_and_vanish() {
        assert_eq!(union(vec![Type::I64, Type::Never]), Type::I64);
        assert_eq!(union(vec![Type::I64, Type::Any]), Type::Any);
        assert_eq!(union(vec![]), Type::Never);
        assert_eq!(union(vec![Type::Bool]), Type::Bool);
    }

    #[test]
    fn optionality_is_hoisted_out_of_unions() {
        let from_member = union(vec![Type::I64.optional(), Type::String]);
        let from_unit = union(vec![Type::I64, Type::Unit, Type::String]);
        assert_eq!(from_member, from_unit);
        let Type::Optional { of } = &from_member else {
            panic!("expected an optional union, got {from_member:?}");
        };
        assert!(matches!(**of, Type::Union { .. }));
    }

    #[test]
    fn both_booleans_together_are_just_bool() {
        assert_eq!(union(vec![lit_bool(true), lit_bool(false)]), Type::Bool);
        assert_eq!(union(vec![lit_bool(true), Type::Bool]), Type::Bool);
        // One of them alone is still a discriminating literal.
        assert_eq!(union(vec![lit_bool(true)]), lit_bool(true));
    }

    #[test]
    fn a_literal_adds_nothing_to_a_union_that_already_admits_it() {
        assert_eq!(union(vec![lit_int(3), Type::I64]), Type::I64);
        assert_eq!(union(vec![Type::lit_str("a"), Type::String]), Type::String);
        assert_eq!(union(vec![lit_int(3), Type::I32]), Type::I32);
        // Out of i32's range, so i32 does not cover it.
        assert!(matches!(
            union(vec![lit_int(i64::MAX), Type::I32]),
            Type::Union { .. }
        ));
    }

    #[test]
    fn discriminating_literals_survive_as_a_union() {
        let tags = union(vec![Type::lit_str("circle"), Type::lit_str("rect")]);
        let Type::Union { members } = &tags else {
            panic!("expected a union of literals, got {tags:?}");
        };
        assert_eq!(members.len(), 2);
    }

    #[test]
    fn optionals_collapse_rather_than_nest() {
        assert_eq!(
            normalize_type(&Type::I64.optional().optional()),
            Type::I64.optional()
        );
        assert_eq!(normalize_type(&Type::Never.optional()), Type::Unit);
        assert_eq!(normalize_type(&Type::Unit.optional()), Type::Unit);
        assert_eq!(normalize_type(&Type::Any.optional()), Type::Any);
    }

    #[test]
    fn futures_keep_their_nesting_because_rust_does() {
        let nested = Type::I64.future().future();
        assert_eq!(normalize_type(&nested), nested);
    }

    #[test]
    fn degenerate_tuples_become_what_they_actually_are() {
        assert_eq!(normalize_type(&Type::Tuple { items: vec![] }), Type::Unit);
        assert_eq!(
            normalize_type(&Type::Tuple {
                items: vec![Type::I64]
            }),
            Type::I64
        );
    }

    #[test]
    fn anonymous_record_fields_sort_but_struct_fields_do_not() {
        let rec = normalize_type(&Type::Record {
            fields: vec![
                Field::new(id("b"), Type::I64),
                Field::new(id("a"), Type::I64),
            ],
        });
        let Type::Record { fields } = &rec else {
            panic!("expected a record");
        };
        assert_eq!(fields[0].name.as_str(), "a");

        let mut def = TypeDef::Struct(StructDef {
            name: id("S"),
            doc: None,
            params: vec![],
            extends: None,
            implements: vec![],
            fields: vec![
                Field::new(id("b"), Type::I64),
                Field::new(id("a"), Type::I64),
            ],
            meta: Default::default(),
        });
        normalize_def(&mut def, &id("u"));
        assert_eq!(
            def.as_struct().unwrap().fields[0].name.as_str(),
            "b",
            "declared field order is semantic and must survive"
        );
    }

    #[test]
    fn normalizing_twice_changes_nothing() {
        let cases = vec![
            union(vec![Type::I64.optional(), Type::String, Type::Never]),
            normalize_type(&Type::I64.optional().optional().list()),
            union(vec![lit_bool(true), lit_bool(false), Type::String]),
        ];
        for t in cases {
            assert_eq!(normalize_type(&t), t, "{t:?} was not already canonical");
        }
    }

    #[test]
    fn references_gain_the_universe_they_were_written_in() {
        let mut universe = Universe::new(id("demo"));
        universe.types.push(TypeDef::Alias(AliasDef {
            name: id("Shapes"),
            doc: None,
            params: vec![],
            target: Type::local_ref("Circle").list(),
            meta: Default::default(),
        }));
        normalize_universe(&mut universe);

        let TypeDef::Alias(alias) = &universe.types[0] else {
            panic!("expected an alias");
        };
        let Type::List { of } = &alias.target else {
            panic!("expected a list");
        };
        let Type::Ref(r) = of.as_ref() else {
            panic!("expected a reference");
        };
        assert_eq!(r.universe.as_ref().unwrap().as_str(), "demo");
    }

    #[test]
    fn an_explicit_universe_is_never_overwritten() {
        let t = qualify_type(
            &Type::Ref(TypeRef::qualified(id("geo"), id("Point"))),
            &id("gfx"),
        );
        let Type::Ref(r) = &t else {
            panic!("expected a reference");
        };
        assert_eq!(r.universe.as_ref().unwrap().as_str(), "geo");
    }
}
