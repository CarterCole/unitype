//! `unitype ls` — what is in this project?

use clap::ValueEnum;
use serde_json::json;
use unitype_core::canon::AliasIndex;

use super::Output;
use crate::Failure;
use crate::project::Project;

/// What to list.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum What {
    /// Everything, in summary.
    All,
    /// The universes.
    Universes,
    /// The type declarations.
    Types,
    /// The propositions.
    Sigs,
    /// The proofs, and where they run.
    Impls,
    /// The links between universes.
    Links,
    /// The places proofs can run.
    Environments,
}

/// Lists what was asked for.
pub fn run(project: &Project, what: What, out: &Output) -> Result<(), Failure> {
    match what {
        What::All => all(project, out),
        What::Universes => universes(project, out),
        What::Types => types(project, out),
        What::Sigs => sigs(project, out),
        What::Impls => impls(project, out),
        What::Links => links(project, out),
        What::Environments => environments(project, out),
    }
    Ok(())
}

fn all(project: &Project, out: &Output) {
    if out.is_machine() {
        universes(project, out);
        return;
    }
    universes(project, out);
    out.heading("Types");
    types(project, out);
    out.heading("Propositions");
    sigs(project, out);
    if !project.workspace.workspace().links.is_empty() {
        out.heading("Links");
        links(project, out);
    }
    out.heading("Proofs");
    impls(project, out);
}

fn universes(project: &Project, out: &Output) {
    let ws = project.workspace.workspace();
    out.result(
        json!(
            ws.universes
                .iter()
                .map(|u| json!({
                    "name": u.name.to_string(),
                    "doc": u.doc,
                    "types": u.types.len(),
                    "signatures": u.functions.len(),
                }))
                .collect::<Vec<_>>()
        ),
        || {
            for universe in &ws.universes {
                out.say(format!(
                    "  {}  {} type{}, {} signature{}",
                    universe.name,
                    universe.types.len(),
                    plural(universe.types.len()),
                    universe.functions.len(),
                    plural(universe.functions.len()),
                ));
                if let Some(doc) = &universe.doc {
                    out.say(format!("      {}", first_sentence(doc)));
                }
            }
        },
    );
}

fn types(project: &Project, out: &Output) {
    let ws = &project.workspace;
    // Aliases are shortened against everything loaded, so the words shown are
    // as short as they can be while still picking one declaration out.
    let index: AliasIndex = ws.alias_index();

    let rows: Vec<serde_json::Value> = ws
        .def_names()
        .map(|name| {
            let shape = ws.shape_hash(name);
            json!({
                "name": name.to_string(),
                "kind": ws.def(name).map(|d| d.kind_name()),
                "address": shape.map(|s| s.to_string()),
                "words": shape.and_then(|s| index.alias_for(&s)),
            })
        })
        .collect();

    out.result(json!(rows), || {
        for row in &rows {
            let words = row["words"].as_str().unwrap_or("");
            out.say(format!(
                "  {:<28} {:<10} {}",
                row["name"].as_str().unwrap_or(""),
                row["kind"].as_str().unwrap_or(""),
                words
            ));
        }
    });
}

fn sigs(project: &Project, out: &Output) {
    let ws = &project.workspace;
    let rows: Vec<serde_json::Value> = ws
        .sig_names()
        .map(|name| {
            let sig = ws.sig(name).expect("listed");
            let proofs = project.registry.proofs_of(name).len();
            json!({
                "name": name.to_string(),
                "type": unitype_core::canon::canonical_string(&sig.function_type()),
                "refines": sig.refines.as_ref().map(ToString::to_string),
                "refined_by": ws.refiners_of(name).iter().map(ToString::to_string).collect::<Vec<_>>(),
                "has_body": sig.body.is_some(),
                "proofs": proofs,
                "address": ws.sig_hash(name).map(|h| h.to_string()),
            })
        })
        .collect();

    out.result(json!(rows), || {
        for row in &rows {
            let name = row["name"].as_str().unwrap_or("");
            let proofs = row["proofs"].as_u64().unwrap_or(0);
            out.say(format!(
                "  {:<28} {} proof{}{}",
                name,
                proofs,
                plural(proofs as usize),
                if row["has_body"].as_bool() == Some(true) {
                    "  (proves itself)"
                } else {
                    ""
                }
            ));
            if let Some(refines) = row["refines"].as_str() {
                out.say(format!("      strengthens {refines}"));
            }
        }
    });
}

fn impls(project: &Project, out: &Output) {
    let rows: Vec<serde_json::Value> = project
        .registry
        .implementations()
        .iter()
        .map(|i| {
            json!({
                "id": i.id.to_string(),
                "proves": i.proves.to_string(),
                "environment": i.environment.to_string(),
                "entry": i.entry,
                "cost": i.cost.to_string(),
                "notes": i.notes,
            })
        })
        .collect();

    out.result(json!(rows), || {
        for row in &rows {
            out.say(format!(
                "  {:<28} {:<20} {:<10} {}",
                row["id"].as_str().unwrap_or(""),
                row["proves"].as_str().unwrap_or(""),
                row["environment"].as_str().unwrap_or(""),
                row["cost"].as_str().unwrap_or(""),
            ));
        }
    });
}

fn links(project: &Project, out: &Output) {
    let rows: Vec<serde_json::Value> = project
        .workspace
        .workspace()
        .links
        .iter()
        .map(|l| {
            json!({
                "name": l.name.to_string(),
                "from": l.from.to_string(),
                "to": l.to.to_string(),
                "kind": match &l.kind {
                    unitype_core::LinkKind::Identical => "identical",
                    unitype_core::LinkKind::FieldMap { .. } => "field map",
                    unitype_core::LinkKind::Witness { .. } => "witnessed",
                },
                "lossy": l.is_lossy(),
            })
        })
        .collect();

    out.result(json!(rows), || {
        for row in &rows {
            out.say(format!(
                "  {} -> {}  ({}{})",
                row["from"].as_str().unwrap_or(""),
                row["to"].as_str().unwrap_or(""),
                row["kind"].as_str().unwrap_or(""),
                if row["lossy"].as_bool() == Some(true) {
                    ", loses fields"
                } else {
                    ""
                }
            ));
        }
    });
}

fn environments(project: &Project, out: &Output) {
    let rows: Vec<serde_json::Value> = project
        .registry
        .environments()
        .map(|e| {
            json!({
                "id": e.id.to_string(),
                "language": e.language.to_string(),
                "reach": e.reach.kind(),
                "doc": e.doc,
            })
        })
        .collect();

    out.result(json!(rows), || {
        for row in &rows {
            out.say(format!(
                "  {:<12} {:<12} {}",
                row["id"].as_str().unwrap_or(""),
                row["language"].as_str().unwrap_or(""),
                row["reach"].as_str().unwrap_or(""),
            ));
        }
    });
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn first_sentence(text: &str) -> String {
    let single = text.trim().replace('\n', " ");
    match single.find(". ") {
        Some(at) => single[..=at].to_string(),
        None => single,
    }
}
