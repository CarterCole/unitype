//! Keeping a process alive, and what that buys.
//!
//! One process per call is correct and simple, and costs an interpreter start
//! every time — around twenty milliseconds for Python, against microseconds of
//! actual work. A program calling repeatedly pays that over and over. This
//! checks that a worker pays it once, and that a worker which goes wrong is
//! dropped rather than reused with its stream in an unknown state.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use unitype_core::check::{CheckedWorkspace, check_workspace};
use unitype_core::value::Value;
use unitype_core::{BigO, Ident, ImplProfile, QualifiedName, Workspace};
use unitype_runtime::environment::Reach;
use unitype_runtime::implementation::Implementation;
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

fn demo_py() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/demo/py/sort_impl.py")
}

fn have_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A dispatcher reaching the demo's Python module, kept alive or not.
fn python(persistent: bool) -> Dispatcher {
    let mut environment = Environment::subprocess(
        "py",
        "python",
        "python3",
        &[demo_py().to_str().expect("a path")],
    );
    if let Reach::Subprocess {
        persistent: keep,
        timeout_ms,
        ..
    } = &mut environment.reach
    {
        *keep = persistent;
        *timeout_ms = 30_000;
    }

    let mut registry = Registry::new();
    registry.add_environment(environment).expect("registers");
    registry
        .register(Implementation::new(
            "sort.py",
            q("demo::sort"),
            Ident::new("py").unwrap(),
            "bubble_sort",
            ImplProfile::new(BigO::ON2, BigO::O1),
        ))
        .expect("registers");

    Dispatcher::new(Arc::new(workspace()), Arc::new(registry))
}

/// A dispatcher reaching a shell snippet, so failure cases need nothing
/// installed beyond a shell.
fn shell(script: &str, timeout_ms: u64) -> Dispatcher {
    let mut environment = Environment::subprocess("sh", "sh", "/bin/sh", &["-c", script]);
    if let Reach::Subprocess {
        persistent,
        timeout_ms: limit,
        ..
    } = &mut environment.reach
    {
        *persistent = true;
        *limit = timeout_ms;
    }

    let mut registry = Registry::new();
    registry.add_environment(environment).expect("registers");
    registry
        .register(Implementation::new(
            "sort.sh",
            q("demo::sort"),
            Ident::new("sh").unwrap(),
            "bubble_sort",
            ImplProfile::new(BigO::ON, BigO::ON),
        ))
        .expect("registers");

    Dispatcher::new(Arc::new(workspace()), Arc::new(registry))
}

