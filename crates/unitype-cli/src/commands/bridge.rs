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

    let hops: Vec<Hop> = path.iter().map(|link| Hop::of(project, link)).collect();
    let lossy = path.iter().any(|l| l.is_lossy());

    // Without data, the route is the answer.
    let Some(data) = data else {
        out.result(
            json!({ "from": source.to_string(), "to": target.to_string(),
                    "hops": hops.iter().map(Hop::as_json).collect::<Vec<_>>(),
                    "lossy": lossy }),
            || {
                out.say(route(&source.to_string(), &hops));
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
            "hops": hops.iter().map(Hop::as_json).collect::<Vec<_>>(),
            "lossy": lossy,
            "value": serde_json::from_str::<serde_json::Value>(&written).unwrap_or_default(),
        }),
        || {
            out.say(route(&source.to_string(), &hops));
            out.say("");
            out.say(&written);
        },
    );
    let _ = source_ty;
    Ok(())
}

/// One step of a route, and what carries it out.
struct Hop {
    link: String,
    to: String,
    /// True when the step follows an equivalence against the way it was
    /// declared.
    backwards: bool,
    /// Where the conversion runs, and how.
    by: String,
    lossy: bool,
}

impl Hop {
    fn of(project: &Project, link: &unitype_core::Link) -> Self {
        // A link's kind decides who does the work, and a witnessed one hands it
        // to whatever proof the dispatcher would pick — which may be in another
        // language entirely. Saying so is the difference between a route you
        // can read and a route you have to guess at.
        let by = match &link.kind {
            unitype_core::LinkKind::Identical => "a cast, nothing to run".to_string(),
            unitype_core::LinkKind::FieldMap { .. } => "mapped here".to_string(),
            unitype_core::LinkKind::Equivalent { .. } => {
                "renamed and reordered here, nothing lost either way".to_string()
            }
            unitype_core::LinkKind::Witness { sig } => {
                match project.registry.proofs_of(sig).first() {
                    Some(proof) => {
                        let language = project
                            .registry
                            .environment(&proof.environment)
                            .map(|e| e.language.to_string())
                            .unwrap_or_else(|| "?".into());
                        format!("{sig} in {language}, via {}", proof.environment)
                    }
                    None => format!("{sig}, which nothing proves yet"),
                }
            }
        };
        // A step backwards is the declared link turned around, so its ends
        // are the other way round from the declaration's.
        let backwards = project
            .workspace
            .workspace()
            .link(&link.name)
            .is_some_and(|declared| declared.from != link.from);
        Self {
            link: link.name.to_string(),
            to: link.to.to_string(),
            backwards,
            by,
            lossy: link.is_lossy(),
        }
    }

    fn as_json(&self) -> serde_json::Value {
        json!({ "link": self.link, "to": self.to, "backwards": self.backwards,
                "by": self.by, "lossy": self.lossy })
    }
}

/// The route, drawn, with what carries out each step.
fn route(from: &str, hops: &[Hop]) -> String {
    if hops.is_empty() {
        return format!("{from} is already what you asked for.");
    }
    let mut drawn = format!("{from}\n");
    for hop in hops {
        drawn.push_str(&format!(
            "  --{}{}-->  {}\n      {}{}\n",
            hop.link,
            if hop.backwards { " (backwards)" } else { "" },
            hop.to,
            hop.by,
            if hop.lossy { ", dropping fields" } else { "" }
        ));
    }
    drawn.trim_end().to_string()
}
