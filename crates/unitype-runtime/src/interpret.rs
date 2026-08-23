//! Running a constructive proof.
//!
//! A signature carrying a body can be answered without anyone implementing it:
//! the body *is* the implementation, and this walks it. That makes every
//! derived bridge runnable the moment it is derived, with no code generated
//! and no language involved.
//!
//! The language is total — no recursion, no unbounded loops, and a call graph
//! the checker has already shown to be acyclic — so evaluation terminates. The
//! depth limit here is a backstop for a body that reached this point without
//! being checked, not something a well-formed body can hit.
//!
//! Calls out to other signatures go back through the dispatcher rather than
//! being resolved here, so a body that calls `sort` gets whichever proof of
//! `sort` the current policy prefers. A body says what to compute; it does not
//! decide who computes it.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use unitype_core::check::CheckedWorkspace;
use unitype_core::ir::{Expr, Functor};
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};

use crate::error::ExecError;

/// A future that can be held in a trait object.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// How a body reaches other signatures.
///
/// The interpreter does not choose implementations; it asks. That keeps policy
/// in one place and means a body written once benefits from a faster proof
/// appearing later.
pub trait Calls: Send + Sync {
    /// Invokes a signature and returns what it produced.
    fn call<'a>(
        &'a self,
        sig: &'a QualifiedName,
        args: Vec<Value>,
    ) -> BoxFuture<'a, Result<Value, ExecError>>;
}

/// A world in which nothing can be called.
///
/// Enough for a body that only rearranges what it was given, which is most
/// derived bridges.
pub struct NoCalls;

impl Calls for NoCalls {
    fn call<'a>(
        &'a self,
        sig: &'a QualifiedName,
        _args: Vec<Value>,
    ) -> BoxFuture<'a, Result<Value, ExecError>> {
        Box::pin(async move { Err(ExecError::UnknownSignature(sig.clone())) })
    }
}

/// The names bound where an expression is being evaluated.
pub type Scope = HashMap<Ident, Value>;

/// How deep evaluation may go before giving up.
const MAX_DEPTH: usize = 256;

/// Runs a body with its parameters bound.
pub async fn eval(
    ws: &CheckedWorkspace,
    expr: &Expr,
    scope: Scope,
    calls: &dyn Calls,
) -> Result<Value, ExecError> {
    run(ws, expr, &scope, calls, 0).await
}

/// Runs the body of a signature against positional arguments.
pub async fn eval_signature(
    ws: &CheckedWorkspace,
    sig: &QualifiedName,
    args: Vec<Value>,
    calls: &dyn Calls,
) -> Result<Value, ExecError> {
    let Some(declared) = ws.sig(sig) else {
        return Err(ExecError::UnknownSignature(sig.clone()));
    };
    let Some(body) = &declared.body else {
        return Err(ExecError::NotRegistered(sig.to_string()));
    };
    let scope: Scope = declared
        .params
        .iter()
        .map(|p| p.name.clone())
        .zip(args)
        .collect();
    eval(ws, body, scope, calls).await
}

