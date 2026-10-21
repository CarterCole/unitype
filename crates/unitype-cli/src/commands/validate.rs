//! `unitype validate` — is this data of that type?
//!
//! The data is read *against* the type, the same way `cast`, `run` and
//! `conform` take data in. JSON has no tuples and no maps with keys that are
//! not strings, so `[1, 2]` only becomes a pair, and `{"a": 1}` a map, once
//! something says that is what they are. Checking the data without the type
//! would answer a different question: whether JSON's own reading of it already
//! fits, which for a tuple it never does.

use std::path::PathBuf;

use serde_json::json;
use unitype_core::value::ReadError;

use super::{Output, named_type, read_one};
use crate::Failure;
use crate::project::Project;

/// Checks a value against a type, saying exactly where it falls short.
pub fn run(
    project: &Project,
    type_name: &str,
    data: Option<String>,
    file: Option<PathBuf>,
    out: &Output,
) -> Result<(), Failure> {
    let (name, ty) = named_type(project, type_name)?;
    let raw = read_one(data, file)?;
    let json = raw
        .to_json()
        .map_err(|e| Failure::check(format!("that data cannot be encoded: {e}")))?;

    match project.workspace.read(&json, &ty) {
        Ok(_) => {
            out.result(json!({ "ok": true, "type": name.to_string() }), || {
                out.say(format!("This is a {name}."));
            });
            Ok(())
        }
        Err(ReadError::Invalid(problems)) => {
            let listed: Vec<serde_json::Value> = problems
                .iter()
                .map(|p| {
                    json!({
                        "path": p.path.to_string(),
                        "detail": p.detail,
                        "expected": p.expected,
                        "found": p.found,
                    })
                })
                .collect();

            if out.is_machine() {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "ok": false,
                        "type": name.to_string(),
                        "problems": listed,
                    }))
                    .expect("encodes")
                );
                return Err(Failure::check(String::new()));
            }
            Err(Failure::check(format!(
                "This is not a {name}:\n{}",
                problems
                    .iter()
                    .map(|p| format!("  {p}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )))
        }
        Err(other) => Err(Failure::check(format!("This is not a {name}: {other}"))),
    }
}
