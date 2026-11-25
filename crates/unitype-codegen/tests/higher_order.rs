//! A workspace generic over a type constructor is refused by every backend,
//! by name, rather than written in a way that reads the wrong thing.
//!
//! No backend has a spelling for a parameter that stands for a constructor
//! yet, so the refusal is the whole behavior. It is tested here through the
//! public backends, once for each language, and once more to show that the
//! refusal follows the universes a caller selects: a workspace may hold a
//! higher-order declaration beside plain ones, and asking for only the plain
//! ones still generates.

use serde_json::json;
use unitype_codegen::{GenError, GenOptions, backend};
use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::{Ident, Workspace};

fn ident(s: &str) -> Ident {
    Ident::new(s).unwrap()
}

fn checked(universes: serde_json::Value) -> CheckedWorkspace {
    let json = json!({ "universes": universes }).to_string();
    check_workspace(Workspace::from_json(&json).expect("reads")).expect("checks")
}

fn monad_universe() -> serde_json::Value {
    json!({
        "name": "monad",
        "types": [{
            "k": "interface", "name": "Monad",
            "params": [{ "name": "M", "arity": 1 }]
        }],
        "functions": [{
            "name": "pure",
            "params_decl": [{ "name": "M", "arity": 1 }, { "name": "A" }],
            "params": [{ "name": "a", "ty": { "k": "param", "name": "A" } }],
            "ret": { "k": "apply", "name": "M", "args": [{ "k": "param", "name": "A" }] }
        }]
    })
}

fn plain_universe() -> serde_json::Value {
    json!({
        "name": "plain",
        "types": [{
            "k": "struct", "name": "Point",
            "fields": [{ "name": "x", "ty": "i64" }]
        }]
    })
}

#[test]
fn every_backend_refuses_a_constructor_generic_declaration_by_name() {
    let ws = checked(json!([monad_universe()]));
    for language in ["typescript", "python", "rust", "haskell"] {
        let generator = backend(language).expect("has a backend");
        match generator.generate(&ws, &GenOptions::default()) {
            Err(GenError::Unsupported {
                language: named,
                what,
                ..
            }) => {
                assert_eq!(named, language);
                assert!(what.contains("monad::"), "{what}");
            }
            Err(other) => panic!("{language} refused for the wrong reason: {other}"),
            Ok(_) => panic!("{language} wrote a constructor-generic declaration"),
        }
    }
}

#[test]
fn a_refusal_follows_the_universes_selected() {
    let ws = checked(json!([monad_universe(), plain_universe()]));
    let generator = backend("python").expect("has a backend");

    let plain_only = GenOptions {
        universes: Some(vec![ident("plain")]),
        ..GenOptions::default()
    };
    generator
        .generate(&ws, &plain_only)
        .expect("the plain universe has nothing to refuse");

    let with_monad = GenOptions {
        universes: Some(vec![ident("monad")]),
        ..GenOptions::default()
    };
    assert!(generator.generate(&ws, &with_monad).is_err());
}

#[test]
fn a_constructor_in_an_interface_field_is_refused_too() {
    // An interface may declare fields as a struct does, so a field that applies
    // a constructor is as much a refusal as a signature that does.
    let ws = checked(json!([{
        "name": "holds",
        "types": [{
            "k": "interface", "name": "Holds",
            "params": [{ "name": "M", "arity": 1 }],
            "fields": [{
                "name": "x",
                "ty": { "k": "apply", "name": "M", "args": [{ "k": "i64" }] }
            }]
        }]
    }]));
    let generator = backend("python").expect("has a backend");
    assert!(matches!(
        generator.generate(&ws, &GenOptions::default()),
        Err(GenError::Unsupported { what, .. }) if what.contains("holds::Holds")
    ));
}