fn run<'a>(
    ws: &'a CheckedWorkspace,
    expr: &'a Expr,
    scope: &'a Scope,
    calls: &'a dyn Calls,
    depth: usize,
) -> BoxFuture<'a, Result<Value, ExecError>> {
    Box::pin(async move {
        if depth > MAX_DEPTH {
            return Err(ExecError::TooDeep);
        }

        match expr {
            Expr::Lit { value } => Ok(value.clone()),

            Expr::Var { name } => scope
                .get(name)
                .cloned()
                .ok_or_else(|| ExecError::Unbound(name.to_string())),

            Expr::Field { of, name } => {
                let subject = run(ws, of, scope, calls, depth + 1).await?;
                match &subject {
                    // A key that is not there means the field was absent, which
                    // is the same state as null throughout this system. The
                    // checker has already established that the field exists on
                    // the type, so this cannot be a misspelling.
                    Value::Record(fields) => {
                        Ok(fields.get(name.as_str()).cloned().unwrap_or(Value::Null))
                    }
                    other => Err(ExecError::NoSuchField {
                        value: other.kind_name().to_string(),
                        field: name.to_string(),
                    }),
                }
            }

            Expr::MakeRecord { fields, .. } => {
                let mut out = indexmap::IndexMap::with_capacity(fields.len());
                for init in fields {
                    out.insert(
                        init.name.to_string(),
                        run(ws, &init.value, scope, calls, depth + 1).await?,
                    );
                }
                Ok(Value::Record(out))
            }

            Expr::MakeList { items } => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(run(ws, item, scope, calls, depth + 1).await?);
                }
                Ok(Value::List(out))
            }

            Expr::MakeTuple { items } => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(run(ws, item, scope, calls, depth + 1).await?);
                }
                Ok(Value::Tuple(out))
            }

            Expr::MakeVariant { tag, value, .. } => {
                let payload = match value {
                    Some(v) => run(ws, v, scope, calls, depth + 1).await?,
                    None => Value::Null,
                };
                Ok(Value::variant(tag.to_string(), payload))
            }

            Expr::Call { sig, args } => {
                let mut evaluated = Vec::with_capacity(args.len());
                for arg in args {
                    evaluated.push(run(ws, arg, scope, calls, depth + 1).await?);
                }
                calls.call(sig, evaluated).await
            }

            Expr::Let { name, value, body } => {
                let bound = run(ws, value, scope, calls, depth + 1).await?;
                let mut inner = scope.clone();
                inner.insert(name.clone(), bound);
                run(ws, body, &inner, calls, depth + 1).await
            }

            Expr::If {
                cond,
                then,
                otherwise,
            } => {
                let taken = match run(ws, cond, scope, calls, depth + 1).await? {
                    Value::Bool(b) => b,
                    other => {
                        return Err(ExecError::Untyped(format!(
                            "a condition must be a boolean, but this is {}",
                            other.kind_name()
                        )));
                    }
                };
                let branch = if taken { then } else { otherwise };
                run(ws, branch, scope, calls, depth + 1).await
            }

            Expr::MapThrough {
                functor,
                value,
                var,
                body,
            } => {
                let subject = run(ws, value, scope, calls, depth + 1).await?;
                map_through(ws, *functor, subject, var, body, scope, calls, depth).await
            }
        }
    })
}

