//! `unitype conform` — do the proofs of each proposition agree?

use std::sync::Arc;

use serde_json::json;
use unitype_runtime::Dispatcher;
use unitype_runtime::conform::{Answer, Report, Suite, run as run_suite};

use super::Output;
use crate::Failure;
use crate::project::Project;

/// Runs every suite in the project, or the one for a named proposition.
pub async fn run(project: &Project, only: Option<String>, out: &Output) -> Result<(), Failure> {
    let suites = load(project, only.as_deref())?;
    if suites.is_empty() {
        out.say("No vector files to run. Put some under `conform/`.");
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

    let disagreed: Vec<&Report> = reports.iter().filter(|r| !r.agreed()).collect();

    out.result(
        json!(
            reports
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
                            .collect::<std::collections::BTreeMap<_, _>>(),
                    })).collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>()
        ),
        || {
            for report in &reports {
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
                // A proof that could not be run is not a disagreement, but it
                // is worth knowing that it was not actually checked.
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
        },
    );

    if disagreed.is_empty() {
        Ok(())
    } else {
        Err(Failure::disagreement(format!(
            "{} proposition{} where the proofs do not agree.",
            disagreed.len(),
            plural(disagreed.len())
        )))
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

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
