//! Reaching a proof compiled to WebAssembly.
//!
//! The other environments trust what they run. A subprocess can read your
//! files; a compiled-in function can do anything this program can. That is
//! usually fine — you wrote it, or you chose it — but it means a proof from
//! somewhere you do not entirely trust has nowhere safe to run.
//!
//! A WebAssembly module has somewhere. It gets a block of memory and two
//! functions to export, and that is the whole of its world: no files, no
//! sockets, no clock, no way to reach the host except by returning. It also
//! runs on a fuel budget, so a proof that loops forever stops rather than
//! taking the process with it — which matters here more than elsewhere,
//! because there is no operating system process to kill.
//!
//! # The ABI
//!
//! The messages are the ones the subprocess protocol specifies. Only the
//! carriage differs: instead of a pipe, the request is written into the guest's
//! own memory and the answer is read back out of it.
//!
//! A guest exports three things:
//!
//! ```text
//! memory                                 its linear memory
//! unitype_alloc(len: i32) -> i32          room for `len` bytes, and where
//! unitype_invoke(ptr: i32, len: i32) -> i64   the answer, as ptr<<32 | len
//! ```
//!
//! The host allocates, writes the request, calls invoke, and reads the answer
//! from wherever the returned pair points. The guest keeps that memory alive
//! until it returns, which is all it has to do, because the instance is thrown
//! away afterwards.
//!
//! # One instance per call
//!
//! Compiling a module is expensive and instantiating one is not, so modules are
//! compiled once and kept while every call gets a fresh instance. That is the
//! same arrangement as one process per call, and for the same reason: nothing
//! survives from one call to the next, so a proof cannot get a later call wrong
//! by remembering an earlier one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::Type;
use unitype_core::value::Value;
use wasmtime::{Engine, Instance, Module, Store};

use crate::environment::{Environment, Reach};
use crate::error::ExecError;
use crate::implementation::Implementation;

/// What the host looks for in a guest.
const MEMORY: &str = "memory";
/// The guest's allocator.
const ALLOC: &str = "unitype_alloc";
/// The guest's entry point.
const INVOKE: &str = "unitype_invoke";

/// How much work a guest may do before it is stopped, when nothing says
/// otherwise.
///
/// Generous enough for real work and finite enough that a runaway proof ends.
/// There is no process to kill here, so this is the only thing that stops one.
pub const DEFAULT_FUEL: u64 = 1_000_000_000;

/// Compiled modules, kept so that only the first call to one pays for it.
pub struct WasmHost {
    engine: Engine,
    compiled: Mutex<HashMap<PathBuf, Module>>,
}

impl Default for WasmHost {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmHost {
    /// A host that compiles guests on demand and remembers them.
    pub fn new() -> Self {
        let mut config = wasmtime::Config::new();
        // Metered, so a guest that does not stop is stopped.
        config.consume_fuel(true);
        Self {
            engine: Engine::new(&config).expect("a default engine configuration is valid"),
            compiled: Mutex::new(HashMap::new()),
        }
    }

    /// Runs an implementation inside its module.
    pub async fn invoke(
        &self,
        ws: &CheckedWorkspace,
        environment: &Environment,
        implementation: &Implementation,
        args: Vec<Value>,
        expected: &Type,
    ) -> Result<Value, ExecError> {
        let Reach::Wasm { module, fuel } = &environment.reach else {
            return Err(ExecError::Unreachable {
                environment: environment.id.to_string(),
                detail: "this is not a WebAssembly environment".into(),
            });
        };

        let compiled = self.module(module, &environment.id)?;
        let body = crate::subprocess::request_line(implementation, &args)?;
        let budget = fuel.unwrap_or(DEFAULT_FUEL);

        let answer = self.exchange(&compiled, &body, budget, implementation)?;
        crate::subprocess::read_answer(ws, implementation, &answer, expected)
    }

    /// The compiled module for a path, compiling it if this is the first ask.
    fn module(&self, path: &Path, environment: &unitype_core::Ident) -> Result<Module, ExecError> {
        let mut compiled = self
            .compiled
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(found) = compiled.get(path) {
            return Ok(found.clone());
        }
        // Accepts both the binary form and the text form, so a fixture can be
        // something a person can read.
        let module =
            Module::from_file(&self.engine, path).map_err(|source| ExecError::Unreachable {
                environment: environment.to_string(),
                detail: format!("could not load {}: {source}", path.display()),
            })?;
        compiled.insert(path.to_path_buf(), module.clone());
        Ok(module)
    }

    /// One request in, one answer out, in a fresh instance.
    fn exchange(
        &self,
        module: &Module,
        body: &[u8],
        fuel: u64,
        implementation: &Implementation,
    ) -> Result<String, ExecError> {
        let id = || implementation.id.to_string();
        let protocol = |detail: String| ExecError::Protocol { id: id(), detail };

        let mut store = Store::new(&self.engine, ());
        store
            .set_fuel(fuel)
            .map_err(|e| protocol(format!("could not meter the guest: {e}")))?;

        let instance = Instance::new(&mut store, module, &[])
            .map_err(|e| protocol(format!("the module would not start: {e}")))?;

        let memory = instance
            .get_memory(&mut store, MEMORY)
            .ok_or_else(|| protocol(format!("it exports no `{MEMORY}`")))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, ALLOC)
            .map_err(|_| protocol(format!("it exports no `{ALLOC}(i32) -> i32`")))?;
        let invoke = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, INVOKE)
            .map_err(|_| protocol(format!("it exports no `{INVOKE}(i32, i32) -> i64`")))?;

        let len = i32::try_from(body.len())
            .map_err(|_| protocol("the request is larger than a guest can address".into()))?;

        let at = alloc
            .call(&mut store, len)
            .map_err(|e| self.trapped(e, &store, implementation, fuel, "asking for room"))?;
        memory
            .write(&mut store, at as usize, body)
            .map_err(|e| protocol(format!("the room it gave back is not usable: {e}")))?;

        let packed = invoke
            .call(&mut store, (at, len))
            .map_err(|e| self.trapped(e, &store, implementation, fuel, "running"))?;

        // The answer's whereabouts, packed into one number because that is all
        // a WebAssembly function can return.
        let out_at = ((packed >> 32) & 0xFFFF_FFFF) as usize;
        let out_len = (packed & 0xFFFF_FFFF) as usize;

        let data = memory.data(&store);
        let end = out_at
            .checked_add(out_len)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| {
                protocol(format!(
                    "it pointed its answer at {out_at}..{}, which is outside its own memory",
                    out_at.saturating_add(out_len)
                ))
            })?;

        String::from_utf8(data[out_at..end].to_vec())
            .map_err(|e| protocol(format!("its answer is not text: {e}")))
    }

    /// What a trap means, which depends on whether it ran out of fuel.
    fn trapped(
        &self,
        error: wasmtime::Error,
        store: &Store<()>,
        implementation: &Implementation,
        budget: u64,
        doing: &str,
    ) -> ExecError {
        // Out of fuel is the sandbox working, not the proof being wrong, so it
        // reads as an environment failure and may be worked around.
        if store.get_fuel().is_ok_and(|left| left == 0) {
            return ExecError::Exhausted {
                id: implementation.id.to_string(),
                budget,
            };
        }
        ExecError::Failed {
            id: implementation.id.to_string(),
            message: format!("it stopped while {doing}: {error}"),
        }
    }
}
