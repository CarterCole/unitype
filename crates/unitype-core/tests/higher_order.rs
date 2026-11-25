//! Higher-order generic types: a parameter that stands for a type constructor,
//! applied to a type, as `M<A>` where `M` may be the list or the optional.
//!
//! The kind rules are checked where a reference is written, a call is worked
//! out from its arguments, and a declaration is addressed, so each is tested
//! from the outside, through a whole workspace.

use serde_json::{Value as Json, json};

use unitype_core::check::{
    CallError, CheckedWorkspace, Diagnostics, View, bind, check_workspace, substitute,
};
use unitype_core::ir::{Functor, Type, TypeParamDecl};
use unitype_core::{Ident, QualifiedName, Workspace};

fn id(s: &str) -> Ident {
    Ident::new(s).unwrap()
}

fn q(s: &str) -> QualifiedName {
    s.parse().unwrap()
}

fn checked(universe: Json) -> Result<CheckedWorkspace, Diagnostics> {
    let json = json!({ "universes": [universe] }).to_string();
    check_workspace(Workspace::from_json(&json).expect("reads"))
}

/// Every error a universe produces, as the text a reader would see.
fn errors(universe: Json) -> Vec<String> {
    match checked(universe) {
        Ok(_) => Vec::new(),
        Err(diags) => diags.errors.iter().map(|e| e.kind.to_string()).collect(),
    }
}

fn param(name: &str) -> Json {
    json!({ "k": "param", "name": name })
}

fn apply(name: &str, args: Vec<Json>) -> Json {
    json!({ "k": "apply", "name": name, "args": args })
}

fn constructor(functor: &str) -> Json {
    json!({ "k": "constructor", "functor": functor })
}

/// `Monad<M>` and the two operations every monad has, over one constructor.
fn monad() -> Json {
    json!({
        "name": "monad",
        "types": [{
            "k": "interface", "name": "Monad",
            "params": [{ "name": "M", "arity": 1 }]
        }],
        "functions": [
            {
                "name": "pure",
                "params_decl": [{ "name": "M", "arity": 1 }, { "name": "A" }],
                "params": [{ "name": "a", "ty": param("A") }],
                "ret": apply("M", vec![param("A")])
            },
            {
                "name": "bind",
                "params_decl": [{ "name": "M", "arity": 1 }, { "name": "A" }, { "name": "B" }],
                "params": [
                    { "name": "m", "ty": apply("M", vec![param("A")]) },
                    {
                        "name": "f",
                        "ty": {
                            "k": "function",
                            "params": [param("A")],
                            "ret": apply("M", vec![param("B")])
                        }
                    }
                ],
                "ret": apply("M", vec![param("B")])
            }
        ]
    })
}

#[test]
fn a_monad_over_a_constructor_checks() {
    assert!(checked(monad()).is_ok(), "{:?}", errors(monad()));
}

#[test]
fn a_higher_order_parameter_must_be_applied_to_a_type() {
    let bare = json!({
        "name": "bad",
        "types": [{
            "k": "struct", "name": "Bare",
            "params": [{ "name": "M", "arity": 1 }],
            "fields": [{ "name": "x", "ty": param("M") }]
        }]
    });
    let found = errors(bare);
    assert!(
        found
            .iter()
            .any(|e| e.contains("must be applied to a type")),
        "{found:?}"
    );
}

#[test]
fn an_ordinary_parameter_takes_no_arguments() {
    let applied = json!({
        "name": "bad",
        "types": [{
            "k": "struct", "name": "Applied",
            "params": [{ "name": "T" }],
            "fields": [{ "name": "x", "ty": apply("T", vec![json!({ "k": "i64" })]) }]
        }]
    });
    let found = errors(applied);
    assert!(
        found.iter().any(|e| e.contains("takes no arguments")),
        "{found:?}"
    );
}

#[test]
fn a_type_constructor_is_refused_where_a_type_is_needed() {
    let loose = json!({
        "name": "bad",
        "types": [{
            "k": "struct", "name": "Loose",
            "fields": [{ "name": "x", "ty": constructor("list") }]
        }]
    });
    let found = errors(loose);
    assert!(found.iter().any(|e| e.contains("not a type")), "{found:?}");
}

#[test]
fn a_higher_order_parameter_is_given_a_constructor_not_a_type() {
    let wrong = json!({
        "name": "bad",
        "types": [
            { "k": "interface", "name": "Monad", "params": [{ "name": "M", "arity": 1 }] },
            {
                "k": "struct", "name": "Uses",
                "fields": [{
                    "name": "m",
                    "ty": { "k": "ref", "name": "Monad", "args": [{ "k": "i64" }] }
                }]
            }
        ]
    });
    let found = errors(wrong);
    assert!(
        found
            .iter()
            .any(|e| e.contains("stands for a type constructor was given a type")),
        "{found:?}"
    );
}

