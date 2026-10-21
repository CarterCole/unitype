//! Choosing which proof answers a call.
//!
//! A proposition may have several proofs. Which one runs is decided here, at
//! the moment of the call, from a policy the caller supplies — ask for speed
//! and you get the fast one, ask for a small footprint and you get the frugal
//! one, and the answer is the same either way. That is the arrangement this
//! whole system exists to make possible, and it is a policy decision rather
//! than an architectural one precisely because nothing above this layer had to
//! know which proof it was getting.
//!
//! # Checking both sides
//!
//! Arguments are checked against the signature before an implementation sees
//! them, and the result is checked before the caller does. The second check is
//! the one that earns its keep: an implementation that returns the wrong shape
//! has failed to prove what it claimed, and that has to be loud. It is the only
//! failure that never falls back to another proof, because trying someone else
//! would be papering over a broken proof rather than working around a broken
//! machine.
//!
//! # Refinement widens the search
//!
//! Asking for `sort` also considers proofs of anything that *refines* `sort`,
//! since a proof of a stronger claim proves the weaker one too. A caller gets
//! the benefit of a specialised implementation without having to know it exists.
//!
//! # Equivalence reorders the call
//!
//! An equivalence link says two signatures are one proposition with the
//! parameters renamed and reordered — `request(url, cb)` and
//! `request(callback, getUrl)`. Asking for either considers the proofs of both,
//! and a proof of the other one is handed the arguments in the order it takes
//! them. Nothing is converted: an equivalence only pairs parameters that hold
//! the same data, so putting them in another order is the whole of it.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::Type;
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::environment::Reach;
use crate::error::{DispatchError, ExecError};
use crate::implementation::{ImplId, Implementation};
use crate::interpret::{BoxFuture, Calls, eval_signature};
use crate::registry::{INTRINSIC_ENTRY, Registry};

/// How to choose among the proofs of a proposition.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Policy {
    /// Whatever is cheapest overall, preferring not to leave the process.
    #[default]
    Default,
    /// The best time complexity on offer.
    Fastest,
    /// The smallest memory footprint on offer.
    MinMemory,
    /// This proof and no other. If it cannot run, the call fails rather than
    /// quietly becoming a different call.
    Pinned(ImplId),
    /// Anything in this environment, if there is one; otherwise the default.
    PreferEnvironment(Ident),
}

/// What the dispatcher checks and how forgiving it is.
#[derive(Clone, Debug)]
pub struct Options {
    /// Check arguments against the signature before running anything.
    pub check_arguments: bool,
    /// Check the result against the signature before returning it.
    pub check_result: bool,
    /// On an environmental failure, try the next proof.
    ///
    /// Off by default: silently succeeding somewhere else hides the fact that
    /// an environment is broken. A proof returning the wrong answer never falls
    /// back regardless of this.
    pub fall_back: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            check_arguments: true,
            check_result: true,
            fall_back: false,
        }
    }
}

/// Why a candidate was passed over, or what happened to it.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// It ran and answered.
    Answered,
    /// It could not be reached from here.
    Unavailable(String),
    /// The policy ruled it out.
    NotPreferred,
    /// It ran and failed.
    Failed(String),
}

/// One candidate and what became of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Considered {
    /// The implementation.
    pub id: ImplId,
    /// Where it would have run.
    pub environment: Ident,
    /// What it claims to cost.
    pub cost: String,
    /// What became of it.
    pub outcome: Outcome,
}

/// What happened when a proposition was called.
#[derive(Clone, Debug)]
pub struct Invocation {
    /// What came back.
    pub value: Value,
    /// The proof that answered.
    pub implementation: ImplId,
    /// Where it ran.
    pub environment: Ident,
    /// How long it took, including reaching the environment.
    pub elapsed: Duration,
    /// Every candidate and what became of it, so a caller can see why.
    pub considered: Vec<Considered>,
}

/// Decides which proof answers a call, and runs it.
pub struct Dispatcher {
    ws: Arc<CheckedWorkspace>,
    registry: Arc<Registry>,
    options: Options,
    /// Processes kept alive between calls, for environments that asked to be.
    workers: crate::worker::Workers,
    /// Compiled WebAssembly guests, kept so only the first call pays for one.
    ///
    /// Built on first use rather than up front. Standing up a WebAssembly
    /// engine is not free — on macOS it installs a Mach exception handler for
    /// the whole process — and a dispatcher with no sandboxed proof registered
    /// should cost nothing for a capability it never uses.
    #[cfg(feature = "wasm")]
    wasm: std::sync::OnceLock<crate::wasm::WasmHost>,
}

impl Dispatcher {
    /// A dispatcher over a workspace and what has been registered against it.
    pub fn new(ws: Arc<CheckedWorkspace>, registry: Arc<Registry>) -> Self {
        Self {
            ws,
            registry,
            options: Options::default(),
            workers: crate::worker::Workers::new(),
            #[cfg(feature = "wasm")]
            wasm: std::sync::OnceLock::new(),
        }
    }

