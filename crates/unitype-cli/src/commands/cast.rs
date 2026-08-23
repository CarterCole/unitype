//! `unitype cast` — read a value here, write it out there.
//!
//! The point of the whole system in one command: data goes in as a literal in
//! one form and comes out as a literal in another, having been checked in
//! between. Nothing is translated by pattern-matching on text; the value is
//! read into the domain against its type and written back out from there.

use std::path::PathBuf;

use serde_json::json;

use super::{Output, named_type, read_one};
use crate::Failure;
use crate::project::Project;

/// The profile name behind a short one.
fn canonical_language(lang: &str) -> &str {
    match lang {
        "ts" => "typescript",
        "py" => "python",
        "rs" => "rust",
        other => other,
    }
}

/// Reads a value as a type and writes it as a literal in some language.
pub fn run(
    project: &Project,
    type_name: &str,
    data: Option<String>,
    file: Option<PathBuf>,
    lang: &str,
    out: &Output,
) -> Result<(), Failure> {
    let (name, ty) = named_type(project, type_name)?;
    let raw = read_one(data, file)?;

    let json = raw
        .to_json()
        .map_err(|e| Failure::check(format!("that data cannot be encoded: {e}")))?;
    let value = project
        .workspace
        .read(&json, &ty)
        .map_err(|e| Failure::check(format!("that is not a {name}:\n  {e}")))?;

    let written = if lang == "json" {
        project
            .workspace
            .write_json(&value, &ty)
            .map_err(|e| Failure::check(e.to_string()))?
    } else {
        // Written against the type, so a struct comes out as something that
        // language would build rather than as a bag of names.
        let profile = unitype_core::lang::profile(canonical_language(lang))
            .map_err(|e| Failure::usage(e.to_string()))?;
        let how = unitype_codegen::constructors(lang).ok_or_else(|| {
            Failure::usage(format!(
                "`{lang}` is not a language this writes. \
                 Try json, typescript, python, or rust."
            ))
        })?;
        unitype_codegen::literal::typed(&project.workspace, &profile, &how, &value, &ty)
    };

    out.result(
        json!({ "type": name.to_string(), "language": lang, "literal": written }),
        || out.say(&written),
    );
    Ok(())
}
