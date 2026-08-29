//! `unitype words` — the address of a thing, and the words that name it.

use serde_json::json;
use unitype_core::canon::{alias_words, full_alias};

use super::{Output, nearby, qualified};
use crate::Failure;
use crate::project::Project;

/// Shows a declaration or a proposition's address in every form it has one.
pub fn run(project: &Project, text: &str, out: &Output) -> Result<(), Failure> {
    let name = qualified(text, "name")?;
    let ws = &project.workspace;

    // A proposition and a declaration are both things with addresses, so
    // asking for one by name should not require knowing which it is.
    if let Some(hash) = ws.sig_hash(&name) {
        return proposition(project, &name, hash, out);
    }

    let Some(shape) = ws.shape_hash(&name) else {
        return Err(Failure::check(format!(
            "`{name}` is not declared. {}",
            nearby(ws, &name)
        )));
    };
    let short = ws
        .alias_index()
        .alias_for(&shape)
        .unwrap_or_else(|| full_alias(&shape));
    let same: Vec<String> = ws
        .defs_with_shape(&shape)
        .iter()
        .filter(|other| **other != name)
        .map(ToString::to_string)
        .collect();

    out.result(
        json!({
            "kind": "declaration",
            "name": name.to_string(),
            "address": shape.to_string(),
            "words": short,
            "full_words": alias_words(&shape).join("-"),
            "declaration_address": ws.decl_hash(&name).map(|h| h.to_string()),
            "same_shape": same,
        }),
        || {
            out.say(format!("{name}"));
            out.say("");
            out.say(format!("  address   {shape}"));
            out.say(format!("  words     {short}"));
            out.say(format!("  in full   {}", alias_words(&shape).join("-")));
            if !same.is_empty() {
                out.say("");
                out.say("  The same data, described elsewhere:");
                for other in &same {
                    out.say(format!("    {other}"));
                }
            }
        },
    );
    Ok(())
}

/// A proposition: its address, and what it goes by in each place that proves it.
///
/// The second half is the point. One thing is being named — send a request, get
/// a response — and every platform calls it something else. The address is what
/// they have in common; `curl`, `fetch` and `requests.request` are what a
/// person has to type.
fn proposition(
    project: &Project,
    name: &unitype_core::QualifiedName,
    hash: unitype_core::canon::TypeId,
    out: &Output,
) -> Result<(), Failure> {
    let ws = &project.workspace;
    let sig = ws.sig(name).expect("it has an address, so it exists");
    let short = ws
        .alias_index()
        .alias_for(&hash)
        .unwrap_or_else(|| full_alias(&hash));

    let same: Vec<String> = ws
        .sigs_with_hash(&hash)
        .iter()
        .filter(|other| **other != name)
        .map(ToString::to_string)
        .collect();

    // Sorted by language so the same question gives the same answer twice, and
    // so a person scanning for their own platform can find it.
    let mut called: Vec<_> = project
        .registry
        .proofs_of(name)
        .into_iter()
        .filter_map(|proof| {
            let environment = project.registry.environment(&proof.environment)?;
            Some((environment, proof))
        })
        .collect();
    called.sort_by(|(a, x), (b, y)| {
        (a.language.as_str(), a.id.as_str(), x.id.as_str()).cmp(&(
            b.language.as_str(),
            b.id.as_str(),
            y.id.as_str(),
        ))
    });

    let names = json!(
        called
            .iter()
            .map(|(environment, proof)| json!({
                "language": environment.language.to_string(),
                "environment": environment.id.to_string(),
                "called": proof.entry.to_string(),
                "implementation": proof.id.as_str(),
                "reach": environment.reach.kind(),
                "notes": proof.notes,
            }))
            .collect::<Vec<_>>()
    );

    out.result(
        json!({
            "kind": "proposition",
            "name": name.to_string(),
            "signature": sig.to_string(),
            "address": hash.to_string(),
            "words": short,
            "full_words": alias_words(&hash).join("-"),
            "called": names,
            "same_proposition": same,
        }),
        || {
            out.say(format!("{}::{sig}", name.universe));
            out.say("");
            out.say(format!("  address   {hash}"));
            out.say(format!("  words     {short}"));
            out.say(format!("  in full   {}", alias_words(&hash).join("-")));
            if !same.is_empty() {
                out.say("");
                out.say("  The same proposition, asked elsewhere:");
                for other in &same {
                    out.say(format!("    {other}"));
                }
            }
            if called.is_empty() {
                out.say("");
                out.say("  Nothing proves it here, so it goes by no name yet.");
                return;
            }
            out.say("");
            out.say("  Called:");
            let widest_lang = called
                .iter()
                .map(|(e, _)| e.language.as_str().len())
                .max()
                .unwrap_or(0);
            let widest_entry = called
                .iter()
                .map(|(_, p)| p.entry.as_str().len())
                .max()
                .unwrap_or(0);
            for (environment, proof) in &called {
                out.say(format!(
                    "    {:widest_lang$}  {:widest_entry$}  in {}",
                    environment.language.as_str(),
                    proof.entry.as_str(),
                    environment.id
                ));
            }
        },
    );
    Ok(())
}
