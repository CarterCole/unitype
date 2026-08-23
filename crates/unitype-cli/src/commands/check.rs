//! `unitype check` — does this project hold together?

use serde_json::json;
use unitype_runtime::RegistryWarning;

use super::Output;
use crate::Failure;
use crate::project::Project;

/// Reports on the project. Loading it is most of the check; what remains is
/// whether the proofs still match the propositions.
pub fn run(project: &Project, out: &Output) -> Result<(), Failure> {
    let ws = &project.workspace;
    let drift: Vec<RegistryWarning> = project.registry.check_against(ws);
    let warnings: Vec<String> = ws
        .warnings()
        .iter()
        .map(ToString::to_string)
        .chain(drift.iter().map(ToString::to_string))
        .collect();

    let universes = ws.workspace().universes.len();
    let types = ws.def_names().count();
    let signatures = ws.sig_names().count();
    let links = ws.workspace().links.len();
    let proofs = project.registry.implementations().len();
    let environments = project.registry.environments().count();

    out.result(
        json!({
            "project": project.name(),
            "universes": universes,
            "types": types,
            "signatures": signatures,
            "links": links,
            "implementations": proofs,
            "environments": environments,
            "warnings": warnings,
        }),
        || {
            out.say(format!("{} checks out.", project.name()));
            out.say("");
            out.say(format!(
                "  {universes} universe{}, {types} type{}, {signatures} signature{}",
                plural(universes),
                plural(types),
                plural(signatures)
            ));
            out.say(format!(
                "  {links} link{}, {proofs} proof{} across {environments} environment{}",
                plural(links),
                plural(proofs),
                plural(environments)
            ));
            if !warnings.is_empty() {
                out.heading("Worth knowing:");
                for warning in &warnings {
                    out.say(format!("  {warning}"));
                }
            }
        },
    );

    Ok(())
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}
