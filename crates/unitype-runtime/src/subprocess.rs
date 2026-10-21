//! Reaching an environment by starting a program.
//!
//! The simplest way to let another language prove something: run it, hand it
//! one JSON message, and read one back. No linking, no bindings, no foreign
//! function interface — a program that can read standard input and write
//! standard output can participate, which is every language worth naming.
//!
//! # The exchange
//!
//! One request in, one response out, and the process exits:
//!
//! ```text
//! → {"unitype":"1","kind":"invoke","signature":"demo::sort",
//!    "entry":"bubble_sort","args":[[3,1,2]]}
//! ← {"ok":true,"value":[1,2,3]}
//! ```
//!
//! Each message is one line, and end of input ends the stream, so a child that
//! reads lines and a child that reads to the end are both correct here. Nothing
//! has to agree on a length prefix, and a child written in ten lines of Python
//! works by construction.
//!
//! Starting a process per call is real overhead — around twenty milliseconds
//! for Python, against microseconds of work — which is why the default policy
//! prefers to stay in this one. A program calling repeatedly should ask for a
//! worker instead; see [`crate::worker`]. This is the version that cannot get
//! stuck holding state it should not have, which is why it stays the default.

use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::Type;
use unitype_core::value::Value;

use crate::environment::{Environment, Reach};
use crate::error::ExecError;
use crate::implementation::Implementation;

/// The wire protocol version this build speaks.
pub const PROTOCOL_VERSION: &str = "1";

/// How much of what a failing child said is worth keeping.
const STDERR_KEPT: usize = 8 * 1024;

/// What the parent sends.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// The protocol version.
    pub unitype: String,
    /// What is being asked for. Only `invoke` for now, but a child that checks
    /// it will keep working when there is more.
    pub kind: String,
    /// The proposition being proved, so a child can log something useful.
    pub signature: String,
    /// Which of the child's functions to call.
    pub entry: String,
    /// The arguments, in declaration order.
    pub args: Vec<serde_json::Value>,
}

/// What the child sends back.
#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    /// Whether it worked.
    pub ok: bool,
    /// What it produced.
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    /// What went wrong.
    #[serde(default)]
    pub error: Option<ResponseError>,
}

/// A failure the child reported for itself.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResponseError {
    /// A short machine-readable tag.
    #[serde(default)]
    pub code: String,
    /// What happened, for a person.
    pub message: String,
}

/// Runs an implementation by starting a program and exchanging one message.
///
/// `params` is the type each argument is written against, in the same order.
pub async fn invoke(
    ws: &CheckedWorkspace,
    environment: &Environment,
    implementation: &Implementation,
    args: Vec<Value>,
    params: &[Type],
    expected: &Type,
) -> Result<Value, ExecError> {
    let Reach::Subprocess {
        command,
        args: argv,
        cwd,
        env,
        timeout_ms,
        ..
    } = &environment.reach
    else {
        return Err(ExecError::Unreachable {
            environment: environment.id.to_string(),
            detail: "this is not a subprocess environment".into(),
        });
    };

    // A trailing newline costs nothing here — JSON ignores it — and means the
    // same bytes work for a child that reads lines, which is what a worker
    // needs. One message, framed two ways at once.
    let mut body = request_line(ws, implementation, &args, params)?;
    body.push(b'\n');

    let mut process = Command::new(command);
    process
        .args(argv)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // If this call is abandoned — a timeout, a cancelled caller — the child
        // goes with it rather than being left behind.
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        process.current_dir(dir);
    }

    let mut child = crate::spawn::spawn(&mut process).map_err(|source| ExecError::Unreachable {
        environment: environment.id.to_string(),
        detail: format!("could not start `{command}`: {source}"),
    })?;

    let mut stdin = child.stdin.take().expect("stdin was piped");
    stdin
        .write_all(&body)
        .await
        .map_err(|source| ExecError::Protocol {
            id: implementation.id.to_string(),
            detail: format!("could not send the request: {source}"),
        })?;
    // Closing this is the end of the message. Without it the child waits.
    drop(stdin);

    let output = tokio::time::timeout(Duration::from_millis(*timeout_ms), child.wait_with_output())
        .await
        .map_err(|_| ExecError::Timeout {
            id: implementation.id.to_string(),
            millis: *timeout_ms,
        })?
        .map_err(|source| ExecError::Unreachable {
            environment: environment.id.to_string(),
            detail: format!("could not read from `{command}`: {source}"),
        })?;

    if !output.status.success() {
        return Err(ExecError::Failed {
            id: implementation.id.to_string(),
            message: format!(
                "exited with {}{}",
                output
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "a signal".into()),
                tail(&output.stderr),
            ),
        });
    }

    let text = String::from_utf8_lossy(&output.stdout);
    read_answer_with(ws, implementation, &text, expected, &tail(&output.stderr))
}

