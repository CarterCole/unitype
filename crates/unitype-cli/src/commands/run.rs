//! `unitype run` — call a proposition and see which proof answered.

use std::sync::Arc;

use clap::ValueEnum;
use serde_json::json;
use unitype_core::ir::Type;
use unitype_core::value::Value;
use unitype_core::{Ident, QualifiedName};
use unitype_runtime::{Dispatcher, ImplId, Policy};

use super::{Output, qualified};
use crate::Failure;
use crate::project::Project;

/// How to choose among the proofs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum Choice {
    /// Whatever is nearest to hand.
    Default,
    /// The best time complexity on offer.
    Fastest,
    /// The smallest memory footprint on offer.
    MinMemory,
}

/// Calls a proposition.
///
/// `at` is the type arguments, as a JSON array, for a proposition that is
/// generic: `reverse<T>` is a claim about every `T`, and a call is made at one.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    project: &Project,
    signature: &str,
    args: &str,
    at: Option<&str>,
    choice: Choice,
    pinned: Option<String>,
    verbose: bool,
    out: &Output,
) -> Result<(), Failure> {
    let name = qualified(signature, "signature name")?;
    let Some(declared) = project.workspace.sig(&name) else {
        return Err(Failure::check(format!(
            "`{name}` is not a proposition here. Try `unitype ls sigs`."
        )));
    };

    let type_args = type_arguments(project, declared, &name, at)?;
    // Everything below reads and checks against the proposition at those
    // types, which for one that is not generic is the proposition itself.
    let sig = &declared.instantiate(&type_args);

    // The arguments arrive as one JSON array and are read against the
    // parameters, so each becomes what its parameter says it is.
    let parsed: serde_json::Value = serde_json::from_str(args)
        .map_err(|e| Failure::usage(format!("--args must be a JSON array: {e}")))?;
    let serde_json::Value::Array(raw) = parsed else {
        return Err(Failure::usage(
            "--args must be a JSON array, even for one argument.",
        ));
    };
    if raw.len() != sig.params.len() {
        return Err(Failure::usage(format!(
            "`{name}` takes {} argument{}, but {} were given.",
            sig.params.len(),
            if sig.params.len() == 1 { "" } else { "s" },
            raw.len()
        )));
    }

    let mut values: Vec<Value> = Vec::with_capacity(raw.len());
    for (param, given) in sig.params.iter().zip(&raw) {
        values.push(
            project
                .workspace
                .read(given, &param.ty)
                .map_err(|e| Failure::check(format!("argument `{}`: {e}", param.name)))?,
        );
    }

    let policy = match pinned {
        Some(id) => Policy::Pinned(ImplId::new(id)),
        None => match choice {
            Choice::Default => Policy::Default,
            Choice::Fastest => Policy::Fastest,
            Choice::MinMemory => Policy::MinMemory,
        },
    };

    // Shared, not rebuilt: the registry holds functions compiled into this
    // process, and rebuilding it from disk would silently lose them.
    let ws = Arc::clone(&project.workspace);
    let dispatcher = Dispatcher::new(Arc::clone(&ws), Arc::clone(&project.registry));

    let considered = dispatcher.candidates(&name, &policy);
    // Padded to the widest, so the costs line up and the ordering the policy
    // chose can be read down the column rather than parsed out of each row.
    let widest_id = considered
        .iter()
        .map(|i| i.id.as_str().len())
        .max()
        .unwrap_or(0);
    let widest_env = considered
        .iter()
        .map(|i| i.environment.as_str().len())
        .max()
        .unwrap_or(0);
    let considered_before: Vec<String> = considered
        .iter()
        .map(|i| {
            format!(
                "{:widest_id$}  {:widest_env$}  {}",
                i.id.as_str(),
                i.environment.as_str(),
                i.cost
            )
        })
        .collect();

    if verbose && !out.is_machine() {
        out.say(format!(
            "{}::{declared}{}",
            name.universe,
            binding(declared, &type_args)
        ));
        out.say("candidates:");
        for line in &considered_before {
            out.say(format!("  {line}"));
        }
    }

    let answer = dispatcher
        .call_at(&name, &type_args, values, &policy)
        .await
        .map_err(|e| Failure::run(e.to_string()))?;

    let written = ws
        .write_json(&answer.value, &returned(sig))
        .unwrap_or_else(|_| answer.value.to_json_string().unwrap_or_default());

    let mut result = json!({
        "signature": name.to_string(),
        "value": serde_json::from_str::<serde_json::Value>(&written).unwrap_or_default(),
        "implementation": answer.implementation.to_string(),
        "environment": answer.environment.to_string(),
        "micros": answer.elapsed.as_micros(),
        "candidates": considered_before,
    });
    if !type_args.is_empty() {
        result["at"] = type_args.iter().map(ToString::to_string).collect();
    }
    out.result(result, || {
        if verbose {
            out.say(format!(
                "chose {} in {}",
                answer.implementation, answer.environment
            ));
            out.say("");
        }
        out.say(&written);
    });
    Ok(())
}

