//! `unitype infer` — what type is this data?

use std::path::PathBuf;

use serde_json::json;
use unitype_core::value::{InferOptions, infer_many_with, lift};
use unitype_core::{Ident, Universe};

use super::{Output, read_data};
use crate::Failure;
use crate::project::Project;

/// Works out the type a pile of samples all satisfy.
///
/// With `--lift` the anonymous records become named declarations, which is the
/// step that turns sample data into something you can refer to, generate code
/// for, and link to another universe.
pub fn run(
    project: &Project,
    data: Option<String>,
    file: Option<PathBuf>,
    tags: bool,
    name: Option<String>,
    out: &Output,
) -> Result<(), Failure> {
    let samples = read_data(data, file)?;
    let options = if tags {
        InferOptions::with_tags()
    } else {
        InferOptions::default()
    };
    let inferred = infer_many_with(&samples, options);

    let Some(name) = name else {
        out.result(
            json!({
                "samples": samples.len(),
                "type": serde_json::from_str::<serde_json::Value>(
                    &unitype_core::canon::canonical_string(&inferred)
                ).expect("re-reads"),
            }),
            || {
                out.say(format!(
                    "From {} sample{}:",
                    samples.len(),
                    if samples.len() == 1 { "" } else { "s" }
                ));
                out.say("");
                out.say(unitype_core::canon::pretty_string(&inferred));
            },
        );
        return Ok(());
    };

    // Lifting needs a universe to put the declarations in. A fresh one is used
    // so that nothing is written into the project by accident: what comes out
    // is a document to look at and save if it is right.
    let target = Ident::new("inferred")
        .map_err(|e| Failure::usage(format!("`inferred` is not usable as a name: {e}")))?;
    let mut universe = Universe::new(target);
    let lifted = lift(&mut universe, &name, &inferred);

    out.result(
        serde_json::from_str::<serde_json::Value>(&unitype_core::canon::canonical_string(
            &universe,
        ))
        .expect("re-reads"),
        || {
            out.say(format!(
                "From {} sample{}, {} declaration{}:",
                samples.len(),
                if samples.len() == 1 { "" } else { "s" },
                lifted.created.len(),
                if lifted.created.len() == 1 { "" } else { "s" }
            ));
            out.say("");
            out.say(unitype_core::canon::pretty_string(&universe));
            out.say("");
            out.say("Save that as a universe, and the samples become a schema.");
        },
    );
    let _ = project;
    Ok(())
}