/// The bytes of one request, ready to send.
///
/// Shared by every way of reaching a child, so a worker, a one-shot process
/// and a sandbox are sent exactly the same thing. Each argument is written
/// against its parameter's type, in the typed form: a tuple arrives as the
/// array the child's own code expects, not as an escape it would have to
/// know about. An argument with no type given is written as it would be with
/// no type at all.
pub(crate) fn request_line(
    ws: &CheckedWorkspace,
    implementation: &Implementation,
    args: &[Value],
    params: &[Type],
) -> Result<Vec<u8>, ExecError> {
    let encoded: Vec<serde_json::Value> = args
        .iter()
        .enumerate()
        .map(|(i, a)| match params.get(i) {
            Some(ty) => ws.typed_json(a, ty),
            None => a.to_json(),
        })
        .collect::<Result<_, _>>()
        .map_err(|e| ExecError::Protocol {
            id: implementation.id.to_string(),
            detail: format!("an argument could not be encoded: {e}"),
        })?;

    let request = Request {
        unitype: PROTOCOL_VERSION.to_string(),
        kind: "invoke".to_string(),
        signature: implementation.proves.to_string(),
        entry: implementation.entry.clone(),
        args: encoded,
    };
    Ok(serde_json::to_vec(&request).expect("a request serializes"))
}

/// Reads what a child sent back, against the type its signature promises.
pub(crate) fn read_answer(
    ws: &CheckedWorkspace,
    implementation: &Implementation,
    text: &str,
    expected: &Type,
) -> Result<Value, ExecError> {
    read_answer_with(ws, implementation, text, expected, "")
}

/// The same, with whatever the child said on its error stream to add if the
/// response turns out to be unreadable — which is usually where the reason is.
fn read_answer_with(
    ws: &CheckedWorkspace,
    implementation: &Implementation,
    text: &str,
    expected: &Type,
    context: &str,
) -> Result<Value, ExecError> {
    let response: Response = serde_json::from_str(text).map_err(|source| ExecError::Protocol {
        id: implementation.id.to_string(),
        detail: format!("could not read the response: {source}{context}"),
    })?;

    if !response.ok {
        let reported = response
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "it said it failed but did not say why".to_string());
        return Err(ExecError::Failed {
            id: implementation.id.to_string(),
            message: reported,
        });
    }

    let Some(json) = response.value else {
        return Err(ExecError::Protocol {
            id: implementation.id.to_string(),
            detail: "it said it worked but sent nothing back".into(),
        });
    };

    // Read against the type the signature promises, so that what comes back is
    // interpreted the same way as anything else entering the system.
    ws.read(&json, expected)
        .map_err(|source| ExecError::Protocol {
            id: implementation.id.to_string(),
            detail: format!(
                "what it sent back is not a `{}`: {source}",
                describe(expected)
            ),
        })
}

fn describe(ty: &Type) -> String {
    unitype_core::canon::canonical_string(ty)
}

