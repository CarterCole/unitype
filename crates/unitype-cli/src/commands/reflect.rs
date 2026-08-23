//! `unitype reflect` — the system, looking at itself.
//!
//! A declaration is a value. That sounds like a slogan until you can print one
//! and hand it back to the same validator that checks your data, which is what
//! this does: it shows a declaration as the value it is, and checks that value
//! against the type that describes declarations.
//!
//! Nothing special is happening. Types serialize to JSON, JSON reads into
//! values, values check against types — and one of the types happens to
//! describe types. The loop closes because it was never opened.

use serde_json::json;
use unitype_core::check::check_workspace;
use unitype_core::ir::{Type, TypeRef};
use unitype_core::value::Value;
use unitype_core::{Ident, Workspace, reflect};

use super::{Output, qualified};
use crate::Failure;
use crate::project::Project;

/// Shows a declaration as data, checked against the type that describes
/// declarations. With no name, shows the whole workspace the same way.
pub fn run(project: &Project, name: Option<String>, out: &Output) -> Result<(), Failure> {
    // The meta-universe is loaded beside the project's own, so that the project
    // and the description of the project are in one place and can be checked
    // against each other.
    let described = describing(project)?;

    let (subject, as_type, shown) = match &name {
        Some(text) => {
            let qualified = qualified(text, "declaration name")?;
            let Some(def) = project.workspace.def(&qualified) else {
                return Err(Failure::check(format!(
                    "`{qualified}` is not declared. {}",
                    super::nearby(&project.workspace, &qualified)
                )));
            };
            (
                qualified.to_string(),
                reflect::TYPE_DEF,
                unitype_core::canon::canonical_string(def),
            )
        }
        None => (
            project.name(),
            reflect::WORKSPACE,
            unitype_core::canon::canonical_string(project.workspace.workspace()),
        ),
    };

    let value = Value::parse_json(&shown)
        .map_err(|e| Failure::check(format!("this could not be read back as a value: {e}")))?;
    let ty = named(as_type)?;

    let problems: Vec<String> = match described.validate(&value, &ty) {
        Ok(()) => Vec::new(),
        Err(found) => found.iter().map(ToString::to_string).collect(),
    };

    out.result(
        json!({
            "subject": subject,
            "is_a": as_type,
            "checks": problems.is_empty(),
            "problems": problems,
            "value": serde_json::from_str::<serde_json::Value>(&shown).unwrap_or_default(),
        }),
        || {
            out.say(unitype_core::canon::pretty_string(
                &serde_json::from_str::<serde_json::Value>(&shown).unwrap_or_default(),
            ));
            out.say("");
            if problems.is_empty() {
                out.say(format!("That is a value of `{as_type}`, checked."));
            } else {
                out.say(format!("That does not check against `{as_type}`:"));
                for problem in &problems {
                    out.say(format!("  {problem}"));
                }
            }
        },
    );

    if problems.is_empty() {
        Ok(())
    } else {
        Err(Failure::check(String::new()))
    }
}

/// The project's universes, with the one that describes them alongside.
fn describing(project: &Project) -> Result<unitype_core::check::CheckedWorkspace, Failure> {
    let mut workspace = Workspace::of([reflect::universe().map_err(|e| {
        Failure::check(format!("the description of the system did not read: {e}"))
    })?]);
    // Only the description is needed to check a declaration; the project's own
    // universes are already checked, and loading them again could only
    // introduce a name clash with `unitype`.
    workspace.links.clear();
    let _ = project;
    check_workspace(workspace)
        .map_err(|d| Failure::check(format!("the description of the system did not check:\n{d}")))
}

fn named(qualified: &str) -> Result<Type, Failure> {
    let name: unitype_core::QualifiedName = qualified
        .parse()
        .map_err(|_| Failure::usage(format!("`{qualified}` is not a name")))?;
    Ok(Type::Ref(TypeRef::qualified(
        Ident::new(name.universe.as_str()).map_err(|e| Failure::usage(e.to_string()))?,
        Ident::new(name.name.as_str()).map_err(|e| Failure::usage(e.to_string()))?,
    )))
}
