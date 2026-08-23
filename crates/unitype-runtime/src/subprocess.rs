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
//! End of input is the frame delimiter, which is why the parent closes its
//! writing end and the child reads to the end. Nothing has to agree on a length
//! prefix or a terminator, and a child written in ten lines of Python is
//! correct by construction.
//!
//! Starting a process per call is real overhead, and it is why the default
//! policy prefers to stay in this one. A long-lived worker is the obvious next
//! step; this request shape leaves room for it, and one process per call is the
//! version that cannot get stuck holding state it should not have.

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
pub async fn invoke(
    ws: &CheckedWorkspace,
    environment: &Environment,
    implementation: &Implementation,
    args: Vec<Value>,
    expected: &Type,
) -> Result<Value, ExecError> {
    let Reach::Subprocess {
        command,
        args: argv,
        cwd,
        env,
        timeout_ms,
    } = &environment.reach
    else {
        return Err(ExecError::Unreachable {
            environment: environment.id.to_string(),
            detail: "this is not a subprocess environment".into(),
        });
    };

    let encoded: Vec<serde_json::Value> = args
        .iter()
        .map(|a| a.to_json())
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
    let body = serde_json::to_vec(&request).expect("a request serializes");

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

    let mut child = process.spawn().map_err(|source| ExecError::Unreachable {
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

    let response: Response =
        serde_json::from_slice(&output.stdout).map_err(|source| ExecError::Protocol {
            id: implementation.id.to_string(),
            detail: format!(
                "could not read the response: {source}{}",
                tail(&output.stderr)
            ),
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
            &Type::I64.list(),
        )
        .await
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

    #[tokio::test]
    async fn a_program_that_is_not_there_is_an_environment_problem() {
        let ws = ws();
        let err = invoke(
            &ws,
            &Environment::subprocess("nope", "sh", "unitype_no_such_program", &[]),
            &implementation(),
            vec![Value::List(vec![])],
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