/// The end of what a failing child said, which is where the reason usually is.
fn tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let kept = if trimmed.len() > STDERR_KEPT {
        &trimmed[trimmed.len() - STDERR_KEPT..]
    } else {
        trimmed
    };
    format!("; it said: {kept}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use unitype_core::check::check_workspace;
    use unitype_core::{BigO, Ident, ImplProfile, Workspace};

    fn ws() -> CheckedWorkspace {
        check_workspace(
            Workspace::from_json(
                r#"{"universes":[{"name":"demo","functions":[
                    {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]}]}"#,
            )
            .expect("reads"),
        )
        .expect("checks")
    }

    /// An environment that runs a shell snippet, so these tests need nothing
    /// installed beyond a shell.
    fn shell(script: &str, timeout_ms: u64) -> Environment {
        Environment {
            id: Ident::new("shell").unwrap(),
            language: Ident::new("sh").unwrap(),
            doc: None,
            reach: Reach::Subprocess {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), script.into()],
                cwd: None,
                env: BTreeMap::new(),
                timeout_ms,
                persistent: false,
            },
            meta: unitype_core::Metadata::default(),
        }
    }

    fn implementation() -> Implementation {
        Implementation::new(
            "sort.shell",
            "demo::sort".parse().unwrap(),
            Ident::new("shell").unwrap(),
            "bubble_sort",
            ImplProfile::new(BigO::ON2, BigO::O1),
        )
    }

    /// Generous on purpose: these tests are about the exchange, not about how
    /// busy the machine is. Only the test that is *about* giving up sets a
    /// short limit.
    const PATIENT: u64 = 30_000;

    async fn run(script: &str) -> Result<Value, ExecError> {
        run_with(script, PATIENT).await
    }

    async fn run_with(script: &str, timeout_ms: u64) -> Result<Value, ExecError> {
        let ws = ws();
        invoke(
            &ws,
            &shell(script, timeout_ms),
            &implementation(),
            vec![Value::List(vec![Value::Int(3), Value::Int(1)])],
            &[Type::I64.list()],
            &Type::I64.list(),
        )
        .await
    }

    /// Short calls started alongside a child that sticks around.
    ///
    /// This is the arrangement that used to hang. Starting a process means
    /// making pipes and then forking, and where those cannot be one atomic
    /// step, a fork landing between them inherits a pipe it has no business
    /// holding. The parent closes its own end to say the request is over, and
    /// the child keeps waiting, because a stranger is still holding the other
    /// copy open.
    ///
    /// Both halves matter, which is why the test has both. `cat` is the
    /// victim: a child reading to end of input hangs where one reading a line
    /// would not notice. The sleeper is the thief, and has to outlive the
    /// patience of the calls it is stealing from — with only short-lived
    /// children around, an inherited descriptor is let go again within
    /// milliseconds and nobody waits long enough to care.
    ///
    /// It is a race, so a green run is weak evidence on its own. The value is
    /// in running it forever after.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn a_long_lived_child_does_not_hold_another_call_s_input_open() {
        // Long enough to outlast the patience below, so a stolen descriptor
        // shows up as a call that never finishes rather than one that is
        // merely slow.
        let thief = tokio::spawn(run_with(
            r#"cat > /dev/null; sleep 2; echo '{"ok":true,"value":[]}'"#,
            30_000,
        ));

        let victims = (0..48).map(|n| async move {
            run_with(
                &format!(r#"cat > /dev/null; echo '{{"ok":true,"value":[{n}]}}'"#),
                900,
            )
            .await
        });

        for (n, got) in futures_join(victims).await.into_iter().enumerate() {
            assert_eq!(
                got.unwrap_or_else(|e| panic!("call {n} should have answered: {e}")),
                Value::List(vec![Value::Int(n as i64)]),
                "each call got its own answer, not somebody else's"
            );
        }
        thief.await.expect("it did not panic").expect("it answers");
    }

    /// Runs everything given to it at once and collects the results in order.
    ///
    /// Hand-rolled because this crate does not depend on `futures`, and one
    /// join is not worth one.
    async fn futures_join<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
        let mut out = Vec::with_capacity(handles.len());
        for handle in handles {
            out.push(handle.await.expect("the call did not panic"));
        }
        out
    }

    #[tokio::test]
    async fn a_program_that_answers_is_believed() {
        let out = run(r#"cat > /dev/null; echo '{"ok":true,"value":[1,3]}'"#)
            .await
            .expect("answers");
        assert_eq!(out, Value::List(vec![Value::Int(1), Value::Int(3)]));
    }

    #[tokio::test]
    async fn the_request_reaches_the_program_intact() {
        // The child echoes back what it was sent, as a string, and we check the
        // parts that matter are in it.
        let ws = ws();
        let out = invoke(
            &ws,
            &shell(
                r#"body=$(cat); printf '{"ok":true,"value":%s}' "$(printf '%s' "$body" | sed 's/\\/\\\\/g; s/"/\\"/g; s/^/"/; s/$/"/')""#,
                PATIENT,
            ),
            &implementation(),
            vec![Value::List(vec![Value::Int(3), Value::Int(1)])],
            &[Type::I64.list()],
            &Type::String,
        )
        .await
        .expect("answers");

        let Value::Str(seen) = out else {
            panic!("expected the request back as a string, got {out:?}");
        };
        assert!(seen.contains(r#""unitype":"1""#), "{seen}");
        assert!(seen.contains(r#""signature":"demo::sort""#), "{seen}");
        assert!(seen.contains(r#""entry":"bubble_sort""#), "{seen}");
        assert!(
            seen.contains("[3,1]"),
            "the arguments should be there: {seen}"
        );
    }

    #[test]
    fn an_argument_is_sent_written_against_its_type() {
        // A child reads JSON the way its own language does, and every language
        // reads a pair as an array. Without the type, the pair would have to
        // say it is one.
        let ws = ws();
        let pair = Value::Tuple(vec![Value::Int(1), Value::Str("a".into())]);
        let pair_type = Type::Tuple {
            items: vec![Type::I64, Type::String],
        };
        let line = request_line(
            &ws,
            &implementation(),
            std::slice::from_ref(&pair),
            &[pair_type],
        )
        .expect("encodes");
        let sent: serde_json::Value = serde_json::from_slice(&line).expect("JSON");
        assert_eq!(sent["args"], serde_json::json!([[1, "a"]]));

        let untyped = request_line(&ws, &implementation(), &[pair], &[]).expect("encodes");
        let sent: serde_json::Value = serde_json::from_slice(&untyped).expect("JSON");
        assert_eq!(sent["args"], serde_json::json!([{"$tuple": [1, "a"]}]));
    }

    #[tokio::test]
    async fn a_program_that_is_not_there_is_an_environment_problem() {
        let ws = ws();
        let err = invoke(
            &ws,
            &Environment::subprocess("nope", "sh", "unitype_no_such_program", &[]),
            &implementation(),
            vec![Value::List(vec![])],
            &[Type::I64.list()],
            &Type::I64.list(),
        )
        .await
        .unwrap_err();

        assert!(
            err.is_environmental(),
            "nothing is wrong with the proof; the machine is missing it: {err}"
        );
    }

    #[tokio::test]
    async fn a_program_reporting_its_own_failure_is_quoted() {
        let err = run(r#"cat > /dev/null; echo '{"ok":false,"error":{"code":"bad","message":"list was too long"}}'"#)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("list was too long"), "{err}");
        assert!(
            !err.is_environmental(),
            "the program worked; what it was asked to do did not"
        );
    }

    #[tokio::test]
    async fn a_program_that_crashes_has_its_last_words_kept() {
        let err = run(r#"cat > /dev/null; echo "something went badly" >&2; exit 3"#)
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("exited with 3"), "{message}");
        assert!(message.contains("something went badly"), "{message}");
    }

    #[tokio::test]
    async fn a_program_that_talks_nonsense_is_a_protocol_failure() {
        let err = run(r#"cat > /dev/null; echo 'not json at all'"#)
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Protocol { .. }), "{err}");
        assert!(
            !err.is_environmental(),
            "a program that cannot speak the protocol will not learn by being asked again"
        );
    }

    #[tokio::test]
    async fn a_program_that_answers_with_the_wrong_shape_is_caught_here_too() {
        let err = run(r#"cat > /dev/null; echo '{"ok":true,"value":"not a list"}'"#)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("is not a"), "{err}");
    }

    #[tokio::test]
    async fn a_program_that_never_answers_is_given_up_on() {
        let err = run_with(r#"cat > /dev/null; sleep 30"#, 150)
            .await
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }), "{err}");
        assert!(
            err.is_environmental(),
            "a hung machine is worth trying around"
        );
    }

    #[tokio::test]
    async fn closing_the_input_is_what_tells_the_program_to_get_on_with_it() {
        // This child reads to the end before answering. If the parent did not
        // close its writing end, it would wait for ever.
        let out = run(r#"cat > /dev/null && echo '{"ok":true,"value":[7]}'"#)
            .await
            .expect("answers");
        assert_eq!(out, Value::List(vec![Value::Int(7)]));
    }
}
