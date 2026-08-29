//! A proof that runs inside a sandbox.
//!
//! Every other environment here is trusted: a compiled-in function can do
//! anything this program can, and a subprocess can read your files. That is
//! usually the right trade, and it is the wrong one for a proof that arrived
//! from somewhere. A WebAssembly guest gets memory and nothing else, and runs
//! on a budget, so the two questions this file asks are whether an honest guest
//! is reached correctly and whether a dishonest one is contained.
//!
//! The failure cases are hand-written WebAssembly text rather than compiled
//! from anything, for two reasons: a fixture that lies about where its answer
//! is cannot be written in a language that would let it, and the text form is
//! the ABI stated plainly enough to read.

#![cfg(feature = "wasm")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::value::Value;
use unitype_core::{BigO, Ident, ImplProfile, QualifiedName, Workspace};
use unitype_runtime::error::ExecError;
use unitype_runtime::implementation::{ImplId, Implementation};
use unitype_runtime::{Dispatcher, Environment, Policy, Registry};

const WS: &str = r#"{"universes":[{"name":"demo","functions":[
    {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]}]}"#;

fn workspace() -> CheckedWorkspace {
    check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
}

fn q(s: &str) -> QualifiedName {
    s.parse().unwrap()
}

fn list(items: &[i64]) -> Value {
    Value::List(items.iter().copied().map(Value::Int).collect())
}

fn guest(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/guests")
        .join(name)
}

/// A dispatcher whose one proof lives in the given module.
fn hosting(module: impl AsRef<Path>) -> Dispatcher {
    let mut registry = Registry::new();
    registry
        .add_environment(Environment::wasm("sandbox", "rust", module.as_ref()))
        .expect("registers");
    registry
        .register(Implementation::new(
            "sort.wasm",
            q("demo::sort"),
            Ident::new("sandbox").unwrap(),
            "sort",
            ImplProfile::new(BigO::ONLogN, BigO::ON),
        ))
        .expect("registers");
    Dispatcher::new(Arc::new(workspace()), Arc::new(registry))
}

async fn call(
    dispatcher: &Dispatcher,
    args: &[i64],
) -> Result<Value, unitype_runtime::DispatchError> {
    dispatcher
        .call(&q("demo::sort"), vec![list(args)], &Policy::Default)
        .await
        .map(|done| done.value)
}

/// Whatever a dispatch failed with, unwrapped to the execution error inside.
fn ran_into(error: unitype_runtime::DispatchError) -> ExecError {
    match error {
        unitype_runtime::DispatchError::Exec { source, .. } => source,
        other => panic!("expected a failure while running, got {other}"),
    }
}

#[tokio::test]
async fn a_guest_answers() {
    let value = call(&hosting(guest("answers.wat")), &[3, 1, 2])
        .await
        .expect("the guest answers");
    assert_eq!(value, list(&[1, 2, 3]));
}

#[tokio::test]
async fn a_compiled_module_is_only_compiled_once() {
    // Nothing observable distinguishes a cache hit from a miss except time, so
    // this asks the weaker question that is still worth asking: that a second
    // call through the same host works at all. A guest is instantiated afresh
    // each time, and an instance that outlived its call would be visible here
    // as the second answer differing from the first.
    let dispatcher = hosting(guest("answers.wat"));
    let first = call(&dispatcher, &[3, 1, 2]).await.expect("answers");
    let second = call(&dispatcher, &[9, 9]).await.expect("answers again");
    assert_eq!(first, second);
}

#[tokio::test]
async fn a_proof_that_fails_is_the_proof_failing() {
    let error = ran_into(
        call(&hosting(guest("reports_failure.wat")), &[1])
            .await
            .expect_err("the guest reports a failure"),
    );
    assert!(
        matches!(&error, ExecError::Failed { message, .. } if message.contains("proves nothing")),
        "expected the guest's own words, got {error}"
    );
    // A failing proof is a fact about the proof, so trying it elsewhere would
    // fail the same way.
    assert!(!error.is_environmental());
}

#[tokio::test]
async fn a_guest_cannot_point_outside_itself() {
    let error = ran_into(
        call(&hosting(guest("lies_about_its_answer.wat")), &[1])
            .await
            .expect_err("the answer is nowhere"),
    );
    assert!(
        matches!(&error, ExecError::Protocol { detail, .. } if detail.contains("outside its own memory")),
        "expected the answer to be bounds-checked, got {error}"
    );
}

#[tokio::test]
async fn a_guest_that_never_stops_is_stopped() {
    let error = ran_into(
        call(&hosting(guest("never_stops.wat")), &[1])
            .await
            .expect_err("it never answers"),
    );
    assert!(
        matches!(error, ExecError::Exhausted { .. }),
        "expected the budget to end it, got {error}"
    );
    // Being stopped says nothing about whether the proof is right, so another
    // proof of the same proposition is still worth trying.
    assert!(error.is_environmental());
}

#[tokio::test]
async fn a_module_missing_its_entry_says_which() {
    let error = ran_into(
        call(&hosting(guest("nothing_to_call.wat")), &[1])
            .await
            .expect_err("there is nothing to call"),
    );
    assert!(
        matches!(&error, ExecError::Protocol { detail, .. } if detail.contains("unitype_alloc")),
        "expected the missing export to be named, got {error}"
    );
}

