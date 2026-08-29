//! Where code runs.
//!
//! An implementation does not run in a language, it runs in an **environment**:
//! a browser, a Node process, a Python interpreter, this Rust process, a
//! WebAssembly sandbox, something already running on another machine. The
//! language says what the code is written in; the environment says where it
//! executes and what else is available to it there.
//!
//! Keeping the two apart matters because the same code can run in more than one
//! place. A Python implementation might be reached by starting an interpreter,
//! or by asking one that is already running, or by loading CPython compiled to
//! WebAssembly. Same proof, same language, three ways of getting to it — and
//! nothing about the proof changes.
//!
//! # How an environment comes to hold anything
//!
//! An environment loads a module, and the module **registers as an execution
//! source**: it announces which propositions it can prove and how to reach each
//! one. Nothing is discovered by scanning or convention. That is why a program
//! embedding this can be a standalone binary or a library inside someone else's
//! application without either arrangement being a special case — in both, some
//! module said what it could do.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use unitype_core::{Ident, Metadata};

/// How this process gets to an environment.
///
/// Separate from the environment itself: it is the road, not the destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Reach {
    /// The environment is this process.
    ///
    /// Implementations here are Rust functions compiled in, or bodies the
    /// interpreter walks. Nothing is serialized and nothing is spawned.
    Here,
    /// A child process, started when needed and spoken to over its standard
    /// streams.
    Subprocess {
        /// The program to run.
        command: String,
        /// Its arguments.
        #[serde(default)]
        args: Vec<String>,
        /// Where to run it, relative to the manifest that declared it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        /// Environment variables to set.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// How long to wait before giving up.
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
        /// Keep the process alive between calls.
        ///
        /// Starting an interpreter costs more than most calls do — around
        /// twenty milliseconds for Python — so a program calling repeatedly
        /// pays that over and over for nothing. A worker pays it once.
        ///
        /// The cost is that state now survives between calls, which is exactly
        /// what one process per call was free of. Off by default for that reason:
        /// a module that holds nothing between calls says so.
        #[serde(default)]
        persistent: bool,
    },
    /// A WebAssembly module instantiated in this process.
    ///
    /// The only environment that is actually a sandbox: a guest gets memory and
    /// nothing else — no files, no sockets, no clock — and runs on a budget, so
    /// a proof that never finishes stops rather than taking this process with
    /// it. There is no operating system process to kill here, which is why the
    /// budget matters more than a timeout would.
    Wasm {
        /// The module to load.
        module: PathBuf,
        /// How much work the guest may do before it is stopped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fuel: Option<u64>,
    },
    /// Something already running that this process talks to.
    Remote {
        /// Where to find it.
        endpoint: String,
        /// How long to wait before giving up.
        #[serde(default = "default_timeout")]
        timeout_ms: u64,
    },
}

fn default_timeout() -> u64 {
    30_000
}

impl Reach {
    /// A short name for this way of reaching an environment.
    pub fn kind(&self) -> &'static str {
        match self {
            Reach::Here => "here",
            Reach::Subprocess { .. } => "subprocess",
            Reach::Wasm { .. } => "wasm",
            Reach::Remote { .. } => "remote",
        }
    }

    /// How long to wait for an answer, where waiting is a thing that happens.
    pub fn timeout_ms(&self) -> Option<u64> {
        match self {
            Reach::Subprocess { timeout_ms, .. } | Reach::Remote { timeout_ms, .. } => {
                Some(*timeout_ms)
            }
            Reach::Here | Reach::Wasm { .. } => None,
        }
    }

    /// Whether the process is kept alive between calls.
    pub fn is_persistent(&self) -> bool {
        matches!(
            self,
            Reach::Subprocess {
                persistent: true,
                ..
            }
        )
    }

    /// Roughly what it costs to get there, ascending.
    ///
    /// Used only to break ties between proofs that are otherwise equally good,
    /// on the reasoning that not leaving the process beats leaving it. A worker
    /// already running costs less to reach than one that has to be started.
    pub fn overhead(&self) -> u8 {
        match self {
            Reach::Here => 0,
            Reach::Wasm { .. } => 1,
            Reach::Subprocess {
                persistent: true, ..
            } => 2,
            Reach::Subprocess { .. } => 3,
            Reach::Remote { .. } => 4,
        }
    }
}

/// A place implementations run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    /// What this environment is called, referred to by implementations.
    pub id: Ident,
    /// The language whose code runs here.
    pub language: Ident,
    /// Prose describing it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// How this process gets to it.
    pub reach: Reach,
    /// Non-identifying annotations.
    #[serde(default, skip_serializing_if = "Metadata::is_empty")]
    pub meta: Metadata,
}