    /// The workers this dispatcher is keeping alive.
    ///
    /// Exposed so a program can stop them at a moment of its choosing; dropping
    /// the dispatcher stops them anyway.
    pub fn workers(&self) -> &crate::worker::Workers {
        &self.workers
    }

    /// The same, with different checking.
    pub fn with_options(mut self, options: Options) -> Self {
        self.options = options;
        self
    }

    /// The workspace being dispatched against.
    pub fn workspace(&self) -> &CheckedWorkspace {
        &self.ws
    }

    /// What is registered.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Every proof that would answer this proposition, best first under the
    /// given policy.
    ///
    /// Useful on its own: it is what "why did it choose that one" looks like
    /// before anything has run.
    pub fn candidates(&self, sig: &QualifiedName, policy: &Policy) -> Vec<&Implementation> {
        self.routes(sig, policy)
            .into_iter()
            .map(|(implementation, _)| implementation)
            .collect()
    }

    /// Every proof that would answer this proposition, best first, each with
    /// the order to hand it the caller's arguments in: `order[j]` is which of
    /// them becomes its `j`th.
    fn routes(&self, sig: &QualifiedName, policy: &Policy) -> Vec<(&Implementation, Vec<usize>)> {
        let mut found: Vec<(&Implementation, Vec<usize>)> = Vec::new();
        let mut seen = HashSet::new();
        let arity = self.ws.sig(sig).map_or(0, |s| s.params.len());

        // A proof of anything that strengthens this proposition proves it too,
        // with the arguments where they were. A proof of anything equivalent to
        // it proves it once they are put in that signature's order.
        let mut frontier = vec![(sig.clone(), (0..arity).collect::<Vec<usize>>())];
        while let Some((name, order)) = frontier.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            found.extend(
                self.registry
                    .proofs_of(&name)
                    .into_iter()
                    .map(|i| (i, order.clone())),
            );
            frontier.extend(
                self.ws
                    .refiners_of(&name)
                    .iter()
                    .map(|r| (r.clone(), order.clone())),
            );
            frontier.extend(
                self.ws
                    .equivalents_of(&name)
                    .iter()
                    .map(|e| (e.sig.clone(), e.apply(&order))),
            );
        }

        if let Policy::Pinned(id) = policy {
            found.retain(|(i, _)| &i.id == id);
            return found;
        }