async fn sort(dispatcher: &Dispatcher, items: &[i64]) -> Result<Value, String> {
    dispatcher
        .call(&q("demo::sort"), vec![list(items)], &Policy::Default)
        .await
        .map(|answer| answer.value)
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_worker_answers_the_same_way_a_fresh_process_would() {
    if !have_python() {
        eprintln!("no python3 on this machine; skipping");
        return;
    }
    let kept = python(true);
    let fresh = python(false);

    for input in [&[3, 1, 2][..], &[][..], &[7][..], &[9, 8, 7, 6, 5]] {
        assert_eq!(
            sort(&kept, input).await.expect("answers"),
            sort(&fresh, input).await.expect("answers"),
            "keeping the process alive must not change the answer for {input:?}"
        );
    }
}

#[tokio::test]
async fn one_process_serves_every_call() {
    if !have_python() {
        return;
    }
    let kept = python(true);
    assert_eq!(kept.workers().running().await, 0, "nothing starts eagerly");

    for _ in 0..5 {
        sort(&kept, &[3, 1, 2]).await.expect("answers");
    }
    assert_eq!(
        kept.workers().running().await,
        1,
        "five calls, one interpreter"
    );
}

#[tokio::test]
async fn keeping_the_process_alive_is_what_makes_repeated_calls_cheap() {
    if !have_python() {
        return;
    }
    const CALLS: usize = 12;

    // Warm both, so what is measured is the calls rather than the first start.
    let kept = python(true);
    let fresh = python(false);
    sort(&kept, &[1]).await.expect("answers");
    sort(&fresh, &[1]).await.expect("answers");

    let started = Instant::now();
    for _ in 0..CALLS {
        sort(&kept, &[3, 1, 2]).await.expect("answers");
    }
    let with_worker = started.elapsed();

    let started = Instant::now();
    for _ in 0..CALLS {
        sort(&fresh, &[3, 1, 2]).await.expect("answers");
    }
    let starting_each_time = started.elapsed();

    eprintln!("{CALLS} calls: worker {with_worker:?}, a process each time {starting_each_time:?}");
    assert!(
        with_worker * 2 < starting_each_time,
        "a worker should be comfortably faster than starting an interpreter every \
         time — worker {with_worker:?}, fresh {starting_each_time:?}"
    );
}

#[tokio::test]
async fn a_worker_that_stops_answering_is_not_kept() {
    // This one answers once and exits, which is what a crash looks like from
    // out here. The next call must start a new process rather than talking to
    // a closed pipe.
    let dispatcher = shell(r#"read line; printf '{"ok":true,"value":[1]}\n'"#, 30_000);

    assert_eq!(sort(&dispatcher, &[1]).await.expect("answers"), list(&[1]));
    let failed = sort(&dispatcher, &[1]).await;
    assert!(failed.is_err(), "the process is gone: {failed:?}");
    assert_eq!(
        dispatcher.workers().running().await,
        0,
        "a dead worker should not be held on to"
    );
}

#[tokio::test]
async fn a_worker_that_talks_nonsense_is_not_reused() {
    // Once a reply cannot be read, where the stream sits is anybody's guess.
    // Reusing it risks reading this call's leftovers as the next call's answer.
    let dispatcher = shell(
        r#"while read line; do printf 'not json at all\n'; done"#,
        30_000,
    );

    assert!(sort(&dispatcher, &[1]).await.is_err());
    assert_eq!(
        dispatcher.workers().running().await,
        0,
        "a worker that cannot speak the protocol will not learn by being asked again"
    );
}

#[tokio::test]
async fn a_worker_that_stops_answering_in_time_is_killed_rather_than_left() {
    // A late answer arriving after a timeout would be read as the *next*
    // call's result, which is worse than starting over.
    let dispatcher = shell(r#"while read line; do sleep 30; done"#, 200);

    let failed = sort(&dispatcher, &[1]).await;
    assert!(
        failed.as_ref().is_err_and(|e| e.contains("did not answer")),
        "{failed:?}"
    );
    assert_eq!(dispatcher.workers().running().await, 0);
}

#[tokio::test]
async fn a_failure_the_worker_reports_leaves_it_running() {
    // Its own failure is not the pipe's failure: the worker is still fine and
    // still knows where it is in the conversation.
    let dispatcher = shell(
        r#"while read line; do printf '{"ok":false,"error":{"code":"nope","message":"not today"}}\n'; done"#,
        30_000,
    );

    let failed = sort(&dispatcher, &[1]).await;
    assert!(
        failed.as_ref().is_err_and(|e| e.contains("not today")),
        "{failed:?}"
    );
    assert_eq!(
        dispatcher.workers().running().await,
        1,
        "nothing is wrong with the worker, so it should still be there"
    );
}

#[tokio::test]
async fn workers_can_be_stopped_on_purpose() {
    if !have_python() {
        return;
    }
    let kept = python(true);
    sort(&kept, &[1]).await.expect("answers");
    assert_eq!(kept.workers().running().await, 1);

    kept.workers().shutdown().await;
    assert_eq!(kept.workers().running().await, 0);

    // And asking again just starts another.
    sort(&kept, &[1]).await.expect("answers");
    assert_eq!(kept.workers().running().await, 1);
}
