//! `unitype words` — the address of a declaration, and the words that name it.

use serde_json::json;
use unitype_core::canon::{alias_words, full_alias};

use super::{Output, nearby, qualified};
use crate::Failure;
use crate::project::Project;

/// Shows a declaration's address in every form it has one.
pub fn run(project: &Project, text: &str, out: &Output) -> Result<(), Failure> {
    let name = qualified(text, "declaration name")?;
    let ws = &project.workspace;

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