#[test]
fn an_ordinary_parameter_is_not_given_a_constructor() {
    let wrong = json!({
        "name": "bad",
        "types": [
            { "k": "interface", "name": "Shows", "params": [{ "name": "T" }] },
            {
                "k": "struct", "name": "Uses",
                "fields": [{
                    "name": "s",
                    "ty": { "k": "ref", "name": "Shows", "args": [constructor("optional")] }
                }]
            }
        ]
    });
    let found = errors(wrong);
    assert!(
        found
            .iter()
            .any(|e| e.contains("a type constructor was given for a parameter that takes a type")),
        "{found:?}"
    );
}

#[test]
fn an_applied_constructor_takes_exactly_one_argument() {
    let two = json!({
        "name": "bad",
        "types": [{
            "k": "struct", "name": "Two",
            "params": [{ "name": "M", "arity": 1 }],
            "fields": [{
                "name": "x",
                "ty": apply("M", vec![json!({ "k": "i64" }), json!({ "k": "i64" })])
            }]
        }]
    });
    let found = errors(two);
    assert!(
        found.iter().any(|e| e.contains("takes one type argument")),
        "{found:?}"
    );
}

#[test]
fn a_higher_order_parameter_cannot_be_bounded_yet() {
    let bounded = json!({
        "name": "bad",
        "types": [
            { "k": "interface", "name": "Monad", "params": [{ "name": "M", "arity": 1 }] },
            {
                "k": "interface", "name": "Bounded",
                "params": [{
                    "name": "M",
                    "arity": 1,
                    "bounds": [{ "name": "Monad", "args": [] }]
                }]
            }
        ]
    });
    let found = errors(bounded);
    assert!(
        found
            .iter()
            .any(|e| e.contains("cannot be bounded by an interface yet")),
        "{found:?}"
    );
}

#[test]
fn a_higher_order_parameter_may_be_passed_on_to_another() {
    // `Twice<M>` is a monad over `M` built from one, so `M` is forwarded as
    // a constructor, which is allowed; passing it where a type goes is not.
    let forwarded = json!({
        "name": "ok",
        "types": [
            { "k": "interface", "name": "Monad", "params": [{ "name": "M", "arity": 1 }] },
            {
                "k": "interface", "name": "Twice",
                "params": [{ "name": "M", "arity": 1 }],
                "extends": [{ "name": "Monad", "args": [param("M")] }]
            }
        ]
    });
    let found = errors(forwarded);
    assert!(found.is_empty(), "{found:?}");
}

#[test]
fn a_call_reads_the_constructor_off_the_argument() {
    let ws = checked(monad()).expect("checks");
    let f = Type::Function {
        params: vec![Type::I64],
        ret: Box::new(Type::String.list()),
    };
    let bound = ws
        .check_call(&q("monad::bind"), &[Type::I64.list(), f.clone()], None)
        .expect("a list is a monad over its own elements");
    assert_eq!(bound, Type::String.list());

    // The same call over an optional yields an optional.
    let optional_f = Type::Function {
        params: vec![Type::I64],
        ret: Box::new(Type::String.optional()),
    };
    let bound = ws
        .check_call(&q("monad::bind"), &[Type::I64.optional(), optional_f], None)
        .expect("an optional is a monad too");
    assert_eq!(bound, Type::String.optional());

    // A future is a constructor as well.
    let future_f = Type::Function {
        params: vec![Type::I64],
        ret: Box::new(Type::Bool.future()),
    };
    let bound = ws
        .check_call(&q("monad::bind"), &[Type::I64.future(), future_f], None)
        .expect("a future is a monad too");
    assert_eq!(bound, Type::Bool.future());
}

#[test]
fn a_plain_type_gives_no_constructor_to_read() {
    let ws = checked(monad()).expect("checks");
    let f = Type::Function {
        params: vec![Type::I64],
        ret: Box::new(Type::I64),
    };
    let err = ws
        .check_call(&q("monad::bind"), &[Type::I64, f], None)
        .expect_err("an integer is not built with a constructor");
    assert!(
        matches!(
            err,
            CallError::Ambiguous { .. } | CallError::Argument { .. }
        ),
        "{err:?}"
    );
}

#[test]
fn an_explicit_constructor_is_checked_for_its_kind() {
    let ws = checked(monad()).expect("checks");
    // `pure` at `list` and `i64` is the list of an integer.
    let ret = ws
        .check_call(
            &q("monad::pure"),
            &[Type::I64],
            Some(&[
                Type::Constructor {
                    functor: Functor::List,
                },
                Type::I64,
            ]),
        )
        .expect("at a constructor and a type");
    assert_eq!(ret, Type::I64.list());

    // A type given for the constructor is the wrong kind, and is refused.
    let err = ws
        .check_call(
            &q("monad::pure"),
            &[Type::I64],
            Some(&[Type::I64, Type::I64]),
        )
        .expect_err("a type is not a constructor");
    assert!(
        matches!(err, CallError::TypeArgKind { index: 0, .. }),
        "{err:?}"
    );
}

#[test]
fn substituting_a_constructor_applies_it() {
    let subst = bind(
        &[TypeParamDecl::constructor(id("M"))],
        &[Type::Constructor {
            functor: Functor::Optional,
        }],
    );
    let applied = Type::Apply {
        name: id("M"),
        args: vec![Type::I64],
    };
    assert_eq!(substitute(&applied, &subst), Type::I64.optional());
}

