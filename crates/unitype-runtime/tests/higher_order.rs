//! The free-theorem harness does not claim a theorem it cannot check.
//!
//! A proposition generic over a type constructor has a free theorem, but the
//! harness fills parameters with types, and it does not yet choose
//! constructors to fill one with. So it says so, by name, instead of checking
//! something narrower and calling it the theorem.

use serde_json::json;
use unitype_core::check::check_workspace;
use unitype_core::{QualifiedName, Workspace};
use unitype_runtime::parametric::{ParametricError, applies};

fn workspace(declaration: serde_json::Value) -> unitype_core::check::CheckedWorkspace {
    let json = json!({ "universes": [{
        "name": "monad",
        "functions": [declaration]
    }] })
    .to_string();
    check_workspace(Workspace::from_json(&json).expect("reads")).expect("checks")
}

#[test]
fn a_proposition_over_a_constructor_is_not_claimed_checkable() {
    let ws = workspace(json!({
        "name": "pure",
        "params_decl": [{ "name": "M", "arity": 1 }, { "name": "A" }],
        "params": [{ "name": "a", "ty": { "k": "param", "name": "A" } }],
        "ret": { "k": "apply", "name": "M", "args": [{ "k": "param", "name": "A" }] }
    }));
    let name: QualifiedName = "monad::pure".parse().unwrap();
    let err = applies(&ws, &name).expect_err("not checkable yet");
    assert!(
        matches!(&err, ParametricError::Unfillable { what, why, .. }
            if what.contains("`M`") && why.contains("type constructor")),
        "{err}"
    );
}

#[test]
fn a_plain_generic_proposition_is_still_checkable() {
    let ws = workspace(json!({
        "name": "first",
        "params_decl": [{ "name": "T" }],
        "params": [{ "name": "xs", "ty": { "k": "list", "of": { "k": "param", "name": "T" } } }],
        "ret": { "k": "optional", "of": { "k": "param", "name": "T" } }
    }));
    let name: QualifiedName = "monad::first".parse().unwrap();
    applies(&ws, &name).expect("an ordinary parameter is filled with types");
}