/// Reads the types a generic proposition is called at.
///
/// Written the way a universe writes a type — `"i64"`, `"[str]"`,
/// `"map<str, i64>"`, or the object form — with a bare name taken to be in
/// the proposition's own universe, as it would be there.
fn type_arguments(
    project: &Project,
    sig: &unitype_core::FunctionSig,
    name: &QualifiedName,
    at: Option<&str>,
) -> Result<Vec<Type>, Failure> {
    let wanted: Vec<String> = sig.params_decl.iter().map(|p| p.name.to_string()).collect();
    let Some(at) = at else {
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        return Err(Failure::usage(format!(
            "`{name}` is generic over {}, so a call says which type{} with --at, \
             as in --at '[{}]'.",
            wanted.join(", "),
            if wanted.len() == 1 {
                " it is at"
            } else {
                "s it is at"
            },
            vec!["\"i64\""; wanted.len()].join(",")
        )));
    };

    let parsed: serde_json::Value = serde_json::from_str(at)
        .map_err(|e| Failure::usage(format!("--at must be a JSON array of types: {e}")))?;
    let serde_json::Value::Array(given) = parsed else {
        return Err(Failure::usage(
            "--at must be a JSON array of types, even for one.",
        ));
    };
    if given.len() != wanted.len() {
        return Err(Failure::usage(if wanted.is_empty() {
            format!("`{name}` is not generic, so it takes no --at.")
        } else {
            format!(
                "`{name}` is generic over {}, but {} type{} given.",
                wanted.join(", "),
                given.len(),
                if given.len() == 1 { " was" } else { "s were" }
            )
        }));
    }

    given
        .into_iter()
        .map(|mut ty| {
            unitype_core::ir::sugar::desugar_type(&mut ty)
                .map_err(|e| Failure::usage(format!("--at: {e}")))?;
            let ty: Type = serde_json::from_value(ty)
                .map_err(|e| Failure::usage(format!("--at: not a type: {e}")))?;
            let ty = in_universe(&ty, &name.universe);
            known(project, &ty)?;
            Ok(ty)
        })
        .collect()
}

/// A type with every reference that names no universe put in `home`.
fn in_universe(ty: &Type, home: &Ident) -> Type {
    let kids: Vec<Type> = ty
        .children()
        .into_iter()
        .map(|c| in_universe(c, home))
        .collect();
    match ty.with_children(kids) {
        Type::Ref(mut r) => {
            r.universe.get_or_insert_with(|| home.clone());
            Type::Ref(r)
        }
        other => other,
    }
}

/// Refuses a type that names a declaration the project does not have, or
/// gives one the wrong number of arguments.
fn known(project: &Project, ty: &Type) -> Result<(), Failure> {
    if let Type::Ref(r) = ty {
        let target = QualifiedName::new(
            r.universe.clone().expect("put in a universe already"),
            r.name.clone(),
        );
        let Some(def) = project.workspace.def(&target) else {
            return Err(Failure::check(format!(
                "--at: `{target}` is not a type here. Try `unitype ls types`."
            )));
        };
        if def.params().len() != r.args.len() {
            return Err(Failure::usage(format!(
                "--at: `{target}` takes {} type argument(s), but {} were given.",
                def.params().len(),
                r.args.len()
            )));
        }
    }
    ty.children()
        .into_iter()
        .try_for_each(|c| known(project, c))
}

/// How a call binds a generic proposition's parameters, as ` at T = i64`, or
/// nothing for one that has none.
fn binding(sig: &unitype_core::FunctionSig, type_args: &[Type]) -> String {
    if type_args.is_empty() {
        return String::new();
    }
    let bound: Vec<String> = sig
        .params_decl
        .iter()
        .zip(type_args)
        .map(|(p, t)| format!("{} = {t}", p.name))
        .collect();
    format!(" at {}", bound.join(", "))
}

/// What a signature actually hands back, once waiting is accounted for.
fn returned(sig: &unitype_core::FunctionSig) -> unitype_core::ir::Type {
    match &sig.ret {
        unitype_core::ir::Type::Future { of } => (**of).clone(),
        other => other.clone(),
    }
}
