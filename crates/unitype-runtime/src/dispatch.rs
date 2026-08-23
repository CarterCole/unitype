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
}

impl Dispatcher {
    /// A dispatcher over a workspace and what has been registered against it.
    pub fn new(ws: Arc<CheckedWorkspace>, registry: Arc<Registry>) -> Self {
        Self {
            ws,
            registry,
            options: Options::default(),
        }
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
        let mut found: Vec<&Implementation> = Vec::new();
        let mut seen = HashSet::new();

        // A proof of anything that strengthens this proposition proves it too.
        let mut frontier = vec![sig.clone()];
        while let Some(name) = frontier.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            found.extend(self.registry.proofs_of(&name));
            frontier.extend(self.ws.refiners_of(&name).iter().cloned());
        }

        if let Policy::Pinned(id) = policy {
            found.retain(|i| &i.id == id);
            return found;
        }

        found.sort_by_key(|i| self.rank(i, policy));
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
    pub fn call<'a>(
        &'a self,
        sig: &'a QualifiedName,
        args: Vec<Value>,
        policy: &'a Policy,
    ) -> BoxFuture<'a, Result<Invocation, DispatchError>> {
        Box::pin(async move { self.dispatch(sig, args, policy).await })
    }

    async fn dispatch(
        &self,
        name: &QualifiedName,
        args: Vec<Value>,
        policy: &Policy,
    ) -> Result<Invocation, DispatchError> {
        let Some(sig) = self.ws.sig(name) else {
            return Err(DispatchError::UnknownSignature(name.clone()));
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

        let candidates = self.candidates(name, policy);
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
        for implementation in candidates {
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
            let outcome = self
                .invoke(implementation, args.clone(), &expected, policy)
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
            // start it, which gives a precise reason rather than a guess.
            Reach::Subprocess { .. } => Ok(()),
            other => Err(format!(
                "reaching a `{}` environment is not built into this binary yet",
                other.kind()
            )),
        }
    }

    /// Runs one implementation.
    async fn invoke(
        &self,
        implementation: &Implementation,
        args: Vec<Value>,
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
            Reach::Subprocess { .. } => {
                crate::subprocess::invoke(&self.ws, environment, implementation, args, expected)
                    .await
            }
            other => Err(ExecError::Unreachable {
                environment: environment.id.to_string(),
                detail: format!(
                    "reaching a `{}` environment is not built in yet",
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
}
