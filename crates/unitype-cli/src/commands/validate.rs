//! `unitype validate` — is this data of that type?

use std::path::PathBuf;

use serde_json::json;

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
    let value = read_one(data, file)?;

    match project.workspace.validate(&value, &ty) {
        Ok(()) => {
            out.result(json!({ "ok": true, "type": name.to_string() }), || {
                out.say(format!("This is a {name}."));
            });
            Ok(())
        }
        Err(problems) => {
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
    }
}
