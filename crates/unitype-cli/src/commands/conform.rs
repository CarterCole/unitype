//! `unitype conform` — do the proofs of each proposition agree?
//!
//! Two kinds of check, with two kinds of evidence. A vector file holds the
//! proofs of a proposition to each other, and to an expected answer where one
//! is given. A generic proposition needs no file: its signature alone says
//! what every proof of it must do, and the proofs are held to that.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use unitype_core::QualifiedName;
use unitype_runtime::Dispatcher;
use unitype_runtime::conform::{Answer, Report, Suite, run as run_suite};
use unitype_runtime::parametric::{ParametricError, Theorems, applies, check as check_theorem};

use super::Output;
use crate::Failure;
use crate::project::Project;

/// Runs every suite in the project, or the one for a named proposition, and
/// holds every generic proposition with a proof to its free theorem.
pub async fn run(project: &Project, only: Option<String>, out: &Output) -> Result<(), Failure> {
    let suites = load(project, only.as_deref())?;
    let generic = generic_propositions(project, only.as_deref());

    if suites.is_empty() && generic.is_empty() {
        out.say(
            "Nothing to check. Put vector files under `conform/`, or declare a generic \
             proposition and prove it.",
        );
        return Ok(());
    }

    let dispatcher = Dispatcher::new(
        Arc::clone(&project.workspace),
        Arc::clone(&project.registry),
    );

    let mut reports = Vec::with_capacity(suites.len());
    for suite in &suites {
        reports.push(
            run_suite(&dispatcher, suite)
                .await
                .map_err(|e| Failure::check(e.to_string()))?,
        );
    }

    let mut theorems: Vec<Theorems> = Vec::new();
    let mut not_checked: Vec<(QualifiedName, String)> = Vec::new();
    for (name, why_not) in generic {
        if let Some(why) = why_not {
            not_checked.push((name, why));
            continue;
        }
        match check_theorem(&dispatcher, &name).await {
            Ok(t) => theorems.push(t),
            // A generic proposition nobody has proved has nothing to hold to
            // its theorem, which is no more worth remarking on than a
            // proposition without a vector file.
            Err(ParametricError::NoProofs(_)) => {}
            Err(e) => return Err(Failure::check(e.to_string())),
        }
    }

    let disagreed: Vec<&Report> = reports.iter().filter(|r| !r.agreed()).collect();
    let broken: Vec<&Theorems> = theorems.iter().filter(|t| !t.held()).collect();

    out.result(
        json!({
            "suites": reports
                .iter()
                .map(|r| json!({
                    "signature": r.signature.to_string(),
                    "proofs": r.proofs.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "agreed": r.agreed(),
                    "cases": r.cases.iter().map(|c| json!({
                        "name": c.name,
                        "agreed": c.agreed,
                        "wrong": c.wrong.iter().map(ToString::to_string).collect::<Vec<_>>(),
                        "answers": c.answers.iter()
                            .map(|(id, a)| (id.to_string(), a.summary()))
                            .collect::<BTreeMap<_, _>>(),
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "theorems": theorems
                .iter()
                .map(|t| json!({
                    "signature": t.signature.to_string(),
                    "params": t.params.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "proofs": t.proofs.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "held": t.held(),
                    "trials": t.trials.iter().map(|trial| json!({
                        "name": trial.name,
                        "held": trial.held(),
                        "verdicts": trial.verdicts.iter()
                            .map(|(id, v)| (id.to_string(), v.summary()))
                            .collect::<BTreeMap<_, _>>(),
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "not_checked": not_checked
                .iter()
                .map(|(name, why)| json!({"signature": name.to_string(), "why": why}))
                .collect::<Vec<_>>(),
        }),
        || {
            for report in &reports {
                say_report(out, report);
            }
            for t in &theorems {
                say_theorems(out, t);
            }
            for (name, why) in &not_checked {
                out.say(format!("{name}  not held to a free theorem: {why}"));
            }
        },
    );

    if disagreed.is_empty() && broken.is_empty() {
        return Ok(());
    }
    let mut complaints = Vec::new();
    if !disagreed.is_empty() {
        complaints.push(format!(
            "{} proposition{} where the proofs do not agree",
            disagreed.len(),
            plural(disagreed.len())
        ));
    }
    if !broken.is_empty() {
        complaints.push(format!(
            "{} proposition{} where a proof breaks its free theorem",
            broken.len(),
            plural(broken.len())
        ));
    }
    Err(Failure::disagreement(format!("{}.", complaints.join("; "))))
}

fn say_report(out: &Output, report: &Report) {
    let cases = report.cases.len();
    out.say(format!(
        "{}  {} proof{}, {cases} case{}{}",
        report.signature,
        report.proofs.len(),
        plural(report.proofs.len()),
        plural(cases),
        if report.agreed() {
            "  — all agree"
        } else {
            ""
        }
    ));

    for case in report.disagreements() {
        out.say(format!("  {}", case.name));
        for (id, answer) in &case.answers {
            let mark = if case.wrong.contains(id) { "x" } else { " " };
            out.say(format!("    {mark} {id}: {}", answer.summary()));
        }
    }
    // A proof that could not be run is not a disagreement, but it is worth
    // knowing that it was not actually checked.
    let skipped: Vec<String> = report
        .cases
        .first()
        .map(|c| {
            c.answers
                .iter()
                .filter(|(_, a)| matches!(a, Answer::Skipped(_)))
                .map(|(id, _)| id.to_string())
                .collect()
        })
        .unwrap_or_default();
    if !skipped.is_empty() {
        out.say(format!("  not checked here: {}", skipped.join(", ")));
    }
}

fn say_theorems(out: &Output, t: &Theorems) {
    out.say(format!(
        "{}  {} proof{}, {} trial{}{}",
        t.heading(),
        t.proofs.len(),
        plural(t.proofs.len()),
        t.trials.len(),
        plural(t.trials.len()),
        if t.held() {
            "  — free theorem holds"
        } else {
            ""
        }
    ));
    for trial in t.broken() {
        out.say(format!("  {}", trial.name));
        for (id, verdict) in &trial.verdicts {
            if verdict.is_against() {
                out.say(format!("    x {id}: {}", verdict.summary()));
            }
        }
    }
}

/// The suites in the project, or the one for a named proposition.
fn load(project: &Project, only: Option<&str>) -> Result<Vec<Suite>, Failure> {
    let mut found = Vec::new();
    for entry in &project.file.conform {
        for path in crate::project::json_files(&project.root.join(entry)) {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| Failure::usage(format!("could not read {}: {e}", path.display())))?;
            let suite = Suite::from_json(&text)
                .map_err(|e| Failure::check(format!("{}: {e}", path.display())))?;
            if only.is_none_or(|wanted| suite.signature.to_string() == wanted) {
                found.push(suite);
            }
        }
    }
    Ok(found)
}

/// Every generic proposition in the workspace, with why it cannot be held to
/// its theorem where it cannot.
fn generic_propositions(
    project: &Project,
    only: Option<&str>,
) -> Vec<(QualifiedName, Option<String>)> {
    let ws = &project.workspace;
    let mut found = Vec::new();
    for name in ws.sig_names() {
        if only.is_some_and(|wanted| name.to_string() != wanted) {
            continue;
        }
        match applies(ws, name) {
            Ok(()) => found.push((name.clone(), None)),
            Err(ParametricError::NotGeneric(_)) => {}
            Err(why) => found.push((name.clone(), Some(why.to_string()))),
        }
    }
    found.sort_by_key(|(name, _)| name.to_string());
    found
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
