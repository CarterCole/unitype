//! Keeping a process alive between calls.
//!
//! Starting an interpreter costs more than most calls do — around twenty
//! milliseconds for Python, against microseconds of actual work — so a program
//! calling repeatedly pays that over and over for nothing. A worker pays it
//! once and then answers as fast as a pipe allows.
//!
//! # What changes, and what does not
//!
//! The messages are the same ones §7.1 of the protocol specifies. What changes
//! is only the framing: with the stream staying open, end of input can no
//! longer mean end of message, so each message is a line. A child that reads
//! lines works either way — one-shot, the parent closes the stream and the loop
//! ends after a single message — which is why the generated harnesses do that
//! and need no mode of their own.
//!
//! # What it costs
//!
//! State survives between calls. That is the whole saving and the whole risk:
//! a module holding something over from the last call is now able to get it
//! wrong, where one process per call could not. Hence off by default, and hence
//! this: a worker that fails, times out, or dies is **dropped rather than
//! reused**. Once an exchange goes wrong the stream's position is unknown, and
//! a stale answer read as the next call's result would be far worse than
//! starting again.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use unitype_core::Ident;
use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::Type;
use unitype_core::value::Value;

use crate::environment::{Environment, Reach};
use crate::error::ExecError;
use crate::implementation::Implementation;
use crate::subprocess::request_line;

/// One live process, and the ends of its pipes.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl Worker {
    /// Starts one.
    ///
    /// Its error stream is inherited rather than piped: nothing here reads it,
    /// and an unread pipe fills up and stops the child. Inheriting also puts a
    /// worker's complaints where a person will see them.
    fn start(environment: &Environment) -> Result<Self, ExecError> {
        let Reach::Subprocess {
            command,
            args,
            cwd,
            env,
            ..
        } = &environment.reach
        else {
            return Err(ExecError::Unreachable {
                environment: environment.id.to_string(),
                detail: "this is not a subprocess environment".into(),
            });
        };

        let mut process = Command::new(command);
        process
            .args(args)
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            process.current_dir(dir);
        }

        let mut child =
            crate::spawn::spawn(&mut process).map_err(|source| ExecError::Unreachable {
                environment: environment.id.to_string(),
                detail: format!("could not start `{command}`: {source}"),
            })?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout was piped")).lines();
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    /// Sends one message and reads one back.
    async fn exchange(&mut self, line: &[u8]) -> Result<String, ExecError> {
        self.stdin
            .write_all(line)
            .await
            .map_err(|e| ExecError::Protocol {
                id: "worker".into(),
                detail: format!("could not send the request: {e}"),
            })?;
        self.stdin.flush().await.map_err(|e| ExecError::Protocol {
            id: "worker".into(),
            detail: format!("could not send the request: {e}"),
        })?;

        match self.stdout.next_line().await {
            Ok(Some(answer)) => Ok(answer),
            // The stream ended, which for a worker means it died.
            Ok(None) => Err(ExecError::Unreachable {
                environment: "worker".into(),
                detail: "it stopped without answering".into(),
            }),
            Err(e) => Err(ExecError::Protocol {
                id: "worker".into(),
                detail: format!("could not read the response: {e}"),
            }),
        }
    }
}

/// The workers this process is keeping alive, one lane per environment.
///
/// A lane is locked for the length of an exchange, so calls into one worker
/// queue up — which they must, since two messages down one pipe have no way to
/// be told apart. Different environments do not wait on each other.
#[derive(Default)]
pub struct Workers {
    lanes: Mutex<HashMap<Ident, Arc<Mutex<Option<Worker>>>>>,
}

impl Workers {
    /// A pool holding nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs an implementation on the worker for its environment, starting one
    /// if there is not already one running.
    pub async fn invoke(
        &self,
        ws: &CheckedWorkspace,
        environment: &Environment,
        implementation: &Implementation,
        args: Vec<Value>,
        params: &[Type],
        expected: &Type,
    ) -> Result<Value, ExecError> {
        let lane = {
            let mut lanes = self.lanes.lock().await;
            Arc::clone(
                lanes
                    .entry(environment.id.clone())
                    .or_insert_with(|| Arc::new(Mutex::new(None))),
            )
        };
        let mut held = lane.lock().await;

        if held.is_none() {
            *held = Some(Worker::start(environment)?);
        }
        let worker = held.as_mut().expect("just started");

        let mut line = request_line(ws, implementation, &args, params)?;
        line.push(b'\n');

        let timeout = environment.reach.timeout_ms().unwrap_or(30_000);
        let answer = match tokio::time::timeout(
            Duration::from_millis(timeout),
            worker.exchange(&line),
        )
        .await
        {
            Ok(Ok(answer)) => answer,
            Ok(Err(failure)) => {
                // Something went wrong mid-exchange, so where the stream sits
                // is anybody's guess. Start again next time.
                *held = None;
                return Err(failure);
            }
            Err(_) => {
                // Same reasoning, and more urgently: a late answer arriving
                // after a timeout would be read as the *next* call's result.
                if let Some(mut dead) = held.take() {
                    let _ = dead.child.start_kill();
                }
                return Err(ExecError::Timeout {
                    id: implementation.id.to_string(),
                    millis: timeout,
                });
            }
        };

        crate::subprocess::read_answer(ws, implementation, &answer, expected).inspect_err(
            |failure| {
                // A worker that cannot speak the protocol will not learn by
                // being asked again.
                if matches!(failure, ExecError::Protocol { .. }) {
                    *held = None;
                }
            },
        )
    }

    /// Stops every worker.
    ///
    /// Dropping the pool does this too, since each child is killed with its
    /// handle; this is for stopping them at a moment of your choosing.
    pub async fn shutdown(&self) {
        let mut lanes = self.lanes.lock().await;
        for lane in lanes.values() {
            if let Some(mut worker) = lane.lock().await.take() {
                let _ = worker.child.start_kill();
            }
        }
        lanes.clear();
    }

    /// How many workers are running.
    pub async fn running(&self) -> usize {
        let lanes = self.lanes.lock().await;
        let mut count = 0;
        for lane in lanes.values() {
            if lane.lock().await.is_some() {
                count += 1;
            }
        }
        count
    }
}