        found.sort_by_key(|(i, _)| self.rank(i, policy));
        found
    }

    /// Where a proof sorts under a policy. Lower is preferred.
    fn rank(&self, implementation: &Implementation, policy: &Policy) -> (u8, u8, u8, u8, String) {
        let overhead = self
            .registry
            .environment(&implementation.environment)
            .map(|e| e.reach.overhead())
            .unwrap_or(u8::MAX);
        let (time, space, _) = implementation.cost_key();
        let id = implementation.id.to_string();

        match policy {
            // Nearest to hand first. Reaching another environment costs
            // something real — a process to start, a message to serialize —
            // and for the sizes most calls actually carry, that dominates the
            // difference between one complexity class and the next. A caller
            // who knows their input is large asks for `Fastest` and means it.
            Policy::Default => (0, overhead, time, space, id),
            Policy::Fastest => (0, time, space, overhead, id),
            Policy::MinMemory => (0, space, time, overhead, id),
            Policy::PreferEnvironment(wanted) => (
                u8::from(&implementation.environment != wanted),
                time,
                space,
                overhead,
                id,
            ),
            Policy::Pinned(_) => (0, time, space, overhead, id),
        }
    }

    /// Calls a proposition, choosing a proof by policy.
    ///
    /// For a generic proposition use [`Dispatcher::call_at`]: this is the same
    /// call with no type arguments, which is only enough when there is nothing
    /// to bind.
    pub fn call<'a>(
        &'a self,
        sig: &'a QualifiedName,
        args: Vec<Value>,
        policy: &'a Policy,
    ) -> BoxFuture<'a, Result<Invocation, DispatchError>> {
        Box::pin(async move { self.dispatch(sig, &[], args, policy).await })
    }

    /// Calls a generic proposition at particular types.
    ///
    /// `reverse<T>` says something about every `T`; `reverse<i64>` is the
    /// claim a list of integers can actually be checked against. The type
    /// arguments bind the proposition's parameters in order, and both sides of
    /// the boundary are then checked against the instantiated signature. The
    /// proof itself is not told: a proof of a generic proposition is one that
    /// did not need to know.
    pub fn call_at<'a>(
        &'a self,
        sig: &'a QualifiedName,
        type_args: &'a [Type],
        args: Vec<Value>,
        policy: &'a Policy,
    ) -> BoxFuture<'a, Result<Invocation, DispatchError>> {
        Box::pin(async move { self.dispatch(sig, type_args, args, policy).await })
    }

    async fn dispatch(
        &self,
        name: &QualifiedName,
        type_args: &[Type],
        args: Vec<Value>,
        policy: &Policy,
    ) -> Result<Invocation, DispatchError> {
        let Some(declared) = self.ws.sig(name) else {
            return Err(DispatchError::UnknownSignature(name.clone()));
        };

        if declared.params_decl.len() != type_args.len() {
            return Err(DispatchError::TypeArity {
                sig: name.clone(),
                expected: declared.params_decl.len(),
                got: type_args.len(),
            });
        }
        // A bound may mention the other parameters, as in `<A, B: Holder<A>>`,
        // so the arguments are put into it before it is asked.
        let subst = unitype_core::check::bind(&declared.params_decl, type_args);
        for (param, arg) in declared.params_decl.iter().zip(type_args) {
            for bound in &param.bounds {
                let required = Type::Ref(unitype_core::check::substitute_ref(bound, &subst));
                if !self.ws.is_subtype(arg, &required) {
                    return Err(DispatchError::BoundNotMet {
                        sig: name.clone(),
                        param: param.name.clone(),
                        arg: arg.to_string(),
                        bound: required.to_string(),
                    });
                }
            }
        }
        // Only a generic signature has anything to substitute, so only one
        // pays for the copy.
        let instantiated;
        let sig = if type_args.is_empty() {
            declared
        } else {
            instantiated = declared.instantiate(type_args);
            &instantiated
        };

        if sig.params.len() != args.len() {
            return Err(DispatchError::Arity {
                sig: name.clone(),
                expected: sig.params.len(),
                got: args.len(),
            });
        }
        if self.options.check_arguments {
            for (index, (param, arg)) in sig.params.iter().zip(&args).enumerate() {
                if let Err(errors) = self.ws.validate(arg, &param.ty) {
                    return Err(DispatchError::BadArgument {
                        sig: name.clone(),
                        index,
                        errors,
                    });
                }
            }
        }

        let candidates = self.routes(name, policy);
        if candidates.is_empty() {
            return match policy {
                Policy::Pinned(id) => Err(DispatchError::Unusable {
                    id: id.to_string(),
                    reason: format!("nothing registered under that name proves `{name}`"),
                }),
                _ => Err(DispatchError::NoImplementation(name.clone())),
            };
        }

        // A future is a promise about when, not about what, so a signature
        // returning one is satisfied by the value it eventually holds.
        let expected = match &sig.ret {
            Type::Future { of } => (**of).clone(),
            other => other.clone(),
        };

        let mut considered = Vec::with_capacity(candidates.len());
        for (implementation, order) in candidates {
            let cost = implementation.cost.to_string();
            let environment = implementation.environment.clone();

            if let Err(reason) = self.reachable(implementation) {
                considered.push(Considered {
                    id: implementation.id.clone(),
                    environment,
                    cost,
                    outcome: Outcome::Unavailable(reason),
                });
                continue;
            }

            let started = Instant::now();
            // In the order the proof's own signature takes them, which is the
            // caller's order unless it proves an equivalent signature.
            let handed: Vec<Value> = order.iter().map(|&i| args[i].clone()).collect();
            // Each written against the type the call checked it at, which an
            // equivalent signature's parameter holds the same data as.
            let types: Vec<Type> = order.iter().map(|&i| sig.params[i].ty.clone()).collect();
            let outcome = self
                .invoke(implementation, handed, &types, &expected, policy)
                .await;
            let elapsed = started.elapsed();

            match outcome {
                Ok(value) => {
                    if self.options.check_result {
                        if let Err(errors) = self.ws.validate(&value, &expected) {
                            // Never a fallback. A proof that does not prove
                            // what it claims is a fact about the proof, and
                            // asking someone else would bury it.
                            return Err(DispatchError::BadResult {
                                id: implementation.id.to_string(),
                                sig: name.clone(),
                                errors,
                            });
                        }
                    }
                    considered.push(Considered {
                        id: implementation.id.clone(),
                        environment: environment.clone(),
                        cost,
                        outcome: Outcome::Answered,
                    });
                    return Ok(Invocation {
                        value,
                        implementation: implementation.id.clone(),
                        environment,
                        elapsed,
                        considered,
                    });
                }
                Err(error) => {
                    let recoverable = self.options.fall_back && error.is_environmental();
                    considered.push(Considered {
                        id: implementation.id.clone(),
                        environment,
                        cost,
                        outcome: Outcome::Failed(error.to_string()),
                    });
                    if !recoverable {
                        return Err(DispatchError::Exec {
                            id: implementation.id.to_string(),
                            source: error,
                        });
                    }
                }
            }
        }

        Err(DispatchError::NoneAvailable {
            sig: name.clone(),
            skipped: considered
                .iter()
                .map(|c| match &c.outcome {
                    Outcome::Unavailable(why) | Outcome::Failed(why) => {
                        format!("{}: {why}", c.id)
                    }
                    other => format!("{}: {other:?}", c.id),
                })
                .collect(),
        })
    }

    /// Whether this process can get to where an implementation lives.
    fn reachable(&self, implementation: &Implementation) -> Result<(), String> {
        let Some(environment) = self.registry.environment(&implementation.environment) else {
            return Err(format!(
                "environment `{}` is not registered",
                implementation.environment
            ));
        };
        match &environment.reach {
            Reach::Here => {
                if implementation.entry == INTRINSIC_ENTRY
                    || self.registry.native(&implementation.entry).is_some()
                {
                    Ok(())
                } else {
                    Err(format!(
                        "nothing is registered in this process under `{}`",
                        implementation.entry
                    ))
                }
            }
            // Whether the program is actually there is discovered by trying to
            // start it, which gives a precise reason rather than a guess. The
            // same goes for a module that will not load.
            Reach::Subprocess { .. } => Ok(()),
            #[cfg(feature = "wasm")]
            Reach::Wasm { .. } => Ok(()),
            other => Err(format!(
                "reaching a `{}` environment is not built into this binary",
                other.kind()
            )),
        }
    }

    /// Runs one implementation.
    ///
    /// `params` is the type each argument was checked at, in the order the
    /// proof takes them, which is what anything reached over a wire is sent
    /// them written against.
    async fn invoke(
        &self,
        implementation: &Implementation,
        args: Vec<Value>,
        params: &[Type],
        expected: &Type,
        policy: &Policy,
    ) -> Result<Value, ExecError> {
        let Some(environment) = self.registry.environment(&implementation.environment) else {
            return Err(ExecError::Unreachable {
                environment: implementation.environment.to_string(),
                detail: "not registered".into(),
            });
        };

        match &environment.reach {
            Reach::Here => {
                // A body proves its own signature, and calls inside it come
                // back through here, so a nested call is chosen under the same
                // policy.
                if implementation.entry == INTRINSIC_ENTRY {
                    let calls = Through {
                        dispatcher: self,
                        policy,
                    };
                    return eval_signature(&self.ws, &implementation.proves, args, &calls).await;
                }
                let Some(native) = self.registry.native(&implementation.entry) else {
                    return Err(ExecError::NotRegistered(implementation.entry.clone()));
                };
                native(args).await
            }
            // An environment that asked to stay alive is answered by whatever
            // worker is already running for it, or by one started now.
            Reach::Subprocess {
                persistent: true, ..
            } => {
                self.workers
                    .invoke(
                        &self.ws,
                        environment,
                        implementation,
                        args,
                        params,
                        expected,
                    )
                    .await
            }
            Reach::Subprocess { .. } => {
                crate::subprocess::invoke(
                    &self.ws,
                    environment,
                    implementation,
                    args,
                    params,
                    expected,
                )
                .await
            }
            #[cfg(feature = "wasm")]
            Reach::Wasm { .. } => {
                self.wasm
                    .get_or_init(crate::wasm::WasmHost::new)
                    .invoke(
                        &self.ws,
                        environment,
                        implementation,
                        args,
                        params,
                        expected,
                    )
                    .await
            }
            other => Err(ExecError::Unreachable {
                environment: environment.id.to_string(),
                detail: format!(
                    "reaching a `{}` environment is not built into this binary",
                    other.kind()
                ),
            }),
        }
    }
}