impl Environment {
    /// The environment that is this process.
    ///
    /// Every registry has one: it is where compiled-in Rust functions live, and
    /// where the interpreter walks bodies.
    pub fn here() -> Self {
        Self {
            id: Ident::new("here").expect("a valid name"),
            language: Ident::new("rust").expect("a valid name"),
            doc: Some(
                "This process. Compiled-in functions and interpreted bodies run here.".to_string(),
            ),
            reach: Reach::Here,
            meta: Metadata::default(),
        }
    }

    /// An environment reached by starting a program.
    pub fn subprocess(id: &str, language: &str, command: &str, args: &[&str]) -> Self {
        Self {
            id: Ident::new(id).expect("a valid name"),
            language: Ident::new(language).expect("a valid name"),
            doc: None,
            reach: Reach::Subprocess {
                command: command.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
                cwd: None,
                env: BTreeMap::new(),
                timeout_ms: default_timeout(),
                persistent: false,
            },
            meta: Metadata::default(),
        }
    }

    /// An environment reached by loading a WebAssembly module.
    ///
    /// The language is what the guest was written in, which is worth keeping
    /// even though the host cannot tell: a route printed for a person should
    /// say a proof came from Rust or from Go, not that it came from bytecode.
    pub fn wasm(id: &str, language: &str, module: impl Into<PathBuf>) -> Self {
        Self {
            id: Ident::new(id).expect("a valid name"),
            language: Ident::new(language).expect("a valid name"),
            doc: None,
            reach: Reach::Wasm {
                module: module.into(),
                fuel: None,
            },
            meta: Metadata::default(),
        }
    }

    /// Whether this environment is this process.
    pub fn is_here(&self) -> bool {
        matches!(self.reach, Reach::Here)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_is_always_an_environment() {
        let here = Environment::here();
        assert!(here.is_here());
        assert_eq!(here.reach.overhead(), 0);
        assert_eq!(
            here.reach.timeout_ms(),
            None,
            "there is nothing to wait for"
        );
    }

    #[test]
    fn staying_in_the_process_costs_less_than_leaving_it() {
        let ordered = [
            Reach::Here,
            Reach::Wasm {
                module: "x.wasm".into(),
                fuel: None,
            },
            Reach::Subprocess {
                command: "python3".into(),
                args: vec![],
                cwd: None,
                env: BTreeMap::new(),
                timeout_ms: 1000,
                persistent: true,
            },
            Reach::Subprocess {
                command: "python3".into(),
                args: vec![],
                cwd: None,
                env: BTreeMap::new(),
                timeout_ms: 1000,
                persistent: false,
            },
            Reach::Remote {
                endpoint: "https://elsewhere".into(),
                timeout_ms: 1000,
            },
        ];
        for pair in ordered.windows(2) {
            assert!(
                pair[0].overhead() < pair[1].overhead(),
                "{} should cost less than {}",
                pair[0].kind(),
                pair[1].kind()
            );
        }
    }

    #[test]
    fn the_same_language_can_be_reached_more_than_one_way() {
        // Which is the point of keeping the two apart: a Python proof does not
        // change because it is reached differently.
        let spawned = Environment::subprocess("py_local", "python", "python3", &["-u"]);
        let hosted = Environment {
            id: Ident::new("py_remote").unwrap(),
            language: Ident::new("python").unwrap(),
            doc: None,
            reach: Reach::Remote {
                endpoint: "http://localhost:9000".into(),
                timeout_ms: 5_000,
            },
            meta: Metadata::default(),
        };
        assert_eq!(spawned.language, hosted.language);
        assert_ne!(spawned.reach.kind(), hosted.reach.kind());
    }

    #[test]
    fn a_worker_already_running_is_cheaper_to_reach_than_one_that_must_start() {
        let started_each_time = Environment::subprocess("py", "python", "python3", &[]);
        let mut kept_alive = started_each_time.clone();
        if let Reach::Subprocess { persistent, .. } = &mut kept_alive.reach {
            *persistent = true;
        }
        assert!(kept_alive.reach.is_persistent());
        assert!(!started_each_time.reach.is_persistent());
        assert!(kept_alive.reach.overhead() < started_each_time.reach.overhead());
    }

    #[test]
    fn a_process_is_started_afresh_unless_asked_otherwise() {
        // Keeping a process alive means state survives between calls, which is
        // a thing to opt into rather than discover.
        let manifest: Environment = serde_json::from_str(
            r#"{"id":"py","language":"python",
                "reach":{"k":"subprocess","command":"python3"}}"#,
        )
        .expect("reads");
        assert!(!manifest.reach.is_persistent());
    }

    #[test]
    fn an_environment_round_trips_through_json() {
        let env = Environment::subprocess("node", "typescript", "node", &["harness.js"]);
        let text = serde_json::to_string(&env).unwrap();
        assert_eq!(serde_json::from_str::<Environment>(&text).unwrap(), env);
    }

    #[test]
    fn waiting_has_a_limit_wherever_waiting_happens() {
        let env = Environment::subprocess("py", "python", "python3", &[]);
        assert_eq!(env.reach.timeout_ms(), Some(30_000));
    }
}