#[test]
fn an_applied_parameter_is_equal_only_to_itself() {
    let ws = checked(monad()).expect("checks");
    let m_of_i64 = Type::Apply {
        name: id("M"),
        args: vec![Type::I64],
    };
    let m_of_i32 = Type::Apply {
        name: id("M"),
        args: vec![Type::I32],
    };
    assert!(ws.is_subtype(&m_of_i64, &m_of_i64));
    assert!(!ws.is_subtype(&m_of_i64, &m_of_i32));
    assert!(!ws.is_subtype(&m_of_i64, &Type::I64.list()));
}

#[test]
fn the_wire_form_of_applied_and_constructor_types_round_trips() {
    let applied = Type::Apply {
        name: id("M"),
        args: vec![Type::I64],
    };
    let json = serde_json::to_value(&applied).expect("serializes");
    assert_eq!(
        json,
        json!({ "k": "apply", "name": "M", "args": [{ "k": "i64" }] })
    );
    assert_eq!(
        serde_json::from_value::<Type>(json).expect("reads back"),
        applied
    );

    let ctor = Type::Constructor {
        functor: Functor::List,
    };
    let json = serde_json::to_value(&ctor).expect("serializes");
    assert_eq!(json, json!({ "k": "constructor", "functor": "list" }));
    assert_eq!(serde_json::from_value::<Type>(json).expect("reads"), ctor);
}

#[test]
fn an_ordinary_parameter_does_not_mention_its_arity() {
    // The arity is on the declaration only when it is not zero, so every
    // universe written before higher-order parameters existed reads the same.
    let plain = serde_json::to_value(TypeParamDecl::new(id("T"))).expect("serializes");
    assert_eq!(plain, json!({ "name": "T" }));
    let higher = serde_json::to_value(TypeParamDecl::constructor(id("M"))).expect("serializes");
    assert_eq!(higher, json!({ "name": "M", "arity": 1 }));
}

#[test]
fn a_declaration_names_nothing_in_its_address() {
    // `Monad<M>`'s `M<i64>` and another universe's `F<i64>` hold the same
    // shape, so they share an address whatever the parameter is called.
    let a = json!({
        "name": "a",
        "types": [{
            "k": "struct", "name": "Wrap",
            "params": [{ "name": "M", "arity": 1 }],
            "fields": [{ "name": "x", "ty": apply("M", vec![json!({ "k": "i64" })]) }]
        }]
    });
    let b = json!({
        "name": "b",
        "types": [{
            "k": "struct", "name": "Holder",
            "params": [{ "name": "F", "arity": 1 }],
            "fields": [{ "name": "x", "ty": apply("F", vec![json!({ "k": "i64" })]) }]
        }]
    });
    let both = check_workspace(
        Workspace::from_json(&json!({ "universes": [a, b] }).to_string()).expect("reads"),
    )
    .expect("checks");
    let wrap = both.address(&q("a::Wrap"), View::Keyed).expect("addressed");
    let holder = both
        .address(&q("b::Holder"), View::Keyed)
        .expect("addressed");
    assert_eq!(wrap, holder);
}

#[test]
fn applying_a_different_constructor_is_a_different_shape() {
    let applied_to_i64 = json!({
        "name": "a",
        "types": [{
            "k": "struct", "name": "Wrap",
            "params": [{ "name": "M", "arity": 1 }],
            "fields": [{ "name": "x", "ty": apply("M", vec![json!({ "k": "i64" })]) }]
        }]
    });
    let applied_to_bool = json!({
        "name": "b",
        "types": [{
            "k": "struct", "name": "Wrap",
            "params": [{ "name": "M", "arity": 1 }],
            "fields": [{ "name": "x", "ty": apply("M", vec![json!({ "k": "bool" })]) }]
        }]
    });
    let both = check_workspace(
        Workspace::from_json(
            &json!({ "universes": [applied_to_i64, applied_to_bool] }).to_string(),
        )
        .expect("reads"),
    )
    .expect("checks");
    assert_ne!(
        both.address(&q("a::Wrap"), View::Keyed),
        both.address(&q("b::Wrap"), View::Keyed)
    );
}

#[test]
fn a_parameter_applied_to_a_type_holds_that_type_exactly() {
    // `Holder<M, T>` holds `T` inside `M<T>`, and an unknown constructor may
    // hold its argument in any way, so `T` must not be widened or narrowed.
    let holder = json!({
        "name": "v",
        "types": [{
            "k": "struct", "name": "Holder",
            "params": [{ "name": "M", "arity": 1 }, { "name": "T" }],
            "fields": [{ "name": "x", "ty": apply("M", vec![param("T")]) }]
        }]
    });
    let ws = checked(holder).expect("checks");
    let variance = ws.variance_of(&q("v::Holder")).expect("declared");
    assert_eq!(variance[1], unitype_core::check::Variance::Invariant);
}
