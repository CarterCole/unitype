//! `unitype classes` — which declarations are the same thing?

use serde_json::json;

use super::Output;
use crate::Failure;
use crate::project::Project;

/// Shows the sets of declarations the project's links say are one concept.
pub fn run(project: &Project, out: &Output) -> Result<(), Failure> {
    let classes = project.workspace.equivalence_classes();

    out.result(
        json!(
            classes
                .iter()
                .map(|c| json!({
                    "members": c.members.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "lossless": c.lossless,
                }))
                .collect::<Vec<_>>()
        ),
        || {
            if classes.is_empty() {
                out.say("Nothing here is linked to anything yet.");
                out.say("Try `unitype suggest` to see what might be.");
                return;
            }
            for class in &classes {
                out.say(format!(
                    "{}{}",
                    class.members[0],
                    if class.lossless {
                        ""
                    } else {
                        "   (something is lost going round)"
                    }
                ));
                for member in &class.members[1..] {
                    out.say(format!("  = {member}"));
                }
                out.say("");
            }
        },
    );
    Ok(())
}