/// The dispatcher, seen from inside a body, with a policy carried along.
struct Through<'a> {
    dispatcher: &'a Dispatcher,
    policy: &'a Policy,
}

impl Calls for Through<'_> {
    fn call<'a>(
        &'a self,
        sig: &'a QualifiedName,
        args: Vec<Value>,
    ) -> BoxFuture<'a, Result<Value, ExecError>> {
        Box::pin(async move {
            // A nested call is pinned only if the caller pinned *this* one, and
            // that pin was about this signature, not about anything it calls.
            let policy = match self.policy {
                Policy::Pinned(_) => &Policy::Default,
                other => other,
            };
            self.dispatcher
                .call(sig, args, policy)
                .await
                .map(|i| i.value)
                .map_err(|e| ExecError::Failed {
                    id: sig.to_string(),
                    message: e.to_string(),
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::check::check_workspace;
    use unitype_core::{BigO, ImplProfile, Workspace};

    use crate::environment::Environment;
    use crate::implementation::Implementation;

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    fn here() -> Ident {
        Ident::new("here").unwrap()
    }

    const SORT_WS: &str = r#"{"universes":[{"name":"demo","functions":[
        {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"},
        {"name":"mergesort","refines":"demo::sort",
         "params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}]}]}"#;

    /// Sorts, slowly but frugally.
    fn bubble(mut items: Vec<i64>) -> Vec<i64> {
        for i in 0..items.len() {
            for j in 0..items.len().saturating_sub(i + 1) {
                if items[j] > items[j + 1] {
                    items.swap(j, j + 1);
                }
            }
        }
        items
    }

    fn ints(value: &Value) -> Vec<i64> {
        let Value::List(items) = value else {
            return Vec::new();
        };
        items.iter().filter_map(Value::as_int).collect()
    }

    /// Two proofs of one proposition, with different costs, both compiled in.
    fn sorting_dispatcher() -> Dispatcher {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(SORT_WS).expect("reads")).expect("checks"),
        );
        let mut registry = Registry::new();

        registry.register_native_sync("merge_sort", |args| {
            let mut items = ints(args.first().unwrap_or(&Value::Null));
            items.sort_unstable();
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });
        registry.register_native_sync("bubble_sort", |args| {
            let items = bubble(ints(args.first().unwrap_or(&Value::Null)));
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });

        registry
            .register(Implementation::new(
                "sort.merge",
                q("demo::sort"),
                here(),
                "merge_sort",
                ImplProfile::new(BigO::ONLogN, BigO::ON),
            ))
            .unwrap();
        registry
            .register(Implementation::new(
                "sort.bubble",
                q("demo::sort"),
                here(),
                "bubble_sort",
                ImplProfile::new(BigO::ON2, BigO::O1),
            ))
            .unwrap();

        Dispatcher::new(ws, Arc::new(registry))
    }

    async fn sort_with(policy: Policy) -> Invocation {
        sorting_dispatcher()
            .call(&q("demo::sort"), vec![list(&[3, 1, 2])], &policy)
            .await
            .expect("answers")
    }

    fn list(items: &[i64]) -> Value {
        Value::List(items.iter().copied().map(Value::Int).collect())
    }

    /// One proposition stated twice, with the arguments the other way round,
    /// and proved only once.
    fn request_dispatcher() -> Dispatcher {
        let ws = Arc::new(
            check_workspace(
                Workspace::from_json(
                    r#"{"universes":[
                        {"name":"node","functions":[{"name":"fetch","ret":"str","params":[
                            {"name":"url","ty":"str"},{"name":"retries","ty":"i64"}]}]},
                        {"name":"browser","functions":[{"name":"get","ret":"str","params":[
                            {"name":"attempts","ty":"i64"},{"name":"getUrl","ty":"str"}]}]}],
                      "links":[{"name":"fetch_is_get","from":"node::fetch","to":"browser::get",
                                "kind":{"k":"equivalent"}}]}"#,
                )
                .expect("reads"),
            )
            .expect("checks"),
        );
        let mut registry = Registry::new();
        // Written against `node::fetch`, so it reads the URL first.
        registry.register_native_sync("node_fetch", |args| {
            let url = args.first().and_then(Value::as_str).unwrap_or("?");
            let retries = args.get(1).and_then(Value::as_int).unwrap_or(-1);
            Ok(Value::Str(format!("{url} x{retries}")))
        });
        registry
            .register(Implementation::new(
                "fetch.node",
                q("node::fetch"),
                here(),
                "node_fetch",
                ImplProfile::new(BigO::O1, BigO::O1),
            ))
            .unwrap();
        Dispatcher::new(ws, Arc::new(registry))
    }

    #[tokio::test]
    async fn a_proof_of_an_equivalent_proposition_gets_the_arguments_its_own_way_round() {
        let dispatcher = request_dispatcher();

        let answered = dispatcher
            .call(
                &q("browser::get"),
                vec![Value::Int(3), Value::Str("https://x".into())],
                &Policy::Default,
            )
            .await
            .expect("the node proof answers the browser's call");
        assert_eq!(answered.implementation.as_str(), "fetch.node");
        assert_eq!(
            answered.value,
            Value::Str("https://x x3".into()),
            "the URL went where the proof reads it"
        );

        let direct = dispatcher
            .call(
                &q("node::fetch"),
                vec![Value::Str("https://x".into()), Value::Int(3)],
                &Policy::Default,
            )
            .await
            .expect("answers its own proposition");
        assert_eq!(direct.value, answered.value, "and the same call either way");
    }

    #[tokio::test]
    async fn pinning_a_proof_of_an_equivalent_proposition_still_reorders() {
        let answered = request_dispatcher()
            .call(
                &q("browser::get"),
                vec![Value::Int(1), Value::Str("https://y".into())],
                &Policy::Pinned(ImplId::new("fetch.node")),
            )
            .await
            .expect("answers");
        assert_eq!(answered.value, Value::Str("https://y x1".into()));
    }

    #[tokio::test]
    async fn a_generic_proposition_is_answered_by_a_proof_of_its_equivalent() {
        // The same claim declared with its generic parameters and its
        // parameters the other way round, and proved only on one side.
        let ws = Arc::new(
            check_workspace(
                Workspace::from_json(
                    r#"{"universes":[
                        {"name":"py","functions":[{"name":"konst",
                            "params_decl":[{"name":"A"},{"name":"B"}],
                            "params":[{"name":"x","ty":{"k":"param","name":"A"}},
                                      {"name":"y","ty":{"k":"param","name":"B"}}],
                            "ret":{"k":"param","name":"A"}}]},
                        {"name":"js","functions":[{"name":"always",
                            "params_decl":[{"name":"Y"},{"name":"X"}],
                            "params":[{"name":"ignored","ty":{"k":"param","name":"Y"}},
                                      {"name":"kept","ty":{"k":"param","name":"X"}}],
                            "ret":{"k":"param","name":"X"}}]}],
                      "links":[{"name":"konst_is_always","from":"py::konst","to":"js::always",
                                "kind":{"k":"equivalent"}}]}"#,
                )
                .expect("reads"),
            )
            .expect("checks"),
        );
        let mut registry = Registry::new();
        // Written against `py::konst`: keeps the first thing it is given.
        registry.register_native_sync("py_konst", |args| Ok(args[0].clone()));
        registry
            .register(Implementation::new(
                "konst.py",
                q("py::konst"),
                here(),
                "py_konst",
                ImplProfile::new(BigO::O1, BigO::O1),
            ))
            .unwrap();
        let dispatcher = Dispatcher::new(ws, Arc::new(registry));

        // `always<str, i64>("dropped", 7)` keeps the integer.
        let answered = dispatcher
            .call_at(
                &q("js::always"),
                &[Type::String, Type::I64],
                vec![Value::Str("dropped".into()), Value::Int(7)],
                &Policy::Default,
            )
            .await
            .expect("the proof of konst answers, checked against always<str, i64>");
        assert_eq!(answered.implementation.as_str(), "konst.py");
        assert_eq!(answered.value, Value::Int(7));
    }

    #[tokio::test]
    async fn asking_for_speed_and_asking_for_memory_choose_differently() {
        let fast = sort_with(Policy::Fastest).await;
        let frugal = sort_with(Policy::MinMemory).await;

        assert_eq!(fast.implementation.as_str(), "sort.merge");
        assert_eq!(frugal.implementation.as_str(), "sort.bubble");
    }

    #[tokio::test]
    async fn and_both_give_the_same_answer() {
        let fast = sort_with(Policy::Fastest).await;
        let frugal = sort_with(Policy::MinMemory).await;
        assert_eq!(fast.value, list(&[1, 2, 3]));
        assert_eq!(
            fast.value, frugal.value,
            "which proof ran should not change what is true"
        );
    }

    #[tokio::test]
    async fn a_caller_can_see_what_was_considered_and_why() {
        let chosen = sort_with(Policy::Fastest).await;
        assert_eq!(chosen.considered.len(), 1, "the first one worked");
        assert_eq!(chosen.considered[0].outcome, Outcome::Answered);
        assert_eq!(chosen.environment, here());

        let ranked: Vec<String> = sorting_dispatcher()
            .candidates(&q("demo::sort"), &Policy::MinMemory)
            .iter()
            .map(|i| i.id.to_string())
            .collect();
        assert_eq!(ranked, ["sort.bubble", "sort.merge"]);
    }

    #[tokio::test]
    async fn the_default_stays_in_the_process_where_it_can() {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(SORT_WS).expect("reads")).expect("checks"),
        );
        let mut registry = Registry::new();
        registry.register_native_sync("bubble_sort", |args| {
            Ok(Value::List(
                bubble(ints(&args[0])).into_iter().map(Value::Int).collect(),
            ))
        });
        registry
            .add_environment(Environment::subprocess("py", "python", "python3", &[]))
            .unwrap();
        // The faster proof is elsewhere; the slower one is right here.
        registry
            .register(Implementation::new(
                "sort.py_merge",
                q("demo::sort"),
                Ident::new("py").unwrap(),
                "merge_sort",
                ImplProfile::new(BigO::ONLogN, BigO::ON),
            ))
            .unwrap();
        registry
            .register(Implementation::new(
                "sort.here_bubble",
                q("demo::sort"),
                here(),
                "bubble_sort",
                ImplProfile::new(BigO::ON2, BigO::O1),
            ))
            .unwrap();

        let dispatcher = Dispatcher::new(ws, Arc::new(registry));
        let ranked: Vec<String> = dispatcher
            .candidates(&q("demo::sort"), &Policy::Default)
            .iter()
            .map(|i| i.id.to_string())
            .collect();
        assert_eq!(
            ranked[0], "sort.here_bubble",
            "starting a process costs more than the difference in complexity, \
             for the sizes most calls carry"
        );

        let ranked: Vec<String> = dispatcher
            .candidates(&q("demo::sort"), &Policy::Fastest)
            .iter()
            .map(|i| i.id.to_string())
            .collect();
        assert_eq!(
            ranked[0], "sort.py_merge",
            "asked for speed, the better algorithm wins wherever it lives"
        );
    }

    #[tokio::test]
    async fn a_pinned_proof_is_the_one_that_runs() {
        let pinned = sorting_dispatcher()
            .call(
                &q("demo::sort"),
                vec![list(&[2, 1])],
                &Policy::Pinned(ImplId::new("sort.bubble")),
            )
            .await
            .expect("answers");
        assert_eq!(pinned.implementation.as_str(), "sort.bubble");
    }

    #[tokio::test]
    async fn pinning_something_that_proves_nothing_fails_rather_than_substituting() {
        let err = sorting_dispatcher()
            .call(
                &q("demo::sort"),
                vec![list(&[1])],
                &Policy::Pinned(ImplId::new("sort.nonexistent")),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DispatchError::Unusable { .. }),
            "asking for one thing and getting another would be worse: {err}"
        );
    }

    #[tokio::test]
    async fn a_proposition_nobody_proves_says_so() {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(SORT_WS).expect("reads")).expect("checks"),
        );
        let dispatcher = Dispatcher::new(ws, Arc::new(Registry::new()));
        assert!(matches!(
            dispatcher
                .call(&q("demo::sort"), vec![list(&[1])], &Policy::Default)
                .await,
            Err(DispatchError::NoImplementation(_))
        ));
    }

    #[tokio::test]
    async fn arguments_are_checked_before_anything_runs() {
        let err = sorting_dispatcher()
            .call(
                &q("demo::sort"),
                vec![Value::Str("not a list".into())],
                &Policy::Default,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DispatchError::BadArgument { index: 0, .. }),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_wrong_number_of_arguments_is_caught() {
        let err = sorting_dispatcher()
            .call(&q("demo::sort"), vec![], &Policy::Default)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            DispatchError::Arity {
                expected: 1,
                got: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn a_proof_that_returns_the_wrong_thing_is_caught_and_never_excused() {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(SORT_WS).expect("reads")).expect("checks"),
        );
        let mut registry = Registry::new();
        registry.register_native_sync("liar", |_| Ok(Value::Str("not a list at all".into())));
        registry.register_native_sync("honest", |args| Ok(args[0].clone()));
        registry
            .register(Implementation::new(
                "sort.liar",
                q("demo::sort"),
                here(),
                "liar",
                ImplProfile::new(BigO::O1, BigO::O1),
            ))
            .unwrap();
        registry
            .register(Implementation::new(
                "sort.honest",
                q("demo::sort"),
                here(),
                "honest",
                ImplProfile::new(BigO::ON, BigO::ON),
            ))
            .unwrap();

        let dispatcher = Dispatcher::new(ws, Arc::new(registry)).with_options(Options {
            fall_back: true,
            ..Options::default()
        });

        let err = dispatcher
            .call(&q("demo::sort"), vec![list(&[1])], &Policy::Fastest)
            .await
            .unwrap_err();

        assert!(
            matches!(err, DispatchError::BadResult { .. }),
            "even with fallback on, a broken proof must not be quietly replaced: {err}"
        );
    }

    #[tokio::test]
    async fn a_proof_of_a_stronger_claim_answers_the_weaker_one() {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(SORT_WS).expect("reads")).expect("checks"),
        );
        let mut registry = Registry::new();
        registry.register_native_sync("merge_sort", |args| {
            let mut items = ints(&args[0]);
            items.sort_unstable();
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });
        // Registered against `mergesort`, which refines `sort`. Nobody has
        // registered anything against `sort` itself.
        registry
            .register(Implementation::new(
                "mergesort.rust",
                q("demo::mergesort"),
                here(),
                "merge_sort",
                ImplProfile::new(BigO::ONLogN, BigO::ON),
            ))
            .unwrap();

        let answer = Dispatcher::new(ws, Arc::new(registry))
            .call(&q("demo::sort"), vec![list(&[3, 1, 2])], &Policy::Default)
            .await
            .expect("a proof of the stronger claim proves this one");

        assert_eq!(answer.implementation.as_str(), "mergesort.rust");
        assert_eq!(answer.value, list(&[1, 2, 3]));
    }

    #[tokio::test]
    async fn an_environment_that_cannot_be_reached_is_reported_with_the_reason() {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(SORT_WS).expect("reads")).expect("checks"),
        );
        let mut registry = Registry::new();
        registry
            .add_environment(Environment::subprocess(
                "missing",
                "python",
                "unitype_no_such_interpreter",
                &[],
            ))
            .unwrap();
        registry
            .register(Implementation::new(
                "sort.elsewhere",
                q("demo::sort"),
                Ident::new("missing").unwrap(),
                "bubble_sort",
                ImplProfile::new(BigO::ON2, BigO::O1),
            ))
            .unwrap();

        let err = Dispatcher::new(ws, Arc::new(registry))
            .call(&q("demo::sort"), vec![list(&[1])], &Policy::Default)
            .await
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("sort.elsewhere"), "{message}");
        assert!(
            message.contains("could not start"),
            "the reason should be the actual one: {message}"
        );
    }

    #[tokio::test]
    async fn a_body_that_calls_out_is_answered_by_the_dispatcher() {
        let ws = Arc::new(
            check_workspace(
                Workspace::from_json(
                    r#"{"universes":[{"name":"demo","functions":[
                        {"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"},
                        {"name":"sorted_twice","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]",
                         "body":{"k":"call","sig":"demo::sort","args":[
                            {"k":"call","sig":"demo::sort","args":[{"k":"var","name":"xs"}]}]}}]}]}"#,
                )
                .expect("reads"),
            )
            .expect("checks"),
        );

        let mut registry = Registry::new();
        registry.register_bodies(&ws).unwrap();
        registry.register_native_sync("merge_sort", |args| {
            let mut items = ints(&args[0]);
            items.sort_unstable();
            Ok(Value::List(items.into_iter().map(Value::Int).collect()))
        });
        registry
            .register(Implementation::new(
                "sort.merge",
                q("demo::sort"),
                here(),
                "merge_sort",
                ImplProfile::new(BigO::ONLogN, BigO::ON),
            ))
            .unwrap();

        let answer = Dispatcher::new(ws, Arc::new(registry))
            .call(
                &q("demo::sorted_twice"),
                vec![list(&[3, 1, 2])],
                &Policy::Default,
            )
            .await
            .expect("answers");

        assert_eq!(answer.value, list(&[1, 2, 3]));
        assert!(
            answer.implementation.as_str().ends_with("#body"),
            "the body proved it, and the dispatcher answered what the body asked"
        );
    }

    const GENERIC_WS: &str = r#"{"universes":[{"name":"demo","functions":[
        {"name":"reverse","params_decl":[{"name":"T"}],
         "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
         "ret":{"k":"list","of":{"k":"param","name":"T"}}}]}]}"#;

    /// One proof of a generic proposition, which never looks at an element.
    fn reversing_dispatcher() -> Dispatcher {
        let ws = Arc::new(
            check_workspace(Workspace::from_json(GENERIC_WS).expect("reads")).expect("checks"),
        );
        let mut registry = Registry::new();
        registry.register_native_sync("reverse", |mut args| {
            let Value::List(items) = args.remove(0) else {
                return Err(ExecError::Untyped("expected a list".into()));
            };
            Ok(Value::List(items.into_iter().rev().collect()))
        });
        registry
            .register(Implementation::new(
                "reverse.here",
                q("demo::reverse"),
                here(),
                "reverse",
                ImplProfile::new(BigO::ON, BigO::ON),
            ))
            .unwrap();
        Dispatcher::new(ws, Arc::new(registry))
    }

    #[tokio::test]
    async fn a_generic_proposition_is_called_at_a_type() {
        let dispatcher = reversing_dispatcher();
        let words = Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]);

        let at_strings = dispatcher
            .call_at(
                &q("demo::reverse"),
                &[Type::String],
                vec![words],
                &Policy::Default,
            )
            .await
            .expect("answers");
        assert_eq!(
            at_strings.value,
            Value::List(vec![Value::Str("b".into()), Value::Str("a".into())])
        );

        let at_ints = dispatcher
            .call_at(
                &q("demo::reverse"),
                &[Type::I64],
                vec![list(&[1, 2])],
                &Policy::Default,
            )
            .await
            .expect("answers");
        assert_eq!(at_ints.value, list(&[2, 1]));
    }

    #[tokio::test]
    async fn a_generic_proposition_called_without_its_type_says_what_is_missing() {
        // A list of integers is not a `[T]`; it is a `[i64]`. The call has to
        // say so before either side of the boundary can be checked.
        let err = reversing_dispatcher()
            .call(&q("demo::reverse"), vec![list(&[1, 2])], &Policy::Default)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                DispatchError::TypeArity {
                    expected: 1,
                    got: 0,
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_generic_proposition_is_not_asked_at_a_type_its_bound_rules_out() {
        let ws = Arc::new(
            check_workspace(
                Workspace::from_json(
                    r#"{"universes":[{"name":"demo",
                        "types":[{"k":"interface","name":"Named","fields":[{"name":"name","ty":"str"}]},
                                 {"k":"struct","name":"Person","implements":["Named"],
                                  "fields":[{"name":"name","ty":"str"}]}],
                        "functions":[{"name":"first_named",
                            "params_decl":[{"name":"T","bounds":["Named"]}],
                            "params":[{"name":"xs","ty":{"k":"list","of":{"k":"param","name":"T"}}}],
                            "ret":{"k":"optional","of":{"k":"param","name":"T"}}}]}]}"#,
                )
                .expect("reads"),
            )
            .expect("checks"),
        );
        let dispatcher = Dispatcher::new(ws, Arc::new(Registry::new()));
        let person = Type::Ref(unitype_core::ir::TypeRef::qualified(
            Ident::new("demo").unwrap(),
            Ident::new("Person").unwrap(),
        ));

        let err = dispatcher
            .call_at(
                &q("demo::first_named"),
                &[Type::I64],
                vec![list(&[1])],
                &Policy::Default,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, DispatchError::BoundNotMet { param, .. } if param.as_str() == "T"),
            "an integer has no name: {err}"
        );

        // At a type that meets the bound, the call gets as far as finding no
        // proof, which is the next thing that can go wrong.
        let err = dispatcher
            .call_at(
                &q("demo::first_named"),
                &[person],
                vec![Value::List(vec![])],
                &Policy::Default,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, DispatchError::NoImplementation(_)), "{err}");
    }

    #[tokio::test]
    async fn the_boundary_is_checked_at_the_type_the_call_named() {
        // Calling at `str` with integers is a mistake in the call, and it is
        // caught before any proof runs.
        let err = reversing_dispatcher()
            .call_at(
                &q("demo::reverse"),
                &[Type::String],
                vec![list(&[1, 2])],
                &Policy::Default,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, DispatchError::BadArgument { index: 0, .. }),
            "{err}"
        );
    }
}
