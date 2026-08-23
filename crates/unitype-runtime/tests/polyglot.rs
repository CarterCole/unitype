//! Two languages proving one proposition, and the policy choosing between them.
//!
//! This is the arrangement the whole system exists to make possible, exercised
//! end to end: a Rust mergesort compiled into this process and a Python bubble
//! sort reached by starting an interpreter, both claiming to prove `demo::sort`.
//! Ask for speed and the Rust one runs. Ask for a small footprint and the
//! Python one runs, over a pipe, in another language. The answer is the same.
//!
//! Nothing above the dispatcher knew which it was getting, which is what makes
//! the choice a policy decision rather than an architectural one.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName, Universe, Workspace};
use unitype_runtime::{Dispatcher, Policy, Registry};

fn demo_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/demo")
}

fn q(s: &str) -> QualifiedName {
    s.parse().unwrap()
}

fn list(items: &[i64]) -> Value {
    Value::List(items.iter().copied().map(Value::Int).collect())
}

fn ints(value: &Value) -> Vec<i64> {
    let Value::List(items) = value else {
        return Vec::new();
    };
    items.iter().filter_map(Value::as_int).collect()
}

/// The demo project's universe, as it ships.
fn workspace() -> CheckedWorkspace {
    let text = std::fs::read_to_string(demo_dir().join("universes/demo.json"))
        .expect("the demo universe ships with the repository");
    let universe = Universe::from_json(&text).expect("it reads");
    check_workspace(Workspace::of([universe])).expect("and it checks")
}

/// Whether a Python interpreter is on this machine.
///
/// The point of the subprocess arrangement is that the other language is not
/// this program's problem — which cuts both ways, so a machine without Python
/// runs the rest of this file and says so.
fn python_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Both proofs registered: one compiled in, one declared by a manifest.
fn dispatcher() -> Dispatcher {
    let ws = Arc::new(workspace());
    let mut registry = Registry::new();

    // The function itself has to be compiled in — there is no loading a Rust
    // function out of a file — so this process offers it under a name.
    registry.register_native_sync("builtin::merge_sort", |args| {
        let mut items = ints(args.first().unwrap_or(&Value::Null));
        items.sort_unstable();
        Ok(Value::List(items.into_iter().map(Value::Int).collect()))
    });

    // Everything else comes from the manifest: which propositions are proved,
    // by what, at what cost, and in which environment. This process is told;
    // it does not know anything about Python, or about which of its own
    // functions matter, until it reads that.
    registry
        .load_manifest_file(&demo_dir().join("impls/demo.json"))
        .expect("the demo manifest ships with the repository");

    // Bodies prove themselves.
    registry.register_bodies(&ws).expect("registers");

    Dispatcher::new(ws, Arc::new(registry))
}

#[tokio::test]
async fn one_proposition_two_languages_and_the_policy_decides() {
    if !python_available() {
        eprintln!("no python3 on this machine; skipping the polyglot half");
        return;
    }

    let dispatcher = dispatcher();
    let unsorted = list(&[5, 3, 9, 1, 7, 2]);
    let sorted = list(&[1, 2, 3, 5, 7, 9]);

    let fast = dispatcher
        .call(&q("demo::sort"), vec![unsorted.clone()], &Policy::Fastest)
        .await
        .expect("answers");
    let frugal = dispatcher
        .call(&q("demo::sort"), vec![unsorted], &Policy::MinMemory)
        .await
        .expect("answers");

    assert_eq!(fast.implementation.as_str(), "sort.rust_merge");
    assert_eq!(fast.environment.as_str(), "here");

    assert_eq!(frugal.implementation.as_str(), "sort.py_bubble");
    assert_eq!(
        frugal.environment.as_str(),
        "python",
        "the frugal proof is in another language, over a pipe"
    );

    assert_eq!(fast.value, sorted);
    assert_eq!(
        frugal.value, sorted,
        "which proof ran should not change what is true"
    );
}

#[tokio::test]
async fn a_caller_can_see_which_proof_answered_and_where() {
    if !python_available() {
        return;
    }

    let dispatcher = dispatcher();
    let answer = dispatcher
        .call(&q("demo::sort"), vec![list(&[2, 1])], &Policy::MinMemory)
        .await
        .expect("answers");

    assert_eq!(answer.considered.len(), 1, "the first choice worked");
    let chosen = &answer.considered[0];
    assert_eq!(chosen.id.as_str(), "sort.py_bubble");
    assert!(
        chosen.cost.contains("O(n^2)"),
        "and it says what it costs: {}",
        chosen.cost
    );
}

#[tokio::test]
async fn a_proof_in_another_language_is_still_held_to_the_signature() {
    if !python_available() {
        return;
    }

    let err = dispatcher()
        .call(
            &q("demo::sort"),
            vec![Value::Str("not a list".into())],
            &Policy::MinMemory,
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("argument 0"),
        "the boundary check happens before Python is even started: {err}"
    );
}

#[tokio::test]
async fn what_python_sends_back_is_read_against_the_type_it_promised() {
    if !python_available() {
        return;
    }

    let answer = dispatcher()
        .call(&q("demo::sort"), vec![list(&[3, 1, 2])], &Policy::MinMemory)
        .await
        .expect("answers");

    let ws = workspace();
    let ty = unitype_core::ir::Type::I64.list();
    assert!(
        ws.validate(&answer.value, &ty).is_ok(),
        "what came back over the pipe is a real List<i64>: {:?}",
        answer.value
    );
}

#[tokio::test]
async fn a_body_needs_no_language_at_all() {
    // `label_of` is written as an expression rather than implemented, so it
    // runs here with nothing generated and nobody's interpreter started.
    let answer = dispatcher()
        .call(
            &q("demo::label_of"),
            vec![
                Value::parse_json(r#"{"id":"r-1","kind":"rect","width":3.0,"height":4.0}"#)
                    .expect("reads"),
            ],
            &Policy::Default,
        )
        .await
        .expect("answers");

    assert_eq!(answer.value.get("text"), Some(&Value::Str("r-1".into())));
    assert_eq!(answer.value.get("sides"), Some(&Value::Int(4)));
    assert_eq!(answer.environment.as_str(), "here");
}

#[tokio::test]
async fn the_manifest_says_where_python_lives_relative_to_itself() {
    let dispatcher = dispatcher();
    let environment = dispatcher
        .registry()
        .environment(&Ident::new("python").unwrap())
        .expect("the manifest introduced it");

    let unitype_runtime::Reach::Subprocess { cwd, args, .. } = &environment.reach else {
        panic!("expected a subprocess environment");
    };
    assert_eq!(args[0], "../py/sort_impl.py");
    assert!(
        cwd.as_deref()
            .is_some_and(|d| d.ends_with(Path::new("impls"))),
        "so the relative path means the same thing wherever this runs: {cwd:?}"
    );
}

#[test]
fn everything_registered_still_proves_something_that_is_asked_for() {
    let ws = workspace();
    let warnings = dispatcher().registry().check_against(&ws);
    assert!(
        warnings.is_empty(),
        "the demo's proofs should match the demo's propositions: {warnings:?}"
    );
}