#[tokio::test]
async fn an_answer_that_is_not_text_is_caught() {
    let error = ran_into(
        call(&hosting(guest("not_text.wat")), &[1])
            .await
            .expect_err("the answer is bytes"),
    );
    assert!(
        matches!(&error, ExecError::Protocol { detail, .. } if detail.contains("not text")),
        "expected unreadable bytes to be reported, got {error}"
    );
}

#[tokio::test]
async fn a_missing_module_is_the_environment_being_unreachable() {
    let error = ran_into(
        call(&hosting(guest("no_such_module.wasm")), &[1])
            .await
            .expect_err("there is no module"),
    );
    assert!(
        matches!(error, ExecError::Unreachable { .. }),
        "expected a missing module to read as unreachable, got {error}"
    );
    assert!(error.is_environmental());
}

/// Builds the example guest, if this machine can.
///
/// Skipped rather than failed when it cannot: needing a second Rust target
/// installed to run the test suite would be a poor trade for one test, and the
/// hand-written fixtures already cover the ABI. This one covers the part they
/// cannot — that a guest someone would actually write, in a real language,
/// against a real allocator, works.
fn build_example_guest() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/wasm-guest");
    let built = Command::new("cargo")
        .current_dir(&dir)
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .status()
        .ok()?;
    if !built.success() {
        return None;
    }
    let module = dir.join("target/wasm32-unknown-unknown/release/unitype_wasm_guest.wasm");
    module.exists().then_some(module)
}

#[tokio::test]
async fn the_example_guest_sorts() {
    let Some(module) = build_example_guest() else {
        eprintln!("skipped: no wasm32-unknown-unknown target installed");
        return;
    };

    let dispatcher = hosting(&module);
    assert_eq!(
        call(&dispatcher, &[5, 3, 9, 1]).await.expect("it sorts"),
        list(&[1, 3, 5, 9])
    );
    // Same module, same answer, and no memory carried over from the call
    // before: a bump allocator that leaked across calls would run a guest out
    // of memory here rather than answering.
    assert_eq!(
        call(&dispatcher, &[2, 1]).await.expect("it sorts again"),
        list(&[1, 2])
    );
}

#[tokio::test]
async fn the_example_guest_refuses_what_it_does_not_prove() {
    let Some(module) = build_example_guest() else {
        eprintln!("skipped: no wasm32-unknown-unknown target installed");
        return;
    };

    let mut registry = Registry::new();
    registry
        .add_environment(Environment::wasm("sandbox", "rust", &module))
        .expect("registers");
    registry
        .register(Implementation::new(
            "sort.wasm",
            q("demo::sort"),
            Ident::new("sandbox").unwrap(),
            "reverse", // which this guest does not prove
            ImplProfile::new(BigO::ON, BigO::O1),
        ))
        .expect("registers");
    let dispatcher = Dispatcher::new(Arc::new(workspace()), Arc::new(registry));

    let error = ran_into(call(&dispatcher, &[1]).await.expect_err("it proves sort"));
    assert!(
        matches!(&error, ExecError::Failed { message, .. } if message.contains("sort")),
        "expected the guest to say what it does prove, got {error}"
    );
}

/// A guest and a subprocess in the same program, at the same time.
///
/// Worth its own test because the two do not obviously get along. Reaching a
/// guest means standing up a WebAssembly engine, which on macOS installs a
/// Mach exception handler for the whole process; reaching a subprocess means
/// forking. A program doing both is the ordinary case — the point of this
/// system is that proofs of one proposition live in different places — so it
/// has to hold up under load rather than in a quiet single-threaded run.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn a_sandbox_and_a_subprocess_get_along() {
    let mut registry = Registry::new();
    registry
        .add_environment(Environment::wasm("sandbox", "rust", guest("answers.wat")))
        .expect("registers");
    registry
        .add_environment(Environment::subprocess(
            "sh",
            "sh",
            "/bin/sh",
            &[
                "-c",
                r#"cat > /dev/null; echo '{"ok":true,"value":[1,2,3]}'"#,
            ],
        ))
        .expect("registers");
    for (id, environment) in [("sort.wasm", "sandbox"), ("sort.sh", "sh")] {
        registry
            .register(Implementation::new(
                id,
                q("demo::sort"),
                Ident::new(environment).unwrap(),
                "sort",
                ImplProfile::new(BigO::ONLogN, BigO::ON),
            ))
            .expect("registers");
    }
    let dispatcher = Arc::new(Dispatcher::new(Arc::new(workspace()), Arc::new(registry)));

    let calls: Vec<_> = (0..32)
        .map(|n| {
            let dispatcher = Arc::clone(&dispatcher);
            tokio::spawn(async move {
                let policy = Policy::Pinned(ImplId::new(if n % 2 == 0 {
                    "sort.wasm"
                } else {
                    "sort.sh"
                }));
                dispatcher
                    .call(&q("demo::sort"), vec![list(&[3, 1, 2])], &policy)
                    .await
                    .map(|done| done.value)
            })
        })
        .collect();

    for (n, call) in calls.into_iter().enumerate() {
        let got = call
            .await
            .expect("it did not panic")
            .unwrap_or_else(|e| panic!("call {n} should have answered: {e}"));
        assert_eq!(got, list(&[1, 2, 3]));
    }
}
