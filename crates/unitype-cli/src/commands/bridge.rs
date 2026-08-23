//! `unitype bridge` — move a value from one universe to another.

use serde_json::json;
use unitype_core::Ident;
use unitype_runtime::{NoCalls, eval};

use super::{Output, named_type, qualified};
use crate::Failure;
use crate::project::Project;

/// Shows the route between two types, and takes a value along it.
pub async fn run(
    project: &Project,
    from: &str,
    to: &str,
    data: Option<String>,
    out: &Output,
) -> Result<(), Failure> {
    let source = qualified(from, "type name")?;
    let target = qualified(to, "type name")?;

    let Some(path) = project.workspace.bridge_path(&source, &target) else {
        return Err(Failure::check(format!(
            "nothing declared gets from `{source}` to `{target}`.\n\
             `unitype suggest` shows what might, and a link makes it so."
        )));
    };

    let hops: Vec<String> = path.iter().map(|l| l.name.to_string()).collect();
    let lossy = path.iter().any(|l| l.is_lossy());

    // Without data, the route is the answer.
    let Some(data) = data else {
        out.result(
            json!({ "from": source.to_string(), "to": target.to_string(),
                    "hops": hops, "lossy": lossy }),
            || {
                out.say(route(
                    &source.to_string(),
                    &target.to_string(),
                    path.as_slice(),
                ));
                if lossy {
                    out.say("");
                    out.say("Fields are dropped along the way.");
                }
            },
        );
        return Ok(());
    };

    let (_, source_ty) = named_type(project, from)?;
    let (_, target_ty) = named_type(project, to)?;

    let value = project
        .workspace
        .read_json(&data, &source_ty)
        .map_err(|e| Failure::check(format!("that is not a {source}:\n  {e}")))?;

    // The route composes into one ordinary body, which then runs like any
    // other: no special path for a conversion.
    let bridge = project
        .workspace
        .compose_bridge(&source, &target, Ident::literal("bridge"))
        .ok_or_else(|| Failure::check("no route".to_string()))?
        .map_err(|e| Failure::check(e.to_string()))?;

    let body = bridge
        .body
        .as_ref()
        .ok_or_else(|| Failure::check("the route composed to nothing to run".to_string()))?;
    let scope = bridge
        .params
        .iter()
        .map(|p| p.name.clone())
        .zip([value])
        .collect();

    let moved = eval(&project.workspace, body, scope, &NoCalls)
        .await
        .map_err(|e| Failure::run(e.to_string()))?;

    let written = project
        .workspace
        .write_json(&moved, &target_ty)
        .map_err(|e| Failure::check(format!("what came out is not a {target}:\n  {e}")))?;

    out.result(
        json!({
            "from": source.to_string(),
            "to": target.to_string(),
            "hops": hops,
            "lossy": lossy,
            "value": serde_json::from_str::<serde_json::Value>(&written).unwrap_or_default(),
        }),
        || {
            out.say(route(
                &source.to_string(),
                &target.to_string(),
                path.as_slice(),
            ));
            out.say("");
            out.say(&written);
        },
    );
    let _ = source_ty;
    Ok(())
}

/// The route, drawn.
fn route(from: &str, to: &str, path: &[&unitype_core::Link]) -> String {
    if path.is_empty() {
        return format!("{from} is already {to}.");
    }
    let mut drawn = String::from(from);
    for link in path {
        drawn.push_str(&format!(" --{}-> {}", link.name, link.to));
    }
    drawn
}