/// Applies a transformation inside whatever constructor holds the value.
///
/// What "inside" means is the functor's business: every element of a list, the
/// value of an optional if there is one, the eventual result of a future. A
/// future has already been awaited by the time a value reaches here, so it is
/// the same as a present optional.
#[allow(clippy::too_many_arguments)]
async fn map_through(
    ws: &CheckedWorkspace,
    functor: Functor,
    subject: Value,
    var: &Ident,
    body: &Expr,
    scope: &Scope,
    calls: &dyn Calls,
    depth: usize,
) -> Result<Value, ExecError> {
    let apply = async |element: Value| -> Result<Value, ExecError> {
        let mut inner = scope.clone();
        inner.insert(var.clone(), element);
        run(ws, body, &inner, calls, depth + 1).await
    };

    match (functor, subject) {
        (Functor::List, Value::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(apply(item).await?);
            }
            Ok(Value::List(out))
        }
        // Absent stays absent. This is the whole reason the form exists: it
        // reaches inside optionality without letting a body ask whether
        // something is there.
        (Functor::Optional | Functor::Future, Value::Null) => Ok(Value::Null),
        (Functor::Optional | Functor::Future, present) => apply(present).await,
        (functor, other) => Err(ExecError::Untyped(format!(
            "cannot map through {} over {}",
            functor.describe(),
            other.kind_name()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unitype_core::Workspace;
    use unitype_core::check::check_workspace;
    use unitype_core::ir::Type;
    use unitype_core::value::Value;

    fn ws(json: &str) -> CheckedWorkspace {
        check_workspace(Workspace::from_json(json).expect("reads")).expect("checks")
    }

    fn q(s: &str) -> QualifiedName {
        s.parse().unwrap()
    }

    fn v(json: &str) -> Value {
        Value::parse_json(json).expect("reads")
    }

    const TYPES: &str = r#"
        {"k":"struct","name":"User","fields":[
            {"name":"first","ty":"str"},{"name":"last","ty":"str"},
            {"name":"nickname","ty":"str?"}]},
        {"k":"struct","name":"Badge","fields":[
            {"name":"label","ty":"str"},{"name":"adult","ty":"bool"}]}"#;

    fn universe(functions: &str) -> CheckedWorkspace {
        ws(&format!(
            r#"{{"universes":[{{"name":"demo","types":[{TYPES}],"functions":[{functions}]}}]}}"#
        ))
    }

    #[tokio::test]
    async fn a_body_builds_what_it_said_it_would() {
        let ws = universe(
            r#"{"name":"badge_of","params":[{"name":"u","ty":"User"}],"ret":"Badge",
                "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}},
                    {"name":"adult","value":{"k":"lit","value":true}}]}}"#,
        );
        let user = v(r#"{"first":"Ada","last":"Lovelace"}"#);
        let badge = eval_signature(&ws, &q("demo::badge_of"), vec![user], &NoCalls)
            .await
            .expect("runs");

        assert_eq!(badge.get("label"), Some(&Value::Str("Ada".into())));
        assert_eq!(badge.get("adult"), Some(&Value::Bool(true)));
    }

    #[tokio::test]
    async fn what_a_body_produces_is_what_its_signature_promised() {
        let ws = universe(
            r#"{"name":"badge_of","params":[{"name":"u","ty":"User"}],"ret":"Badge",
                "body":{"k":"make_record","ty":"Badge","fields":[
                    {"name":"label","value":{"k":"field","of":{"k":"var","name":"u"},"name":"last"}},
                    {"name":"adult","value":{"k":"lit","value":false}}]}}"#,
        );
        let badge = eval_signature(
            &ws,
            &q("demo::badge_of"),
            vec![v(r#"{"first":"Ada","last":"Lovelace"}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");

        let badge_ty = Type::Ref(unitype_core::ir::TypeRef::qualified(
            Ident::new("demo").unwrap(),
            Ident::new("Badge").unwrap(),
        ));
        assert!(
            ws.validate(&badge, &badge_ty).is_ok(),
            "the body should produce a real Badge: {badge:?}"
        );
    }

    #[tokio::test]
    async fn a_binding_is_visible_in_what_follows_it() {
        let ws = universe(
            r#"{"name":"f","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"let","name":"n",
                        "value":{"k":"field","of":{"k":"var","name":"u"},"name":"first"},
                        "body":{"k":"var","name":"n"}}}"#,
        );
        let out = eval_signature(
            &ws,
            &q("demo::f"),
            vec![v(r#"{"first":"Ada","last":"L"}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");
        assert_eq!(out, Value::Str("Ada".into()));
    }

    #[tokio::test]
    async fn only_the_branch_that_was_taken_runs() {
        // The untaken arm calls a signature that nothing here can answer. If
        // both arms ran, this would fail rather than returning the fallback.
        let ws = universe(
            r#"{"name":"shout","params":[{"name":"s","ty":"str"}],"ret":"str"},
               {"name":"f","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"if","cond":{"k":"lit","value":false},
                        "then":{"k":"call","sig":"demo::shout",
                                "args":[{"k":"field","of":{"k":"var","name":"u"},"name":"first"}]},
                        "else":{"k":"lit","value":"fallback"}}}"#,
        );
        let out = eval_signature(
            &ws,
            &q("demo::f"),
            vec![v(r#"{"first":"A","last":"B"}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");
        assert_eq!(out, Value::Str("fallback".into()));
    }

    #[tokio::test]
    async fn a_field_that_was_not_sent_reads_as_absent() {
        // Missing, null, and none are one state here, so a body reading an
        // optional field that nobody sent gets absence rather than an error.
        let ws = universe(
            r#"{"name":"nick_raw","params":[{"name":"u","ty":"User"}],"ret":"str?",
                "body":{"k":"field","of":{"k":"var","name":"u"},"name":"nickname"}}"#,
        );
        let out = eval_signature(
            &ws,
            &q("demo::nick_raw"),
            vec![v(r#"{"first":"A","last":"B"}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");
        assert_eq!(out, Value::Null);
    }

    #[tokio::test]
    async fn mapping_through_a_list_visits_every_element() {
        let ws = universe(
            r#"{"name":"names","params":[{"name":"us","ty":"[User]"}],"ret":"[str]",
                "body":{"k":"map_through","functor":"list","value":{"k":"var","name":"us"},
                        "var":"u",
                        "body":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}}}"#,
        );
        let out = eval_signature(
            &ws,
            &q("demo::names"),
            vec![v(
                r#"[{"first":"Ada","last":"L"},{"first":"Grace","last":"H"}]"#,
            )],
            &NoCalls,
        )
        .await
        .expect("runs");

        assert_eq!(
            out,
            Value::List(vec![Value::Str("Ada".into()), Value::Str("Grace".into())])
        );
    }

    #[tokio::test]
    async fn mapping_through_an_empty_list_produces_an_empty_list() {
        let ws = universe(
            r#"{"name":"names","params":[{"name":"us","ty":"[User]"}],"ret":"[str]",
                "body":{"k":"map_through","functor":"list","value":{"k":"var","name":"us"},
                        "var":"u",
                        "body":{"k":"field","of":{"k":"var","name":"u"},"name":"first"}}}"#,
        );
        let out = eval_signature(&ws, &q("demo::names"), vec![v("[]")], &NoCalls)
            .await
            .expect("runs");
        assert_eq!(out, Value::List(vec![]));
    }

    #[tokio::test]
    async fn what_was_absent_stays_absent() {
        let ws = universe(
            r#"{"name":"nick","params":[{"name":"u","ty":"User"}],"ret":"str?",
                "body":{"k":"map_through","functor":"optional",
                        "value":{"k":"field","of":{"k":"var","name":"u"},"name":"nickname"},
                        "var":"n","body":{"k":"var","name":"n"}}}"#,
        );

        let present = eval_signature(
            &ws,
            &q("demo::nick"),
            vec![v(r#"{"first":"A","last":"B","nickname":"Ace"}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");
        assert_eq!(present, Value::Str("Ace".into()));

        let absent = eval_signature(
            &ws,
            &q("demo::nick"),
            vec![v(r#"{"first":"A","last":"B"}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");
        assert_eq!(absent, Value::Null, "nothing was there to transform");
    }

    #[tokio::test]
    async fn a_body_calling_another_signature_goes_back_out_to_ask() {
        struct Shouts;
        impl Calls for Shouts {
            fn call<'a>(
                &'a self,
                _sig: &'a QualifiedName,
                args: Vec<Value>,
            ) -> BoxFuture<'a, Result<Value, ExecError>> {
                Box::pin(async move {
                    let Some(Value::Str(s)) = args.first() else {
                        return Err(ExecError::Untyped("expected a string".into()));
                    };
                    Ok(Value::Str(s.to_uppercase()))
                })
            }
        }

        let ws = universe(
            r#"{"name":"shout","params":[{"name":"s","ty":"str"}],"ret":"str"},
               {"name":"loud_name","params":[{"name":"u","ty":"User"}],"ret":"str",
                "body":{"k":"call","sig":"demo::shout",
                        "args":[{"k":"field","of":{"k":"var","name":"u"},"name":"first"}]}}"#,
        );
        let out = eval_signature(
            &ws,
            &q("demo::loud_name"),
            vec![v(r#"{"first":"Ada","last":"L"}"#)],
            &Shouts,
        )
        .await
        .expect("runs");
        assert_eq!(out, Value::Str("ADA".into()));
    }

    #[tokio::test]
    async fn a_signature_with_no_body_has_nothing_to_run() {
        let ws = universe(r#"{"name":"sort","params":[{"name":"xs","ty":"[i64]"}],"ret":"[i64]"}"#);
        let err = eval_signature(&ws, &q("demo::sort"), vec![v("[3,1,2]")], &NoCalls)
            .await
            .unwrap_err();
        assert!(
            matches!(err, ExecError::NotRegistered(_)),
            "sorting is a proposition someone has to prove: {err}"
        );
    }

    /// The point of all of this: a bridge nobody wrote runs.
    #[tokio::test]
    async fn a_derived_bridge_runs_without_anyone_writing_it() {
        let base = ws(r#"{"universes":[
            {"name":"api","types":[{"k":"struct","name":"User","fields":[
                {"name":"userId","ty":"str"},{"name":"age","ty":"i32"}]}]},
            {"name":"db","types":[{"k":"struct","name":"UserRow","fields":[
                {"name":"user_id","ty":"str"},{"name":"age","ty":"i64"}]}]}]}"#);

        // Work out how one becomes the other...
        let kind = base
            .derive_link(&q("api::User"), &q("db::UserRow"), &[])
            .expect("derives");

        let mut workspace = base.workspace().clone();
        workspace.links.push(unitype_core::link::Link {
            name: Ident::new("user_row").unwrap(),
            doc: None,
            from: q("api::User"),
            to: q("db::UserRow"),
            kind,
            derived: true,
            meta: Default::default(),
        });
        let ws = check_workspace(workspace).expect("checks");

        // ...turn that into a body, put it in the workspace, and run it.
        let link = ws
            .workspace()
            .link(&Ident::new("user_row").unwrap())
            .unwrap();
        let bridge = ws.compile_link(link).expect("compiles").expect("succeeds");

        let mut workspace = ws.workspace().clone();
        workspace.universes[0].functions.push(bridge);
        let ws = check_workspace(workspace).expect("the derived bridge checks");

        let row = eval_signature(
            &ws,
            &q("api::user_row"),
            vec![v(r#"{"userId":"u-1","age":37}"#)],
            &NoCalls,
        )
        .await
        .expect("runs");

        assert_eq!(row.get("user_id"), Some(&Value::Str("u-1".into())));
        assert_eq!(row.get("age"), Some(&Value::Int(37)));

        let row_ty = Type::Ref(unitype_core::ir::TypeRef::qualified(
            Ident::new("db").unwrap(),
            Ident::new("UserRow").unwrap(),
        ));
        assert!(
            ws.validate(&row, &row_ty).is_ok(),
            "and what comes out really is a db::UserRow"
        );
    }
}
