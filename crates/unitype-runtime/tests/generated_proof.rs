//! Generate, register, dispatch — with nothing written by hand in between.
//!
//! The claim that generated code is *reachable* is easy to make and easy to
//! leave untested, because the pieces sit in different crates and each one
//! passes its own tests. So this walks the whole path: ask unitype for a
//! harness, write a proof module that uses it, point a manifest at that module,
//! and call the proposition through the dispatcher.
//!
//! If a backend ever emits a harness that does not speak the protocol, this is
//! where it shows up rather than in somebody's project.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use unitype_codegen::{Backend, GenOptions, python};
use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::value::Value;
use unitype_core::{BigO, Ident, ImplProfile, QualifiedName, Workspace};
use unitype_runtime::environment::Reach;
use unitype_runtime::implementation::Implementation;
use unitype_runtime::{Dispatcher, Environment, Policy, Registry};

const WS: &str = r#"{"universes":[{"name":"demo","types":[
    {"k":"struct","name":"Tally","fields":[
        {"name":"total","ty":"i64"},{"name":"seen","ty":"[str]"}]}],
  "functions":[
    {"name":"tally","params":[{"name":"words","ty":"[str]"}],"ret":"Tally"}]}]}"#;

fn workspace() -> CheckedWorkspace {
    check_workspace(Workspace::from_json(WS).expect("reads")).expect("checks")
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Writes the generated Python out, plus a module that uses its harness.
fn generated_module(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("makes a directory");

    for file in python::Python
        .generate(&workspace(), &GenOptions::default())
        .expect("generates")
    {
        let at = dir.join(file.path.file_name().expect("a file name"));
        std::fs::write(at, file.contents).expect("writes");
    }

    // The only thing written by hand: the proof itself, and the line saying
    // which proposition it proves.
    std::fs::write(
        dir.join("proof.py"),
        r#"from unitype_harness import unitype_main
from demo import Tally
import dataclasses


def tally(words):
    counted = Tally(total=len(words), seen=sorted(set(words)))
    return dataclasses.asdict(counted)


if __name__ == "__main__":
    unitype_main({"tally": tally})
"#,
    )
    .expect("writes the proof");
    dir
}

/// A dispatcher pointed at that module, kept alive or not.
fn dispatcher(dir: &Path, persistent: bool) -> Dispatcher {
    let mut environment = Environment::subprocess("py", "python", "python3", &["proof.py"]);
    if let Reach::Subprocess {
        cwd,
        persistent: keep,
        timeout_ms,
        ..
    } = &mut environment.reach
    {
        *cwd = Some(dir.to_path_buf());
        *keep = persistent;
        *timeout_ms = 30_000;
    }

    let mut registry = Registry::new();
    registry.add_environment(environment).expect("registers");
    registry
        .register(Implementation::new(
            "tally.py",
            "demo::tally".parse::<QualifiedName>().unwrap(),
            Ident::new("py").unwrap(),
            "tally",
            ImplProfile::new(BigO::ON, BigO::ON),
        ))
        .expect("registers");

    Dispatcher::new(Arc::new(workspace()), Arc::new(registry))
}

fn words(items: &[&str]) -> Value {
    Value::List(items.iter().map(|w| Value::Str((*w).into())).collect())
}

#[tokio::test]
async fn a_generated_harness_is_reachable_as_a_proof() {
    if !have_python() {
        eprintln!("no python3 on this machine; skipping");
        return;
    }

    let dir = generated_module("generated_oneshot");
    let answer = dispatcher(&dir, false)
        .call(
            &"demo::tally".parse().unwrap(),
            vec![words(&["a", "b", "a"])],
            &Policy::Default,
        )
        .await
        .expect("answers");

    // Checked against the signature on the way back, so this really is a Tally
    // rather than something Tally-shaped.
    assert_eq!(answer.value.get("total"), Some(&Value::Int(3)));
    assert_eq!(
        answer.value.get("seen"),
        Some(&words(&["a", "b"])),
        "{:?}",
        answer.value
    );
}

#[tokio::test]
async fn the_same_generated_harness_serves_a_worker_without_changing() {
    if !have_python() {
        return;
    }

    // Nothing about the module differs. Reading lines was always going to work
    // for one message or many, which is why there is no mode to get wrong.
    let dir = generated_module("generated_worker");
    let dispatcher = dispatcher(&dir, true);

    for (input, total) in [
        (&["a"][..], 1),
        (&["a", "b"][..], 2),
        (&["a", "b", "c"][..], 3),
    ] {
        let answer = dispatcher
            .call(
                &"demo::tally".parse().unwrap(),
                vec![words(input)],
                &Policy::Default,
            )
            .await
            .expect("answers");
        assert_eq!(answer.value.get("total"), Some(&Value::Int(total)));
    }

    assert_eq!(
        dispatcher.workers().running().await,
        1,
        "three calls, one interpreter"
    );
}

#[tokio::test]
async fn a_generated_proof_is_held_to_the_signature_like_any_other() {
    if !have_python() {
        return;
    }

    let dir = generated_module("generated_wrong");
    // Replace the proof with one that returns the wrong shape. The generated
    // dataclass would have stopped this; going around it should not get past
    // the boundary check.
    std::fs::write(
        dir.join("proof.py"),
        r#"from unitype_harness import unitype_main

if __name__ == "__main__":
    unitype_main({"tally": lambda words: {"total": "not a number"}})
"#,
    )
    .expect("writes");

    let failed = dispatcher(&dir, false)
        .call(
            &"demo::tally".parse().unwrap(),
            vec![words(&["a"])],
            &Policy::Default,
        )
        .await;

    assert!(
        failed
            .as_ref()
            .is_err_and(|e| e.to_string().contains("not a")),
        "{failed:?}"
    );
}
