//! `unitype run` — call a proposition and see which proof answered.

use std::sync::Arc;

use clap::ValueEnum;
use serde_json::json;
use unitype_core::value::Value;
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
#[allow(clippy::too_many_arguments)]
pub async fn run(
    project: &Project,
    signature: &str,
    args: &str,
    choice: Choice,
    pinned: Option<String>,
    verbose: bool,
    out: &Output,
) -> Result<(), Failure> {
    let name = qualified(signature, "signature name")?;
    let Some(sig) = project.workspace.sig(&name) else {
        return Err(Failure::check(format!(
            "`{name}` is not a proposition here. Try `unitype ls sigs`."
        )));
    };

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
        out.say(format!("{}::{sig}", name.universe));
        out.say("candidates:");
        for line in &considered_before {
            out.say(format!("  {line}"));
        }
    }

    let answer = dispatcher
        .call(&name, values, &policy)
        .await
        .map_err(|e| Failure::run(e.to_string()))?;

    let written = ws
        .write_json(&answer.value, &returned(sig))
        .unwrap_or_else(|_| answer.value.to_json_string().unwrap_or_default());

    out.result(
        json!({
            "signature": name.to_string(),
            "value": serde_json::from_str::<serde_json::Value>(&written).unwrap_or_default(),
            "implementation": answer.implementation.to_string(),
            "environment": answer.environment.to_string(),
            "micros": answer.elapsed.as_micros(),
            "candidates": considered_before,
        }),
        || {
            if verbose {
                out.say(format!(
                    "chose {} in {}",
                    answer.implementation, answer.environment
                ));
                out.say("");
            }
            out.say(&written);
        },
    );
    Ok(())
}

/// What a signature actually hands back, once waiting is accounted for.
fn returned(sig: &unitype_core::FunctionSig) -> unitype_core::ir::Type {
    match &sig.ret {
        unitype_core::ir::Type::Future { of } => (**of).clone(),
        other => other.clone(),
    }
}
